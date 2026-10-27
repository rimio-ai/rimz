//! Codex app-server JSON-RPC client — read-only realtime enrichment.
//!
//! Codex has no statusline, so its rich per-session context (the analogue of
//! Claude's statusline `AgentContext`) is read from the official Codex
//! app-server (`codex app-server`, JSON-RPC 2.0 over stdio). This client speaks
//! only **read-only, non-interfering** methods — `initialize`/`initialized`,
//! `account/rateLimits/read`, `model/list`, `thread/read`, `thread/list`, and
//! `thread/loaded/list` — and never `thread/resume` or `turn/start` (which would
//! rejoin/own the user's live Codex thread). It is the out-of-band producer
//! behind `rimz agents refresh-context` and the daemon-mode liveness probe behind
//! the sidebar cache refresher's TTL-gated ghost-session reap;
//! disk_usage
//! ([`crate::store::agent_context`]) and the snapshot fold-in are
//! transport-agnostic, exactly as for Claude.
//!
//! Connection preference (warmest first): this session's broker
//! ([`crate::agents::adapters::codex::broker`]) over its unix socket — a held, already
//! handshaked `codex app-server` that amortizes the per-datapoint handshake;
//! then the per-user daemon `codex remote-control start` brings up (managed by
//! [`daemon`]), re-used over its WebSocket control socket; then a fresh
//! cold-spawned `codex app-server`. The cold-spawn is
//! always the final fallback, so enrichment never depends on either being up
//! (headless / no-mux just cold-spawns). Set `RIMZ_CODEX_APP_SERVER_SOCK` to an
//! empty value to drop the daemon from the order.
//!
//! A reading reaches a login's entry only from that login's Codex home. The
//! broker reports the child that served each response and this client decides:
//! every broker response must name the canonical home and auth stamp of the
//! login ([`ServedBy`]), or the whole attempt is discarded and the ladder goes on.
//! An older broker never names its child, so it is skipped after `initialize`.
//! The daemon socket is the login's own and the cold spawn runs under the
//! login's env, so neither needs the check. Enrichment and the model catalog
//! share the one ladder.
//!
//! Why no token gauge here: as of the pinned Codex app-server, token /
//! context-window usage is exposed only on the live `thread/tokenUsage/updated`
//! notification (requires a subscribing `thread/resume`), never on a read-only
//! method. So the context gauge stays sourced from the rollout tail in
//! [`crate::agents::adapters::codex`]; this client supplies what the app-server *does* expose
//! read-only: rate-limit windows, paid/reset credits, model display name,
//! thread preview/name, and version.
//!
//! Best-effort, never correctness: every failure maps to an omitted field or a
//! `None` record — it never fails a hook or a turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::agents::context::{AgentAccount, AgentContext, AgentRateLimits};
use crate::agents::{ExtraCredits, ResetCredits};

pub mod daemon;
#[cfg(test)]
mod tests;
mod transport;
mod wire;

use super::CodexAdapter;
use crate::agents::capabilities::LaunchCapability;
use transport::WsTransport;
pub(super) use transport::{
    AppServerErr, FramedTransport, JsonRpcTransport, ServedBy, codex_bin, initialize,
    installed_codex_bin, write_frame,
};
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

/// Skip an app-server refresh when its owned fields were written recently, so
/// two close turn boundaries (a quick `UserPromptSubmit` then `Stop`) do not
/// each spawn an app-server. Transcript context is stat-gated separately and
/// always gets a chance to merge.
const REFRESH_THROTTLE_SECS: i64 = 20;

/// Whether app-server-owned context fields need another refresh.
pub fn app_server_due(record: Option<&crate::agents::context::record::AgentContextRecord>) -> bool {
    let now = Timestamp::now().as_second();
    record
        .and_then(|record| record.rate_limits_observed_at)
        .is_none_or(|observed_at| now - observed_at.as_second() >= REFRESH_THROTTLE_SECS)
}

/// Merge app-server-owned fields onto the record while preserving
/// transcript-owned context. Pure over the record value: the caller owns the
/// lock and the durable write.
pub fn merge_app_server_context(
    record: &mut crate::agents::context::record::AgentContextRecord,
    context: &AgentContext,
) -> bool {
    let observed_at = context.observed_at;
    record.context.source.clone_from(&context.source);
    if context.session_name.is_some() {
        record
            .context
            .session_name
            .clone_from(&context.session_name);
    }
    if context.session_preview.is_some() {
        record
            .context
            .session_preview
            .clone_from(&context.session_preview);
    }
    if context.model_id.is_some() {
        record.context.model_id.clone_from(&context.model_id);
    }
    record
        .context
        .model_display_name
        .clone_from(&context.model_display_name);
    record
        .context
        .agent_version
        .clone_from(&context.agent_version);
    record.context.rate_limits.clone_from(&context.rate_limits);
    record.context.account.clone_from(&context.account);
    record.context.observed_at = observed_at;
    record.rate_limits_observed_at = Some(observed_at);
    true
}

/// Default budget for warm unix-socket probes. A stale broker or daemon socket
/// costs little before the cold-spawn fallback takes over; the sidebar's daemon
/// ghost reap pays this only from the cache refresher, behind its own TTL.
const DAEMON_PROBE_DEADLINE_SECS: u64 = 2;
const DAEMON_PROBE_DEADLINE: Duration = Duration::from_secs(DAEMON_PROBE_DEADLINE_SECS);

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

impl AppServerObservation {
    pub(super) fn account_usage(&self) -> crate::AccountUsageSnapshot {
        crate::AccountUsageSnapshot {
            plan: self
                .context
                .account
                .as_ref()
                .and_then(|account| account.plan.clone()),
            rate_limits: self.context.rate_limits.clone(),
            extra_credits: self.extra_credits.clone(),
            reset_credits: self.reset_credits.clone(),
        }
    }
}

type RateLimitRead = (
    Option<AgentRateLimits>,
    Option<AgentAccount>,
    Option<ExtraCredits>,
    Option<ResetCredits>,
);

/// One way [`CodexAppServer::observe_login`] tries to reach an app-server, in
/// preference order.
#[derive(Debug)]
enum ConnectAttempt {
    /// Connect to the warm per-session broker over its unix socket — the fast
    /// path, on the short probe budget.
    Broker(PathBuf),
    /// Connect to the per-user daemon's WebSocket control socket.
    DaemonWs(PathBuf),
    /// Spawn a `codex` invocation for the throwaway cold-spawn fallback.
    Spawn,
}

/// Run `read` against the first attempt that opens, handshakes, and reads
/// whole. An attempt that fails at any step, including a broker response that
/// does not confirm the login, is discarded with everything it returned. A
/// `budget` of `None` stops the ladder.
fn read_from_attempts<T: JsonRpcTransport, R>(
    attempts: Vec<ConnectAttempt>,
    mut budget: impl FnMut(&ConnectAttempt) -> Option<Duration>,
    mut open: impl FnMut(&ConnectAttempt, Duration) -> Result<T, AppServerErr>,
    mut read: impl FnMut(&mut CodexAppServer<T>) -> Result<R, AppServerErr>,
) -> Result<R, AppServerErr> {
    let mut last_error = None;
    for attempt in attempts {
        let Some(budget) = budget(&attempt) else {
            break;
        };
        let result = open(&attempt, budget).and_then(|transport| {
            let mut client = CodexAppServer::new(transport);
            client.handshake()?;
            read(&mut client)
        });
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = Some(AppServerErr::Attempt {
                    attempt: match attempt {
                        ConnectAttempt::Broker(_) => "broker",
                        ConnectAttempt::DaemonWs(_) => "daemon",
                        ConnectAttempt::Spawn => "cold spawn",
                    },
                    source: Box::new(error),
                });
            }
        }
    }
    Err(last_error.unwrap_or(AppServerErr::Timeout))
}

fn catalog_from_attempts<T: JsonRpcTransport>(
    attempts: Vec<ConnectAttempt>,
    deadline: Instant,
    open: impl FnMut(&ConnectAttempt, Duration) -> Result<T, AppServerErr>,
) -> Result<Vec<crate::agents::capabilities::ModelCatalogEntry>, AppServerErr> {
    read_from_attempts(
        attempts,
        |_| {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
        },
        open,
        CodexAppServer::model_catalog,
    )
}

/// [`CodexAppServer::observe`] on the first attempt that serves the login. Each
/// attempt has its own budget: the short probe for a warm socket, the cold-spawn
/// budget for a spawn.
fn observe_from_attempts<T: JsonRpcTransport>(
    attempts: Vec<ConnectAttempt>,
    open: impl FnMut(&ConnectAttempt, Duration) -> Result<T, AppServerErr>,
    source: &str,
    session_id: Option<&str>,
    model_hint: Option<&str>,
    observed_at: Timestamp,
) -> Result<AppServerObservation, AppServerErr> {
    read_from_attempts(
        attempts,
        |attempt| {
            Some(match attempt {
                ConnectAttempt::Spawn => APP_SERVER_DEADLINE,
                _ => DAEMON_PROBE_DEADLINE,
            })
        },
        open,
        |client| client.observe(source, session_id, model_hint, observed_at),
    )
}

pub(crate) enum Transport {
    Framed(FramedTransport),
    Ws(Box<WsTransport>),
}

impl Transport {
    /// `expected` is the identity a broker must confirm; without one (the login's
    /// home cannot be resolved) no broker attempt can be confirmed.
    fn open(
        attempt: &ConnectAttempt,
        bin: &Path,
        login_env: &BTreeMap<String, String>,
        expected: Option<&ServedBy>,
        budget: Duration,
    ) -> Result<Self, AppServerErr> {
        match attempt {
            ConnectAttempt::Broker(path) => {
                let expected = expected.ok_or(AppServerErr::Unconfirmed)?;
                FramedTransport::connect(path, budget, expected.clone()).map(Self::Framed)
            }
            ConnectAttempt::DaemonWs(path) => WsTransport::connect(path, budget)
                .map(Box::new)
                .map(Self::Ws),
            ConnectAttempt::Spawn => {
                FramedTransport::spawn(bin, budget, login_env).map(Self::Framed)
            }
        }
    }
}

impl JsonRpcTransport for Transport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr> {
        match self {
            Self::Framed(transport) => transport.request(method, params),
            Self::Ws(transport) => transport.request(method, params),
        }
    }

    fn notify_frame(&mut self, frame: &Value) -> Result<(), AppServerErr> {
        match self {
            Self::Framed(transport) => transport.notify_frame(frame),
            Self::Ws(transport) => transport.notify_frame(frame),
        }
    }
}

impl CodexAppServer<Transport> {
    /// Observe the login through the warmest app-server that serves it: the
    /// per-session broker socket first (only when it confirms the login), then
    /// the per-user daemon control socket, then a fresh cold-spawned
    /// `app-server`. `None` when none do (codex missing, not runnable, protocol
    /// mismatch) — best-effort.
    pub(crate) fn observe_login(
        broker_socket: Option<&Path>,
        login_env: &BTreeMap<String, String>,
        source: &str,
        session_id: Option<&str>,
        model_hint: Option<&str>,
        observed_at: Timestamp,
    ) -> Option<AppServerObservation> {
        let bin = codex_bin();
        let expected = ServedBy::expected(login_env);
        observe_from_attempts(
            connect_attempts(broker_socket, login_env),
            |attempt, budget| Transport::open(attempt, &bin, login_env, expected.as_ref(), budget),
            source,
            session_id,
            model_hint,
            observed_at,
        )
        .ok()
    }

    pub(super) fn fetch_catalog(
        broker_socket: Option<&Path>,
        login_env: &BTreeMap<String, String>,
        budget: Duration,
    ) -> Result<Vec<crate::agents::capabilities::ModelCatalogEntry>, AppServerErr> {
        let deadline = Instant::now() + budget;
        let bin = codex_bin();
        let expected = ServedBy::expected(login_env);
        catalog_from_attempts(
            connect_attempts(broker_socket, login_env),
            deadline,
            |attempt, remaining| {
                Transport::open(attempt, &bin, login_env, expected.as_ref(), remaining)
            },
        )
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
    /// by the sidebar cache refresher's TTL-gated ghost reap.
    pub(crate) fn connect_daemon(login_env: &BTreeMap<String, String>) -> Option<Self> {
        Self::connect_daemon_at(
            &daemon_socket(login_env)?,
            loaded_threads_timeout(login_env),
        )
    }

    /// [`Self::connect_daemon`] on one named control socket, for the
    /// account-link gate, which asks the socket under the account home.
    fn connect_daemon_at(socket: &Path, timeout: Duration) -> Option<Self> {
        if !socket.exists() {
            return None;
        }
        let transport = WsTransport::connect(socket, timeout).ok()?;
        let mut client = Self::new(transport);
        client.handshake().ok()?;
        Some(client)
    }
}

/// The attempts [`CodexAppServer::observe_login`] tries, in preference order, after
/// resolving which sockets actually exist on disk.
fn connect_attempts(
    broker_socket: Option<&Path>,
    login_env: &BTreeMap<String, String>,
) -> Vec<ConnectAttempt> {
    attempts_for(
        broker_socket.filter(|path| path.exists()),
        daemon_socket(login_env)
            .filter(|path| path.exists())
            .as_deref(),
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
    attempts.push(ConnectAttempt::Spawn);
    attempts
}

/// The daemon control socket to prefer: an explicit `RIMZ_CODEX_APP_SERVER_SOCK`
/// path, or the default `$CODEX_HOME/app-server-control/app-server-control.sock`
/// (`~/.codex/...`). An empty override means "no daemon" — cold-spawn only.
fn daemon_socket(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    match login_env.get(CODEX_APP_SERVER_SOCK_ENV) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
        None => Some(daemon::control_socket(
            &CodexAdapter.config_home(login_env)?,
        )),
    }
}

fn loaded_threads_timeout(login_env: &BTreeMap<String, String>) -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(ms) = login_env
        .get("RIMZ_TEST_CODEX_LOADED_THREADS_MS")
        .and_then(|value| value.parse::<std::num::NonZeroU64>().ok())
    {
        return Duration::from_millis(ms.get());
    }
    #[cfg(not(feature = "testkit"))]
    let _ = login_env;
    DAEMON_PROBE_DEADLINE
}

/// A failed read yields the type's default, except one the broker did not
/// confirm, which discards the attempt.
fn best_effort<R: Default>(read: Result<R, AppServerErr>) -> Result<R, AppServerErr> {
    match read {
        Err(error) if !error.is_unconfirmed() => Ok(R::default()),
        other => other,
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
        let result = transport::initialize(&mut self.transport, Some(json!({})))?;
        self.user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
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
                profile_fingerprint: None,
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

    pub(super) fn model_catalog(
        &mut self,
    ) -> Result<Vec<crate::agents::capabilities::ModelCatalogEntry>, AppServerErr> {
        let mut entries = Vec::new();
        let mut params = json!({ "includeHidden": true });
        let mut cursors = std::collections::HashSet::new();
        loop {
            let result = self.transport.request("model/list", params.clone())?;
            let page: ModelListResponse = serde_json::from_value(result)
                .map_err(|err| AppServerErr::Protocol(err.to_string()))?;
            entries.extend(page.data.into_iter().map(|entry| {
                crate::agents::capabilities::ModelCatalogEntry {
                    id: entry.id,
                    hidden: entry.hidden,
                    upgrade: entry.upgrade,
                    efforts: entry
                        .supported_reasoning_efforts
                        .into_iter()
                        .map(|effort| effort.reasoning_effort)
                        .collect(),
                }
            }));
            let Some(cursor) = page.next_cursor else {
                return Ok(entries);
            };
            if !cursors.insert(cursor.clone()) {
                return Err(AppServerErr::Protocol(
                    "model/list repeated a pagination cursor".into(),
                ));
            }
            params["cursor"] = json!(cursor);
        }
    }

    /// Read the thread's short metadata without resuming or subscribing to it.
    /// `thread/read` is direct by id; `thread/list` is the documented history UI
    /// surface that reliably carries `preview`, so it fills any missing field.
    /// Both reads are best-effort and read-only.
    fn thread_summary(&mut self, session_id: &str) -> Result<Option<ThreadSummary>, AppServerErr> {
        if session_id.is_empty() {
            return Ok(None);
        }
        let read = best_effort(self.thread_read_summary(session_id))?;
        if read
            .as_ref()
            .is_some_and(|summary| summary.preview.is_some())
        {
            return Ok(read);
        }
        let listed = best_effort(self.thread_list_summary(session_id))?;
        Ok(match (read, listed) {
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
        })
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
    /// and version. A response the broker did not confirm is the one failure that
    /// is not tolerated: it fails the whole observation. Assumes
    /// [`Self::handshake`] already ran.
    pub(crate) fn observe(
        &mut self,
        source: &str,
        session_id: Option<&str>,
        model_hint: Option<&str>,
        observed_at: Timestamp,
    ) -> Result<AppServerObservation, AppServerErr> {
        let (rate_limits, account, extra_credits, reset_credits) = best_effort(self.rate_limits())?;
        let model = match model_hint {
            Some(hint) => best_effort(self.matched_model(hint))?,
            None => None,
        };
        let thread = match session_id {
            Some(id) => self.thread_summary(id)?,
            None => None,
        };
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
        Ok(AppServerObservation {
            context,
            extra_credits,
            reset_credits,
        })
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
