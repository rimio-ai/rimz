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
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers.
//! Blocking events are marked `_rimz_sync = true`; an existing async marker
//! on a blocking event is a hard install error (see [`BLOCKING_EVENTS`] and
//! `docs/internals/agent.md`).

use std::env;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    AgentErr, AgentHookClass, AgentIntegration, AgentLifecycleObservation, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, choice_is_allow,
    optional_payload_string, stop_status_from_payload,
};
use crate::feed::{AgentStatus, FeedItem, FeedKind, PermissionPosture, Resolution};
use crate::ledger::atomic;

/// Claude's effective hook cap. The upstream cap is ~125s; we leave a small
/// margin so the bridge never holds the hook past Claude's kill window.
const CLAUDE_HOOK_CAP: Duration = Duration::from_secs(120);

/// Per-hook timeout written into the Claude config (seconds). Matches
/// [`CLAUDE_HOOK_CAP`] so the agent and bridge agree on the ceiling.
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 120;

/// Default-install events. Tuple is `(event_name, optional_matcher)` — the
/// matcher is `Some(_)` for `PreToolUse` sub-events that target a specific
/// Claude tool (`ExitPlanMode`, `AskUserQuestion`).
const DEFAULT_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("SessionEnd", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreToolUse", Some("ExitPlanMode")),
    ("PreToolUse", Some("AskUserQuestion")),
];

/// Telemetry-install events (added when `--telemetry` is passed). These are
/// high-frequency, content-heavy hooks for audit depth — `UserPromptSubmit`
/// and `Stop` are state signal and live in the default set.
const TELEMETRY_EVENTS: &[(&str, Option<&str>)] = &[("PreToolUse", None), ("PostToolUse", None)];

/// Events that hold the agent open while the bridge waits for an answer.
/// Installing one with `_rimz_sync = false` in the existing config is a hard
/// error — the source of truth for "must block" is this constant, never the
/// on-disk file.
const BLOCKING_EVENTS: &[(&str, Option<&str>)] = &[
    ("PermissionRequest", None),
    ("PreToolUse", Some("ExitPlanMode")),
    ("PreToolUse", Some("AskUserQuestion")),
];

const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_SYNC_KEY: &str = "_rimz_sync";

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
                | "PreToolUse" | "PostToolUse" => AgentHookClass::Lifecycle,
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
            FeedKind::Permission => Ok(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": if choice_is_allow(resolution) { "allow" } else { "deny" }
                    }
                }
            })),
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
        let (status, posture) = match event_name {
            "SessionStart" => (AgentStatus::Idle, Some(posture_from_payload(payload))),
            "UserPromptSubmit" => (AgentStatus::Running, None),
            "Stop" => (
                stop_status_with_background(payload, &pending_background),
                None,
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
        let context_pct = payload
            .get("context_pct")
            .or_else(|| payload.get("context_window_pct"))
            .and_then(Value::as_u64)
            .map(|v| v.min(100) as u8)
            .or(usage.context_pct);
        let total_tokens = payload
            .get("total_tokens")
            .or_else(|| payload.get("token_count"))
            .and_then(Value::as_u64)
            .or(usage.total_tokens);
        let (todo_done, todo_total) = todos_from_payload(payload);
        Some(AgentLifecycleObservation {
            agent_id: payload
                .get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            status,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            permission_posture: posture,
            worktree_path: optional_payload_string(payload, &["worktree_path", "cwd"]),
            worktree_branch: optional_payload_string(payload, &["worktree_branch"]),
            task: background_task_label(&pending_background)
                .or_else(|| optional_payload_string(payload, &["task", "prompt"])),
            model: optional_payload_string(payload, &["model"]).or(usage.model),
            effort: optional_payload_string(payload, &["thinking_level", "effort"]),
            context_pct,
            total_tokens,
            todo_done,
            todo_total,
            pane_id: None,
        })
    }

    fn install_hooks(&self, telemetry: bool) -> Result<HookInstallReport> {
        let path = claude_settings_path()?;
        install_into(&path, telemetry)
    }

    fn preview_hook_install(&self, telemetry: bool) -> Result<HookInstallPreview> {
        let path = claude_settings_path()?;
        preview_install_at(&path, telemetry)
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
                    arr.iter().any(|entry| {
                        entry
                            .get(RIMZ_MANAGED_KEY)
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
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

/// Translate a Claude payload's mode/permission hints onto the four-value
/// permission posture pill. `permission_mode = "bypassPermissions"` (the value
/// `--dangerously-skip-permissions` surfaces) maps to `Yolo`. Claude's `plan`
/// mode is still default-posture — the human approves each tool call from the
/// plan — so it folds into `Default`.
fn posture_from_payload(payload: &Value) -> PermissionPosture {
    let raw = payload
        .get("permission_mode")
        .or_else(|| payload.get("mode"))
        .and_then(Value::as_str);
    match raw {
        Some("bypassPermissions") | Some("bypass") => PermissionPosture::Yolo,
        Some("acceptEdits") | Some("auto") => PermissionPosture::Auto,
        Some("plan") | Some("default") | Some("interactive") | Some("ask") => {
            PermissionPosture::Default
        }
        Some(_) => PermissionPosture::Unknown,
        None => PermissionPosture::Default,
    }
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

/// Context-window usage derived from a Claude transcript tail.
#[derive(Default)]
struct TranscriptUsage {
    context_pct: Option<u8>,
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
            context_pct: Some(0),
            total_tokens: Some(0),
            model: None,
        }
    }
}

/// Current Claude models expose a 200k-token context window. Kept as a single
/// lookup so a future per-model window (e.g. a 1M beta) lands in one place.
fn context_window_for(_model: Option<&str>) -> u64 {
    200_000
}

/// Derive context-window usage from the tail of a Claude transcript JSONL.
/// Claude never puts token counts in the hook payload — they live in the
/// transcript — so this is the only place the context gauge can be sourced.
/// Reads a bounded tail and takes the most recent assistant `message.usage`.
/// Best-effort: any IO or parse failure yields empty fields (enrichment, never
/// correctness).
fn usage_from_transcript(path: &str) -> TranscriptUsage {
    const TAIL_BYTES: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return TranscriptUsage::default();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if file
        .seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .is_err()
    {
        return TranscriptUsage::default();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return TranscriptUsage::default();
    }
    let text = String::from_utf8_lossy(&buf);
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
        let window = context_window_for(model.as_deref()).max(1);
        let context_pct = (context_tokens.saturating_mul(100) / window).min(100) as u8;
        return TranscriptUsage {
            context_pct: Some(context_pct),
            total_tokens: Some(context_tokens + output),
            model,
        };
    }
    TranscriptUsage::fresh()
}

fn claude_settings_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_CLAUDE_SETTINGS`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    if let Some(raw) = env::var_os("RIMZ_CLAUDE_SETTINGS").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AgentErr::Install {
            agent: "claude",
            reason: "$HOME is not set; cannot resolve ~/.claude/settings.json".to_owned(),
        })?;
    Ok(home.join(".claude").join("settings.json"))
}

fn install_into(path: &Path, telemetry: bool) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path, telemetry)?;
    write_json(path, &root)?;

    Ok(HookInstallReport {
        agent: "claude",
        config_path: path.to_path_buf(),
        installed_events: installed,
        merged: existed,
        telemetry,
    })
}

fn preview_install_at(path: &Path, telemetry: bool) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = original_text(path)?;
    let (root, installed) = install_candidate(path, telemetry)?;
    Ok(HookInstallPreview {
        agent: "claude",
        config_path: path.to_path_buf(),
        planned_events: installed,
        original_config,
        candidate_config: render_json(&root)?,
        merged: existed,
        telemetry,
    })
}

fn install_candidate(path: &Path, telemetry: bool) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_existing_json(path)?;

    // Defensive: a tampered or stale Rimz write may carry a `_rimz_sync =
    // false` marker on a blocking event. Refuse — installing a blocking
    // hook as non-sync is a hard error.
    reject_async_blocking_in_existing(&root)?;

    // Strip any existing Rimz-managed matchers before writing the fresh
    // set. Mirrors codex's "single source of truth = installer constants".
    let _ = strip_rimz_matchers(&mut root);

    let mut installed = Vec::new();
    for &(event, matcher) in DEFAULT_EVENTS {
        upsert_rimz_matcher(&mut root, event, matcher);
        installed.push(event_label(event, matcher));
    }
    if telemetry {
        for &(event, matcher) in TELEMETRY_EVENTS {
            upsert_rimz_matcher(&mut root, event, matcher);
            installed.push(event_label(event, matcher));
        }
    }

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
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParseJson {
                    agent: "claude",
                    path: path.to_path_buf(),
                    source,
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
        AgentErr::InstallSerializeJson {
            agent: "claude",
            source,
        }
    })?;
    Ok(format!("{text}\n"))
}

fn original_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "claude",
            path: path.to_path_buf(),
            source,
        }),
    }
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
            if !obj
                .get(RIMZ_MANAGED_KEY)
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
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
        if !obj
            .get(RIMZ_MANAGED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
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
    let command =
        format!("RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event {event}");
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

/// Remove every matcher tagged `_rimz_managed = true` across the hook tree.
/// Returns the labels of removed entries (`Event` or `Event:matcher`).
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
            let managed = obj
                .get(RIMZ_MANAGED_KEY)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if managed {
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
    fn hook_cap_is_120_seconds() {
        assert_eq!(ClaudeIntegration.hook_cap(), Duration::from_secs(120));
    }

    #[test]
    fn install_into_empty_dir_creates_managed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_into(&path, false).unwrap();
        assert!(!report.merged);
        assert_eq!(report.agent, "claude");
        assert!(report.installed_events.contains(&"SessionStart".to_owned()));
        assert!(
            report
                .installed_events
                .contains(&"PreToolUse:ExitPlanMode".to_owned())
        );
        assert!(
            report
                .installed_events
                .contains(&"PreToolUse:AskUserQuestion".to_owned())
        );
        assert!(
            report
                .installed_events
                .contains(&"PermissionRequest".to_owned())
        );

        // Lock the full on-disk shape: event set, matcher ordering, sync
        // flags, command strings, and the 120 s blocking-hook timeout. The
        // file is deterministic — fixed commands, constant timeout, no paths —
        // so the whole settings.json snapshots cleanly.
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
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Notification",
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
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PermissionRequest",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PreToolUse": [
              {
                "_rimz_managed": true,
                "_rimz_sync": true,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse",
                    "timeout": 120,
                    "type": "command"
                  }
                ],
                "matcher": "ExitPlanMode"
              },
              {
                "_rimz_managed": true,
                "_rimz_sync": true,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse",
                    "timeout": 120,
                    "type": "command"
                  }
                ],
                "matcher": "AskUserQuestion"
              }
            ],
            "SessionEnd": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event SessionEnd",
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
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event SessionStart",
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
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Stop",
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
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event UserPromptSubmit",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ]
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
        let report = install_into(&path, false).unwrap();
        assert!(report.merged);

        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["model"], "claude-opus-4-7");
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        // user `Bash` matcher + 2 rimz matchers (ExitPlanMode, AskUserQuestion).
        assert_eq!(pre_tool.len(), 3);
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
    fn telemetry_install_adds_broad_pre_post_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_into(&path, true).unwrap();
        assert!(report.telemetry);
        assert!(
            report
                .installed_events
                .contains(&"UserPromptSubmit".to_owned())
        );
        assert!(report.installed_events.contains(&"PreToolUse".to_owned()));
        assert!(report.installed_events.contains(&"PostToolUse".to_owned()));

        let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        // 2 blocking matchers + 1 telemetry (no matcher) = 3.
        assert_eq!(pre_tool.len(), 3);
        // Telemetry entry has no matcher key and sync = false.
        let telemetry_entry = pre_tool
            .iter()
            .find(|e| !e.as_object().unwrap().contains_key("matcher"))
            .unwrap();
        assert_eq!(telemetry_entry["_rimz_managed"], true);
        assert_eq!(telemetry_entry["_rimz_sync"], false);
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_into(&path, true).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        install_into(&path, true).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            first, second,
            "second install must produce identical config"
        );
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
        install_into(&path, true).unwrap();
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
    fn hooks_installed_at_detects_managed_matcher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(
            !hooks_installed_at(&path),
            "a missing settings file reads as not installed"
        );
        install_into(&path, false).unwrap();
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
        let err = install_into(&path, false).unwrap_err();
        assert!(matches!(
            err,
            AgentErr::Install {
                agent: "claude",
                ..
            }
        ));
    }

    #[test]
    fn install_rejects_async_blocking_marker_on_pretooluse_matcher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "PreToolUse": [
                  {
                    "matcher": "ExitPlanMode",
                    "_rimz_managed": true,
                    "_rimz_sync": false,
                    "hooks": [{ "type": "command", "command": "x" }]
                  }
                ]
              }
            }"#,
        )
        .unwrap();
        let err = install_into(&path, false).unwrap_err();
        let AgentErr::Install { agent, reason } = err else {
            panic!("expected Install error");
        };
        assert_eq!(agent, "claude");
        assert!(
            reason.contains("PreToolUse:ExitPlanMode"),
            "reason should name the violating event: {reason}"
        );
    }

    #[test]
    fn install_rejects_top_level_non_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "[]").unwrap();
        let err = install_into(&path, false).unwrap_err();
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
    fn plan_mode_collapses_to_default_posture() {
        // Plan is a workflow mode, not a permission posture — the human still
        // approves each tool call when the plan executes. It folds into the
        // omitted baseline so the sidebar's posture pill only flags Auto/Yolo.
        let obs = ClaudeIntegration
            .observe_lifecycle("SessionStart", &json!({ "permission_mode": "plan" }))
            .unwrap();
        assert_eq!(obs.permission_posture, Some(PermissionPosture::Default));
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
}
