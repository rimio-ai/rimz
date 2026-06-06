//! Claude Code hook adapter.
//!
//! Classifies the blocking events (`PermissionRequest`, `PreToolUse:
//! ExitPlanMode`, `PreToolUse: AskUserQuestion`) and the lifecycle events
//! (`SessionStart` registers idle, `UserPromptSubmit` moves to running with
//! the prompt as task, `Stop` completes the turn — success, or failed on an
//! error signal, or back to running when the payload's `background_tasks`
//! still has work in flight, `SessionEnd` exits, `Notification` silent);
//! renders the Claude-shaped `hookSpecificOutput` / `updatedInput` decision
//! payload and the silent neutral fallback. Context budget is read from the
//! transcript tail.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers. The
//! `PermissionRequest` blocking hook is marked `_rimz_sync = true`; an existing
//! async marker on it is a hard install error (see [`BLOCKING_EVENTS`] and
//! `docs/internals/hooks.md`). The `PreToolUse` blocking sub-events ride the
//! broad `PreToolUse` hook and self-classify from `tool_name`.

pub(crate) mod account;
pub(crate) mod payloads;
pub(crate) mod spend;
mod statusline;
mod subagent_statusline;

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Map, Value};

use self::payloads::{
    ClaudePermissionBehavior, ClaudePermissionDecisionOutput, ClaudePermissionHookOutput,
    ClaudePreToolUseDecisionOutput, ClaudePreToolUseHookOutput, parse_pre_tool_use,
    parse_session_start, parse_stop, parse_subagent_start, parse_subagent_stop,
    parse_user_prompt_submit,
};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, PlanLabel, ThreadKey, ToolClassification,
};
use super::hook_types::BackgroundTask;
use super::lifecycle::LifecycleSignal;
use super::observation::{payload_context_pct, payload_total_tokens};
use super::pricing::PriceBook;
use super::{
    AgentAdapter, AgentContext, AgentErr, AgentLifecycleObservation, AgentTurnError,
    ClassifiedHook, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    RootIdentity, StatusLineChange, SubagentIdentity, SubagentObservation, agent_config_path,
    choice_is_allow, classify_agent_hook, optional_payload_string, read_optional_file,
    read_transcript_tail, resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt,
    stop_payload_errored,
};
use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::ledger::atomic;

/// Claude's effective hook cap. The upstream cap is ~125s; we leave a small
/// margin so the bridge never holds the hook past Claude's kill window.
const CLAUDE_HOOK_CAP: Duration = Duration::from_secs(120);

/// Everything `const` about Claude Code, in one place. See
/// [`AgentDescriptor`] for the descriptor-vs-trait split.
static CLAUDE_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "claude",
    display_name: "Claude",
    brand: Brand {
        emblem: "
 ▐▛███▜▌
▝▜█████▛▘
  ▘▘ ▝▝",
        color: 173,
    },
    plan_label: PlanLabel::Prefixed { prefix: "Claude" },
    // An Anthropic OAuth subscription is the account Claude meters, so a
    // multi-provider client (Pi) on that sub shares this budget.
    sub_providers: &["anthropic"],
    tools: ToolClassification {
        mutating: &["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"],
        editing: &["Edit", "Write", "MultiEdit", "NotebookEdit"],
    },
    capabilities: Capabilities {
        blocking_feed: true,
        native_ask_ui: true,
        rate_limit_windows: true,
        subagents: true,
        background_tasks: true,
        // Claude stamps a live pane on every session, so a pane with no
        // session is genuinely gone — never idle-synthesized or cwd-rescued.
        registers_lazily: false,
        hook_install: true,
    },
    default_context_window: None,
    hook_cap: CLAUDE_HOOK_CAP,
    process_names: &["claude"],
    // `PreToolUse` (races the blocking ask) and `Notification` (idle) are
    // deliberately absent.
    activity_events: &[
        "PostToolUse",
        "Stop",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    // A Claude session spreads across `<session_id>/chat.jsonl` plus
    // `<session_id>/subagents/*.jsonl`; the session directory is the thread.
    thread_key: ThreadKey::SessionDir,
};

/// Per-hook timeout written into the Claude config (seconds). Matches
/// [`CLAUDE_HOOK_CAP`] so the agent and bridge agree on the ceiling.
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 120;

/// Installed events. Tuple is `(event_name, optional_matcher)`. Rimz installs
/// every event as a single broad hook with no matcher: the helper classifies
/// each call from the payload's `tool_name`, so `PreToolUse: ExitPlanMode` and
/// `PreToolUse: AskUserQuestion` still route to their blocking feed kinds off
/// the broad `PreToolUse` hook. A dedicated `ExitPlanMode|AskUserQuestion`
/// matcher would only double-fire — Claude runs every matching matcher group,
/// and the broad entry already matches those tools. The broad
/// `PreToolUse`/`PostToolUse` hooks also keep the sidebar's enrichment current
/// and feed `rimz feed list --audit` depth, with their payload content gated by
/// `[privacy] payload_mode`. The matcher slot stays in the tuple because the
/// reclaim path still reasons about on-disk matchers left by users or older
/// builds.
const INSTALLED_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("SessionEnd", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
    // Subagent lifecycle (Claude Code's Task-tool children, parity with Codex's
    // threads): `SubagentStart` registers a child row keyed by its `agent_id`,
    // `SubagentStop` returns it to idle. Both carry the parent root `session_id`.
    ("SubagentStart", None),
    ("SubagentStop", None),
    // Fires before the agent compacts its context window (manual `/compact` or
    // auto): the sidebar shows a transient "compacting" head while it condenses.
    // The next lifecycle event clears it.
    ("PreCompact", None),
];

/// Events that hold the agent open while the bridge waits for an answer.
/// Installing one with `_rimz_sync = false` in the existing config is a hard
/// error — the source of truth for "must block" is this constant, never the
/// on-disk file.
const BLOCKING_EVENTS: &[(&str, Option<&str>)] = &[("PermissionRequest", None)];

const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_SYNC_KEY: &str = "_rimz_sync";

/// The exact command every rimz-managed Claude hook runs. Identical across all
/// events — the helper reads the event from the stdin payload's
/// `hook_event_name`, so no `--event` flag is needed.
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude";

/// Stable substring identifying a rimz-owned hook command across every form an
/// older build may have written (with `--event`, without `exec`). Used to
/// reclaim legacy and unmarked entries on install and uninstall, so duplicates
/// never accumulate.
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source claude";

/// `settings.json` key holding the statusline command Claude `exec`s on every
/// render. Rimz wraps it so it can capture the rich JSON Claude pipes there.
const STATUS_LINE_KEY: &str = "statusLine";
/// Marker key, on a Rimz-managed `statusLine` object, holding the user's
/// original `statusLine` value verbatim so uninstall restores it exactly.
const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";
/// The statusline command Rimz installs. Fixed (no per-user content) so the
/// install stays idempotent and snapshot-stable; the wrapped original lives
/// under [`RIMZ_WRAPPED_KEY`], not embedded in this string.
const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude";
/// Stable substring identifying Rimz's own statusline reader across command
/// variants — and across both render commands, since the `subagentStatusLine`
/// command is a superstring of this. A statusline command matching this marker
/// is never a user command to wrap or pass through.
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source claude";

/// A statusline-style `settings.json` command Rimz wraps: the key it lives under
/// and the fixed reader command Rimz installs there. The wrap markers
/// ([`RIMZ_WRAPPED_KEY`], [`RIMZ_MANAGED_KEY`]) and the recursion guard
/// ([`RIMZ_STATUS_LINE_MARKER`], a substring of every Rimz reader command) are
/// shared, so one set of upsert/strip/classify logic serves every spec.
struct StatusLineSpec {
    key: &'static str,
    command: &'static str,
}

/// The session statusline: the rich per-render JSON blob Claude pipes for the
/// whole conversation.
const STATUS_LINE: StatusLineSpec = StatusLineSpec {
    key: STATUS_LINE_KEY,
    command: STATUS_LINE_COMMAND,
};

/// The per-child render command Claude `exec`s for each subagent row, carrying
/// the `tasks` array Rimz harvests. Wrapped the same way as the session
/// statusline; its command is the session reader plus `--subagent`.
const SUBAGENT_STATUS_LINE: StatusLineSpec = StatusLineSpec {
    key: "subagentStatusLine",
    command: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude --subagent",
};

#[derive(Clone, Debug, Default)]
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CLAUDE_DESCRIPTOR
    }

    /// `claude --resume <id>` launches straight into the prior session,
    /// restoring its conversation and firing `SessionStart` with
    /// `source: "resume"`. The cwd is set by the launching pane, not the argv.
    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "claude".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn launch_command(&self, prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["claude".to_owned()];
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.push(prompt.to_owned());
        }
        Some(argv)
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let feed_kind = match event_name {
            "PermissionRequest" => Some(FeedKind::Permission),
            // ExitPlanMode / AskUserQuestion self-classify off the tool name on
            // the broad PreToolUse hook; every other tool call is plain lifecycle.
            "PreToolUse" => match parse_pre_tool_use(payload).tool_name.as_deref() {
                Some("ExitPlanMode") => Some(FeedKind::PlanApproval),
                Some("AskUserQuestion") => Some(FeedKind::Question),
                _ => None,
            },
            _ => None,
        };

        classify_agent_hook(
            event_name,
            feed_kind,
            &[
                "SessionStart",
                "SessionEnd",
                "Stop",
                "Notification",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "SubagentStart",
                "SubagentStop",
                "PreCompact",
            ],
        )
    }

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => {
                let output = ClaudePermissionDecisionOutput {
                    hook_specific_output: ClaudePermissionHookOutput {
                        hook_event_name: "PermissionRequest",
                        decision: ClaudePermissionBehavior {
                            behavior: if choice_is_allow(resolution) {
                                "allow"
                            } else {
                                "deny"
                            },
                            updated_input: None,
                            applied_rule: None,
                        },
                    },
                };
                Ok(serde_json::to_value(output)
                    .expect("ClaudePermissionDecisionOutput is infallible"))
            }
            FeedKind::PlanApproval | FeedKind::Question => {
                let updated_input = resolution
                    .decision
                    .get("updatedInput")
                    .or_else(|| resolution.decision.get("updated_input"))
                    .ok_or(AgentErr::MissingField {
                        agent: "claude",
                        field: "updatedInput",
                    })?
                    .clone();
                let output = ClaudePreToolUseDecisionOutput {
                    hook_specific_output: ClaudePreToolUseHookOutput {
                        hook_event_name: "PreToolUse",
                        permission_decision: if choice_is_allow(resolution) {
                            "allow"
                        } else {
                            "deny"
                        },
                        updated_input,
                    },
                };
                Ok(serde_json::to_value(output)
                    .expect("ClaudePreToolUseDecisionOutput is infallible"))
            }
            other => Err(AgentErr::Render {
                agent: "claude",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Claude treats stdout as a control/context surface. The safe no-op is
        // exit 0 with no stdout; only resolver decisions write JSON.
        Ok(None)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // A new prompt starts a fresh turn; a Stop ends the current one. Either
        // way the agent is past any native_ui ask it raised mid-turn — Claude's
        // *main thread* blocks on its own prompt and emits no events until the
        // human answers it, so by the time one of these arrives the ask is
        // settled in its UI. A backgrounded subagent does keep emitting while
        // the main thread blocks, but every in-subagent payload carries the
        // child `agent_id`, so expiry (keyed by `payload_agent_id`) scopes to
        // the child and the lifecycle channel drops the event entirely
        // (`resolve_root_identity`) — neither can settle the parent's ask.
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        // Each event that yields an observation parses through its own typed
        // struct; silent events (read-only PostToolUse, Notification) and the
        // blocking PermissionRequest return `None`. The native-event → signal
        // mapping is the Claude column of docs/internals/hooks.md.
        let session_start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let user_prompt =
            (event_name == "UserPromptSubmit").then(|| parse_user_prompt_submit(payload));
        let subagent_start = (event_name == "SubagentStart").then(|| parse_subagent_start(payload));
        let subagent_stop = (event_name == "SubagentStop").then(|| parse_subagent_stop(payload));
        let stop = (event_name == "Stop").then(|| parse_stop(payload));
        let pending_background = stop
            .as_ref()
            .map(|p| pending_background_tasks(&p.background_tasks))
            .unwrap_or_default();
        // The status decision lives in the shared `lifecycle::step` table —
        // here the adapter only names the intent.
        let signal = match event_name {
            "SessionStart" => LifecycleSignal::Registered,
            "UserPromptSubmit" => LifecycleSignal::TurnStarted,
            // A subagent fires before the child model request, so it registers
            // running under the child `agent_id`; a finished child resolves to
            // success, or failed on a non-zero exit code.
            "SubagentStart" => LifecycleSignal::SubagentStarted,
            "SubagentStop" => LifecycleSignal::SubagentStopped {
                errored: subagent_stop
                    .as_ref()
                    .and_then(|p| p.exit_code)
                    .is_some_and(|code| code != 0),
            },
            // A clean Stop completes the turn; in-flight `background_tasks`
            // (Claude Code v2.1.145+) mean the main thread only parked, so `step`
            // keeps it running and the row paints a secondary background marker
            // rather than a false success.
            "Stop" => LifecycleSignal::TurnEnded {
                errored: stop_payload_errored(payload),
                parked_on_background: !pending_background.is_empty(),
            },
            // Only a *mutating* tool rides the lifecycle channel: it is proof of
            // real work (read-only tools stay silent). The `edits` bit marks the
            // file-writing subset, which ends the turn's thinking head.
            "PostToolUse" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
            },
            // Compaction is a transient head, not a transition: `step` keeps the
            // prior status and only stamps the compacting marker.
            "PreCompact" => LifecycleSignal::Compacting,
            // SessionEnd drops the row; `ends_session` then expires its pending asks.
            "SessionEnd" => LifecycleSignal::Ended,
            _ => return None,
        };
        // Both subagent events flatten the same `ClaudeCommon`; unify on it so the
        // child id / type / parent reads are written once.
        let subagent_common = subagent_start
            .as_ref()
            .map(|p| &p.common)
            .or_else(|| subagent_stop.as_ref().map(|p| &p.common));
        // A subagent keys on its own child id under its parent root; a malformed
        // subagent event (no distinct child id) is quarantined — never folded
        // onto, and never corrupting, the parent's row. Root events key on the
        // session id and carry no parent link — and Claude stamps `agent_id` on
        // every payload fired inside a subagent, so a non-Subagent* event
        // carrying a distinct one is the child's per-tool latency, dropped here
        // rather than folded onto the parent (it would advance the parent past
        // a pending ask; the child-keyed heartbeat carries the activity).
        let (agent_id, parent_agent_id) = match subagent_common {
            Some(c) => match resolve_subagent_identity(
                self.descriptor().kind,
                event_name,
                c.agent_id.as_deref(),
                c.common.session_id.as_deref(),
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
        // Context budget lives in the transcript, not the payload — read its tail
        // on these low-frequency events. Resolve the model first: only the payload
        // id carries the `[1m]` marker that widens the window (the transcript id
        // never does), so it wins over the bare transcript id before the gauge.
        let usage = optional_payload_string(payload, &["session_id"])
            .and_then(|_| optional_payload_string(payload, &["transcript_path"]))
            .map(|path| usage_from_transcript(&path))
            .unwrap_or_default();
        let payload_model = session_start
            .as_ref()
            .and_then(|p| p.common.model.clone())
            .or_else(|| optional_payload_string(payload, &["model"]));
        let model = payload_model.clone().or(usage.model);
        let window = context_window_for(model.as_deref()).max(1);
        let context_pct = payload_context_pct(
            payload,
            usage
                .context_tokens
                .map(|tokens| (tokens.saturating_mul(100) / window).min(100) as u8),
        );
        let (todo_done, todo_total) = todos_from_payload(payload);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        // A subagent labels its row with what it is (`agent_type`) or what it was
        // asked (`description`) — trusted agent metadata, kept across stop so a
        // finished child stays labelled while it lingers in the parent's list. A
        // root labels with the user's *sanitized* task/prompt, so a synthetic
        // background-task count and harness control text never reach the row.
        observation.task = match subagent_common {
            Some(c) => c.agent_type.clone().or_else(|| {
                optional_payload_string(payload, &["subagent_type", "description", "task"])
            }),
            None => sanitize_user_prompt(
                optional_payload_string(payload, &["task", "prompt"]).as_deref(),
            ),
        };
        observation.prompt =
            sanitize_user_prompt(user_prompt.as_ref().and_then(|p| p.prompt.as_deref()));
        observation.model = model;
        // `effort` is an `{ "level": … }` object on the tool-use-context events
        // (Stop / SubagentStop here); the flat `thinking_level` string is a legacy
        // fallback the typed struct does not model.
        observation.effort = stop
            .as_ref()
            .and_then(|p| p.common.effort.as_ref())
            .or_else(|| {
                subagent_stop
                    .as_ref()
                    .and_then(|p| p.common.effort.as_ref())
            })
            .and_then(|e| e.level.clone())
            .or_else(|| optional_payload_string(payload, &["thinking_level"]));
        observation.context_pct = context_pct;
        // The resolved window doubles as the card's window label — published
        // only when the payload named the model, since only the payload id can
        // carry the `[1m]` marker: a transcript-resolved bare id would read as
        // the standard window and clobber a wider carry-forward.
        observation.context_window = payload_model.is_some().then_some(window);
        observation.total_tokens = payload_total_tokens(payload, usage.total_tokens);
        observation.todo_done = todo_done;
        observation.todo_total = todo_total;
        Some(observation)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        // Claude's transport is the statusline JSON blob. Tolerant parse: any
        // non-object payload yields `None` rather than an error.
        let parsed: statusline::StatuslinePayload = serde_json::from_value(payload.clone()).ok()?;
        Some(parsed.into_context(source, Timestamp::now()))
    }

    fn observe_turn_error(&self, payload: &Value) -> Option<AgentTurnError> {
        // The statusline payload names the live transcript, and its tail is the
        // only record of an API-error abort — Claude fires no `Stop` for one
        // (docs/internals/hooks.md → Appendix Claude). Best-effort: an absent
        // path or unreadable file is `None`, never an error.
        let path = optional_payload_string(payload, &["transcript_path"])?;
        let tail = read_transcript_tail(Path::new(&path))?;
        statusline::detect_turn_error(&tail)
    }

    fn observe_subagent_context(&self, payload: &Value) -> Vec<SubagentObservation> {
        // Claude's transport is the `subagentStatusLine` tasks array. Tolerant
        // parse: a non-object payload yields no observations rather than an error.
        let Ok(parsed) = serde_json::from_value::<subagent_statusline::SubagentStatuslinePayload>(
            payload.clone(),
        ) else {
            return Vec::new();
        };
        parsed.into_observations(Timestamp::now())
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        let path = claude_settings_path().ok()?;
        let root = read_existing_json(&path).ok()?;
        wrapped_status_line_command_from(&root, &STATUS_LINE)
    }

    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        let path = claude_settings_path().ok()?;
        let root = read_existing_json(&path).ok()?;
        wrapped_status_line_command_from(&root, &SUBAGENT_STATUS_LINE)
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        let path = claude_settings_path()?;
        install_into(&path)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        let path = claude_settings_path()?;
        preview_install_at(&path)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = claude_settings_path()?;
        uninstall_from(&path)
    }

    fn hooks_installed(&self) -> bool {
        claude_settings_path().is_ok_and(|path| hooks_installed_at(&path))
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::all_jsonl_files()
    }

    /// Current Claude transcripts log no `costUSD`, so each turn is priced
    /// from its `message.usage` through the book; an older transcript's
    /// positive `costUSD` is used verbatim. Lines are independent, so a
    /// resume is a plain offset.
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse_claude_spend(path, resume.map_or(0, |cursor| cursor.offset), prices)
    }
}

/// Whether `path` carries any Rimz-owned hook entry. Best-effort: a missing
/// file or parse error reads as "not installed". Uses [`entry_is_rimz_owned`]
/// (the same ownership predicate as install/uninstall) so that entries whose
/// `_rimz_managed` marker was stripped by an external tool but whose command is
/// still the rimz feed command are still detected as installed.
fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    root.get(HOOKS_KEY)
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.values().any(|entries| {
                entries.as_array().is_some_and(|arr| {
                    arr.iter()
                        .any(|entry| entry.as_object().is_some_and(entry_is_rimz_owned))
                })
            })
        })
}

/// In-flight background tasks from a typed Claude `Stop` payload
/// (`background_tasks`, Claude Code v2.1.145+), as display labels. A `Stop`
/// with pending background work is the main thread parking, not a turn end —
/// it reawakens when the work reports back — so the row must stay live. Each
/// in-flight entry's label is its `description`, else `command`, else `id`; an
/// entry with a terminal `status` (`completed`/`failed`) is no longer in
/// flight and is skipped. An all-terminal or empty slice yields an empty vec:
/// a genuine turn end. Older Claude builds omit the field entirely, which
/// degrades to the same empty vec via the typed struct's `Vec::default()`.
fn pending_background_tasks(tasks: &[BackgroundTask]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            task.status
                .as_deref()
                .is_none_or(|status| !matches!(status, "completed" | "failed"))
        })
        .map(|task| {
            [&task.description, &task.command, &task.id]
                .into_iter()
                .find_map(|opt| opt.as_deref().filter(|label| !label.is_empty()))
                .unwrap_or("background task")
                .to_owned()
        })
        .collect()
}

/// Read a Claude `TodoWrite` payload's `tool_input.todos` (or the post-hook
/// shape under `tool_response.todos`) into a `(done, total)` pair. Returns
/// `(None, None)` when the payload carries no todo state — the snapshot
/// reducer then carries the prior pair forward.
fn todos_from_payload(payload: &Value) -> (Option<u32>, Option<u32>) {
    let todos = ["tool_input", "tool_response", "input", "response"]
        .into_iter()
        .find_map(|key| payload.get(key).and_then(|v| v.get("todos")))
        .or_else(|| payload.get("todos"))
        .and_then(Value::as_array);
    let Some(todos) = todos else {
        return (None, None);
    };
    let total = todos.len() as u32;
    let done = todos
        .iter()
        .filter(|todo| {
            todo.get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s, "completed" | "done"))
        })
        .count() as u32;
    (Some(done), Some(total))
}

/// Context-window usage derived from a Claude transcript tail. Carries the raw
/// context-token count, not a percentage: the window divisor depends on the
/// model variant, and the authoritative model (with its `[1m]` marker) rides
/// the hook payload — not the transcript — so the caller resolves the model and
/// computes the percentage.
#[derive(Default)]
struct TranscriptUsage {
    context_tokens: Option<u64>,
    total_tokens: Option<u64>,
    model: Option<String>,
}

impl TranscriptUsage {
    /// A transcript that opened cleanly but carries no assistant usage yet — a
    /// brand-new session. Report an explicit zero so the gauge draws an empty
    /// bar at 0% instead of vanishing until the first turn completes. A
    /// transcript that cannot be read stays `default()` (all `None`): unknown,
    /// not zero.
    fn fresh() -> Self {
        Self {
            context_tokens: Some(0),
            total_tokens: Some(0),
            model: None,
        }
    }
}

/// Claude's context window depends on the model variant. The 1M-token beta is
/// signalled by a `[1m]` marker on the model id (`claude-opus-4-8[1m]`);
/// everything else is the standard 200k window. The marker only rides the hook
/// payload's `model` field — the transcript always writes the bare id — so
/// callers must resolve the payload model before asking, or the bump is lost.
fn context_window_for(model: Option<&str>) -> u64 {
    const STANDARD: u64 = 200_000;
    const EXTENDED: u64 = 1_000_000;
    match model {
        Some(model) if model.contains("[1m]") => EXTENDED,
        _ => STANDARD,
    }
}

/// Derive context-window usage from the tail of a Claude transcript JSONL.
/// Claude never puts token counts in the hook payload — they live in the
/// transcript — so this is the only place the context gauge can be sourced.
/// Reads a bounded tail and takes the most recent assistant `message.usage`.
/// Best-effort: any IO or parse failure yields empty fields (enrichment, never
/// correctness).
fn usage_from_transcript(path: &str) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(Path::new(path)) else {
        return TranscriptUsage::default();
    };
    // Newest-first: the last assistant usage record wins. A truncated leading
    // line from the tail seek simply fails to parse and is skipped.
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = value.get("message");
        let Some(usage) = message.and_then(|m| m.get("usage")) else {
            continue;
        };
        let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let context_tokens = field("input_tokens")
            + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens");
        let output = field("output_tokens");
        if context_tokens == 0 && output == 0 {
            continue;
        }
        let model = message
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned);
        // Raw tokens only: the window divisor is resolved by the caller from
        // the payload model, which is the one carrying the `[1m]` marker.
        return TranscriptUsage {
            context_tokens: Some(context_tokens),
            total_tokens: Some(context_tokens + output),
            model,
        };
    }
    TranscriptUsage::fresh()
}

fn claude_settings_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_CLAUDE_SETTINGS`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    agent_config_path(
        "claude",
        "RIMZ_CLAUDE_SETTINGS",
        Path::new(".claude/settings.json"),
    )
}

fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path)?;
    write_json(path, &root)?;

    Ok(HookInstallReport {
        agent: "claude",
        config_path: path.to_path_buf(),
        installed_events: installed,
        merged: existed,
    })
}

fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("claude", path)?;
    let existing = read_existing_json(path)?;
    let status_line_change = classify_status_line_change(&existing, &STATUS_LINE);
    let subagent_status_line_change = classify_status_line_change(&existing, &SUBAGENT_STATUS_LINE);
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "claude",
        config_path: path.to_path_buf(),
        planned_events: installed,
        original_config,
        candidate_config: render_json(&root)?,
        merged: existed,
        status_line_change: Some(status_line_change),
        subagent_status_line_change: Some(subagent_status_line_change),
    })
}

fn install_candidate(path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_existing_json(path)?;

    // Defensive: a tampered or stale Rimz write may carry a `_rimz_sync =
    // false` marker on a blocking event. Refuse — installing a blocking
    // hook as non-sync is a hard error.
    reject_async_blocking_in_existing(&root)?;

    // Strip any existing Rimz-managed matchers before writing the fresh
    // set. Mirrors codex's "single source of truth = installer constants".
    let _ = strip_rimz_matchers(&mut root);

    let mut installed = Vec::new();
    for &(event, matcher) in INSTALLED_EVENTS {
        upsert_rimz_matcher(&mut root, event, matcher);
        installed.push(event_label(event, matcher));
    }

    // Wrap both render commands so Rimz captures Claude's rich per-render JSON —
    // the session `statusLine` and the per-child `subagentStatusLine`. Idempotent
    // by construction: a prior Rimz-managed value carries the user's original
    // under `_rimz_wrapped`, which the upsert reads back rather than re-wrapping.
    upsert_rimz_status_line(&mut root, &STATUS_LINE);
    upsert_rimz_status_line(&mut root, &SUBAGENT_STATUS_LINE);

    Ok((root, installed))
}

fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    if !existed {
        return Ok(HookUninstallReport {
            agent: "claude",
            config_path: path.to_path_buf(),
            removed_events: Vec::new(),
            existed: false,
        });
    }
    let mut root = read_existing_json(path)?;
    let removed = strip_rimz_matchers(&mut root);
    // Restore both render commands (or drop the field if Rimz added it).
    strip_rimz_status_line(&mut root, &STATUS_LINE);
    strip_rimz_status_line(&mut root, &SUBAGENT_STATUS_LINE);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "claude",
        config_path: path.to_path_buf(),
        removed_events: removed,
        existed: true,
    })
}

fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: "claude",
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(map) => Ok(map),
                other => Err(AgentErr::Install {
                    agent: "claude",
                    reason: format!(
                        "expected JSON object at the top level of {}; found {}",
                        path.display(),
                        json_type_name(&other),
                    ),
                }),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "claude",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    let text = render_json(root)?;
    atomic::write_bytes_atomically(path, text.as_bytes())?;
    Ok(())
}

fn render_json(root: &Map<String, Value>) -> Result<String> {
    let text = serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
        AgentErr::InstallSerialize {
            agent: "claude",
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Walk the `hooks.<Event>` arrays and reject any matcher tagged
/// `_rimz_managed = true` that targets a blocking event but is marked
/// `_rimz_sync = false`. The set of "must block" events is owned by
/// [`BLOCKING_EVENTS`] — not the on-disk file.
fn reject_async_blocking_in_existing(root: &Map<String, Value>) -> Result<()> {
    let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
        return Ok(());
    };
    for &(event, expected_matcher) in BLOCKING_EVENTS {
        let Some(entries) = hooks.get(event).and_then(Value::as_array) else {
            continue;
        };
        for entry in entries {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            if !is_rimz_managed_object(obj) {
                continue;
            }
            // Match on the same matcher we'd install — entries that don't
            // line up with our (event, matcher) tuple aren't ours even if
            // tagged.
            let actual_matcher = obj.get("matcher").and_then(Value::as_str);
            if matcher_matches(expected_matcher, actual_matcher)
                && !obj
                    .get(RIMZ_SYNC_KEY)
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return Err(AgentErr::Install {
                    agent: "claude",
                    reason: format!(
                        "existing config marks blocking hook `{}` as async; refusing to install",
                        event_label(event, expected_matcher)
                    ),
                });
            }
        }
    }
    Ok(())
}

fn matcher_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (None, Some("")) => true,
        (Some(e), Some(a)) => e == a,
        _ => false,
    }
}

/// Insert or replace the rimz-managed matcher for `(event, matcher)`. Any
/// existing matcher tagged `_rimz_managed = true` with the same matcher value
/// is replaced; user-managed matchers are left untouched.
fn upsert_rimz_matcher(root: &mut Map<String, Value>, event: &str, matcher: Option<&str>) {
    let hooks = root
        .entry(HOOKS_KEY.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = match hooks {
        Value::Object(map) => map,
        _ => {
            // User had a non-object `hooks` value (unusual). Replace it.
            *hooks = Value::Object(Map::new());
            hooks.as_object_mut().expect("just inserted an object")
        }
    };
    let entries = hooks_obj
        .entry(event.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let entries_arr = match entries {
        Value::Array(arr) => arr,
        _ => {
            *entries = Value::Array(Vec::new());
            entries.as_array_mut().expect("just inserted an array")
        }
    };
    // Drop any pre-existing rimz-managed entry for the same matcher.
    entries_arr.retain(|entry| {
        let Some(obj) = entry.as_object() else {
            return true;
        };
        if !is_rimz_managed_object(obj) {
            return true;
        }
        let actual = obj.get("matcher").and_then(Value::as_str);
        !matcher_matches(matcher, actual)
    });
    entries_arr.push(build_matcher_entry(event, matcher));
}

fn build_matcher_entry(event: &str, matcher: Option<&str>) -> Value {
    let blocking = BLOCKING_EVENTS
        .iter()
        .any(|&(e, m)| e == event && matcher_matches(m, matcher));
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".to_owned(), Value::String(m.to_owned()));
    }
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    entry.insert(RIMZ_SYNC_KEY.to_owned(), Value::Bool(blocking));
    // No `--event`: the helper reads `hook_event_name` from the hook's stdin
    // payload, so every installed command is identical (`RIMZ_HOOK_COMMAND`).
    let command = RIMZ_HOOK_COMMAND.to_owned();
    let mut hook = Map::new();
    hook.insert("type".to_owned(), Value::String("command".to_owned()));
    hook.insert("command".to_owned(), Value::String(command));
    hook.insert(
        "timeout".to_owned(),
        Value::Number(CLAUDE_HOOK_TIMEOUT_SECS.into()),
    );
    entry.insert("hooks".to_owned(), Value::Array(vec![Value::Object(hook)]));
    Value::Object(entry)
}

/// Whether a `hooks.<Event>` entry belongs to Rimz: either tagged
/// `_rimz_managed = true`, or its handlers are *solely* the rimz feed command in
/// any historical form (with `--event`, without `exec`). The "solely" guard
/// leaves a user entry that merely embeds a rimz command alongside their own
/// untouched. The command-substring arm reclaims legacy and unmarked entries
/// older builds left behind, so reinstall never stacks duplicates.
fn entry_is_rimz_owned(obj: &Map<String, Value>) -> bool {
    if is_rimz_managed_object(obj) {
        return true;
    }
    let Some(handlers) = obj.get(HOOKS_KEY).and_then(Value::as_array) else {
        return false;
    };
    !handlers.is_empty()
        && handlers.iter().all(|handler| {
            handler
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
        })
}

/// Remove every rimz-owned matcher across the hook tree (see
/// [`entry_is_rimz_owned`]). Returns the labels of removed entries (`Event` or
/// `Event:matcher`).
fn strip_rimz_matchers(root: &mut Map<String, Value>) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(hooks_value) = root.get_mut(HOOKS_KEY) else {
        return removed;
    };
    let Some(hooks_obj) = hooks_value.as_object_mut() else {
        return removed;
    };
    let event_names: Vec<String> = hooks_obj.keys().cloned().collect();
    for event in event_names {
        let Some(entries) = hooks_obj.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        entries.retain(|entry| {
            let Some(obj) = entry.as_object() else {
                return true;
            };
            if entry_is_rimz_owned(obj) {
                let matcher = obj.get("matcher").and_then(Value::as_str);
                removed.push(event_label(&event, matcher));
                false
            } else {
                true
            }
        });
        if entries.is_empty() {
            hooks_obj.remove(&event);
        }
    }
    if hooks_obj.is_empty() {
        root.remove(HOOKS_KEY);
    }
    removed
}

fn event_label(event: &str, matcher: Option<&str>) -> String {
    match matcher {
        Some(m) if !m.is_empty() => format!("{event}:{m}"),
        _ => event.to_owned(),
    }
}

fn is_rimz_managed_object(obj: &Map<String, Value>) -> bool {
    obj.get(RIMZ_MANAGED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Insert or refresh Rimz's `statusLine` wrapper. Idempotent: a prior
/// Rimz-managed statusline has the user's original under `_rimz_wrapped`, which
/// is read back (never double-wrapped); a user's prior statusline of any shape
/// (command object, bare string, or other type) is captured whole; no prior
/// statusline leaves `_rimz_wrapped` absent.
///
/// When the original is a command object, its sibling rendering keys
/// (`padding`, `refreshInterval`, …) are carried onto the managed object so the
/// wrap stays visually faithful while installed — Claude reads them off the
/// top-level object, which would otherwise lose them until uninstall. The whole
/// original is still stored under `_rimz_wrapped` for exact restoration.
fn upsert_rimz_status_line(root: &mut Map<String, Value>, spec: &StatusLineSpec) {
    let existing = root.remove(spec.key);
    let original = match &existing {
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => obj
            .get(RIMZ_WRAPPED_KEY)
            .cloned()
            .and_then(non_recursive_status_line_value),
        Some(other) => non_recursive_status_line_value(other.clone()),
        None => None,
    };
    let mut entry = Map::new();
    // Carry rendering options forward (everything but the command we're
    // replacing and our own markers). Prefer the currently effective object so
    // a repaired managed statusline keeps its visual settings even when its
    // wrapped command is discarded as recursive.
    if let Some(Value::Object(orig)) = existing.as_ref().or(original.as_ref()) {
        for (key, value) in orig {
            if key == "command" || key == RIMZ_MANAGED_KEY || key == RIMZ_WRAPPED_KEY {
                continue;
            }
            entry.insert(key.clone(), value.clone());
        }
    }
    entry.insert("type".to_owned(), Value::String("command".to_owned()));
    entry.insert("command".to_owned(), Value::String(spec.command.to_owned()));
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        entry.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    }
    root.insert(spec.key.to_owned(), Value::Object(entry));
}

/// Restore the user's original command under `spec.key`. When the current one is
/// Rimz-managed, replace it with the captured `_rimz_wrapped` value, or remove
/// the key entirely when nothing was wrapped. A non-Rimz value is left
/// untouched. Returns whether a Rimz-managed value was found.
fn strip_rimz_status_line(root: &mut Map<String, Value>, spec: &StatusLineSpec) -> bool {
    let managed = matches!(
        root.get(spec.key),
        Some(Value::Object(obj)) if is_rimz_managed_object(obj)
    );
    if !managed {
        return false;
    }
    let original = match root.remove(spec.key) {
        Some(Value::Object(mut obj)) => obj
            .remove(RIMZ_WRAPPED_KEY)
            .and_then(non_recursive_status_line_value),
        _ => None,
    };
    if let Some(original) = original {
        root.insert(spec.key.to_owned(), original);
    }
    true
}

/// Classify how an install would change `spec.key`, for the consent summary.
fn classify_status_line_change(
    root: &Map<String, Value>,
    spec: &StatusLineSpec,
) -> StatusLineChange {
    match root.get(spec.key) {
        None => StatusLineChange::Added,
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => StatusLineChange::Unchanged,
        Some(other) => StatusLineChange::Wrapping {
            original: status_line_display(other),
        },
    }
}

/// A readable one-line form of a statusline value for the consent summary: the
/// inner `command` of an object, a bare string verbatim, else compact JSON.
fn status_line_display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(obj) => obj
            .get("command")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    }
}

/// The user's original command that a Rimz-managed value under `spec.key`
/// currently wraps, if any — read from `_rimz_wrapped` (handling both the
/// `{type,command}` object form and a bare command string). `None` when the key
/// is absent, not Rimz-managed, or wraps nothing runnable.
fn wrapped_status_line_command_from(
    root: &Map<String, Value>,
    spec: &StatusLineSpec,
) -> Option<String> {
    let Some(Value::Object(obj)) = root.get(spec.key) else {
        return None;
    };
    if !is_rimz_managed_object(obj) {
        return None;
    }
    extract_status_line_command(obj.get(RIMZ_WRAPPED_KEY)?)
}

fn extract_status_line_command(value: &Value) -> Option<String> {
    status_line_command(value)
        .filter(|command| !is_rimz_status_line_command(command))
        .map(ToOwned::to_owned)
}

fn non_recursive_status_line_value(value: Value) -> Option<Value> {
    if status_line_command(&value).is_some_and(is_rimz_status_line_command) {
        None
    } else {
        Some(value)
    }
}

fn status_line_command(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Object(obj) => obj
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty()),
        _ => None,
    }
}

fn is_rimz_status_line_command(command: &str) -> bool {
    command.contains(RIMZ_STATUS_LINE_MARKER)
}

#[cfg(test)]
mod tests;
