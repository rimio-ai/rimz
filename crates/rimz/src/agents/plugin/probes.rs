//! Bounded subprocess probes for plugin-owned pull enrichment.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::agents::account::AccountProbe;
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse};
use crate::agents::{
    AccountUsageSnapshot, AgentAccount, AgentRateLimits, OauthUsageProbe, RateLimitWindow,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_STDOUT: u64 = 1024 * 1024;
const MAX_STDERR: u64 = 16 * 1024;

#[derive(Serialize)]
struct SpendRequest<'a> {
    file: &'a Path,
    cursor: Option<&'a Value>,
}

#[derive(Deserialize)]
struct SpendResponse {
    #[serde(default)]
    entries: Vec<ProbeSpendEntry>,
    #[serde(default)]
    cursor: Option<Value>,
    #[serde(default)]
    origin: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ProbeSpendEntry {
    thread_id: Option<String>,
    timestamp: String,
    model: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    cost_usd: Option<f64>,
}

#[derive(Deserialize)]
struct AccountResponse {
    plan: Option<String>,
    account_id: Option<String>,
    #[serde(default)]
    logged_out: bool,
    rate_limit_windows: Option<Vec<RateLimitWindow>>,
}

#[derive(Clone, Debug)]
pub(super) enum ProbeCheck {
    Passed(String),
    Failed(String),
}

pub(super) fn spend(
    kind: &str,
    plugin_dir: &Path,
    argv: &[String],
    path: &Path,
    resume: Option<&SpendCursor>,
) -> SpendParse {
    let request = SpendRequest {
        file: path,
        cursor: resume.and_then(|cursor| cursor.state.as_ref()),
    };
    let Some(output) = run_json(kind, "spend", plugin_dir, argv, Some(&request)) else {
        return SpendParse::default();
    };
    let response: SpendResponse = match serde_json::from_slice(&output) {
        Ok(response) => response,
        Err(err) => {
            warn!(kind, error = %err, "agent plugin spend probe returned invalid JSON");
            return SpendParse::default();
        }
    };
    let entries = response
        .entries
        .into_iter()
        .filter_map(|entry| {
            let ts_secs = crate::agents::spending::iso_to_unix_secs(&entry.timestamp)?;
            let cost_usd = entry
                .cost_usd
                .filter(|cost| cost.is_finite())
                .unwrap_or(0.0);
            Some(CachedEntry {
                ts_secs,
                cost_usd,
                input: entry.input_tokens.unwrap_or(0),
                output: entry.output_tokens.unwrap_or(0),
                cache_write: entry.cache_write.unwrap_or(0),
                cache_read: entry.cache_read.unwrap_or(0),
                message_id: None,
                request_id: None,
                dedup_key: None,
                thread_id: entry.thread_id,
                is_sidechain: false,
                has_speed: false,
                model: entry.model,
                rolled: false,
            })
        })
        .collect();
    SpendParse {
        entries,
        origin: response.origin,
        cursor: SpendCursor {
            offset: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            state: response.cursor,
        },
        unknown_models: Default::default(),
        replace_entries: false,
    }
}

pub(super) fn account(kind: &str, plugin_dir: &Path, argv: &[String]) -> AccountProbe {
    let Some(response) = account_response(kind, plugin_dir, argv) else {
        return AccountProbe::Unavailable;
    };
    if response.logged_out {
        return AccountProbe::LoggedOut;
    }
    if response.plan.is_none()
        && response.account_id.is_none()
        && response.rate_limit_windows.is_none()
    {
        return AccountProbe::LoggedOut;
    }
    AccountProbe::Found(AgentAccount {
        plan: response.plan,
        account_id: response.account_id,
        ..AgentAccount::default()
    })
}

pub(super) fn account_usage(kind: &str, plugin_dir: &Path, argv: &[String]) -> OauthUsageProbe {
    let Some(response) = account_response(kind, plugin_dir, argv) else {
        return OauthUsageProbe::Failed;
    };
    if response.logged_out
        || (response.plan.is_none()
            && response.account_id.is_none()
            && response.rate_limit_windows.is_none())
    {
        return OauthUsageProbe::NoCredentials;
    }
    OauthUsageProbe::Found(AccountUsageSnapshot {
        rate_limits: response
            .rate_limit_windows
            .map(|windows| AgentRateLimits { windows }),
        plan: response.plan,
        ..AccountUsageSnapshot::default()
    })
}

pub(super) fn account_key(kind: &str, plugin_dir: &Path, argv: &[String]) -> Option<String> {
    account_response(kind, plugin_dir, argv)?.account_id
}

pub(super) fn version(kind: &str, plugin_dir: &Path, argv: &[String]) -> Option<String> {
    let output = run_json::<Value>(kind, "version", plugin_dir, argv, None)?;
    String::from_utf8(output)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn check_spend(
    kind: &str,
    plugin_dir: &Path,
    argv: &[String],
    path: &Path,
) -> ProbeCheck {
    let request = SpendRequest {
        file: path,
        cursor: None,
    };
    let output = match run_json_diagnostic(kind, "spend", plugin_dir, argv, Some(&request)) {
        Ok(output) => output,
        Err(error) => return ProbeCheck::Failed(error),
    };
    let response: SpendResponse = match serde_json::from_slice(&output) {
        Ok(response) => response,
        Err(error) => return ProbeCheck::Failed(format!("invalid JSON response: {error}")),
    };
    if let Some(entry) = response
        .entries
        .iter()
        .find(|entry| crate::agents::spending::iso_to_unix_secs(&entry.timestamp).is_none())
    {
        return ProbeCheck::Failed(format!(
            "entry timestamp is not RFC 3339: {}",
            entry.timestamp
        ));
    }
    ProbeCheck::Passed(format!("{} entries", response.entries.len()))
}

pub(super) fn check_account(kind: &str, plugin_dir: &Path, argv: &[String]) -> ProbeCheck {
    let output = match run_json_diagnostic::<Value>(kind, "account", plugin_dir, argv, None) {
        Ok(output) => output,
        Err(error) => return ProbeCheck::Failed(error),
    };
    match serde_json::from_slice::<AccountResponse>(&output) {
        Ok(_) => ProbeCheck::Passed("canonical account response".into()),
        Err(error) => ProbeCheck::Failed(format!("invalid JSON response: {error}")),
    }
}

pub(super) fn check_version(kind: &str, plugin_dir: &Path, argv: &[String]) -> ProbeCheck {
    let output = match run_json_diagnostic::<Value>(kind, "version", plugin_dir, argv, None) {
        Ok(output) => output,
        Err(error) => return ProbeCheck::Failed(error),
    };
    match String::from_utf8(output).ok().and_then(|output| {
        output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(ToOwned::to_owned)
    }) {
        Some(version) => ProbeCheck::Passed(version),
        None => ProbeCheck::Failed("response has no non-empty UTF-8 line".into()),
    }
}

fn account_response(kind: &str, plugin_dir: &Path, argv: &[String]) -> Option<AccountResponse> {
    let output = run_json::<Value>(kind, "account", plugin_dir, argv, None)?;
    match serde_json::from_slice(&output) {
        Ok(response) => Some(response),
        Err(err) => {
            warn!(kind, error = %err, "agent plugin account probe returned invalid JSON");
            None
        }
    }
}

fn run_json<T: Serialize>(
    kind: &str,
    probe: &str,
    plugin_dir: &Path,
    argv: &[String],
    request: Option<&T>,
) -> Option<Vec<u8>> {
    match run_json_diagnostic(kind, probe, plugin_dir, argv, request) {
        Ok(output) => Some(output),
        Err(error) => {
            warn!(kind, probe, error, "agent plugin probe failed");
            None
        }
    }
}

fn run_json_diagnostic<T: Serialize>(
    _kind: &str,
    _probe: &str,
    plugin_dir: &Path,
    argv: &[String],
    request: Option<&T>,
) -> Result<Vec<u8>, String> {
    let Some(executable_arg) = argv.first() else {
        return Err("probe command is empty".into());
    };
    let executable = resolve_executable(plugin_dir, executable_arg);
    let mut command = Command::new(&executable);
    command
        .args(&argv[1..])
        .current_dir(plugin_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("did not start: {error}"))?;
    let mut stdin = child.stdin.take();
    let request_failed = request.is_some_and(|request| {
        let Some(stdin) = stdin.as_mut() else {
            return true;
        };
        serde_json::to_writer(&mut *stdin, request).is_err() || stdin.write_all(b"\n").is_err()
    });
    drop(stdin);
    if request_failed {
        let _ = child.kill();
        let _ = child.wait();
        return Err("request write failed".into());
    }
    let stdout = child.stdout.take().map(|pipe| drain(pipe, MAX_STDOUT));
    let stderr = child.stderr.take().map(|pipe| drain(pipe, MAX_STDERR));
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out after 3 seconds".into());
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait failed: {err}"));
            }
        }
    };
    let stdout = join(stdout);
    let stderr = join(stderr);
    if !status.success() {
        return Err(format!(
            "exited with {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if stdout.len() as u64 > MAX_STDOUT {
        return Err("output exceeded 1 MiB".into());
    }
    Ok(stdout)
}

pub(super) fn resolve_executable(plugin_dir: &Path, executable: &str) -> PathBuf {
    let path = Path::new(executable);
    if path.is_absolute() || path.components().count() > 1 {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            plugin_dir.join(path)
        }
    } else {
        PathBuf::from(executable)
    }
}

fn drain(pipe: impl Read + Send + 'static, limit: u64) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let _ = pipe.take(limit + 1).read_to_end(&mut output);
        output
    })
}

fn join(handle: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn spend_probe_round_trips_opaque_cursor() {
        let dir = TempDir::new().unwrap();
        let probe = dir.path().join("spend");
        fs::write(
            &probe,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"entries\":[{\"timestamp\":\"2026-01-01T00:00:00Z\",\"cost_usd\":0.5}],\"cursor\":{\"line\":1}}'\n",
        )
        .unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        let transcript = dir.path().join("session.jsonl");
        fs::write(&transcript, "one\n").unwrap();
        let parsed = spend(
            "testbot",
            dir.path(),
            &["./spend".into()],
            &transcript,
            None,
        );
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].cost_usd, 0.5);
        assert_eq!(parsed.cursor.offset, 4);
        assert_eq!(parsed.cursor.state, Some(serde_json::json!({ "line": 1 })));
    }

    #[test]
    #[cfg(unix)]
    fn account_probe_maps_identity_and_usage_windows() {
        let dir = TempDir::new().unwrap();
        let probe = dir.path().join("account");
        fs::write(
            &probe,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"plan\":\"pro\",\"account_id\":\"acct-1\",\"rate_limit_windows\":[{\"used_percentage\":42,\"duration_mins\":300}]}'\n",
        )
        .unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        let argv = vec!["./account".to_owned()];
        let AccountProbe::Found(account) = account("testbot", dir.path(), &argv) else {
            panic!("account probe should find login");
        };
        assert_eq!(account.plan.as_deref(), Some("pro"));
        assert_eq!(account.account_id.as_deref(), Some("acct-1"));

        let OauthUsageProbe::Found(usage) = account_usage("testbot", dir.path(), &argv) else {
            panic!("account usage probe should find windows");
        };
        assert_eq!(
            usage
                .rate_limits
                .and_then(|limits| limits.windows.first().cloned())
                .and_then(|window| window.used_percentage),
            Some(42)
        );
    }
}
