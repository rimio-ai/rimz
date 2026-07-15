//! Codex app-server JSON-RPC client — read-only realtime enrichment.
//!
//! Codex has no statusline, so its rich per-session context (the analogue of
//! Claude's statusline `AgentContext`) is read from the official Codex
//! app-server (`codex app-server`, JSON-RPC 2.0 over stdio). This client speaks
//! only **read-only, non-interfering** methods — `initialize`/`initialized`,
//! `account/rateLimits/read`, `model/list`, `thread/read`, `thread/list`, and
//! `thread/loaded/list` — and never `thread/resume` or `turn/start` (which would
//! rejoin/own the user's live Codex thread). It is the out-of-band producer
//! behind `rimz codex refresh-context` and the daemon-mode liveness probe behind
//! the sidebar cache refresher's TTL-gated ghost-session reap;
//! disk_usage
//! ([`crate::store::agent_context`]) and the snapshot fold-in are
//! transport-agnostic, exactly as for Claude.
//!
//! Connection preference (warmest first): this session's broker
//! ([`crate::agents::codex::broker`]) over its unix socket — a held, already
//! handshaked `codex app-server` that amortizes the per-datapoint handshake;
//! then the per-user daemon `codex remote-control start` brings up (which
//! [`crate::remote_control`] can auto-launch), re-used over its WebSocket control
//! socket; then a fresh cold-spawned `codex app-server`. The cold-spawn is
//! always the final fallback, so enrichment never depends on either being up
//! (headless / no-mux just cold-spawns). Set `RIMZ_CODEX_APP_SERVER_SOCK` to an
//! empty value to drop the daemon from the order.
//!
//! Why no token gauge here: as of the pinned Codex app-server, token /
//! context-window usage is exposed only on the live `thread/tokenUsage/updated`
//! notification (requires a subscribing `thread/resume`), never on a read-only
//! method. So the context gauge stays sourced from the rollout tail in
//! [`crate::agents::codex`]; this client supplies what the app-server *does* expose
//! read-only: rate-limit windows, paid/reset credits, model display name,
//! thread preview/name, and version.
//!
//! Best-effort, never correctness: every failure maps to an omitted field or a
//! `None` record — it never fails a hook or a turn.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::agents::context::{AgentAccount, AgentContext, AgentRateLimits};
use crate::agents::{ExtraCredits, ResetCredits};

#[cfg(test)]
mod tests;
mod transport;
mod wire;

pub(crate) use transport::{
    AppServerErr, JsonRpcTransport, codex_bin, codex_home, recv_response, spawn_frame_reader,
    write_frame,
};
use transport::{FramedTransport, WsTransport};
use wire::{
    MatchedModel, ModelListResponse, RateLimitsResponse, ThreadListResponse, ThreadReadResponse,
    ThreadSummary, codex_version_from_user_agent, collect_reset_credits, collect_usage,
    into_context, parse_loaded_threads, thread_matches_session, thread_summary_from_raw,
};

/// Total wall-clock budget for one refresh (spawn + handshake + reads). The
/// caller is a detached background helper with no user waiting, so this is
/// generous enough for `model/list` to fetch its catalog, but bounded so a
/// wedged app-server is killed rather than lingering.
const APP_SERVER_DEADLINE_SECS: u64 = 6;
const APP_SERVER_DEADLINE: Duration = Duration::from_secs(APP_SERVER_DEADLINE_SECS);

/// Short budget for warm unix-socket probes. A stale broker or daemon socket
/// costs little before the cold-spawn fallback takes over; the sidebar's daemon
/// ghost reap pays this only from the cache refresher, behind its own TTL.
const DAEMON_PROBE_DEADLINE_SECS: u64 = 2;
const DAEMON_PROBE_DEADLINE: Duration = Duration::from_secs(DAEMON_PROBE_DEADLINE_SECS);

/// Longest ordered broker, daemon, and cold-spawn account-usage fallback.
#[cfg(test)]
pub(crate) const MAX_REALTIME_ACCOUNT_USAGE_DURATION: Duration =
    Duration::from_secs(DAEMON_PROBE_DEADLINE_SECS * 2 + APP_SERVER_DEADLINE_SECS);

/// Override for the daemon control socket. A path re-uses that daemon directly;
/// an empty value forces the cold-spawn path (tests, opt-out).
const CODEX_APP_SERVER_SOCK_ENV: &str = "RIMZ_CODEX_APP_SERVER_SOCK";
const LOADED_THREADS_MAX_PAGES: usize = 16;

pub(crate) struct CodexAppServer<T: JsonRpcTransport> {
    transport: T,
    /// The `userAgent` the server returned at `initialize`, e.g.
    /// `"rimz/0.135.0 (Ubuntu ...)"` — its version token is the Codex version.
    user_agent: Option<String>,
}

pub(crate) struct AppServerObservation {
    pub(crate) context: AgentContext,
    pub(crate) extra_credits: Option<ExtraCredits>,
    pub(crate) reset_credits: Option<ResetCredits>,
}

type RateLimitRead = (
    Option<AgentRateLimits>,
    Option<AgentAccount>,
    Option<ExtraCredits>,
    Option<ResetCredits>,
);

/// One way [`CodexAppServer::connect`] tries to reach an app-server, in
/// preference order.
#[derive(Debug)]
enum ConnectAttempt {
    /// Connect to the warm per-session broker over its unix socket — the fast
    /// path, on the short probe budget.
    Broker(PathBuf),
    /// Connect to the per-user daemon's WebSocket control socket.
    DaemonWs(PathBuf),
    /// Spawn a `codex` invocation for the throwaway cold-spawn fallback. Carries
    /// argv (program omitted) + budget.
    Spawn(Vec<String>, Duration),
}

pub(crate) enum Transport {
    Framed(FramedTransport),
    Ws(Box<WsTransport>),
}

impl JsonRpcTransport for Transport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr> {
        match self {
            Self::Framed(transport) => transport.request(method, params),
            Self::Ws(transport) => transport.request(method, params),
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), AppServerErr> {
        match self {
            Self::Framed(transport) => transport.notify(method, params),
            Self::Ws(transport) => transport.notify(method, params),
        }
    }
}

impl CodexAppServer<Transport> {
    /// Connect to a Codex app-server and complete the initialize handshake,
    /// trying each attempt in preference order: the per-session broker socket
    /// (warm) first, then the per-user daemon control socket, then a fresh
    /// cold-spawned `app-server`. The first that handshakes wins. `None` when none
    /// do (codex missing, not runnable, protocol mismatch) — best-effort.
    pub(crate) fn connect(broker_socket: Option<&Path>) -> Option<Self> {
        let bin = codex_bin();
        for attempt in connect_attempts(broker_socket) {
            let transport = match &attempt {
                ConnectAttempt::Broker(path) => {
                    FramedTransport::connect(path, DAEMON_PROBE_DEADLINE).map(Transport::Framed)
                }
                ConnectAttempt::DaemonWs(path) => WsTransport::connect(path, DAEMON_PROBE_DEADLINE)
                    .map(Box::new)
                    .map(Transport::Ws),
                ConnectAttempt::Spawn(args, deadline) => {
                    FramedTransport::spawn(&bin, args, *deadline).map(Transport::Framed)
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

impl CodexAppServer<WsTransport> {
    /// Connect to the **per-user daemon specifically** and handshake — the only
    /// app-server whose `thread/loaded/list` is authoritative for daemon-mode
    /// sessions. No broker, and deliberately no cold-spawn fallback: a fresh
    /// `app-server` holds no threads, so reporting its empty loaded set would mass-
    /// reap every daemon-mode session. `None` when no daemon control socket exists
    /// or it does not speak the current WebSocket control protocol — the liveness
    /// caller reads that as "unknown, keep all", never as "zero loaded". Used
    /// only by the sidebar cache refresher's TTL-gated ghost reap.
    pub(crate) fn connect_daemon() -> Option<Self> {
        let socket = daemon_socket().filter(|path| path.exists())?;
        let transport = WsTransport::connect(&socket, DAEMON_PROBE_DEADLINE).ok()?;
        let mut client = Self::new(transport);
        client.handshake().ok()?;
        Some(client)
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
/// disk), order the attempts — broker (warm) first, then the daemon WebSocket,
/// always followed by a cold-spawned `app-server` fallback so enrichment never
/// depends on either being up.
fn attempts_for(broker: Option<&Path>, daemon: Option<&Path>) -> Vec<ConnectAttempt> {
    let mut attempts = Vec::new();
    if let Some(broker) = broker {
        attempts.push(ConnectAttempt::Broker(broker.to_path_buf()));
    }
    if let Some(daemon) = daemon {
        attempts.push(ConnectAttempt::DaemonWs(daemon.to_path_buf()));
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

    /// Read the account's rate-limit windows, plan tier, and credit summaries
    /// in one call. The windows ride [`AgentRateLimits`]; the plan tier (when
    /// present) marks a metered subscription account on [`AgentAccount`]. An
    /// API-key account returns neither, so the account is left `None` and the
    /// dashboard infers the unmetered "infinite" bar.
    fn rate_limits(&mut self) -> Result<RateLimitRead, AppServerErr> {
        let result = self
            .transport
            .request("account/rateLimits/read", Value::Null)?;
        let parsed: RateLimitsResponse = serde_json::from_value(result)
            .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
        let reset_credits = collect_reset_credits(&parsed);
        let usage = collect_usage(parsed);
        let account =
            (usage.plan.is_some() || usage.rate_limits.is_some()).then_some(AgentAccount {
                metered: Some(true),
                plan: usage.plan,
                ..Default::default()
            });
        Ok((
            usage.rate_limits,
            account,
            usage.extra_credits,
            reset_credits,
        ))
    }

    /// Match the session's model `hint` (a raw model id from the lifecycle
    /// observation) against the catalog, returning its display name. `None`
    /// when there is no hint or no match — never a guess.
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
            })
            .filter(|model| !model.display_name.is_empty()))
    }

    /// Read the thread's short metadata without resuming or subscribing to it.
    /// `thread/read` is direct by id; `thread/list` is the documented history UI
    /// surface that reliably carries `preview`, so it fills any missing field.
    /// Both reads are best-effort and read-only.
    fn thread_summary(&mut self, session_id: &str) -> Option<ThreadSummary> {
        let session_id = (!session_id.is_empty()).then_some(session_id)?;
        let read = self.thread_read_summary(session_id).ok().flatten();
        if read
            .as_ref()
            .is_some_and(|summary| summary.preview.is_some())
        {
            return read;
        }
        let listed = self.thread_list_summary(session_id).ok().flatten();
        match (read, listed) {
            (Some(mut read), Some(listed)) => {
                if read.preview.is_none() {
                    read.preview = listed.preview;
                }
                if read.name.is_none() {
                    read.name = listed.name;
                }
                Some(read)
            }
            (Some(read), None) => Some(read),
            (None, Some(listed)) => Some(listed),
            (None, None) => None,
        }
    }

    fn thread_read_summary(
        &mut self,
        session_id: &str,
    ) -> Result<Option<ThreadSummary>, AppServerErr> {
        let result = self.transport.request(
            "thread/read",
            json!({ "threadId": session_id, "includeTurns": false }),
        )?;
        let parsed: ThreadReadResponse = serde_json::from_value(result.clone())
            .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
        Ok(parsed
            .thread
            .or_else(|| serde_json::from_value(result).ok())
            .and_then(thread_summary_from_raw))
    }

    fn thread_list_summary(
        &mut self,
        session_id: &str,
    ) -> Result<Option<ThreadSummary>, AppServerErr> {
        let result = self.transport.request(
            "thread/list",
            json!({ "cursor": null, "limit": 100, "sortKey": "updated_at" }),
        )?;
        let parsed: ThreadListResponse = serde_json::from_value(result)
            .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
        Ok(parsed
            .data
            .into_iter()
            .find(|thread| thread_matches_session(thread, session_id))
            .and_then(thread_summary_from_raw))
    }

    /// Read every read-only field the app-server exposes and project it onto an
    /// [`AgentContext`]. Each read is independent and best-effort: a failed
    /// `account/rateLimits/read` (e.g. API-key account) still yields the model
    /// and version. Assumes [`Self::handshake`] already ran.
    #[cfg(test)]
    pub(crate) fn observe_context(
        &mut self,
        source: &str,
        session_id: Option<&str>,
        model_hint: Option<&str>,
        observed_at: Timestamp,
    ) -> AgentContext {
        self.observe(source, session_id, model_hint, observed_at)
            .context
    }

    pub(crate) fn observe(
        &mut self,
        source: &str,
        session_id: Option<&str>,
        model_hint: Option<&str>,
        observed_at: Timestamp,
    ) -> AppServerObservation {
        let (rate_limits, account, extra_credits, reset_credits) =
            self.rate_limits().unwrap_or_default();
        let model = model_hint.and_then(|hint| self.matched_model(hint).ok().flatten());
        let thread = session_id.and_then(|id| self.thread_summary(id));
        let agent_version = self
            .user_agent
            .as_deref()
            .and_then(codex_version_from_user_agent);
        let context = into_context(
            source,
            rate_limits,
            account,
            model,
            thread,
            agent_version,
            observed_at,
        );
        AppServerObservation {
            context,
            extra_credits,
            reset_credits,
        }
    }

    /// The thread ids the connected app-server currently holds in memory
    /// (`thread/loaded/list`). Read-only and non-interfering — it lists ids, never
    /// resumes or owns a thread. This is the daemon-mode liveness signal: a daemon-
    /// backed Codex session absent from this set is reapable even while the shared
    /// daemon pid lives. A response with no recognized id field is an error, not an
    /// empty set, so a wire-shape drift degrades to keep-all rather than mass-reap.
    /// Assumes [`Self::handshake`] already ran.
    pub(crate) fn loaded_threads(&mut self) -> Result<Vec<String>, AppServerErr> {
        let mut loaded = Vec::new();
        let mut cursor = None;
        for _ in 0..LOADED_THREADS_MAX_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self.transport.request("thread/loaded/list", params)?;
            let (mut ids, next_cursor) = parse_loaded_threads(&result)?;
            loaded.append(&mut ids);
            let Some(next) = next_cursor else {
                return Ok(loaded);
            };
            cursor = Some(next);
        }
        Err(AppServerErr::Protocol(format!(
            "thread/loaded/list: exceeded {LOADED_THREADS_MAX_PAGES} pages"
        )))
    }
}
