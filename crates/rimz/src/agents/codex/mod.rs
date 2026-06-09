//! Codex hook adapter.
//!
//! Classifies `PermissionRequest` (blocking) and the lifecycle events
//! (`SessionStart` registers idle, `SubagentStart` / `UserPromptSubmit` move
//! to running, `SubagentStop` returns the child to idle, `Stop` completes the
//! root turn — success, or failed on an error signal); renders the Codex-shaped
//! `PermissionRequest` `hookSpecificOutput` decision payload (neutral is empty
//! stdout).
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.codex/config.toml` using Codex's inline `[[hooks.Event]]` tables.
//!
//! Realtime details split across two sources. Usage (`context_pct`,
//! `total_tokens`, token composition, and cost) is read from the rollout tail
//! through [`refresh_transcript_context`], because the Codex app-server exposes
//! token usage only on a live, subscribing `thread/resume` — never read-only.
//! Metadata Claude gets from its statusline (rate-limit windows, model display
//! name, thread preview/name, version) comes from the app-server read-only
//! methods via [`refresh_app_server_context`], spawned out-of-band by `rimz codex
//! refresh-context`.

pub(crate) mod account;
pub(crate) mod app_server;
pub mod broker;
mod install;
pub(crate) mod payloads;
pub(crate) mod spend;
mod transcript;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use jiff::Timestamp;

use self::app_server::CodexAppServer;
use self::install::{
    codex_config_path, hooks_installed_at, install_into, preview_install_at, uninstall_from,
    untrusted_hook_events_at,
};
#[cfg(test)]
use self::install::{has_rimz_hook_command, snake_event_token};
use self::payloads::{
    CodexPermissionBehavior, CodexPermissionDecisionOutput, CodexPermissionHookOutput,
    parse_post_compact, parse_session_start, parse_subagent_start, parse_subagent_stop,
    parse_user_prompt_submit,
};
pub use self::transcript::refresh_transcript_context;
#[cfg(test)]
use self::transcript::{
    TranscriptUsage, configured_model_at, configured_reasoning_effort_at,
    find_session_transcript_under, transcript_enrichment, transcript_stat,
};
use self::transcript::{
    configured_model, configured_reasoning_effort, find_session_transcript,
    payload_reasoning_effort, usage_from_transcript,
};
use super::context::AgentContext;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, PlanLabel, ThreadKey, ToolClassification,
};
use super::hook_types::{CompactTrigger, SessionSource};
use super::lifecycle::LifecycleSignal;
use super::observation::{payload_context_pct, payload_total_tokens};
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentErr, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview,
    HookInstallReport, HookUninstallReport, LifecycleRefreshCtx, LocalContextRefresh,
    LocalContextRefreshCtx, RefreshSpawn, Result, RootIdentity, SubagentIdentity, choice_is_allow,
    classify_agent_hook, optional_payload_string, resolve_root_identity, resolve_subagent_identity,
    sanitize_user_prompt, stop_payload_errored,
};
use crate::feed::{FeedItem, FeedKind, Resolution};

/// Codex's effective hook cap. Upstream's blocking-hook deadline is shorter
/// than Claude's; this leaves a small safety margin so the bridge never holds
/// the hook past the kill window. Verify against the active Codex hook docs
/// before tightening.
const CODEX_HOOK_CAP: Duration = Duration::from_secs(60);

/// Codex's effective GPT-5.5 context tier. The rollout's
/// `model_context_window` replaces this as soon as it appears; until then the
/// agent card uses this stable provider fallback instead of briefly omitting
/// the window token.
const DEFAULT_CONTEXT_WINDOW: u64 = 258_000;
const DEFAULT_MODEL: &str = "GPT-5.5";

/// Everything `const` about Codex, in one place. See [`AgentDescriptor`] for
/// the descriptor-vs-trait split.
static CODEX_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "codex",
    display_name: "Codex",
    brand: Brand {
        emblem: "
 ▗▛███▜▖
▐▜▌ ▚ ▐▛▌
 ▝▀▀▀▀▀▘",
        color: 38,
    },
    plan_label: PlanLabel::Prefixed { prefix: "ChatGPT" },
    // An OpenAI OAuth subscription is the ChatGPT account Codex meters; Pi's
    // auth file names it `openai` (legacy installs `openai-codex`).
    sub_providers: &["openai", "openai-codex"],
    tools: ToolClassification {
        mutating: &["shell", "apply_patch", "exec_command", "local_shell"],
        editing: &["apply_patch"],
    },
    capabilities: Capabilities {
        blocking_feed: true,
        native_ask_ui: true,
        rate_limit_windows: true,
        subagents: true,
        // Codex has no background-task parking.
        background_tasks: false,
        // Codex fires no `SessionStart` on a plain CLI launch — it rides the
        // first `UserPromptSubmit` — and its hooks fire from the app-server
        // with no mux pane env, so a session is unstamped. Both make a Codex
        // instance present before any session binds: the sidebar binds it to
        // its pane by cwd and renders a wired-but-unprompted `codex` pane as
        // an idle agent.
        registers_lazily: true,
        hook_install: true,
    },
    default_context_window: Some(DEFAULT_CONTEXT_WINDOW),
    default_model: Some(DEFAULT_MODEL),
    hook_cap: CODEX_HOOK_CAP,
    // Codex commonly runs as a `node` bundle, so PID attribution accepts the
    // launcher process name beside its own.
    process_names: &["codex", "node"],
    // Codex hooks ride Claude-style event names; `PreToolUse` (races the
    // blocking ask) and `Notification` (idle) are deliberately absent.
    activity_events: &[
        "PostToolUse",
        "Stop",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    // Codex logs one rollout file per session.
    thread_key: ThreadKey::PerFile,
};

/// Installed events. Tuple is `(event_name, optional_matcher)` — the single
/// source of truth for which Codex events Rimz wires and with which matcher,
/// mirroring the Claude adapter's table. `SessionStart` filters to its
/// lifecycle subtypes; the per-call hooks match everything (`.*`); the
/// turn-boundary events (`UserPromptSubmit`, `Stop`) carry no matcher.
/// `UserPromptSubmit` is state signal — it moves the root agent to running and
/// carries the task. The broad `PreToolUse`/`PostToolUse` hooks fire on every
/// tool call; they keep the sidebar's enrichment current and feed
/// `rimz feed list --audit` depth, with their payload content gated by
/// `[privacy] payload_mode`.
const INSTALLED_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear|compact")),
    ("UserPromptSubmit", None),
    ("SubagentStart", Some(".*")),
    ("SubagentStop", Some(".*")),
    ("Stop", None),
    ("PermissionRequest", Some(".*")),
    ("PreToolUse", Some(".*")),
    ("PostToolUse", Some(".*")),
    ("PreCompact", Some(".*")),
    ("PostCompact", Some(".*")),
];

/// Legacy config block written by older Rimz builds. Codex ignores this block;
/// uninstall still removes it so users can clean up stale config.
const RIMZ_BLOCK: &str = "rimz";
const HOOKS_TABLE: &str = "hooks";

/// The exact command every rimz-managed Codex hook runs. Identical across all
/// events — the helper reads the event from the stdin payload's
/// `hook_event_name`, so no `--event` flag is needed.
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex";

/// Stable substring identifying a rimz-owned hook command across every form an
/// older build may have written (with `--event`, without `exec`). Used to
/// reclaim legacy entries on install and uninstall, so duplicates never
/// accumulate.
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source codex";

#[derive(Clone, Debug, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CODEX_DESCRIPTOR
    }

    fn default_launch_model(&self) -> Option<String> {
        configured_model().or_else(|| self.descriptor().default_model.map(ToOwned::to_owned))
    }

    /// `codex resume <id>` resolves the UUID to its rollout file and restores
    /// the session interactively, firing `SessionStart` with
    /// `source: "resume"`. `resume` is a top-level command (the non-interactive
    /// form is `codex exec resume <id>`); the launching pane sets the cwd.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "codex".to_owned(),
            "resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["codex".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.push(prompt.to_owned());
        }
        Some(argv)
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        let feed_kind = (event_name == "PermissionRequest").then_some(FeedKind::Permission);
        classify_agent_hook(
            event_name,
            feed_kind,
            &[
                "SessionStart",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "PreCompact",
                "PostCompact",
            ],
        )
    }

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => {
                let output = CodexPermissionDecisionOutput {
                    hook_specific_output: CodexPermissionHookOutput {
                        hook_event_name: "PermissionRequest",
                        decision: CodexPermissionBehavior {
                            behavior: if choice_is_allow(resolution) {
                                "allow"
                            } else {
                                "deny"
                            },
                            // Drift fix #1: upstream spec includes decision.message.
                            // Populated from the resolver's reason when present;
                            // absent (None) when not set so golden tests stay unchanged.
                            message: resolution.reason.clone(),
                        },
                    },
                };
                Ok(serde_json::to_value(output)
                    .expect("CodexPermissionDecisionOutput is infallible"))
            }
            other => Err(AgentErr::Render {
                agent: "codex",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Codex permission hooks expect empty stdout on the neutral path —
        // the agent's own UI then asks the human. Per docs/internals/hooks.md:
        // never emit `updatedInput` / `interrupt` for Codex permission hooks.
        Ok(None)
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // Same turn-boundary signal as Claude: a fresh prompt or the root Stop
        // means the agent is past any native_ui ask it raised mid-turn. A
        // SubagentStop is a child finishing, not the human answering, so it does
        // not clear the root's asks.
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        // Each event that yields an observation parses through its own typed
        // struct; silent events (PermissionRequest, read-only PostToolUse) return
        // `None`. The per-event status mapping is the Codex column of
        // docs/internals/hooks.md.
        let session_start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let user_prompt =
            (event_name == "UserPromptSubmit").then(|| parse_user_prompt_submit(payload));
        let subagent_start = (event_name == "SubagentStart").then(|| parse_subagent_start(payload));
        let subagent_stop = (event_name == "SubagentStop").then(|| parse_subagent_stop(payload));
        let post_compact = (event_name == "PostCompact").then(|| parse_post_compact(payload));
        // The status decision lives in the shared `lifecycle::step` table —
        // here the adapter only names the intent.
        let signal = match event_name {
            "SessionStart" => {
                let p = session_start.as_ref().unwrap();
                if p.source == SessionSource::Compact {
                    return None;
                } else {
                    LifecycleSignal::Registered
                }
            }
            // A subagent fires before the child model request, so it registers
            // running under the child `agent_id`.
            "SubagentStart" => LifecycleSignal::SubagentStarted,
            "UserPromptSubmit" => LifecycleSignal::TurnStarted,
            // A child finishing resolves to success — Codex reports no subagent
            // error signal; the root Stop completes the turn (success), or fails
            // it on an error signal. Codex has no background-task parking.
            "SubagentStop" => LifecycleSignal::SubagentStopped { errored: false },
            "Stop" => LifecycleSignal::TurnEnded {
                errored: stop_payload_errored(payload),
                parked_on_background: false,
            },
            // Only a *mutating* tool rides the lifecycle channel: it is proof of
            // real work (read-only tools stay silent). The `edits` bit marks the
            // file-writing subset, which ends the turn's thinking head.
            "PostToolUse" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
            },
            // A non-blocking PreToolUse is proof-of-work only: the ingestion
            // path persists it only when it reconciles a resting row to running.
            "PreToolUse" => LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
            "PreCompact" => LifecycleSignal::Compacting,
            "PostCompact" => LifecycleSignal::CompactionEnded {
                auto: Some(
                    post_compact
                        .as_ref()
                        .is_some_and(|p| matches!(p.trigger, CompactTrigger::Auto)),
                ),
            },
            _ => return None,
        };
        // Both subagent events carry the same (child id, type, parent session);
        // unify them so the identity reads below are written once. Codex keeps
        // `agent_id`/`agent_type` beside `common` (not inside it, unlike Claude).
        let subagent = subagent_start
            .as_ref()
            .map(|p| (&p.agent_id, &p.agent_type, &p.common.common.session_id))
            .or_else(|| {
                subagent_stop
                    .as_ref()
                    .map(|p| (&p.agent_id, &p.agent_type, &p.common.common.session_id))
            });
        // A subagent keys on its own child id under its parent root; a malformed
        // subagent event (no distinct child id) is quarantined — never folded
        // onto, and never corrupting, the parent's row. Root events key on the
        // session id and carry no parent link — a non-Subagent* event carrying
        // a distinct `agent_id` fired inside a subagent and is dropped rather
        // than keyed as a parentless phantom root (Codex stamps `agent_id` only
        // on Subagent* today, so this is the same guard Claude needs, latent).
        let (agent_id, parent_agent_id) = match subagent {
            Some((child, _, parent)) => match resolve_subagent_identity(
                self.descriptor().kind,
                event_name,
                child.as_deref(),
                parent.as_deref(),
                payload,
            ) {
                SubagentIdentity::Resolved {
                    agent_id,
                    parent_agent_id,
                } => (Some(agent_id), Some(parent_agent_id)),
                SubagentIdentity::Quarantined => return None,
            },
            None => match resolve_root_identity(
                self.descriptor().kind,
                event_name,
                optional_payload_string(payload, &["agent_id"]).as_deref(),
                optional_payload_string(payload, &["session_id"]).as_deref(),
            ) {
                RootIdentity::Root { agent_id } => (agent_id, None),
                RootIdentity::ForeignChild => return None,
            },
        };
        // Context budget lives in the rollout JSONL, not the payload — locate the
        // session's file by id and read its tail. The rollout carries a precomputed
        // percentage (it has the window directly), unlike Claude's raw tokens.
        let transcript_path = optional_payload_string(payload, &["session_id"])
            .and_then(|id| find_session_transcript(&id));
        let usage = transcript_path
            .as_deref()
            .map(usage_from_transcript)
            .unwrap_or_default();
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        // A subagent labels its row with its `agent_type` (trusted agent
        // metadata), kept across stop so a finished child stays labelled while it
        // lingers in the parent's list. A root labels with the user's *sanitized*
        // task/prompt, so harness control text never reaches the row.
        observation.task = match subagent {
            Some((_, agent_type, _)) => agent_type.clone().or_else(|| {
                sanitize_user_prompt(
                    optional_payload_string(payload, &["task", "prompt"]).as_deref(),
                )
            }),
            None => sanitize_user_prompt(
                optional_payload_string(payload, &["task", "prompt"]).as_deref(),
            ),
        };
        observation.prompt =
            sanitize_user_prompt(user_prompt.as_ref().and_then(|p| p.prompt.as_deref()));
        observation.transcript_path =
            transcript_path.map(|path| path.to_string_lossy().into_owned());
        let reported_context_window = usage.reported_context_window();
        observation.model = optional_payload_string(payload, &["model"]).or(usage.model);
        observation.effort = payload_reasoning_effort(payload).or_else(configured_reasoning_effort);
        observation.context_pct = payload_context_pct(payload, usage.context_pct);
        // The rollout's `model_context_window` (e.g. 258k for GPT-5.5) doubles
        // as the card's exact window label; the 258k fallback stays in the
        // sidecar/view-model path so it never overwrites an exact rollup value.
        observation.context_window = reported_context_window;
        observation.total_tokens = payload_total_tokens(payload, usage.total_tokens);
        // The latest call's split — the card's composition line. The rollout's
        // `input_tokens` includes the cached slice (the protocol reports no
        // per-call cache-write), so fresh input is the uncached remainder.
        observation.cache_read_input_tokens = usage.last_cached_input_tokens;
        observation.fresh_input_tokens = usage
            .last_input_tokens
            .map(|input| input.saturating_sub(usage.last_cached_input_tokens.unwrap_or(0)));
        observation.output_tokens = usage.last_output_tokens;
        // Codex exposes no stable todo-state hook field; the dots stay None.
        observation.todo_done = None;
        observation.todo_total = None;
        Some(observation)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = codex_config_path()?;
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = codex_config_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = codex_config_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        codex_config_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn untrusted_installed_hooks(&self) -> Vec<String> {
        codex_config_path()
            .map(|path| untrusted_hook_events_at(&path))
            .unwrap_or_default()
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    /// Codex has no statusline, so app-server-owned metadata (rate-limit
    /// windows, model display name, thread preview/name, version) refreshes
    /// out-of-band on turn boundaries: `SessionStart` populates it early (rate
    /// limits + model need no thread); `UserPromptSubmit`/`Stop` keep it
    /// current. Per-tool events are excluded — an app-server spawn per tool call
    /// is too frequent. Local transcript usage has its own stat-gated inline
    /// refresh below.
    fn post_lifecycle_refresh(
        &self,
        event_name: &str,
        ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        if !matches!(event_name, "SessionStart" | "UserPromptSubmit" | "Stop") {
            return None;
        }
        let mut args = vec![
            "codex".to_owned(),
            "refresh-context".to_owned(),
            "--session-id".to_owned(),
            ctx.agent_id.to_owned(),
            "--workspace-id".to_owned(),
            ctx.workspace_id.to_owned(),
        ];
        if let Some(model) = ctx.model_hint {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        Some(RefreshSpawn { args })
    }

    fn local_context_refresh(
        &self,
        event_name: &str,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if !matches!(
            event_name,
            "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop"
        ) {
            return None;
        }
        refresh_transcript_context(
            ctx.agent_id,
            ctx.model_hint,
            ctx.prior_effort,
            ctx.prior_transcript_path,
            ctx.prior_transcript_stat,
        )
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::codex_session_files()
    }

    /// Codex logs token counts, not dollars — each event is multiplied
    /// through the price book. The resume cursor carries the cumulative-total
    /// and tracked-model fold state, so a suffix parse subtracts exactly.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_codex_spend(path, resume, prices)
    }
}

/// Read Codex's read-only realtime details from the app-server and project them
/// onto an [`AgentContext`] for the session sidecar. Spawned out-of-band by
/// `rimz codex refresh-context` (never inline in a hook). The app-server owns
/// rate-limit windows, account plan, model display name, thread preview/name,
/// and version.
/// Transcript-derived tokens and cost are refreshed separately from the local
/// rollout tail, so an unreachable app-server never suppresses them.
pub fn refresh_app_server_context(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<AgentContext> {
    let mut client = CodexAppServer::connect(broker_socket)?;
    Some(client.observe_context("codex", session_id, model_hint, Timestamp::now()))
}

/// Backwards-compatible name for the app-server-only context read. New callers
/// use [`refresh_app_server_context`] and [`refresh_transcript_context`] so local
/// transcript data is independent from app-server availability.
pub fn refresh_context(
    session_id: Option<&str>,
    model_hint: Option<&str>,
    broker_socket: Option<&Path>,
) -> Option<AgentContext> {
    refresh_app_server_context(session_id, model_hint, broker_socket)
}

/// The thread ids the per-user Codex app-server daemon currently holds in memory,
/// for the sidebar's daemon-mode ghost reap
/// ([`crate::ledger::snapshot::SidebarSnapshot::drop_dead_daemon_sessions`]).
/// Connects to the daemon **specifically** — never a cold-spawn, whose empty set
/// would mass-reap — and reads `thread/loaded/list`. `None` when there is no daemon
/// to ask or its list cannot be trusted, which the caller reads as "unknown, keep
/// all". Spawned out-of-band by the sidebar producer; read-only, best-effort.
pub fn loaded_daemon_threads() -> Option<std::collections::BTreeSet<String>> {
    let mut client = CodexAppServer::connect_daemon()?;
    let ids = client.loaded_threads().ok()?;
    Some(ids.into_iter().collect())
}

#[cfg(test)]
mod tests;
