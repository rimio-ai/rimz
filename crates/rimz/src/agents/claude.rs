//! Claude Code hook adapter.
//!
//! Classifies the blocking events (`PermissionRequest`, `PreToolUse:
//! ExitPlanMode`, `PreToolUse: AskUserQuestion`) and the lifecycle events
//! (`SessionStart` registers idle, `UserPromptSubmit` moves to running with
//! the prompt as task, `Stop` completes the turn — success, or failed on an
//! error signal, or back to running when the payload's `background_tasks`
//! still has work in flight, `SessionEnd` exits, `Notification` silent);
//! renders the
//! Claude-shaped `hookSpecificOutput` / `updatedInput` decision payload and the
//! neutral fallback. Context budget is read from the transcript tail.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers. The
//! `PermissionRequest` blocking hook is marked `_rimz_sync = true`; an existing
//! async marker on it is a hard install error (see [`BLOCKING_EVENTS`] and
//! `docs/internals/agent.md`). The `PreToolUse` blocking sub-events ride the
//! broad `PreToolUse` hook and self-classify from `tool_name`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Map, Value, json};

use super::{
    AgentContext, AgentErr, AgentHookClass, AgentIntegration, AgentLifecycleObservation,
    ClassifiedHook, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    StatusLineChange, agent_config_path, choice_is_allow, optional_payload_string,
    permission_decision, read_optional_file, read_transcript_tail, stop_status_from_payload,
};
use crate::feed::{AgentStatus, FeedItem, FeedKind, PermissionPosture, Resolution};
use crate::ledger::atomic;

/// Claude's effective hook cap. The upstream cap is ~125s; we leave a small
/// margin so the bridge never holds the hook past Claude's kill window.
const CLAUDE_HOOK_CAP: Duration = Duration::from_secs(120);

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
/// variants. A statusline command matching this marker is never a user command
/// to wrap or pass through.
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source claude";

#[derive(Clone, Debug, Default)]
pub struct ClaudeIntegration;

impl AgentIntegration for ClaudeIntegration {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let feed_kind = match event_name {
            "PermissionRequest" => Some(FeedKind::Permission),
            "PreToolUse" => match payload.get("tool_name").and_then(Value::as_str) {
                Some("ExitPlanMode") => Some(FeedKind::PlanApproval),
                Some("AskUserQuestion") => Some(FeedKind::Question),
                _ => None,
            },
            _ => None,
        };

        let class = if feed_kind.is_some() {
            AgentHookClass::BlockingFeed
        } else {
            match event_name {
                "SessionStart" | "SessionEnd" | "Stop" | "Notification" | "UserPromptSubmit"
                | "PreToolUse" | "PostToolUse" | "SubagentStart" | "SubagentStop" => {
                    AgentHookClass::Lifecycle
                }
                _ => AgentHookClass::Unknown,
            }
        };

        ClassifiedHook {
            class,
            feed_kind,
            event_name: event_name.to_owned(),
        }
    }

    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value> {
        match item.kind {
            FeedKind::Permission => Ok(permission_decision(resolution)),
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
                Ok(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": if choice_is_allow(resolution) { "allow" } else { "deny" },
                        "updatedInput": updated_input,
                    }
                }))
            }
            other => Err(AgentErr::Render {
                agent: "claude",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Empty object is the documented safe no-op for Claude blocking hooks.
        Ok(Some(json!({})))
    }

    fn hook_cap(&self) -> Duration {
        CLAUDE_HOOK_CAP
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        // A new prompt starts a fresh turn; a Stop ends the current one. Either
        // way the agent is past any native_ui ask it raised mid-turn — Claude
        // blocks on its own prompt and emits no events until the human answers
        // it, so by the time one of these arrives the ask is settled in its UI.
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        // SessionStart registers the agent idle (wired in, nothing asked yet);
        // the prompt is what moves it to running; a clean Stop completes the
        // turn (success), an errored Stop fails it. Only SessionStart
        // establishes the permission posture — the prompt and stop carry no
        // permission field, so they report `None` and the reducer keeps the
        // established posture. SessionEnd records the exit so the reducer drops
        // the agent from the rollup (posture carries no meaning on exit);
        // `ends_session` then expires any asks the dead session left pending.
        // A Claude `Stop` carries `background_tasks` (Claude Code v2.1.145+):
        // the main thread has parked and will reawaken when its background work
        // reports back. While any task is in flight the turn is not over, so we
        // label the row with the background work and (below) keep it running —
        // never paint a false `success` on an agent that is still busy.
        let pending_background = if event_name == "Stop" {
            pending_background_tasks(payload)
        } else {
            Vec::new()
        };
        // The permission slider rides every hook of a session, so sample it on
        // every event: last sample wins, and an event that names no slider
        // returns `None` so the reducer carries the prior posture forward. The
        // slider is self-correcting — Claude moves it off `plan` when the human
        // approves a plan — so the next hook reports the real position; no
        // turn-boundary special-case is needed. `SessionEnd` drops the row, so
        // its posture carries no meaning.
        let (status, posture) = match event_name {
            "SessionStart" => (AgentStatus::Idle, posture_from_payload(payload)),
            "UserPromptSubmit" => (AgentStatus::Running, posture_from_payload(payload)),
            // A subagent fires before the child model request, so it registers
            // running under the child `agent_id`; a finished child returns to
            // idle. Parity with the Codex adapter. Posture is sampled like every
            // other event (the slider is self-correcting).
            "SubagentStart" => (AgentStatus::Running, posture_from_payload(payload)),
            "SubagentStop" => (AgentStatus::Idle, posture_from_payload(payload)),
            "Stop" => (
                stop_status_with_background(payload, &pending_background),
                posture_from_payload(payload),
            ),
            "SessionEnd" => (AgentStatus::Idle, None),
            _ => return None,
        };
        // Context budget lives in the transcript, not the hook payload. Read its
        // tail on the low-frequency events Rimz already fires so the gauge
        // populates without a per-tool hook; an explicit payload field (rare)
        // still wins when present.
        let usage = optional_payload_string(payload, &["session_id"])
            .and_then(|_| optional_payload_string(payload, &["transcript_path"]))
            .map(|path| usage_from_transcript(&path))
            .unwrap_or_default();
        // Resolve the model before the gauge: the payload id carries the `[1m]`
        // marker that sets the window, the transcript id never does.
        let model = optional_payload_string(payload, &["model"]).or(usage.model);
        let context_pct = payload
            .get("context_pct")
            .or_else(|| payload.get("context_window_pct"))
            .and_then(Value::as_u64)
            .map(|v| v.min(100) as u8)
            .or_else(|| {
                let window = context_window_for(model.as_deref()).max(1);
                usage
                    .context_tokens
                    .map(|tokens| (tokens.saturating_mul(100) / window).min(100) as u8)
            });
        let total_tokens = payload
            .get("total_tokens")
            .or_else(|| payload.get("token_count"))
            .and_then(Value::as_u64)
            .or(usage.total_tokens);
        let (todo_done, todo_total) = todos_from_payload(payload);
        Some(AgentLifecycleObservation {
            // Root events key on `session_id`; a subagent event keys on the
            // child's own `agent_id` so the child gets its own row instead of
            // overwriting the parent session's. (`session_id` on a subagent
            // event is the parent root — captured as `parent_agent_id` below.)
            agent_id: match event_name {
                "SubagentStart" | "SubagentStop" => optional_payload_string(payload, &["agent_id"])
                    .or_else(|| optional_payload_string(payload, &["session_id"])),
                _ => optional_payload_string(payload, &["session_id"]),
            },
            status,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            permission_posture: posture,
            worktree_path: optional_payload_string(payload, &["worktree_path", "cwd"]),
            worktree_branch: optional_payload_string(payload, &["worktree_branch"]),
            // A subagent labels its row with what it is (`subagent_type`) or what
            // it was asked (`description`). The type rides both start and stop so
            // a *finished* child keeps its label while it lingers in the parent's
            // list. Root events keep the background-work / prompt label.
            task: match event_name {
                "SubagentStart" | "SubagentStop" => optional_payload_string(
                    payload,
                    &["subagent_type", "agent_type", "description", "task"],
                ),
                _ => background_task_label(&pending_background)
                    .or_else(|| optional_payload_string(payload, &["task", "prompt"])),
            },
            model,
            effort: optional_payload_string(payload, &["thinking_level", "effort"]),
            context_pct,
            total_tokens,
            todo_done,
            todo_total,
            pane_id: None,
            // A subagent event's `session_id` is the parent root the child nests
            // under; root events carry no parent.
            parent_agent_id: match event_name {
                "SubagentStart" | "SubagentStop" => {
                    optional_payload_string(payload, &["session_id"])
                }
                _ => None,
            },
        })
    }

    fn posture_from_payload(&self, payload: &Value) -> Option<PermissionPosture> {
        posture_from_payload(payload)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        // Claude's transport is the statusline JSON blob. Tolerant parse: any
        // non-object payload yields `None` rather than an error.
        let parsed: super::statusline::StatuslinePayload =
            serde_json::from_value(payload.clone()).ok()?;
        Some(parsed.into_context(source, Timestamp::now()))
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        let path = claude_settings_path().ok()?;
        let root = read_existing_json(&path).ok()?;
        wrapped_status_line_command_from(&root)
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

    fn supports_hook_install(&self) -> bool {
        true
    }

    fn hooks_installed(&self) -> bool {
        claude_settings_path().is_ok_and(|path| hooks_installed_at(&path))
    }
}

/// Whether `path` carries any `_rimz_managed` hook matcher. Best-effort: a
/// missing file or parse error reads as "not installed".
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
                        .any(|entry| entry.as_object().is_some_and(is_rimz_managed_object))
                })
            })
        })
}

/// In-flight background tasks reported on a Claude `Stop` payload
/// (`background_tasks`, Claude Code v2.1.145+), as display labels. A `Stop`
/// with pending background work is the main thread parking, not a turn end —
/// it reawakens when the work reports back — so the row must stay live. Each
/// in-flight entry's label is its `description`, else `command`, else `id`; an
/// entry with a terminal `status` (`completed`/`failed`) is no longer in
/// flight and is skipped. An absent or all-terminal array yields an empty vec:
/// a genuine turn end. Older Claude builds omit the field entirely, which
/// degrades to the same empty vec.
fn pending_background_tasks(payload: &Value) -> Vec<String> {
    let Some(tasks) = payload.get("background_tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter(|task| {
            task.get("status")
                .and_then(Value::as_str)
                .is_none_or(|status| !matches!(status, "completed" | "failed"))
        })
        .map(|task| {
            ["description", "command", "id"]
                .into_iter()
                .find_map(|key| task.get(key).and_then(Value::as_str))
                .filter(|label| !label.is_empty())
                .unwrap_or("background task")
                .to_owned()
        })
        .collect()
}

/// Status for a Claude `Stop`. An explicit error wins — the failure is the
/// attention signal — otherwise pending background work upgrades the clean
/// stop to `running` so the sidebar keeps the row live instead of painting a
/// false `success`.
fn stop_status_with_background(payload: &Value, pending: &[String]) -> AgentStatus {
    match stop_status_from_payload(payload) {
        AgentStatus::Success if !pending.is_empty() => AgentStatus::Running,
        other => other,
    }
}

/// Concise task label for a `Stop` parked on background work: the single
/// task's label, or a count when several are in flight. `None` when nothing is
/// pending, so the caller falls back to the payload's own task/prompt.
fn background_task_label(pending: &[String]) -> Option<String> {
    match pending {
        [] => None,
        [one] => Some(one.clone()),
        many => Some(format!("{} background tasks", many.len())),
    }
}

/// Sample a Claude payload's permission slider onto the posture enum.
/// `permission_mode = "bypassPermissions"` (the value
/// `--dangerously-skip-permissions` surfaces) maps to `Yolo`; `plan` is its own
/// first-class read-only posture. `None` when the payload names no slider field,
/// so the reducer carries the prior posture forward rather than resetting it —
/// the slider is sticky and rides every hook, so absence means "unchanged".
fn posture_from_payload(payload: &Value) -> Option<PermissionPosture> {
    let raw = payload
        .get("permission_mode")
        .or_else(|| payload.get("mode"))
        .and_then(Value::as_str);
    Some(match raw {
        Some("bypassPermissions") | Some("bypass") => PermissionPosture::Yolo,
        Some("acceptEdits") | Some("auto") => PermissionPosture::Auto,
        Some("plan") => PermissionPosture::Plan,
        Some("default") | Some("interactive") | Some("ask") => PermissionPosture::Default,
        Some(_) => PermissionPosture::Unknown,
        None => return None,
    })
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
    let status_line_change = classify_status_line_change(&read_existing_json(path)?);
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "claude",
        config_path: path.to_path_buf(),
        planned_events: installed,
        original_config,
        candidate_config: render_json(&root)?,
        merged: existed,
        status_line_change: Some(status_line_change),
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

    // Wrap the statusline so Rimz captures Claude's rich per-render JSON. Idempotent
    // by construction: a prior Rimz-managed statusline carries the user's original
    // under `_rimz_wrapped`, which the upsert reads back rather than re-wrapping.
    upsert_rimz_status_line(&mut root);

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
    // Restore the user's original statusline (or drop the field if Rimz added it).
    strip_rimz_status_line(&mut root);
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
fn upsert_rimz_status_line(root: &mut Map<String, Value>) {
    let existing = root.remove(STATUS_LINE_KEY);
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
    entry.insert(
        "command".to_owned(),
        Value::String(STATUS_LINE_COMMAND.to_owned()),
    );
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        entry.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    }
    root.insert(STATUS_LINE_KEY.to_owned(), Value::Object(entry));
}

/// Restore the user's original `statusLine`. When the current one is
/// Rimz-managed, replace it with the captured `_rimz_wrapped` value, or remove
/// the key entirely when nothing was wrapped. A non-Rimz statusline is left
/// untouched. Returns whether a Rimz-managed statusline was found.
fn strip_rimz_status_line(root: &mut Map<String, Value>) -> bool {
    let managed = matches!(
        root.get(STATUS_LINE_KEY),
        Some(Value::Object(obj)) if is_rimz_managed_object(obj)
    );
    if !managed {
        return false;
    }
    let original = match root.remove(STATUS_LINE_KEY) {
        Some(Value::Object(mut obj)) => obj
            .remove(RIMZ_WRAPPED_KEY)
            .and_then(non_recursive_status_line_value),
        _ => None,
    };
    if let Some(original) = original {
        root.insert(STATUS_LINE_KEY.to_owned(), original);
    }
    true
}

/// Classify how an install would change the statusline, for the consent summary.
fn classify_status_line_change(root: &Map<String, Value>) -> StatusLineChange {
    match root.get(STATUS_LINE_KEY) {
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

/// The user's original statusline command that a Rimz-managed `statusLine`
/// currently wraps, if any — read from `_rimz_wrapped` (handling both the
/// `{type,command}` object form and a bare command string). `None` when the
/// statusline is absent, not Rimz-managed, or wraps nothing runnable.
fn wrapped_status_line_command_from(root: &Map<String, Value>) -> Option<String> {
    let Some(Value::Object(obj)) = root.get(STATUS_LINE_KEY) else {
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
mod tests {
    use super::*;
    use crate::feed::{ResolutionMethod, Surface};
    use crate::ids::WorkspaceId;
    use std::path::Path;

    fn fixture(kind: FeedKind) -> FeedItem {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
        FeedItem::new(
            workspace,
            Surface::Bridge,
            kind,
            "allow?",
            "claude",
            "agent-hook",
        )
    }

    #[test]
    fn permission_allow_shape_is_pinned() {
        let item = fixture(FeedKind::Permission);
        let resolution =
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
        let rendered = ClaudeIntegration
            .render_decision(&item, &resolution)
            .unwrap();
        insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "allow"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
        assert_eq!(
            rendered["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        assert_eq!(
            rendered["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
    }

    #[test]
    fn plan_approval_requires_updated_input() {
        let item = fixture(FeedKind::PlanApproval);
        let resolution =
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
        let err = ClaudeIntegration
            .render_decision(&item, &resolution)
            .unwrap_err();
        assert!(matches!(
            err,
            AgentErr::MissingField {
                agent: "claude",
                field: "updatedInput"
            }
        ));
    }

    #[test]
    fn neutral_payload_is_empty_object() {
        let value = ClaudeIntegration
            .render_neutral("PermissionRequest")
            .unwrap();
        insta::assert_snapshot!(
            serde_json::to_string(&value).unwrap(),
            @"{}"
        );
        assert_eq!(value, Some(json!({})));
    }

    #[test]
    fn permission_deny_shape_is_pinned() {
        let item = fixture(FeedKind::Permission);
        let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
        let rendered = ClaudeIntegration
            .render_decision(&item, &resolution)
            .unwrap();

        insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
    }

    #[test]
    fn plan_approval_allow_shape_is_pinned() {
        let item = fixture(FeedKind::PlanApproval);
        let resolution = Resolution::new(
            json!({ "choice": "allow", "updatedInput": "ship the plan" }),
            ResolutionMethod::HookBridge,
        );
        let rendered = ClaudeIntegration
            .render_decision(&item, &resolution)
            .unwrap();

        insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": "ship the plan"
          }
        }
        "###);
    }

    #[test]
    fn ask_user_question_allow_shape_carries_updated_input_object() {
        let item = fixture(FeedKind::Question);
        let resolution = Resolution::new(
            json!({ "choice": "allow", "updatedInput": { "question": "ready?" } }),
            ResolutionMethod::HookBridge,
        );
        let rendered = ClaudeIntegration
            .render_decision(&item, &resolution)
            .unwrap();

        insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
              "question": "ready?"
            }
          }
        }
        "###);
    }

    #[test]
    fn classify_pretooluse_exit_plan_mode_is_plan_approval() {
        let c =
            ClaudeIntegration.classify_hook("PreToolUse", &json!({ "tool_name": "ExitPlanMode" }));
        assert_eq!(c.class, AgentHookClass::BlockingFeed);
        assert_eq!(c.feed_kind, Some(FeedKind::PlanApproval));
    }

    #[test]
    fn classify_pretooluse_ask_user_question_is_question() {
        let c = ClaudeIntegration
            .classify_hook("PreToolUse", &json!({ "tool_name": "AskUserQuestion" }));
        assert_eq!(c.class, AgentHookClass::BlockingFeed);
        assert_eq!(c.feed_kind, Some(FeedKind::Question));
    }

    #[test]
    fn classify_subagent_events_are_lifecycle() {
        for event in ["SubagentStart", "SubagentStop"] {
            let c = ClaudeIntegration.classify_hook(event, &json!({}));
            assert_eq!(c.class, AgentHookClass::Lifecycle, "{event}");
            assert_eq!(c.feed_kind, None, "{event}");
        }
    }

    #[test]
    fn subagent_start_observes_running_child_keyed_by_agent_id() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SubagentStart",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-1",
                    "subagent_type": "Explore",
                    "description": "search the ledger",
                    "permission_mode": "acceptEdits",
                }),
            )
            .unwrap();

        // Keyed off the child's own id, not the parent session.
        assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
        assert_eq!(obs.status, AgentStatus::Running);
        // The type labels the child row; `session_id` is captured as the parent.
        assert_eq!(obs.task.as_deref(), Some("Explore"));
        assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
        assert_eq!(obs.permission_posture, Some(PermissionPosture::Auto));
    }

    #[test]
    fn subagent_stop_returns_child_idle_keeping_its_label() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SubagentStop",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-1",
                    "agent_type": "Explore",
                }),
            )
            .unwrap();

        assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
        assert_eq!(obs.status, AgentStatus::Idle);
        // The label persists past stop; the parent link survives.
        assert_eq!(obs.task.as_deref(), Some("Explore"));
        assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
    }

    #[test]
    fn root_lifecycle_event_carries_no_parent() {
        let obs = ClaudeIntegration
            .observe_lifecycle("UserPromptSubmit", &json!({ "session_id": "sess-root" }))
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-root"));
        assert_eq!(obs.parent_agent_id, None);
    }

    #[test]
    fn hook_cap_is_120_seconds() {
        assert_eq!(ClaudeIntegration.hook_cap(), Duration::from_secs(120));
    }

    #[test]
    fn install_into_empty_dir_creates_managed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_into(&path).unwrap();
        assert!(!report.merged);
        assert_eq!(report.agent, "claude");
        assert!(report.installed_events.contains(&"SessionStart".to_owned()));
        assert!(report.installed_events.contains(&"PreToolUse".to_owned()));
        assert!(
            report
                .installed_events
                .contains(&"PermissionRequest".to_owned())
        );

        // Lock the full on-disk shape: event set, sync flags, command strings,
        // and the 120 s blocking-hook timeout. Every command is identical (no
        // `--event`; the helper reads the event from stdin), and every event
        // installs as a single broad hook with no matcher — `PreToolUse`
        // self-classifies its blocking sub-events from `tool_name`. The file is
        // deterministic, so the whole settings.json snapshots cleanly.
        let written = std::fs::read_to_string(&path).unwrap();
        insta::assert_snapshot!(written, @r###"
        {
          "hooks": {
            "Notification": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PermissionRequest": [
              {
                "_rimz_managed": true,
                "_rimz_sync": true,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PostToolUse": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PreToolUse": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SessionEnd": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SessionStart": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "Stop": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SubagentStart": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SubagentStop": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "UserPromptSubmit": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ]
          },
          "statusLine": {
            "_rimz_managed": true,
            "command": "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude",
            "type": "command"
          }
        }
        "###);
    }

    #[test]
    fn install_preserves_user_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "model": "claude-opus-4-7",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ],
                "UserPromptSubmit": [
                  { "hooks": [{ "type": "command", "command": "echo prompt" }] }
                ]
              }
            }"#,
        )
        .unwrap();
        let report = install_into(&path).unwrap();
        assert!(report.merged);

        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["model"], "claude-opus-4-7");
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        // user `Bash` matcher + 1 rimz broad per-tool hook (no matcher).
        assert_eq!(pre_tool.len(), 2);
        assert!(pre_tool.iter().any(|e| e["matcher"] == "Bash"
            && e.get("_rimz_managed").and_then(Value::as_bool) != Some(true)));
        // UserPromptSubmit is state signal, so a default install wires it. The
        // user's own UserPromptSubmit hook is preserved alongside ours.
        let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2);
        assert!(
            ups.iter()
                .any(|e| e.get("_rimz_managed").and_then(Value::as_bool) != Some(true))
        );
        assert!(
            ups.iter()
                .any(|e| e.get("_rimz_managed").and_then(Value::as_bool) == Some(true))
        );
    }

    #[test]
    fn install_wires_non_blocking_per_tool_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_into(&path).unwrap();
        assert!(report.installed_events.contains(&"PreToolUse".to_owned()));
        assert!(report.installed_events.contains(&"PostToolUse".to_owned()));

        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        // Exactly 1 broad per-tool hook (no matcher); the blocking sub-events
        // self-classify off it rather than getting a dedicated matcher entry.
        assert_eq!(pre_tool.len(), 1);
        // The broad per-tool hook has no matcher key and is non-blocking.
        let broad = pre_tool
            .iter()
            .find(|e| !e.as_object().unwrap().contains_key("matcher"))
            .unwrap();
        assert_eq!(broad["_rimz_managed"], true);
        assert_eq!(broad["_rimz_sync"], false);
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_into(&path).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        install_into(&path).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            first, second,
            "second install must produce identical config"
        );
    }

    #[test]
    fn install_reclaims_legacy_and_duplicate_entries() {
        // Reproduces a bloated real-world file: legacy *unmarked* rimz copies
        // (older builds wrote `--event` and no marker) stacked alongside an old
        // separate-matcher managed entry, plus a genuine user hook. Install must
        // reclaim every rimz-owned entry — marked or not — and leave exactly the
        // canonical set, with the user hook untouched.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "Notification": [
                  { "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Notification" }] },
                  { "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Notification" }] }
                ],
                "PreToolUse": [
                  { "matcher": "ExitPlanMode", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "_rimz_managed": true, "_rimz_sync": true, "matcher": "ExitPlanMode", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
              }
            }"#,
        )
        .unwrap();
        install_into(&path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("--event"),
            "every legacy `--event` command must be reclaimed: {written}"
        );

        let parsed: Value = serde_json::from_slice(written.as_bytes()).unwrap();
        let managed =
            |entry: &Value| entry.get("_rimz_managed").and_then(Value::as_bool) == Some(true);

        // Two stacked legacy copies collapse to one managed Notification hook.
        let notif = parsed["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(notif.len(), 1);
        assert!(managed(&notif[0]));

        // PreToolUse: the user `Bash` hook survives; the two unmarked legacy
        // matchers and the old separate managed matcher are all reclaimed,
        // replaced by the single broad hook.
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 2);
        assert!(
            pre_tool
                .iter()
                .any(|e| e["matcher"] == "Bash" && !managed(e)),
            "user Bash hook preserved"
        );
        assert!(
            pre_tool.iter().any(|e| managed(e)
                && !e.as_object().unwrap().contains_key("matcher")
                && e["_rimz_sync"] == false),
            "broad enrichment hook present"
        );
        // Exactly the one canonical rimz entry — no stale duplicates.
        assert_eq!(pre_tool.iter().filter(|e| managed(e)).count(), 1);
    }

    #[test]
    fn uninstall_removes_managed_entries_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "model": "claude-opus-4-7",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
              }
            }"#,
        )
        .unwrap();
        install_into(&path).unwrap();
        let report = uninstall_from(&path).unwrap();
        assert!(report.existed);
        assert!(!report.removed_events.is_empty());

        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["model"], "claude-opus-4-7");
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1);
        assert_eq!(pre_tool[0]["matcher"], "Bash");
        // PermissionRequest was rimz-only — entire key removed when empty.
        assert!(parsed["hooks"].get("PermissionRequest").is_none());
    }

    #[test]
    fn uninstall_on_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = uninstall_from(&path).unwrap();
        assert!(!report.existed);
        assert!(report.removed_events.is_empty());
    }

    #[test]
    fn install_adds_status_line_when_none_existed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_into(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
        assert_eq!(parsed["statusLine"]["_rimz_managed"], true);
        // Nothing was wrapped, so no `_rimz_wrapped`.
        assert!(parsed["statusLine"].get("_rimz_wrapped").is_none());
    }

    #[test]
    fn install_wraps_existing_status_line_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "npx -y ccstatusline@latest" } }"#,
        )
        .unwrap();
        install_into(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
        assert_eq!(parsed["statusLine"]["_rimz_managed"], true);
        // The user's whole original value is captured verbatim.
        assert_eq!(
            parsed["statusLine"]["_rimz_wrapped"]["command"],
            "npx -y ccstatusline@latest"
        );
        assert_eq!(parsed["statusLine"]["_rimz_wrapped"]["type"], "command");
    }

    #[test]
    fn install_preserves_status_line_sibling_keys() {
        // A real ccstatusline config carries rendering options alongside the
        // command. They must ride the managed object so the wrap stays visually
        // faithful while installed, and the whole original still restores.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "npx -y ccstatusline@latest", "padding": 0, "refreshInterval": 10 } }"#,
        )
        .unwrap();
        install_into(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
        // Sibling rendering keys are carried onto the managed object.
        assert_eq!(parsed["statusLine"]["padding"], 0);
        assert_eq!(parsed["statusLine"]["refreshInterval"], 10);
        // The whole original is still captured for restoration.
        assert_eq!(parsed["statusLine"]["_rimz_wrapped"]["refreshInterval"], 10);

        uninstall_from(&path).unwrap();
        let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            restored["statusLine"]["command"],
            "npx -y ccstatusline@latest"
        );
        assert_eq!(restored["statusLine"]["padding"], 0);
        assert_eq!(restored["statusLine"]["refreshInterval"], 10);
        assert!(restored["statusLine"].get("_rimz_managed").is_none());
    }

    #[test]
    fn reinstall_does_not_double_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "user-line" } }"#,
        )
        .unwrap();
        install_into(&path).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        install_into(&path).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "re-install must be byte-identical");
        let parsed: Value = serde_json::from_str(&second).unwrap();
        // Still the user's command, not a nested Rimz wrapper.
        assert_eq!(
            parsed["statusLine"]["_rimz_wrapped"]["command"],
            "user-line"
        );
        assert!(
            parsed["statusLine"]["_rimz_wrapped"]
                .get("_rimz_wrapped")
                .is_none()
        );
    }

    #[test]
    fn reinstall_repairs_recursive_status_line_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "statusLine": {
                    "_rimz_managed": true,
                    "_rimz_wrapped": {
                        "type": "command",
                        "command": STATUS_LINE_COMMAND,
                        "padding": 0,
                        "refreshInterval": 10
                    },
                    "type": "command",
                    "command": STATUS_LINE_COMMAND,
                    "padding": 0,
                    "refreshInterval": 10
                }
            }))
            .unwrap(),
        )
        .unwrap();

        install_into(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
        assert!(
            parsed["statusLine"].get("_rimz_wrapped").is_none(),
            "a Rimz statusline command is not a user command to wrap"
        );
        assert_eq!(parsed["statusLine"]["padding"], 0);
        assert_eq!(parsed["statusLine"]["refreshInterval"], 10);
    }

    #[test]
    fn uninstall_removes_recursive_status_line_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "statusLine": {
                    "_rimz_managed": true,
                    "_rimz_wrapped": {
                        "type": "command",
                        "command": STATUS_LINE_COMMAND
                    },
                    "type": "command",
                    "command": STATUS_LINE_COMMAND
                }
            }))
            .unwrap(),
        )
        .unwrap();

        uninstall_from(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            parsed.get("statusLine").is_none(),
            "uninstall must not restore Rimz's own statusline command"
        );
    }

    #[test]
    fn wrapped_status_line_command_ignores_recursive_rimz_wrap() {
        let root: Map<String, Value> = serde_json::from_value(json!({
            "statusLine": {
                "_rimz_managed": true,
                "_rimz_wrapped": {
                    "type": "command",
                    "command": STATUS_LINE_COMMAND
                },
                "type": "command",
                "command": STATUS_LINE_COMMAND
            }
        }))
        .unwrap();

        assert_eq!(wrapped_status_line_command_from(&root), None);
    }

    #[test]
    fn uninstall_restores_original_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = r#"{ "statusLine": { "type": "command", "command": "npx ccstatusline" } }"#;
        std::fs::write(&path, original).unwrap();
        install_into(&path).unwrap();
        uninstall_from(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["command"], "npx ccstatusline");
        assert_eq!(parsed["statusLine"]["type"], "command");
        assert!(parsed["statusLine"].get("_rimz_managed").is_none());
    }

    #[test]
    fn uninstall_removes_status_line_when_none_existed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_into(&path).unwrap();
        uninstall_from(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            parsed.get("statusLine").is_none(),
            "a Rimz-added statusLine is removed on uninstall"
        );
    }

    #[test]
    fn install_captures_and_restores_bare_string_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{ "statusLine": "echo hi" }"#).unwrap();
        install_into(&path).unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["statusLine"]["_rimz_wrapped"], "echo hi");
        // The feed command reads the bare string back as the pass-through target.
        let root = read_existing_json(&path).unwrap();
        assert_eq!(
            wrapped_status_line_command_from(&root).as_deref(),
            Some("echo hi")
        );
        uninstall_from(&path).unwrap();
        let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored["statusLine"], "echo hi");
    }

    #[test]
    fn classify_status_line_change_reports_each_case() {
        let none = Map::new();
        assert_eq!(classify_status_line_change(&none), StatusLineChange::Added);

        let user: Map<String, Value> = serde_json::from_str(
            r#"{ "statusLine": { "type": "command", "command": "npx ccstatusline" } }"#,
        )
        .unwrap();
        assert_eq!(
            classify_status_line_change(&user),
            StatusLineChange::Wrapping {
                original: "npx ccstatusline".to_owned()
            }
        );

        let mut managed = Map::new();
        upsert_rimz_status_line(&mut managed);
        assert_eq!(
            classify_status_line_change(&managed),
            StatusLineChange::Unchanged
        );
    }

    #[test]
    fn hooks_installed_at_detects_managed_matcher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(
            !hooks_installed_at(&path),
            "a missing settings file reads as not installed"
        );
        install_into(&path).unwrap();
        assert!(
            hooks_installed_at(&path),
            "an installed settings file reads as installed"
        );
        uninstall_from(&path).unwrap();
        assert!(
            !hooks_installed_at(&path),
            "an uninstalled settings file reads as not installed"
        );
    }

    #[test]
    fn hooks_installed_at_ignores_user_only_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [] } ] } }"#,
        )
        .unwrap();
        assert!(
            !hooks_installed_at(&path),
            "user-managed hooks with no _rimz_managed marker are not installed"
        );
    }

    #[test]
    fn install_rejects_async_blocking_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A tampered config marks a rimz-managed PermissionRequest matcher
        // with `_rimz_sync = false`. The installer must refuse — the source
        // of truth for "must block" is BLOCKING_EVENTS, never the file.
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "PermissionRequest": [
                  {
                    "_rimz_managed": true,
                    "_rimz_sync": false,
                    "hooks": [{ "type": "command", "command": "x" }]
                  }
                ]
              }
            }"#,
        )
        .unwrap();
        let err = install_into(&path).unwrap_err();
        assert!(matches!(
            err,
            AgentErr::Install {
                agent: "claude",
                ..
            }
        ));
    }

    #[test]
    fn install_rejects_top_level_non_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "[]").unwrap();
        let err = install_into(&path).unwrap_err();
        assert!(matches!(
            err,
            AgentErr::Install {
                agent: "claude",
                ..
            }
        ));
    }

    #[test]
    fn session_start_observes_idle_status() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "permission_mode": "default" }),
            )
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        // Wired in, nothing asked yet — idle, no task.
        assert_eq!(obs.status, AgentStatus::Idle);
        assert_eq!(obs.task, None);
        assert_eq!(obs.permission_posture, Some(PermissionPosture::Default));
    }

    #[test]
    fn user_prompt_submit_observes_running_with_prompt_task() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
            )
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.task.as_deref(), Some("fix auth flow"));
        // The prompt reports no posture, so the reducer keeps the posture
        // SessionStart established (a yolo agent stays yolo).
        assert_eq!(obs.permission_posture, None);
    }

    #[test]
    fn bypass_permissions_observes_yolo_posture() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "permission_mode": "bypassPermissions" }),
            )
            .unwrap();
        assert_eq!(obs.permission_posture, Some(PermissionPosture::Yolo));
    }

    #[test]
    fn posture_sampled_from_permission_mode() {
        // The slider maps onto the posture enum: `plan` is a first-class sticky
        // posture (the read-only "thinking" signal), `acceptEdits` is `Auto`, and
        // an absent mode reports `None` so the reducer carries the prior posture
        // forward.
        let plan = ClaudeIntegration
            .observe_lifecycle("SessionStart", &json!({ "permission_mode": "plan" }))
            .unwrap();
        assert_eq!(plan.permission_posture, Some(PermissionPosture::Plan));

        let acting = ClaudeIntegration
            .observe_lifecycle("SessionStart", &json!({ "permission_mode": "acceptEdits" }))
            .unwrap();
        assert_eq!(acting.permission_posture, Some(PermissionPosture::Auto));

        let silent = ClaudeIntegration
            .observe_lifecycle("UserPromptSubmit", &json!({ "session_id": "sess-1" }))
            .unwrap();
        assert_eq!(silent.permission_posture, None);
    }

    #[test]
    fn stop_samples_slider_last_sample_wins() {
        // The slider is sticky and rides every hook, `Stop` included: a `Stop`
        // still carrying `plan` reports `Plan` (the session is still in plan
        // mode), and a mode-less `Stop` reports `None` so the reducer carries the
        // prior posture forward. No turn-boundary special-case — the slider
        // self-corrects when the human approves and Claude moves it off `plan`.
        let mode_less = ClaudeIntegration
            .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
            .unwrap();
        assert_eq!(mode_less.permission_posture, None);

        let slider_still_plan = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({ "session_id": "sess-1", "permission_mode": "plan" }),
            )
            .unwrap();
        assert_eq!(
            slider_still_plan.permission_posture,
            Some(PermissionPosture::Plan)
        );
    }

    #[test]
    fn todo_write_payload_extracts_progress() {
        // Claude TodoWrite hooks expose the todo list in `tool_input.todos`;
        // the reducer projects the count of completed items onto the row.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({
                    "session_id": "sess-1",
                    "tool_input": {
                        "todos": [
                            { "status": "completed" },
                            { "status": "completed" },
                            { "status": "in_progress" },
                            { "status": "pending" },
                        ]
                    }
                }),
            )
            .unwrap();
        assert_eq!(obs.todo_done, Some(2));
        assert_eq!(obs.todo_total, Some(4));
    }

    #[test]
    fn notification_event_is_not_a_lifecycle_observation() {
        let obs = ClaudeIntegration.observe_lifecycle("Notification", &json!({}));
        assert!(obs.is_none());
    }

    #[test]
    fn clean_stop_observes_success() {
        // A Stop fires only after a turn ran; a clean end completes it.
        let obs = ClaudeIntegration
            .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Success);
        // Turn over: the task clears back to "—".
        assert_eq!(obs.task, None);
    }

    #[test]
    fn errored_stop_observes_failed() {
        let obs = ClaudeIntegration
            .observe_lifecycle("Stop", &json!({ "session_id": "sess-1", "is_error": true }))
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Failed);
    }

    #[test]
    fn stop_with_pending_background_tasks_observes_running() {
        // Claude Code v2.1.145+ reports in-flight `background_tasks` on Stop.
        // The main thread has parked waiting for that work to reawaken it — the
        // turn is not over, so the row stays running (never a false success)
        // and is labelled with the single task's description.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "background_tasks": [
                        {
                            "id": "task-1",
                            "type": "command",
                            "command": "npm run build",
                            "status": "running",
                            "description": "Build process"
                        }
                    ]
                }),
            )
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.task.as_deref(), Some("Build process"));
    }

    #[test]
    fn stop_with_multiple_pending_background_tasks_labels_count() {
        // Several in-flight tasks collapse to a count — the row says it is busy
        // without trying to render every label.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "background_tasks": [
                        { "id": "a", "status": "running", "description": "lint" },
                        { "id": "b", "status": "running", "description": "test" }
                    ]
                }),
            )
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.task.as_deref(), Some("2 background tasks"));
    }

    #[test]
    fn stop_with_only_completed_background_tasks_observes_success() {
        // A registry that reports only terminal tasks has nothing in flight —
        // this is a genuine turn end, so the clean stop stays success.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "background_tasks": [
                        { "id": "task-1", "status": "completed", "description": "Build process" }
                    ]
                }),
            )
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Success);
        assert_eq!(obs.task, None);
    }

    #[test]
    fn errored_stop_with_pending_background_tasks_still_observes_failed() {
        // The failure is the attention signal: an errored turn stays failed
        // even while background work is still in flight.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "is_error": true,
                    "background_tasks": [
                        { "id": "task-1", "status": "running", "description": "Build process" }
                    ]
                }),
            )
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Failed);
    }

    #[test]
    fn transcript_tail_populates_context_gauge() {
        // Claude reports token usage only in the transcript JSONL; the Stop hook
        // reads its tail to fill the context gauge. 100k of a 200k window = 50%.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "transcript_path": transcript.to_str().unwrap(),
                }),
            )
            .unwrap();
        assert_eq!(obs.context_pct, Some(50));
        assert_eq!(obs.total_tokens, Some(100_500));
        assert_eq!(obs.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn payload_one_million_marker_widens_the_context_window() {
        // The 1M beta is signalled by a `[1m]` marker that rides only the hook
        // payload's model field — the transcript writes the bare id. The gauge
        // must divide by the payload-resolved window: 100k of 1M = 10%, where
        // the bare-id default would have over-read it as 50%.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "model": "claude-opus-4-8[1m]",
                    "transcript_path": transcript.to_str().unwrap(),
                }),
            )
            .unwrap();
        assert_eq!(obs.context_pct, Some(10));
        assert_eq!(obs.total_tokens, Some(100_500));
        assert_eq!(obs.model.as_deref(), Some("claude-opus-4-8[1m]"));
    }

    #[test]
    fn fresh_transcript_reports_zero_context_not_unknown() {
        // A brand-new session has a transcript with no assistant usage yet. It
        // must read as 0% (empty gauge), not None (no gauge), so a just-launched
        // idle agent shows an empty context bar.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n",
        )
        .unwrap();
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({
                    "session_id": "sess-1",
                    "transcript_path": transcript.to_str().unwrap(),
                }),
            )
            .unwrap();
        assert_eq!(obs.context_pct, Some(0));
        assert_eq!(obs.total_tokens, Some(0));
    }

    #[test]
    fn missing_transcript_leaves_context_unknown() {
        // No readable transcript means unknown, not zero — the gauge stays
        // hidden rather than asserting a false 0%.
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({
                    "session_id": "sess-1",
                    "transcript_path": "/nonexistent/path/session.jsonl",
                }),
            )
            .unwrap();
        assert_eq!(obs.context_pct, None);
        assert_eq!(obs.total_tokens, None);
    }

    #[test]
    fn transcript_requires_session_id() {
        // Transcript reads are keyed by the agent's own session identity. A
        // transcript path without a session id stays unknown; the sidebar row
        // projection is responsible for the visible 0% baseline.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":\
             {\"input_tokens\":100000,\"output_tokens\":500}}}\n",
        )
        .unwrap();
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "transcript_path": transcript.to_str().unwrap() }),
            )
            .unwrap();
        assert_eq!(obs.context_pct, None);
        assert_eq!(obs.total_tokens, None);
    }

    #[test]
    fn session_end_is_recorded_and_ends_the_session() {
        // SessionEnd must produce an observation so the reducer drops the agent
        // from the rollup, and must report `ends_session` so the CLI expires
        // the dead session's pending asks.
        let obs = ClaudeIntegration
            .observe_lifecycle("SessionEnd", &json!({ "session_id": "sess-1" }))
            .expect("SessionEnd is a recorded lifecycle observation");
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert!(ClaudeIntegration.ends_session("SessionEnd"));
        assert!(!ClaudeIntegration.ends_session("Stop"));
    }

    #[test]
    fn turn_boundaries_move_the_session_on() {
        // Stop and a fresh prompt clear the session's mid-turn native_ui asks;
        // SessionStart/SessionEnd and tool events do not.
        assert!(ClaudeIntegration.moves_on("Stop"));
        assert!(ClaudeIntegration.moves_on("UserPromptSubmit"));
        assert!(!ClaudeIntegration.moves_on("SessionStart"));
        assert!(!ClaudeIntegration.moves_on("SessionEnd"));
        assert!(!ClaudeIntegration.moves_on("PostToolUse"));
    }
}
