//! `zellij`-shaped trace shim used by `tests/integration/backend/zellij.rs`.
//!
//! Writes one line per invocation to the file at `$RIMZ_TEST_ZELLIJ_LOG` of
//! the form `argv0\targv1\t...\n`, then returns a small zellij-shaped response.
//! Tests set `$RIMZ_TEST_ZELLIJ_MODE` for failure modes that need more than the
//! default trace.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let log_path = env::var_os("RIMZ_TEST_ZELLIJ_LOG").expect("RIMZ_TEST_ZELLIJ_LOG unset");
    let log_path = std::path::PathBuf::from(log_path);
    let args = env::args().collect::<Vec<_>>();
    let line = args.join("\t");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open trace log");
    writeln!(file, "{line}").expect("write trace line");

    let cli = &args[1..];
    if cli.first().is_some_and(|arg| arg == "--version") {
        write_stdout("zellij 0.44.3");
        return;
    }

    if cli.first().is_some_and(|arg| arg == "list-sessions") {
        let scripted = env::var("RIMZ_TEST_ZELLIJ_LIST_SESSIONS").ok();
        let suppress_created = env::var_os("RIMZ_TEST_ZELLIJ_DISABLE_CREATED_SESSIONS").is_some()
            || birth_mode_fails(&trace_mode(&log_path));
        let created = if suppress_created {
            Vec::new()
        } else {
            created_sessions(&log_path)
        };
        if scripted.is_some() || !created.is_empty() {
            let output = merge_list_sessions(scripted.as_deref().unwrap_or(""), &created);
            write_stdout_raw(&output);
            return;
        }
        write_stderr("No active zellij sessions found.");
        std::process::exit(1);
    }

    if cli.first().is_some_and(|arg| arg == "web") {
        match cli.get(1).map(String::as_str) {
            Some("--help") => {
                write_stdout("zellij-web");
                return;
            }
            Some("--status") => {
                let status = web_status_output(&log_path);
                write_stdout_raw(&status);
                return;
            }
            Some("--list-tokens") => {
                if let Ok(tokens) = env::var("RIMZ_TEST_ZELLIJ_WEB_TOKENS") {
                    write_stdout_raw(&tokens);
                }
                return;
            }
            Some("--start")
            | Some("--stop")
            | Some("--create-token")
            | Some("--create-read-only-token")
            | Some("--revoke-token")
            | Some("--revoke-all-tokens") => {
                if matches!(
                    cli.get(1).map(String::as_str),
                    Some("--create-token" | "--create-read-only-token")
                ) && cli.len() > 2
                {
                    write_stderr(&format!(
                        "error: The argument '{}' cannot be used with one or more of the other specified arguments",
                        cli[1]
                    ));
                    std::process::exit(1);
                }
                if cli.get(1).is_some_and(|arg| arg == "--start")
                    && let Ok(stdout) = env::var("RIMZ_TEST_ZELLIJ_WEB_START_STDOUT")
                {
                    write_stdout_raw(&stdout);
                }
                if (cli.get(1).is_some_and(|arg| arg == "--create-token")
                    || cli
                        .get(1)
                        .is_some_and(|arg| arg == "--create-read-only-token"))
                    && let Ok(stdout) = env::var("RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN")
                {
                    write_stdout_raw(&stdout);
                }
                return;
            }
            _ => {}
        }
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "action" && window[1] == "list-clients")
    {
        if let Ok(output) = env::var("RIMZ_TEST_ZELLIJ_LIST_CLIENTS") {
            write_stdout_raw(&output);
        }
        return;
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "action" && window[1] == "list-panes")
    {
        if let Ok(fail_after) = env::var("RIMZ_TEST_ZELLIJ_LIST_PANES_FAIL_AFTER")
            && command_count(&log_path, "action\tlist-panes") > fail_after.parse().unwrap_or(0)
        {
            write_stdout("There is no active session!");
            return;
        }
        if let Ok(output) = env::var("RIMZ_TEST_ZELLIJ_LIST_PANES") {
            write_stdout_raw(&output);
        }
        return;
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "--name" && window[1] == "rimz:share_session")
    {
        if let Some(session) = arg_after(cli, "--session") {
            write_web_clients_allowed_metadata(session);
        }
        return;
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "--name" && window[1] == "rimz:dump_topology")
    {
        if let Some(session) = arg_after(cli, "--session")
            && let Some(configuration) = arg_after(cli, "--plugin-configuration")
            && let Some(workspace_id) = configuration_value(configuration, "workspace_id")
        {
            write_topology_cache(session, workspace_id);
        }
        return;
    }

    let mode = trace_mode(&log_path);
    if mode == "fail-write"
        && cli
            .windows(2)
            .any(|window| window[0] == "action" && window[1].starts_with("write"))
    {
        write_stderr("simulated zellij write failure");
        std::process::exit(7);
    }
    if mode == "socket-overflow-on-birth"
        && cli.first().is_some_and(|arg| arg == "attach")
        && cli.get(1).is_some_and(|arg| arg == "--create-background")
    {
        write_stderr("failed to bind socket: File name too long");
        std::process::exit(5);
    }
    if mode == "birth-fails"
        && cli.first().is_some_and(|arg| arg == "attach")
        && cli.get(1).is_some_and(|arg| arg == "--create-background")
    {
        write_stderr("simulated zellij birth failure");
        std::process::exit(5);
    }
}

fn created_sessions(log_path: &Path) -> Vec<String> {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for line in log.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        for window in fields.windows(3) {
            if window[0] == "attach" && window[1] == "--create-background" {
                let session = window[2];
                if !sessions.iter().any(|seen| seen == session) {
                    sessions.push(session.to_owned());
                }
            }
        }
    }
    sessions
}

fn mode_path(log_path: &Path) -> std::path::PathBuf {
    log_path.with_extension("mode")
}

fn trace_mode(log_path: &Path) -> String {
    mode_from_log_path(log_path)
        .or_else(|| {
            env::var("RIMZ_TEST_ZELLIJ_MODE")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| std::fs::read_to_string(mode_path(log_path)).ok())
        .unwrap_or_default()
}

fn birth_mode_fails(mode: &str) -> bool {
    matches!(mode, "socket-overflow-on-birth" | "birth-fails")
}

fn mode_from_log_path(log_path: &Path) -> Option<String> {
    let name = log_path.file_name()?.to_str()?;
    ["socket-overflow-on-birth", "birth-fails", "fail-write"]
        .into_iter()
        .find(|mode| name.contains(mode))
        .map(str::to_owned)
}

fn merge_list_sessions(scripted: &str, created: &[String]) -> String {
    let mut output = scripted.to_owned();
    for session in created {
        if !scripted.lines().any(|line| line.starts_with(session)) {
            output.push_str(session);
            output.push_str(" [Created 0s ago]\n");
        }
    }
    output
}

fn configuration_value<'a>(configuration: &'a str, key: &str) -> Option<&'a str> {
    configuration.split(',').find_map(|entry| {
        let (candidate, value) = entry.split_once('=')?;
        (candidate == key && !value.is_empty()).then_some(value)
    })
}

fn write_topology_cache(session: &str, workspace_id: &str) {
    let Some(runtime_root) = env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let path = std::path::PathBuf::from(runtime_root)
        .join("rimz")
        .join(workspace_id)
        .join("pane-topology.json");
    let panes = match env::var("RIMZ_TEST_ZELLIJ_TOPOLOGY_PANES")
        .or_else(|_| env::var("RIMZ_TEST_ZELLIJ_LIST_PANES"))
    {
        Ok(raw) => raw,
        Err(_) => return,
    };
    let panes: serde_json::Value = match serde_json::from_str(&panes) {
        Ok(value) => value,
        Err(_) => return,
    };
    let topology = serde_json::json!({
        "session_name": session,
        "produced_at_ms": now_ms(),
        "panes": panes,
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fake topology dir");
    }
    std::fs::write(
        path,
        serde_json::to_vec(&topology).expect("serialize fake topology"),
    )
    .expect("write fake topology");
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

fn write_web_clients_allowed_metadata(session: &str) {
    let path = cache_home()
        .join("zellij")
        .join("contract_version_1")
        .join("session_info")
        .join(session);
    std::fs::create_dir_all(&path).expect("create fake session metadata dir");
    std::fs::write(
        path.join("session-metadata.kdl"),
        "web_clients_allowed true\n",
    )
    .expect("write fake session metadata");
}

fn cache_home() -> std::path::PathBuf {
    env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache")))
        .unwrap_or_else(env::temp_dir)
}

fn web_status_output(log_path: &Path) -> String {
    if command_seen(log_path, "web\t--start")
        && let Ok(after_start) = env::var("RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START")
    {
        return after_start;
    }
    env::var("RIMZ_TEST_ZELLIJ_WEB_STATUS")
        .unwrap_or_else(|_| "Web server is offline, checked: http://127.0.0.1:8082".to_owned())
}

fn command_seen(log_path: &Path, needle: &str) -> bool {
    command_count(log_path, needle) > 0
}

fn command_count(log_path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log_path)
        .map(|log| log.lines().filter(|line| line.contains(needle)).count())
        .unwrap_or(0)
}

fn write_stdout(line: &str) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}").expect("write zellij trace stdout");
}

fn write_stdout_raw(text: &str) {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{text}").expect("write zellij trace stdout");
}

fn write_stderr(line: &str) {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{line}").expect("write zellij trace stderr");
}
