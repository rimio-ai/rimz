//! Ephemeral startup checks using the broker's process and protocol paths.

use crate::config::LspServerConfig;
use crate::lsp::registry::{CrashCause, Entry, State};
use crate::lsp::{LspErr, Result, admission, server};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
pub struct Check {
    pub server: String,
    pub root: PathBuf,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Running { state: State },
    Started { version: Option<String> },
    Failed { cause: CrashCause, fix: String },
    TimedOut { cause: CrashCause, fix: String },
    Invalid { error: String, fix: String },
    Untrusted { fix: String },
}

pub fn check_startup(
    root: &Path,
    servers: &BTreeMap<String, LspServerConfig>,
    untrusted: &[String],
    entries: &[Entry],
) -> Vec<Check> {
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for name in untrusted {
            let _ = sender.send(Check {
                server: name.clone(),
                root: root.to_owned(),
                outcome: Outcome::Untrusted {
                    fix: "run rimz trust".into(),
                },
            });
        }
        for (name, config) in servers {
            if untrusted.contains(name) {
                continue;
            }
            let outcome = match admission::select_server(root, name, config) {
                Ok(false) => continue,
                Err(error) => Some(Outcome::Invalid {
                    error: error.to_string(),
                    fix: format!("fix or remove [lsp.servers.{name}]"),
                }),
                Ok(true) => entries
                    .iter()
                    .find(|entry| {
                        entry.root == root
                            && entry.server == *name
                            && entry.state.is_running()
                            && entry.broker_is_alive()
                    })
                    .map(|entry| Outcome::Running {
                        state: entry.state.clone(),
                    }),
            };
            let sender = sender.clone();
            scope.spawn(move || {
                let outcome = outcome.unwrap_or_else(|| probe(root, name, config));
                let _ = sender.send(Check {
                    server: name.clone(),
                    root: root.to_owned(),
                    outcome,
                });
            });
        }
    });
    drop(sender);
    let mut checks: Vec<_> = receiver.into_iter().collect();
    checks.sort_by(|left, right| left.server.cmp(&right.server));
    checks
}

fn rustup_fix(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        let (_, rest) = line.split_once("Unknown binary '")?;
        let (binary, rest) = rest.split_once("' in official toolchain '")?;
        let (toolchain, _) = rest.split_once('\'')?;
        let safe = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        (safe(binary) && safe(toolchain))
            .then(|| format!("rustup component add {binary} --toolchain {toolchain}"))
    })
}

fn command_fix(root: &Path, name: &str, config: &LspServerConfig) -> String {
    let command = shlex::try_join(config.command.iter().map(String::as_str))
        .unwrap_or_else(|_| format!("{:?}", config.command));
    format!(
        "run `{command}` in {} to see the full error, then fix the install or remove [lsp.servers.{name}]",
        root.display()
    )
}

fn probe(root: &Path, name: &str, config: &LspServerConfig) -> Outcome {
    let deadline = Instant::now() + TIMEOUT;
    let mut server = match server::spawn(root, config) {
        Ok(server) => server,
        Err(error) => {
            let fix = if matches!(&error, LspErr::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            {
                format!(
                    "{} not found on PATH; install it or remove [lsp.servers.{name}]",
                    config.command[0]
                )
            } else {
                command_fix(root, name, config)
            };
            return Outcome::Failed {
                cause: CrashCause {
                    at_ms: crate::utils::time::unix_now_ms(),
                    exit_code: None,
                    signal: None,
                    stderr_tail: String::new(),
                    error: Some(error.to_string()),
                },
                fix,
            };
        }
    };
    let (messages, _) = mpsc::channel();
    let params = super::initialize_params(root, config);
    // The shared spawn always pipes these handles, and neither has been taken.
    let transport = super::transport::Transport::start(
        server.child.stdout.take().expect("piped stdout"),
        server.child.stdin.take().expect("piped stdin"),
        std::sync::Arc::new(std::sync::Mutex::new(super::Settings {
            kind: config.resolved_kind(),
            options: config.init_options.clone().unwrap_or(Value::Null),
            editor_check_on_save: config.editor_check_on_save.then_some(false),
        })),
        params
            .as_ref()
            .map_or(Value::Null, |params| params["workspaceFolders"].clone()),
        messages,
    );
    std::thread::scope(|scope| {
        let (reply, initialized) = mpsc::channel();
        let (finished, shutdown) = mpsc::channel();
        scope.spawn(move || {
            let result = (|| -> Result<Value> {
                let (_, receiver) = transport.request("initialize", params?)?;
                receiver
                    .recv()
                    .map_err(|_| LspErr::Protocol("server closed".into()))?
            })();
            let started = result.is_ok();
            let _ = reply.send(result);
            if started {
                if let Ok((_, receiver)) = transport.request("shutdown", Value::Null) {
                    let _ = receiver.recv_timeout(Duration::from_millis(100));
                }
                let _ = transport.notify("exit", Value::Null);
            }
            let _ = finished.send(());
        });
        let result = initialized.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let outcome = match result {
            Ok(Ok(value)) => {
                let _ = shutdown.recv_timeout(Duration::from_millis(100));
                Outcome::Started {
                    version: value["serverInfo"]["version"].as_str().map(str::to_owned),
                }
            }
            failure => {
                let timed_out = matches!(failure, Err(mpsc::RecvTimeoutError::Timeout));
                let error = match failure {
                    Ok(Err(error)) => error.to_string(),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        format!("no initialize reply within {}s", TIMEOUT.as_secs())
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => "server closed".into(),
                    Ok(Ok(_)) => unreachable!("successful initialize returned above"),
                };
                let cause = server.crash(Some(error));
                let fix = rustup_fix(&cause.stderr_tail)
                    .unwrap_or_else(|| command_fix(root, name, config));
                if timed_out && cause.exit_code.is_none() && cause.signal.is_none() {
                    Outcome::TimedOut { cause, fix }
                } else {
                    Outcome::Failed { cause, fix }
                }
            }
        };
        // Killing before the scope joins also unblocks a write to a non-reading server.
        drop(server);
        outcome
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustup_component_fix_matches_the_incident_only() {
        assert_eq!(
            rustup_fix(
                "error: Unknown binary 'rust-analyzer' in official toolchain '1.98.1-x86_64-unknown-linux-gnu'.\n"
            ),
            Some(
                "rustup component add rust-analyzer --toolchain 1.98.1-x86_64-unknown-linux-gnu"
                    .into()
            )
        );
        assert_eq!(rustup_fix("permission denied"), None);
    }

    #[test]
    fn startup_uses_live_entries_and_validates_each_selected_server() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("marker"), "").unwrap();
        let config: LspServerConfig = serde_json::from_value(serde_json::json!({
            "command": ["sh", "-c", "touch sentinel"], "extensions": ["rs"], "root-markers": ["marker"]
        })).unwrap();
        let mut invalid = config.clone();
        invalid.command.clear();
        let pid = std::process::id();
        let entry: Entry = serde_json::from_value(serde_json::json!({
            "root": root.path(), "server": "live", "nonce": "n", "broker_pid": pid,
            "broker_start_token": crate::proc::process_start_token(pid).unwrap(), "state": "ready", "started_at_ms": 0,
            "estimate_bytes": 0, "settings_hash": "h", "request_count": 0,
            "peak_rss_kb": 0, "leases": []
        }))
        .unwrap();
        let stale = Entry {
            server: "stale".into(),
            broker_start_token: "a reused pid".into(),
            ..entry.clone()
        };
        let mut probed = config.clone();
        probed.command = vec!["sh".into(), "-c".into(), "touch stale-probed".into()];
        let checks = check_startup(
            root.path(),
            &BTreeMap::from([
                ("live".into(), config.clone()),
                ("invalid".into(), invalid),
                ("stale".into(), probed),
                ("untrusted".into(), config),
            ]),
            &["untrusted".into()],
            &[entry, stale],
        );
        assert_eq!(checks.len(), 4);
        assert!(matches!(checks[0].outcome, Outcome::Invalid { .. }));
        assert!(matches!(
            checks[1].outcome,
            Outcome::Running {
                state: State::Ready
            }
        ));
        assert!(
            matches!(checks[2].outcome, Outcome::Failed { .. }),
            "{:?}",
            checks[2]
        );
        assert!(root.path().join("stale-probed").exists());
        assert!(matches!(checks[3].outcome, Outcome::Untrusted { .. }));
        assert!(!root.path().join("sentinel").exists());
    }

    #[test]
    fn startup_timeouts_are_concurrent_and_reap_nonreading_servers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("marker"), "").unwrap();
        let servers = (0..3)
            .map(|index| {
                let name = index.to_string();
                let config = serde_json::from_value(serde_json::json!({
                    "command": ["sh", "-c", format!("echo $$ > {name}.pid; exec sleep 30")],
                    "extensions": ["rs"], "root-markers": ["marker"],
                    "init-options": {"large": "x".repeat(128 * 1024)}
                }))
                .unwrap();
                (name, config)
            })
            .collect();
        let started = std::time::Instant::now();
        let checks = check_startup(root.path(), &servers, &[], &[]);
        assert_eq!(checks.len(), 3);
        assert!(started.elapsed() < std::time::Duration::from_secs(14));
        for check in checks {
            let Outcome::TimedOut { cause, .. } = &check.outcome else {
                panic!("{check:?}");
            };
            assert_eq!(
                cause.error.as_deref(),
                Some("no initialize reply within 10s")
            );
            let pid: u32 =
                std::fs::read_to_string(root.path().join(format!("{}.pid", check.server)))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
            assert!(!crate::proc::process_is_live(pid, None));
        }
    }
}
