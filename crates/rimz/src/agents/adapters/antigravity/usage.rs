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
    LoggedOut,
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
            Self::LoggedOut => AccountProbe::LoggedOut,
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
            Self::Usage(Quota::Absent) | Self::LoggedOut => {
                AccountUsageProbe::NoCredentials(Default::default())
            }
            Self::Usage(Quota::Invalid) | Self::Failed(_) => {
                AccountUsageProbe::Failed(Default::default())
            }
        }
    }
}

/// The read for `provider_home`, run with the login's `HOME` when it has one.
/// While account-usage fetches are offline, agy is never started.
pub(super) fn read(provider_home: Option<PathBuf>, login_home: Option<&str>) -> Read {
    if crate::agents::credits::oauth_usage_offline() {
        return Read::Failed(AccountProbeError::SourceUnavailable);
    }
    read_with(&READS, provider_home, Instant::now(), || {
        run(
            crate::agents::locate_binary(&super::ANTIGRAVITY_DESCRIPTOR),
            login_home,
            DEADLINE,
        )
    })
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
    let mut command = Command::new(binary);
    command
        .args(ARGS)
        .env(SSH_SESSION_VAR, SSH_SESSION_VALUE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(home) = login_home {
        command.env("HOME", home);
    }
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
        let logged_out = envelope
            .error
            .as_deref()
            .is_some_and(|error| error.to_ascii_lowercase().contains("authentication"));
        return if logged_out {
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
