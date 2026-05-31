//! Codex app-server JSON-RPC client — read-only realtime enrichment.
//!
//! Codex has no statusline, so its rich per-session context (the analogue of
//! Claude's statusline `AgentContext`) is read from the official Codex
//! app-server (`codex app-server`, JSON-RPC 2.0 over stdio). This client speaks
//! only **read-only, non-interfering** methods — `initialize`/`initialized`,
//! `account/rateLimits/read`, and `model/list` — and never `thread/resume` or
//! `turn/start` (which would rejoin/own the user's live Codex thread). It is the
//! out-of-band producer behind `rimz codex refresh-context`; storage
//! ([`crate::ledger::agent_context`]) and the snapshot fold-in are
//! transport-agnostic, exactly as for Claude.
//!
//! Connection preference (warmest first): this session's broker
//! ([`crate::agents::codex_broker`]) over its unix socket — a held, already
//! handshaked `codex app-server` that amortizes the per-datapoint handshake;
//! then the per-user daemon `codex remote-control start` brings up (which
//! [`crate::remote_control`] can auto-launch), re-used via `codex app-server
//! proxy`; then a fresh cold-spawned `codex app-server`. The cold-spawn is always
//! the final fallback, so enrichment never depends on either being up (headless /
//! no-mux just cold-spawns). Set `RIMZ_CODEX_APP_SERVER_SOCK` to an empty value to
//! drop the daemon from the order.
//!
//! Why no token gauge here: as of the pinned Codex app-server, token /
//! context-window usage is exposed only on the live `thread/tokenUsage/updated`
//! notification (requires a subscribing `thread/resume`), never on a read-only
//! method. So the context gauge stays sourced from the rollout tail in
//! [`super::codex`]; this client supplies what the app-server *does* expose
//! read-only: rate-limit windows, model display name + effort, and version.
//!
//! Best-effort, never correctness: every failure maps to an omitted field or a
//! `None` record — it never fails a hook or a turn.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};

use super::context::{AgentAccount, AgentContext, AgentRateLimits, RateLimitWindow};

/// Total wall-clock budget for one refresh (spawn + handshake + reads). The
/// caller is a detached background helper with no user waiting, so this is
/// generous enough for `model/list` to fetch its catalog, but bounded so a
/// wedged app-server is killed rather than lingering.
const APP_SERVER_DEADLINE: Duration = Duration::from_secs(6);

/// Shorter budget for the daemon `proxy` probe. The proxy either bridges to a
/// live daemon promptly or it does not; a tight bound means a stale socket
/// costs little before the cold-spawn fallback takes over.
const PROXY_PROBE_DEADLINE: Duration = Duration::from_secs(2);

/// Override for the `codex` binary path (tests/tooling point this at a stub).
const CODEX_BIN_ENV: &str = "RIMZ_CODEX_BIN";

/// Override for the daemon control socket. A path re-uses that daemon via
/// `proxy`; an empty value forces the cold-spawn path (tests, opt-out).
const CODEX_APP_SERVER_SOCK_ENV: &str = "RIMZ_CODEX_APP_SERVER_SOCK";

#[derive(Debug, thiserror::Error)]
pub(crate) enum AppServerErr {
    #[error("spawning codex app-server: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("codex app-server io: {0}")]
    Io(#[source] std::io::Error),
    #[error("codex app-server timed out")]
    Timeout,
    #[error("codex app-server protocol error: {0}")]
    Protocol(String),
    #[error("codex app-server returned error {code}: {message}")]
    JsonRpc { code: i64, message: String },
    #[error("codex app-server stream closed before responding")]
    Closed,
}

/// One JSON-RPC round-trip surface. The production impl spawns `codex
/// app-server`; tests feed canned responses so the mapping is exercised without
/// a process.
pub(crate) trait JsonRpcTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), AppServerErr>;
}

/// Resolve the `codex` binary: explicit override, then `PATH`, then the bare
/// name (which `Command` resolves against `PATH` at spawn). Shared with the
/// broker ([`crate::agents::codex_broker`]) so both resolve the same binary.
pub(crate) fn codex_bin() -> PathBuf {
    if let Some(raw) = std::env::var_os(CODEX_BIN_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(raw);
    }
    which::which("codex").unwrap_or_else(|_| PathBuf::from("codex"))
}

/// Spawn a thread draining newline-framed lines from `reader` into a channel, so
/// a request can wait with its remaining deadline. Shared by both transports and
/// the broker ([`crate::agents::codex_broker`]).
pub(crate) fn spawn_frame_reader<R: BufRead + Send + 'static>(reader: R) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(std::mem::take(&mut line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Write one newline-framed JSON-RPC frame and flush.
pub(crate) fn write_frame(writer: &mut dyn Write, frame: &Value) -> Result<(), AppServerErr> {
    let mut bytes =
        serde_json::to_vec(frame).map_err(|err| AppServerErr::Protocol(err.to_string()))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).map_err(AppServerErr::Io)?;
    writer.flush().map_err(AppServerErr::Io)
}

/// Wait for the response frame matching `id`, skipping non-JSON noise,
/// server notifications, and server-initiated requests (a different id or none),
/// until `deadline`.
pub(crate) fn recv_response(
    rx: &Receiver<String>,
    deadline: Instant,
    id: i64,
) -> Result<Value, AppServerErr> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(AppServerErr::Timeout)?;
        let line = rx.recv_timeout(remaining).map_err(|err| match err {
            RecvTimeoutError::Timeout => AppServerErr::Timeout,
            RecvTimeoutError::Disconnected => AppServerErr::Closed,
        })?;
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if let Some(err) = value.get("error") {
            return Err(AppServerErr::JsonRpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// Newline-framed JSON-RPC over one byte stream, with a reader thread draining
/// frames so each request can wait with its remaining deadline. Backs two
/// sources: a spawned `codex` child (cold-spawn, or `app-server proxy --sock …`
/// bridged to the per-user daemon) and a [`UnixStream`] to the per-session broker
/// ([`crate::agents::codex_broker`]). Only the child case owns a process to reap.
pub(crate) struct FramedTransport {
    writer: Box<dyn Write + Send>,
    rx: Receiver<String>,
    next_id: i64,
    deadline: Instant,
    /// `Some` when we spawned a `codex` child — killed and reaped on drop. `None`
    /// for a unix-socket connection (dropping `writer` closes it; the broker
    /// reaps the client on EOF).
    child: Option<Child>,
}

impl FramedTransport {
    /// Spawn `bin` with `args` (e.g. `["app-server"]` or
    /// `["app-server", "proxy", "--sock", <path>]`), giving the handshake +
    /// reads `total` wall-clock.
    fn spawn(bin: &Path, args: &[String], total: Duration) -> Result<Self, AppServerErr> {
        let mut child = Command::new(bin)
            .args(args)
            // stdin/stdout are the JSON-RPC channel; stderr is diagnostics we
            // never want — null it (the fresh-stdio invariant for helpers).
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(AppServerErr::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppServerErr::Protocol("app-server stdin unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppServerErr::Protocol("app-server stdout unavailable".to_owned()))?;
        let rx = spawn_frame_reader(BufReader::new(stdout));
        Ok(Self {
            writer: Box::new(stdin),
            rx,
            next_id: 1,
            deadline: Instant::now() + total,
            child: Some(child),
        })
    }

    /// Connect to the per-session broker socket at `path`, giving the handshake +
    /// reads `total` wall-clock. The broker is warm, so this is the fast path.
    fn connect(path: &Path, total: Duration) -> Result<Self, AppServerErr> {
        let stream = UnixStream::connect(path).map_err(AppServerErr::Io)?;
        let reader = stream.try_clone().map_err(AppServerErr::Io)?;
        let rx = spawn_frame_reader(BufReader::new(reader));
        Ok(Self {
            writer: Box::new(stream),
            rx,
            next_id: 1,
            deadline: Instant::now() + total,
            child: None,
        })
    }
}

impl JsonRpcTransport for FramedTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(
            &mut self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        recv_response(&self.rx, self.deadline, id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), AppServerErr> {
        write_frame(
            &mut self.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
    }
}

impl Drop for FramedTransport {
    fn drop(&mut self) {
        // Kill+reap a spawned child so no wedged server lingers. A unix-socket
        // connection has no child; dropping `writer` closes it.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// --- wire models (tolerant: camelCase, defaulted, unknown fields ignored) ---

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    #[serde(default)]
    rate_limits: RateLimitSnapshot,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitSnapshot {
    #[serde(default)]
    primary: Option<RawWindow>,
    #[serde(default)]
    secondary: Option<RawWindow>,
    /// The account's plan tier (`plus`, `pro`, `team`, …), reported alongside
    /// the windows. Account-scoped, so the provider dashboard reads it from the
    /// freshest session and uses it to label the block + mark it metered.
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    #[serde(default)]
    used_percent: Option<i64>,
    #[serde(default)]
    resets_at: Option<i64>,
    #[serde(default)]
    window_duration_mins: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelListResponse {
    #[serde(default)]
    data: Vec<RawModel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    default_reasoning_effort: Option<String>,
}

/// A model from `model/list` matched to the session's model hint.
#[derive(Debug, Clone, PartialEq)]
struct MatchedModel {
    id: String,
    display_name: String,
    effort: Option<String>,
}

// --- client ---

pub(crate) struct CodexAppServer<T: JsonRpcTransport> {
    transport: T,
    /// The `userAgent` the server returned at `initialize`, e.g.
    /// `"rimz/0.135.0 (Ubuntu ...)"` — its version token is the Codex version.
    user_agent: Option<String>,
}

/// One way [`CodexAppServer::connect`] tries to reach an app-server, in
/// preference order.
#[derive(Debug)]
enum ConnectAttempt {
    /// Connect to the warm per-session broker over its unix socket — the fast
    /// path, on the short probe budget.
    Broker(PathBuf),
    /// Spawn a `codex` invocation: the per-user daemon via `proxy --sock …`, or
    /// a throwaway cold-spawn fallback. Carries argv (program omitted) + budget.
    Spawn(Vec<String>, Duration),
}

impl CodexAppServer<FramedTransport> {
    /// Connect to a Codex app-server and complete the initialize handshake,
    /// trying each attempt in preference order: the per-session broker socket
    /// (warm) first, then the per-user daemon via `proxy`, then a fresh
    /// cold-spawned `app-server`. The first that handshakes wins. `None` when none
    /// do (codex missing, not runnable, protocol mismatch) — best-effort.
    pub(crate) fn connect(broker_socket: Option<&Path>) -> Option<Self> {
        let bin = codex_bin();
        for attempt in connect_attempts(broker_socket) {
            let transport = match &attempt {
                ConnectAttempt::Broker(path) => {
                    FramedTransport::connect(path, PROXY_PROBE_DEADLINE)
                }
                ConnectAttempt::Spawn(args, deadline) => {
                    FramedTransport::spawn(&bin, args, *deadline)
                }
            };
            let Ok(transport) = transport else {
                continue;
            };
            let mut client = Self::new(transport);
            if client.handshake().is_ok() {
                return Some(client);
            }
        }
        None
    }
}

/// The attempts [`CodexAppServer::connect`] tries, in preference order, after
/// resolving which sockets actually exist on disk.
fn connect_attempts(broker_socket: Option<&Path>) -> Vec<ConnectAttempt> {
    attempts_for(
        broker_socket.filter(|path| path.exists()),
        daemon_socket().filter(|path| path.exists()).as_deref(),
    )
}

/// Pure core of [`connect_attempts`]: given a reachable per-session `broker`
/// socket and/or per-user `daemon` socket (each present only when it exists on
/// disk), order the attempts — broker (warm) first, then the daemon via `proxy`,
/// always followed by a cold-spawned `app-server` fallback so enrichment never
/// depends on either being up.
fn attempts_for(broker: Option<&Path>, daemon: Option<&Path>) -> Vec<ConnectAttempt> {
    let mut attempts = Vec::new();
    if let Some(broker) = broker {
        attempts.push(ConnectAttempt::Broker(broker.to_path_buf()));
    }
    if let Some(daemon) = daemon {
        attempts.push(ConnectAttempt::Spawn(
            vec![
                "app-server".to_owned(),
                "proxy".to_owned(),
                "--sock".to_owned(),
                daemon.to_string_lossy().into_owned(),
            ],
            PROXY_PROBE_DEADLINE,
        ));
    }
    attempts.push(ConnectAttempt::Spawn(
        vec!["app-server".to_owned()],
        APP_SERVER_DEADLINE,
    ));
    attempts
}

/// The daemon control socket to prefer: an explicit `RIMZ_CODEX_APP_SERVER_SOCK`
/// path, or the default `$CODEX_HOME/app-server-control/app-server-control.sock`
/// (`~/.codex/...`). An empty override means "no daemon" — cold-spawn only.
fn daemon_socket() -> Option<PathBuf> {
    match std::env::var_os(CODEX_APP_SERVER_SOCK_ENV) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
        None => Some(
            codex_home()?
                .join("app-server-control")
                .join("app-server-control.sock"),
        ),
    }
}

/// Codex's home directory: `CODEX_HOME` when set, else `~/.codex`. Mirrors the
/// resolution Codex itself uses, so the control socket — and the managed
/// standalone install [`crate::remote_control::codex_standalone_bin`] looks for —
/// are found where Codex places them.
pub(crate) fn codex_home() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(raw));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".codex"))
}

impl<T: JsonRpcTransport> CodexAppServer<T> {
    fn new(transport: T) -> Self {
        Self {
            transport,
            user_agent: None,
        }
    }

    /// `initialize` then the `initialized` acknowledgement. Every other method
    /// is rejected by the server until this completes.
    fn handshake(&mut self) -> Result<(), AppServerErr> {
        let result = self.transport.request(
            "initialize",
            json!({
                "clientInfo": { "name": "rimz", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        self.user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        self.transport.notify("initialized", json!({}))?;
        Ok(())
    }

    /// Read the account's rate-limit windows and plan tier in one call. The
    /// windows ride [`AgentRateLimits`]; the plan tier (when present) marks a
    /// metered subscription account on [`AgentAccount`]. An API-key account
    /// returns neither, so the account is left `None` and the dashboard infers
    /// the unmetered "infinite" bar.
    fn rate_limits(
        &mut self,
    ) -> Result<(Option<AgentRateLimits>, Option<AgentAccount>), AppServerErr> {
        let result = self
            .transport
            .request("account/rateLimits/read", Value::Null)?;
        let parsed: RateLimitsResponse = serde_json::from_value(result)
            .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
        let windows = bucket_windows(parsed.rate_limits.primary, parsed.rate_limits.secondary);
        let plan = parsed.rate_limits.plan_type.filter(|plan| !plan.is_empty());
        let account = (plan.is_some() || windows.is_some()).then_some(AgentAccount {
            metered: Some(true),
            plan,
        });
        Ok((windows, account))
    }

    /// Match the session's model `hint` (a raw model id from the lifecycle
    /// observation) against the catalog, returning its display name + default
    /// effort. `None` when there is no hint or no match — never a guess.
    fn matched_model(&mut self, hint: &str) -> Result<Option<MatchedModel>, AppServerErr> {
        let result = self
            .transport
            .request("model/list", json!({ "includeHidden": true }))?;
        let parsed: ModelListResponse = serde_json::from_value(result)
            .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
        Ok(parsed
            .data
            .into_iter()
            .find(|model| model.model == hint || model.id == hint)
            .map(|model| MatchedModel {
                id: if model.model.is_empty() {
                    model.id
                } else {
                    model.model
                },
                display_name: model.display_name,
                effort: model.default_reasoning_effort.filter(|e| !e.is_empty()),
            })
            .filter(|model| !model.display_name.is_empty()))
    }

    /// Read every read-only field the app-server exposes and project it onto an
    /// [`AgentContext`]. Each read is independent and best-effort: a failed
    /// `account/rateLimits/read` (e.g. API-key account) still yields the model
    /// and version. Assumes [`Self::handshake`] already ran.
    pub(crate) fn observe_context(
        &mut self,
        source: &str,
        model_hint: Option<&str>,
        observed_at: Timestamp,
    ) -> AgentContext {
        let (rate_limits, account) = self.rate_limits().unwrap_or_default();
        let model = model_hint.and_then(|hint| self.matched_model(hint).ok().flatten());
        let agent_version = self
            .user_agent
            .as_deref()
            .and_then(codex_version_from_user_agent);
        into_context(
            source,
            rate_limits,
            account,
            model,
            agent_version,
            observed_at,
        )
    }
}

/// Project the gathered read-only parts onto the transport-agnostic record.
/// Pure and deterministic so it is unit-testable from canned JSON; `observed_at`
/// is stamped by the caller. Codex has no read-only source for the session name,
/// tokens, cost, PR, thinking toggle, output style, or vim mode — those stay
/// `None`.
#[allow(clippy::too_many_arguments)]
fn into_context(
    source: &str,
    rate_limits: Option<AgentRateLimits>,
    account: Option<AgentAccount>,
    model: Option<MatchedModel>,
    agent_version: Option<String>,
    observed_at: Timestamp,
) -> AgentContext {
    AgentContext {
        source: source.to_owned(),
        session_name: None,
        model_id: model.as_ref().map(|model| model.id.clone()),
        model_display_name: model.as_ref().map(|model| model.display_name.clone()),
        effort: model.and_then(|model| model.effort),
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits,
        pr: None,
        account,
        observed_at,
    }
}

/// Route Codex's positional rate-limit windows onto Claude's named buckets so
/// the two agents share one shape. Codex reports `primary` (≈300 min = 5h) and
/// `secondary` (≈10080 min = 7d); route by `windowDurationMins` (a day or less
/// → the short window, longer → the weekly one), falling back to position when
/// the duration is absent.
fn bucket_windows(
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
) -> Option<AgentRateLimits> {
    const DAY_MINS: i64 = 24 * 60;
    let mut five_hour = None;
    let mut seven_day = None;
    for window in [primary, secondary].into_iter().flatten() {
        let mapped = RateLimitWindow {
            used_percentage: window.used_percent.map(clamp_pct),
            resets_at: window
                .resets_at
                .and_then(|secs| Timestamp::from_second(secs).ok()),
        };
        match window.window_duration_mins {
            Some(mins) if mins > DAY_MINS => seven_day = Some(mapped),
            Some(_) => five_hour = Some(mapped),
            None if five_hour.is_none() => five_hour = Some(mapped),
            None => seven_day = Some(mapped),
        }
    }
    (five_hour.is_some() || seven_day.is_some()).then_some(AgentRateLimits {
        five_hour,
        seven_day,
    })
}

fn clamp_pct(value: i64) -> u8 {
    value.clamp(0, 100) as u8
}

/// Extract the Codex version from the server's `userAgent`. The first token is
/// `"<clientName>/<version>"`; the version is what we surface. `None` when the
/// shape is unexpected.
fn codex_version_from_user_agent(user_agent: &str) -> Option<String> {
    user_agent
        .split_whitespace()
        .next()
        .and_then(|token| token.split('/').nth(1))
        .filter(|version| !version.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use serde_json::json;

    use super::*;

    /// In-memory transport: returns canned results by method and records the
    /// call order so the handshake sequence can be asserted.
    struct CannedTransport {
        results: HashMap<&'static str, Value>,
        errors: HashSet<&'static str>,
        calls: Vec<String>,
    }

    impl CannedTransport {
        fn new() -> Self {
            let mut results = HashMap::new();
            results.insert(
                "initialize",
                json!({
                    "userAgent": "rimz/0.135.0 (Ubuntu 25.4.0; x86_64) xterm-256color",
                    "codexHome": "/home/u/.codex",
                    "platformFamily": "unix",
                    "platformOs": "linux"
                }),
            );
            Self {
                results,
                errors: HashSet::new(),
                calls: Vec::new(),
            }
        }

        fn with(mut self, method: &'static str, result: Value) -> Self {
            self.results.insert(method, result);
            self
        }

        fn failing(mut self, method: &'static str) -> Self {
            self.errors.insert(method);
            self
        }
    }

    impl JsonRpcTransport for CannedTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, AppServerErr> {
            self.calls.push(method.to_owned());
            if self.errors.contains(method) {
                return Err(AppServerErr::JsonRpc {
                    code: -32000,
                    message: "boom".to_owned(),
                });
            }
            Ok(self.results.get(method).cloned().unwrap_or(Value::Null))
        }

        fn notify(&mut self, method: &str, _params: Value) -> Result<(), AppServerErr> {
            self.calls.push(format!("notify:{method}"));
            Ok(())
        }
    }

    fn rate_limits_result() -> Value {
        json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1_780_092_691_i64 },
                "secondary": { "usedPercent": 88, "windowDurationMins": 10080, "resetsAt": 1_780_186_207_i64 },
                "planType": "team"
            }
        })
    }

    fn model_list_result() -> Value {
        json!({
            "data": [
                { "id": "gpt-5.5-codex", "model": "gpt-5.5-codex", "displayName": "GPT-5.5 Codex",
                  "defaultReasoningEffort": "high", "isDefault": true, "description": "", "hidden": false,
                  "supportedReasoningEfforts": [] },
                { "id": "o4-mini", "model": "o4-mini", "displayName": "o4-mini",
                  "defaultReasoningEffort": "medium", "isDefault": false, "description": "", "hidden": false,
                  "supportedReasoningEfforts": [] }
            ]
        })
    }

    fn ts() -> Timestamp {
        Timestamp::from_second(1_780_000_000).unwrap()
    }

    #[test]
    fn handshake_initializes_then_acknowledges_before_reads() {
        let transport = CannedTransport::new()
            .with("account/rateLimits/read", rate_limits_result())
            .with("model/list", model_list_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let _ = client.observe_context("codex", Some("gpt-5.5-codex"), ts());
        // initialize, then the initialized notification, then the reads.
        assert_eq!(client.transport.calls[0], "initialize");
        assert_eq!(client.transport.calls[1], "notify:initialized");
        assert!(
            client
                .transport
                .calls
                .iter()
                .position(|c| c == "account/rateLimits/read")
                .unwrap()
                > 1
        );
    }

    #[test]
    fn maps_rate_limit_windows_to_five_hour_and_seven_day() {
        let transport =
            CannedTransport::new().with("account/rateLimits/read", rate_limits_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let ctx = client.observe_context("codex", None, ts());
        let limits = ctx.rate_limits.expect("rate limits present");
        let five = limits.five_hour.expect("five-hour window");
        let seven = limits.seven_day.expect("seven-day window");
        assert_eq!(five.used_percentage, Some(12));
        assert_eq!(seven.used_percentage, Some(88));
        assert_eq!(five.resets_at, Timestamp::from_second(1_780_092_691).ok());
        assert_eq!(seven.resets_at, Timestamp::from_second(1_780_186_207).ok());
    }

    #[test]
    fn account_plan_type_rides_the_rate_limit_read() {
        // The plan tier sits beside the windows in `account/rateLimits/read`, so
        // it lands on the context account — metered, since a tier is present.
        let transport =
            CannedTransport::new().with("account/rateLimits/read", rate_limits_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let account = client
            .observe_context("codex", None, ts())
            .account
            .expect("account from planType");
        assert_eq!(account.plan.as_deref(), Some("team"));
        assert_eq!(account.metered, Some(true));
    }

    #[test]
    fn api_key_account_leaves_account_none() {
        // No windows and no plan tier (an API-key account) means no account —
        // the dashboard then infers the unmetered "infinite" bar.
        let result = json!({ "rateLimits": {} });
        let transport = CannedTransport::new().with("account/rateLimits/read", result);
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        assert_eq!(client.observe_context("codex", None, ts()).account, None);
    }

    #[test]
    fn windows_without_duration_fall_back_to_position() {
        let result = json!({
            "rateLimits": {
                "primary": { "usedPercent": 5 },
                "secondary": { "usedPercent": 50 }
            }
        });
        let transport = CannedTransport::new().with("account/rateLimits/read", result);
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let limits = client
            .observe_context("codex", None, ts())
            .rate_limits
            .expect("rate limits");
        assert_eq!(limits.five_hour.and_then(|w| w.used_percentage), Some(5));
        assert_eq!(limits.seven_day.and_then(|w| w.used_percentage), Some(50));
    }

    #[test]
    fn used_percent_is_clamped() {
        let result = json!({
            "rateLimits": { "primary": { "usedPercent": 250, "windowDurationMins": 300 },
                            "secondary": { "usedPercent": -5, "windowDurationMins": 10080 } }
        });
        let transport = CannedTransport::new().with("account/rateLimits/read", result);
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let limits = client
            .observe_context("codex", None, ts())
            .rate_limits
            .unwrap();
        assert_eq!(limits.five_hour.unwrap().used_percentage, Some(100));
        assert_eq!(limits.seven_day.unwrap().used_percentage, Some(0));
    }

    #[test]
    fn model_hint_resolves_display_name_and_effort() {
        let transport = CannedTransport::new().with("model/list", model_list_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let ctx = client.observe_context("codex", Some("gpt-5.5-codex"), ts());
        assert_eq!(ctx.model_id.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(ctx.model_display_name.as_deref(), Some("GPT-5.5 Codex"));
        assert_eq!(ctx.effort.as_deref(), Some("high"));
        assert_eq!(ctx.agent_version.as_deref(), Some("0.135.0"));
    }

    #[test]
    fn unmatched_or_absent_model_hint_leaves_model_none() {
        let transport = CannedTransport::new().with("model/list", model_list_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let unmatched = client.observe_context("codex", Some("does-not-exist"), ts());
        assert_eq!(unmatched.model_display_name, None);
        assert_eq!(unmatched.model_id, None);

        let absent = client.observe_context("codex", None, ts());
        assert_eq!(absent.model_display_name, None);
        assert_eq!(absent.effort, None);
    }

    #[test]
    fn rate_limit_read_error_still_yields_version_and_model() {
        let transport = CannedTransport::new()
            .failing("account/rateLimits/read")
            .with("model/list", model_list_result());
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let ctx = client.observe_context("codex", Some("o4-mini"), ts());
        assert_eq!(ctx.rate_limits, None);
        assert_eq!(ctx.model_display_name.as_deref(), Some("o4-mini"));
        assert_eq!(ctx.agent_version.as_deref(), Some("0.135.0"));
        assert_eq!(ctx.source, "codex");
    }

    #[test]
    fn version_parsed_from_user_agent_token() {
        assert_eq!(
            codex_version_from_user_agent("rimz/0.135.0 (Ubuntu 25.4.0; x86_64)").as_deref(),
            Some("0.135.0")
        );
        assert_eq!(codex_version_from_user_agent("nogap").as_deref(), None);
        assert_eq!(codex_version_from_user_agent("trailing/").as_deref(), None);
    }

    /// Assert a `Spawn` attempt's argv + budget; panics on a `Broker` attempt.
    fn assert_spawn(attempt: &ConnectAttempt, args: &[&str], deadline: Duration) {
        match attempt {
            ConnectAttempt::Spawn(got_args, got_deadline) => {
                assert_eq!(
                    got_args,
                    &args.iter().map(|s| s.to_string()).collect::<Vec<_>>()
                );
                assert_eq!(*got_deadline, deadline);
            }
            ConnectAttempt::Broker(path) => panic!("expected a spawn attempt, got broker {path:?}"),
        }
    }

    #[test]
    fn attempts_cold_spawn_only_without_any_socket() {
        let attempts = attempts_for(None, None);
        assert_eq!(attempts.len(), 1, "no broker, no daemon → cold-spawn only");
        assert_spawn(&attempts[0], &["app-server"], APP_SERVER_DEADLINE);
    }

    #[test]
    fn attempts_prefer_daemon_proxy_then_cold_spawn() {
        let sock = Path::new("/run/codex/app-server-control.sock");
        let attempts = attempts_for(None, Some(sock));
        assert_eq!(attempts.len(), 2, "daemon → proxy then fallback");
        assert_spawn(
            &attempts[0],
            &[
                "app-server",
                "proxy",
                "--sock",
                "/run/codex/app-server-control.sock",
            ],
            PROXY_PROBE_DEADLINE,
        );
        assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);
    }

    #[test]
    fn attempts_prefer_broker_first_then_daemon_then_cold_spawn() {
        let broker = Path::new("/run/user/1000/rimz/w/sock/codex-app-server.sock");
        let daemon = Path::new("/run/codex/app-server-control.sock");
        let attempts = attempts_for(Some(broker), Some(daemon));
        assert_eq!(attempts.len(), 3, "broker → daemon proxy → cold-spawn");
        match &attempts[0] {
            ConnectAttempt::Broker(path) => assert_eq!(path, broker),
            other => panic!("broker must come first, got a spawn attempt: {other:?}"),
        }
        assert_spawn(
            &attempts[1],
            &[
                "app-server",
                "proxy",
                "--sock",
                "/run/codex/app-server-control.sock",
            ],
            PROXY_PROBE_DEADLINE,
        );
        assert_spawn(&attempts[2], &["app-server"], APP_SERVER_DEADLINE);
    }

    #[test]
    fn attempts_broker_then_cold_spawn_without_a_daemon() {
        let broker = Path::new("/run/user/1000/rimz/w/sock/codex-app-server.sock");
        let attempts = attempts_for(Some(broker), None);
        assert_eq!(attempts.len(), 2, "broker → cold-spawn (no daemon)");
        match &attempts[0] {
            ConnectAttempt::Broker(path) => assert_eq!(path, broker),
            other => panic!("broker must come first, got a spawn attempt: {other:?}"),
        }
        assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);
    }
}
