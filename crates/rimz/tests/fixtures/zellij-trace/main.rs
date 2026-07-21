//! Stateful `zellij`-shaped trace shim used across the integration suites.
//!
//! Writes one `argv0\targv1\t...\n` line per invocation to `$RIMZ_TEST_ZELLIJ_LOG`, then returns a small zellij-shaped response or applies stateful filesystem side effects.
//! Tests set `$RIMZ_TEST_ZELLIJ_MODE` for injected write and session-birth failures.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

enum Invocation<'a> {
    Version,
    ListSessions,
    ActionQuery(ActionQuery),
    PresenceBoot {
        session: Option<&'a str>,
        configuration: Option<&'a str>,
    },
    DumpTopology {
        session: Option<&'a str>,
        configuration: Option<&'a str>,
    },
    Write,
    Birth,
    Unhandled,
}

enum ActionQuery {
    Clients,
    Panes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureMode {
    Normal,
    FailWrite,
    FailEnter,
    SocketOverflowOnBirth,
    BirthFails,
}

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
    match classify_invocation(cli) {
        Invocation::Version => write_stdout("zellij 0.44.3"),
        Invocation::ListSessions => handle_list_sessions(&log_path),
        Invocation::ActionQuery(query) => handle_action_query(query),
        Invocation::PresenceBoot {
            session,
            configuration,
        } => handle_presence_boot(&log_path, session, configuration),
        Invocation::DumpTopology {
            session,
            configuration,
        } => handle_dump_topology(&log_path, session, configuration),
        Invocation::Write => handle_write(&log_path, cli),
        Invocation::Birth => handle_birth(&log_path),
        Invocation::Unhandled => {}
    }
}

fn classify_invocation(cli: &[String]) -> Invocation<'_> {
    if let Some(invocation) = classify_leading_invocation(cli) {
        return invocation;
    }
    classify_nested_invocation(cli).unwrap_or(Invocation::Unhandled)
}

fn classify_leading_invocation(cli: &[String]) -> Option<Invocation<'_>> {
    if cli.first().is_some_and(|arg| arg == "--version") {
        return Some(Invocation::Version);
    }
    if cli.first().is_some_and(|arg| arg == "list-sessions") {
        return Some(Invocation::ListSessions);
    }
    None
}

fn classify_nested_invocation(cli: &[String]) -> Option<Invocation<'_>> {
    if has_pair(cli, "action", "list-clients") {
        return Some(Invocation::ActionQuery(ActionQuery::Clients));
    }
    if has_pair(cli, "action", "list-panes") {
        return Some(Invocation::ActionQuery(ActionQuery::Panes));
    }
    if has_pair(cli, "--name", "rimz_presence_boot") {
        return Some(Invocation::PresenceBoot {
            session: arg_after(cli, "--session"),
            configuration: arg_after(cli, "--plugin-configuration"),
        });
    }
    if has_pair(cli, "--name", "rimz:dump_topology") {
        return Some(Invocation::DumpTopology {
            session: arg_after(cli, "--session"),
            configuration: arg_after(cli, "--plugin-configuration"),
        });
    }
    if has_prefixed_pair(cli, "action", "write") {
        return Some(Invocation::Write);
    }
    if has_pair_at_start(cli, "attach", "--create-background") {
        return Some(Invocation::Birth);
    }
    None
}

fn handle_list_sessions(log_path: &Path) {
    let scripted = env::var("RIMZ_TEST_ZELLIJ_LIST_SESSIONS").ok();
    let suppress_created = env::var_os("RIMZ_TEST_ZELLIJ_DISABLE_CREATED_SESSIONS").is_some()
        || trace_mode(log_path).birth_fails();
    let created = if suppress_created {
        Vec::new()
    } else {
        created_sessions(log_path)
    };
    if scripted.is_some() || !created.is_empty() {
        let output = merge_list_sessions(scripted.as_deref().unwrap_or(""), &created);
        write_stdout_raw(&output);
        return;
    }
    write_stderr("No active zellij sessions found.");
    std::process::exit(1);
}

fn handle_action_query(query: ActionQuery) {
    match query {
        ActionQuery::Clients => write_env_raw("RIMZ_TEST_ZELLIJ_LIST_CLIENTS"),
        ActionQuery::Panes => write_env_raw("RIMZ_TEST_ZELLIJ_LIST_PANES"),
    }
}

fn handle_presence_boot(log_path: &Path, session: Option<&str>, configuration: Option<&str>) {
    if let (Some(session), Some(configuration)) = (session, configuration) {
        let mut configurations = read_presence_configurations(log_path);
        configurations.insert(session.to_owned(), configuration.to_owned());
        std::fs::write(
            presence_configurations_path(log_path),
            serde_json::to_vec(&configurations).expect("serialize presence configurations"),
        )
        .expect("write presence configurations");
    }
}

fn handle_dump_topology(log_path: &Path, session: Option<&str>, configuration: Option<&str>) {
    let recorded =
        session.and_then(|session| read_presence_configurations(log_path).remove(session));
    if let Some(session) = session
        && let Some(configuration) = configuration.or(recorded.as_deref())
        && let Some(workspace_id) = configuration_value(configuration, "workspace_id")
    {
        write_topology_cache(session, workspace_id);
    }
}

fn presence_configurations_path(log_path: &Path) -> std::path::PathBuf {
    log_path.with_extension("presence.json")
}

fn read_presence_configurations(log_path: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read(presence_configurations_path(log_path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn handle_write(log_path: &Path, args: &[String]) {
    match trace_mode(log_path) {
        FailureMode::FailWrite => {
            write_stderr("simulated zellij write failure");
            std::process::exit(7);
        }
        FailureMode::FailEnter
            if has_pair(args, "action", "write") && args.last().is_some_and(|arg| arg == "13") =>
        {
            write_stderr("simulated zellij Enter failure");
            std::process::exit(7);
        }
        FailureMode::Normal
        | FailureMode::FailEnter
        | FailureMode::SocketOverflowOnBirth
        | FailureMode::BirthFails => {}
    }
}

fn handle_birth(log_path: &Path) {
    match trace_mode(log_path) {
        FailureMode::SocketOverflowOnBirth => {
            write_stderr("failed to bind socket: File name too long");
            std::process::exit(5);
        }
        FailureMode::BirthFails => {
            write_stderr("simulated zellij birth failure");
            std::process::exit(5);
        }
        FailureMode::Normal | FailureMode::FailWrite | FailureMode::FailEnter => {}
    }
}

fn has_pair(args: &[String], first: &str, second: &str) -> bool {
    args.windows(2)
        .any(|window| window[0] == first && window[1] == second)
}

fn has_prefixed_pair(args: &[String], first: &str, second_prefix: &str) -> bool {
    args.windows(2)
        .any(|window| window[0] == first && window[1].starts_with(second_prefix))
}

fn has_pair_at_start(args: &[String], first: &str, second: &str) -> bool {
    args.first().is_some_and(|arg| arg == first) && args.get(1).is_some_and(|arg| arg == second)
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

fn trace_mode(log_path: &Path) -> FailureMode {
    mode_from_log_path(log_path)
        .or_else(|| {
            env::var("RIMZ_TEST_ZELLIJ_MODE")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| FailureMode::from(value.as_str()))
        })
        .or_else(|| {
            std::fs::read_to_string(mode_path(log_path))
                .ok()
                .map(|value| FailureMode::from(value.as_str()))
        })
        .unwrap_or(FailureMode::Normal)
}

fn mode_from_log_path(log_path: &Path) -> Option<FailureMode> {
    let name = log_path.file_name()?.to_str()?;
    [
        "socket-overflow-on-birth",
        "birth-fails",
        "fail-write",
        "fail-enter",
    ]
    .into_iter()
    .find(|mode| name.contains(mode))
    .map(FailureMode::from)
}

impl FailureMode {
    fn birth_fails(self) -> bool {
        matches!(self, Self::SocketOverflowOnBirth | Self::BirthFails)
    }
}

impl From<&str> for FailureMode {
    fn from(value: &str) -> Self {
        match value {
            "fail-write" => Self::FailWrite,
            "fail-enter" => Self::FailEnter,
            "socket-overflow-on-birth" => Self::SocketOverflowOnBirth,
            "birth-fails" => Self::BirthFails,
            _ => Self::Normal,
        }
    }
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

fn write_env_raw(name: &str) {
    if let Ok(output) = env::var(name) {
        write_stdout_raw(&output);
    }
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
