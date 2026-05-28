//! Claude Code hook adapter.
//!
//! Classifies the blocking events (`PermissionRequest`, `PreToolUse:
//! ExitPlanMode`, `PreToolUse: AskUserQuestion`) and the lifecycle events
//! (`SessionStart` registers idle, `UserPromptSubmit` moves to running with
//! the prompt as task, `Stop` back to idle, `SessionEnd` exits, `Notification`
//! silent); renders the Claude-shaped `hookSpecificOutput` / `updatedInput`
//! decision payload and the neutral fallback.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.claude/settings.json` under per-matcher `_rimz_managed` markers.
//! Blocking events are marked `_rimz_sync = true`; an existing async marker
//! on a blocking event is a hard install error (see [`BLOCKING_EVENTS`] and
//! `docs/internals/agent.md`).

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    AgentErr, AgentHookClass, AgentIntegration, AgentLifecycleObservation, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, choice_is_allow,
    optional_payload_string,
};
use crate::feed::{AgentMode, AgentStatus, FeedItem, FeedKind, Resolution};
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
    ("Stop", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreToolUse", Some("ExitPlanMode")),
    ("PreToolUse", Some("AskUserQuestion")),
];

/// Telemetry-install events (added when `--telemetry` is passed).
const TELEMETRY_EVENTS: &[(&str, Option<&str>)] = &[
    ("UserPromptSubmit", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
];

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
        // the prompt is what moves it to running. Only SessionStart establishes
        // the mode pill — the prompt and stop carry no permission field, so they
        // report `None` and the reducer keeps the established mode. SessionEnd
        // records the exit so the reducer drops the agent from the rollup
        // (mode carries no meaning on exit); `ends_session` then expires any
        // asks the dead session left pending.
        let (status, mode) = match event_name {
            "SessionStart" => (AgentStatus::Idle, Some(mode_from_payload(payload))),
            "UserPromptSubmit" => (AgentStatus::Running, None),
            "Stop" => (AgentStatus::Idle, None),
            "SessionEnd" => (AgentStatus::Idle, None),
            _ => return None,
        };
        Some(AgentLifecycleObservation {
            agent_id: payload
                .get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            status,
            agent_pid: None,
            agent_process_start: None,
            mode,
            worktree_path: optional_payload_string(payload, &["worktree_path", "cwd"]),
            worktree_branch: optional_payload_string(payload, &["worktree_branch"]),
            task: optional_payload_string(payload, &["task", "prompt"]),
            model: optional_payload_string(payload, &["model"]),
            effort: optional_payload_string(payload, &["thinking_level", "effort"]),
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

/// Translate a Claude payload's mode/permission hints onto the five-value
/// mode pill. `permission_mode = "bypassPermissions"` (the value
/// `--dangerously-skip-permissions` surfaces) maps to `Bypass`.
fn mode_from_payload(payload: &Value) -> AgentMode {
    let raw = payload
        .get("permission_mode")
        .or_else(|| payload.get("mode"))
        .and_then(Value::as_str);
    match raw {
        Some("bypassPermissions") | Some("bypass") => AgentMode::Bypass,
        Some("acceptEdits") | Some("auto") => AgentMode::Auto,
        Some("plan") => AgentMode::Plan,
        Some("default") | Some("interactive") | Some("ask") => AgentMode::Interactive,
        Some(_) => AgentMode::Unknown,
        None => AgentMode::Interactive,
    }
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
        // User UserPromptSubmit hook stays untouched in default install.
        let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        assert!(ups[0].get("_rimz_managed").is_none());
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
        assert_eq!(obs.mode, Some(AgentMode::Interactive));
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
        // The prompt reports no mode, so the reducer keeps the mode
        // SessionStart established (a bypass agent stays bypass).
        assert_eq!(obs.mode, None);
    }

    #[test]
    fn bypass_permissions_observes_bypass_mode() {
        let obs = ClaudeIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "permission_mode": "bypassPermissions" }),
            )
            .unwrap();
        assert_eq!(obs.mode, Some(AgentMode::Bypass));
    }

    #[test]
    fn notification_event_is_not_a_lifecycle_observation() {
        let obs = ClaudeIntegration.observe_lifecycle("Notification", &json!({}));
        assert!(obs.is_none());
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
