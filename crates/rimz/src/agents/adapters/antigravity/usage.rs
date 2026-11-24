//! The `agy -p /usage --output-format json` read behind both account probes.
//!
//! One print-mode run answers the login and the quota at once and spends no model
//! turn. A logged-out agy waits 60 s for a sign-in and opens a browser unless its
//! environment carries an SSH session variable and its stdin answers the code
//! prompt, so every run carries both and a deadline that kills its process group.
//! A read is reused for 60 s per provider home, whichever probe asked.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::account::{AccountProbe, AccountProbeError};
use crate::agents::context::{AgentAccount, AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::{AccountUsageProbe, AccountUsageSnapshot};
use crate::proc::{KillScope, pump_child};

const ARGS: [&str; 4] = ["-p", "/usage", "--output-format", "json"];
/// Offline took 7.3 s and a logged-in read at most 2.8 s on agy 1.5.12.
const DEADLINE: Duration = Duration::from_secs(10);
const REUSE: Duration = Duration::from_secs(60);
/// Any of agy's SSH session variables selects its URL-and-code sign-in, which
/// opens no browser; the newline on stdin then answers the code prompt.
const SSH_SESSION_VAR: &str = "SSH_CONNECTION";
pub(super) const SSH_SESSION_VALUE: &str = "127.0.0.1 0 127.0.0.1 0";
const CREDENTIAL_FILE: &str = "antigravity-oauth-token";
const FIVE_HOURS_MINS: u32 = 300;
const WEEK_MINS: u32 = 7 * 24 * 60;

type Reads = Mutex<BTreeMap<Option<PathBuf>, (Instant, Read)>>;

static READS: Reads = Mutex::new(BTreeMap::new());

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Read {
    Usage(Quota),
    /// Signed in by `GEMINI_API_KEY`, which has no quota for `/usage` to read.
    ApiKey,
    LoggedOut,
    /// A named account's read found a login with no credential file in its directory: agy
    /// took it from the OS keyring or the environment, so it is not this account's.
    ForeignLogin,
    Failed(AccountProbeError),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Quota {
    /// The envelope carried no bucket at all: no quota surface.
    Absent,
    Windows(AgentRateLimits),
    /// Buckets that fail validation or name no known window.
    Invalid,
}

impl Read {
    /// The login the read found, named by the credential file under
    /// `provider_home` where agy keeps one (keyring installs keep none).
    pub(super) fn account(&self, provider_home: Option<&Path>) -> AccountProbe {
        match self {
            Self::Usage(_) => {
                let credential = provider_home.and_then(credential_email);
                AccountProbe::Found(AgentAccount {
                    metered: Some(true),
                    credentials_updated_at_ms: credential.as_ref().and_then(|(_, at)| *at),
                    account_id: credential.map(|(email, _)| email),
                    ..AgentAccount::default()
                })
            }
            Self::ApiKey => AccountProbe::Found(AgentAccount {
                plan: Some("API key".to_owned()),
                metered: Some(false),
                ..AgentAccount::default()
            }),
            Self::LoggedOut => AccountProbe::LoggedOut,
            Self::ForeignLogin => AccountProbe::Unavailable(AccountProbeError::SourceUnavailable),
            Self::Failed(cause) => AccountProbe::Unavailable(*cause),
        }
    }

    pub(super) fn usage(&self) -> AccountUsageProbe {
        match self {
            Self::Usage(Quota::Windows(rate_limits)) => AccountUsageProbe::Found {
                identity: Default::default(),
                snapshot: AccountUsageSnapshot {
                    rate_limits: Some(rate_limits.clone()),
                    ..Default::default()
                },
            },
            Self::Usage(Quota::Absent) | Self::ApiKey | Self::LoggedOut => {
                AccountUsageProbe::NoCredentials(Default::default())
            }
            Self::Usage(Quota::Invalid) | Self::ForeignLogin | Self::Failed(_) => {
                AccountUsageProbe::Failed(Default::default())
            }
        }
    }
}

/// The read for `provider_home`, run with the login's `HOME` when it has one, inside a mount of
/// the account's directory for a named login. While account-usage fetches are offline, agy is
/// never started.
pub(super) fn read(
    provider_home: Option<PathBuf>,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Read {
    if crate::agents::credits::oauth_usage_offline() {
        return Read::Failed(AccountProbeError::SourceUnavailable);
    }
    let login_home = login_env.get("HOME").map(String::as_str);
    let binary = crate::agents::locate_binary(&super::ANTIGRAVITY_DESCRIPTOR);
    let Some((source, target)) = super::session::mounted_view(login_env) else {
        return read_with(&READS, provider_home, Instant::now(), || {
            run(binary, login_home, DEADLINE)
        });
    };
    read_with(&READS, provider_home, Instant::now(), || {
        let Some(binary) = binary else {
            return Read::Failed(AccountProbeError::SpawnFailed);
        };
        let command = mounted_command(&source, &target, binary, login_home);
        own_login(run_command(command, DEADLINE), &source)
    })
}

/// `read`, unless it found a login while `account` holds no credential file of its own.
pub(super) fn own_login(read: Read, account: &Path) -> Read {
    match read {
        Read::Usage(_) | Read::ApiKey if !holds_credential(account) => Read::ForeignLogin,
        read => read,
    }
}

impl Read {
    /// Why a named account holding no credential file may not launch on this read.
    pub(super) fn launch_refusal(&self) -> Option<&'static str> {
        match self {
            Self::LoggedOut => None,
            Self::ForeignLogin => Some(
                "agy found a login outside this account's directory (`GEMINI_API_KEY`, or the OS \
                 keyring every data directory shares), so it would run on another account",
            ),
            Self::Usage(_) | Self::ApiKey | Self::Failed(_) => Some(
                "RimZ could not confirm that agy's login is this account's: the directory holds \
                 no credential file and agy's read did not report it signed out, so it could \
                 run on another account",
            ),
        }
    }
}

pub(super) fn holds_credential(account: &Path) -> bool {
    account.join(CREDENTIAL_FILE).is_file()
}

pub(super) fn read_with(
    reads: &Reads,
    provider_home: Option<PathBuf>,
    now: Instant,
    run: impl FnOnce() -> Read,
) -> Read {
    let mut reads = reads.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, read)) = reads.get(&provider_home)
        && now.saturating_duration_since(*at) < REUSE
    {
        return read.clone();
    }
    let read = run();
    reads.insert(provider_home, (now, read.clone()));
    read
}

/// The credential file's `id_token_email` and its mtime, when the token names one.
fn credential_email(provider_home: &Path) -> Option<(String, Option<u64>)> {
    #[derive(Deserialize)]
    struct Credential {
        id_token_email: Option<String>,
    }
    let path = provider_home.join(CREDENTIAL_FILE);
    let credential: Credential = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let email = trimmed(credential.id_token_email)?;
    Some((email, crate::agents::account::file_mtime_ms(&path)))
}

pub(super) fn run(binary: Option<PathBuf>, login_home: Option<&str>, deadline: Duration) -> Read {
    let Some(binary) = binary else {
        return Read::Failed(AccountProbeError::SpawnFailed);
    };
    run_command(command(binary, login_home), deadline)
}

fn run_command(mut command: Command, deadline: Duration) -> Read {
    let observed_at = Timestamp::now();
    crate::proc::testkit::count_spawn();
    let Ok(child) = command.spawn() else {
        return Read::Failed(AccountProbeError::SpawnFailed);
    };
    let Ok(output) = pump_child(
        child,
        Some(b"\n"),
        Instant::now() + deadline,
        KillScope::Group,
    ) else {
        return Read::Failed(AccountProbeError::ReadFailed);
    };
    if output.timed_out {
        return Read::Failed(AccountProbeError::TimedOut);
    }
    classify(output.status.code(), &output.stdout, observed_at)
}

/// The read's child. agy starts the rimz plugin's MCP server for this print run too, so the
/// child drops the selection of any launch the read runs inside. It leads a session of its own:
/// in the caller's, agy reads its sign-in prompt from the caller's terminal, not from stdin.
pub(super) fn command(binary: PathBuf, login_home: Option<&str>) -> Command {
    let mut command = base_command(binary, login_home);
    command.args(ARGS);
    #[expect(
        unsafe_code,
        reason = "setsid is async-signal-safe and touches no memory of the forked child"
    )]
    // SAFETY: the closure only calls setsid(2), which is async-signal-safe.
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(drop).map_err(Into::into));
    }
    command
}

/// `command`, run with the account's directory mounted where agy reads its own.
pub(super) fn mounted_command(
    source: &Path,
    target: &Path,
    binary: PathBuf,
    login_home: Option<&str>,
) -> Command {
    let mut command = base_command(PathBuf::from("bwrap"), login_home);
    command
        .process_group(0)
        // Off the pane's terminal: agy would read its sign-in prompt there, not from stdin.
        .args(["--dev-bind", "/", "/", "--die-with-parent", "--new-session", "--bind"])
        .args([source, target])
        .arg("--")
        .arg(binary)
        .args(ARGS);
    command
}

fn base_command(program: PathBuf, login_home: Option<&str>) -> Command {
    let mut command = Command::new(program);
    command
        .env(SSH_SESSION_VAR, SSH_SESSION_VALUE)
        .env_remove(crate::mcp::launch_env::ENV_SERVE)
        .env_remove(crate::mcp::launch_env::ENV_CWD)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = login_home {
        command.env("HOME", home);
    }
    command
}

#[derive(Deserialize)]
struct Envelope {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    command: Option<CommandResult>,
}

/// The quota data stays a `Value` so that a bucket that fails its shape reads
/// as an invalid quota, never as an unreadable login.
#[derive(Deserialize)]
struct CommandResult {
    name: String,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Default, Deserialize)]
struct UsageData {
    #[serde(default)]
    groups: Vec<BucketGroup>,
}

/// Buckets stay `Value`s until their id selects them, so a bucket outside
/// the Gemini quota cannot fail the read whatever its shape.
#[derive(Deserialize)]
struct BucketGroup {
    #[serde(default)]
    buckets: Vec<Value>,
}

#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    disabled: bool,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

pub(super) fn classify(code: Option<i32>, stdout: &[u8], observed_at: Timestamp) -> Read {
    let envelope = serde_json::from_slice::<Envelope>(stdout).ok();
    if let Some(envelope) = &envelope
        && envelope.status == "ERROR"
    {
        let error = envelope.error.as_deref().unwrap_or_default();
        // agy 1.5.14 refuses `/usage` this way when `GEMINI_API_KEY` selects the session.
        if error.contains("not available with a Gemini API key") {
            return Read::ApiKey;
        }
        return if error.to_ascii_lowercase().contains("authentication") {
            Read::LoggedOut
        } else {
            Read::Failed(AccountProbeError::SourceUnavailable)
        };
    }
    if code != Some(0) {
        return Read::Failed(AccountProbeError::ExitNonZero(code));
    }
    match envelope {
        Some(Envelope {
            status,
            command: Some(command),
            ..
        }) if status == "SUCCESS" && command.name == "usage" => {
            Read::Usage(fold(command.data, observed_at))
        }
        _ => Read::Failed(AccountProbeError::UnparseableOutput),
    }
}

#[derive(Clone, Debug)]
struct UsableBucket {
    remaining: f64,
    reset: Timestamp,
}

#[derive(Default)]
struct PeriodBuckets {
    recognized: bool,
    usable: Vec<UsableBucket>,
}

fn fold(data: Option<Value>, observed_at: Timestamp) -> Quota {
    let data = match data.filter(|data| !data.is_null()) {
        None => UsageData::default(),
        Some(data) => match serde_json::from_value::<UsageData>(data) {
            Ok(data) => data,
            Err(_) => return Quota::Invalid,
        },
    };
    let buckets: Vec<Value> = data
        .groups
        .into_iter()
        .flat_map(|group| group.buckets)
        .collect();
    if buckets.is_empty() {
        return Quota::Absent;
    }
    let gemini = buckets.into_iter().filter_map(|bucket| {
        let period = gemini_period(bucket.get("id")?.as_str()?)?;
        Some((period, bucket))
    });
    rate_limits(gemini, observed_at, WindowSource::Authoritative)
        .map_or(Quota::Invalid, Quota::Windows)
}

/// The Gemini quota windows in a status line's `quota` map, keyed by bucket id.
/// A map with no valid Gemini bucket, or a `quota` that is no map, is no reading.
pub(super) fn statusline_windows(quota: Value, observed_at: Timestamp) -> Option<AgentRateLimits> {
    let Value::Object(quota) = quota else {
        return None;
    };
    let gemini = quota
        .into_iter()
        .filter_map(|(id, bucket)| Some((gemini_period(&id)?, bucket)));
    rate_limits(gemini, observed_at, WindowSource::BestEffort)
}

/// The period a Gemini bucket id names (`gemini-5h`, `gemini-weekly`). Every
/// other bucket, the "Claude and GPT models" `3p-` ones included, is ignored.
fn gemini_period(id: &str) -> Option<Period> {
    Period::from_suffix(id.trim().strip_prefix("gemini-")?)
}

/// The strictest bucket per period: least remaining, then the later reset.
/// A selected bucket of the wrong shape invalidates the whole reading.
fn rate_limits(
    buckets: impl Iterator<Item = (Period, Value)>,
    observed_at: Timestamp,
    source: WindowSource,
) -> Option<AgentRateLimits> {
    let mut periods = [PeriodBuckets::default(), PeriodBuckets::default()];
    for (period, bucket) in buckets {
        let bucket = serde_json::from_value::<Bucket>(bucket).ok()?;
        let period_buckets = &mut periods[period.index()];
        period_buckets.recognized = true;
        if bucket.disabled {
            continue;
        }
        let Some(remaining) = bucket.remaining_fraction else {
            continue;
        };
        if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
            return None;
        }
        let reset = bucket
            .reset_time
            .as_deref()
            .and_then(|value| value.parse::<Timestamp>().ok())?;
        // A bucket that has just reset says nothing of its period's next fill.
        if reset <= observed_at {
            continue;
        }
        period_buckets
            .usable
            .push(UsableBucket { remaining, reset });
    }

    let mut windows = Vec::new();
    for period in [Period::FiveHours, Period::Week] {
        let buckets = &mut periods[period.index()];
        if !buckets.recognized {
            continue;
        }
        buckets.usable.sort_by(compare_buckets);
        let (used_percentage, resets_at) = buckets.usable.first().map_or((None, None), |bucket| {
            (Some(used_percentage(bucket.remaining)), Some(bucket.reset))
        });
        windows.push(RateLimitWindow {
            scope: None,
            used_percentage,
            amount: None,
            resets_at,
            duration_mins: Some(period.duration_mins()),
            observed_at: Some(observed_at),
            source,
            lifted: false,
            fixed_period: false,
        });
    }
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

#[derive(Clone, Copy)]
enum Period {
    FiveHours,
    Week,
}

impl Period {
    fn from_suffix(suffix: &str) -> Option<Self> {
        match suffix {
            "5h" => Some(Self::FiveHours),
            "weekly" => Some(Self::Week),
            _ => None,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::FiveHours => 0,
            Self::Week => 1,
        }
    }

    const fn duration_mins(self) -> u32 {
        match self {
            Self::FiveHours => FIVE_HOURS_MINS,
            Self::Week => WEEK_MINS,
        }
    }
}

fn compare_buckets(left: &UsableBucket, right: &UsableBucket) -> Ordering {
    left.remaining
        .total_cmp(&right.remaining)
        .then_with(|| right.reset.cmp(&left.reset))
}

fn used_percentage(remaining: f64) -> u8 {
    if remaining == 0.0 {
        return 100;
    }
    ((1.0 - remaining) * 100.0).round().clamp(0.0, 99.0) as u8
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}
