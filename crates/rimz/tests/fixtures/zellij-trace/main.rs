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
    Web(WebCommand<'a>),
    ActionQuery(ActionQuery),
    ShareSession(Option<&'a str>),
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

enum WebCommand<'a> {
    Help,
    Status,
    ListTokens,
    Start,
    CreateToken { flag: &'a str, has_extra_args: bool },
    OtherMutation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureMode {
    Normal,
    FailWrite,
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
        Invocation::Web(command) => handle_web(command, &log_path),
        Invocation::ActionQuery(query) => handle_action_query(query),
        Invocation::ShareSession(session) => handle_share_session(session),
        Invocation::DumpTopology {
            session,
            configuration,
        } => handle_dump_topology(session, configuration),
        Invocation::Write => handle_write(&log_path),
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
    if cli.first().is_some_and(|arg| arg == "web") {
        return classify_web_command(&cli[1..]).map(Invocation::Web);
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
    if has_pair(cli, "--name", "rimz:share_session") {
        return Some(Invocation::ShareSession(arg_after(cli, "--session")));
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

fn classify_web_command(cli: &[String]) -> Option<WebCommand<'_>> {
    let (command, rest) = cli.split_first()?;
    match command.as_str() {
        "--help" => Some(WebCommand::Help),
        "--status" => Some(WebCommand::Status),
        "--list-tokens" => Some(WebCommand::ListTokens),
        "--start" => Some(WebCommand::Start),
        "--create-token" | "--create-read-only-token" => Some(WebCommand::CreateToken {
            flag: command,
            has_extra_args: !rest.is_empty(),
        }),
        "--stop" | "--revoke-token" | "--revoke-all-tokens" => Some(WebCommand::OtherMutation),
        _ => None,
    }
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

fn handle_web(command: WebCommand<'_>, log_path: &Path) {
    match command {
        WebCommand::Help => write_stdout("zellij-web"),
        WebCommand::Status => write_stdout_raw(&web_status_output(log_path)),
        WebCommand::ListTokens => write_env_raw("RIMZ_TEST_ZELLIJ_WEB_TOKENS"),
        WebCommand::Start => write_env_raw("RIMZ_TEST_ZELLIJ_WEB_START_STDOUT"),
        WebCommand::CreateToken {
            flag,
            has_extra_args: true,
        } => {
            write_stderr(&format!(
                "error: The argument '{flag}' cannot be used with one or more of the other specified arguments"
            ));
            std::process::exit(1);
        }
        WebCommand::CreateToken {
            has_extra_args: false,
            ..
        } => write_env_raw("RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN"),
        WebCommand::OtherMutation => {}
    }
}

fn handle_action_query(query: ActionQuery) {
    match query {
        ActionQuery::Clients => write_env_raw("RIMZ_TEST_ZELLIJ_LIST_CLIENTS"),
        ActionQuery::Panes => write_env_raw("RIMZ_TEST_ZELLIJ_LIST_PANES"),
    }
}

fn handle_share_session(session: Option<&str>) {
    if let Some(session) = session {
        write_web_clients_allowed_metadata(session);
    }
}

fn handle_dump_topology(session: Option<&str>, configuration: Option<&str>) {
    if let Some(session) = session
        && let Some(configuration) = configuration
        && let Some(workspace_id) = configuration_value(configuration, "workspace_id")
    {
        write_topology_cache(session, workspace_id);
    }
}

fn handle_write(log_path: &Path) {
    if trace_mode(log_path) == FailureMode::FailWrite {
        write_stderr("simulated zellij write failure");
        std::process::exit(7);
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
        FailureMode::Normal | FailureMode::FailWrite => {}
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
    ["socket-overflow-on-birth", "birth-fails", "fail-write"]
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
    std::fs::read_to_string(log_path).is_ok_and(|log| log.lines().any(|line| line.contains(needle)))
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
