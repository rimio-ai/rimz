//! Codex hook adapter.
//!
//! Classifies `PermissionRequest` (blocking) and the lifecycle events
//! (`SessionStart` registers idle, `SubagentStart` / `UserPromptSubmit` move
//! to running, `SubagentStop` / `Stop` back to idle); renders the Codex-shaped
//! `PermissionRequest` `hookSpecificOutput` decision payload (neutral is empty
//! stdout). `permission_mode` from the agent payload drives the mode pill.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.codex/config.toml` using Codex's inline `[[hooks.Event]]` tables.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::{
    AgentErr, AgentHookClass, AgentIntegration, AgentLifecycleObservation, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, choice_is_allow,
    optional_payload_string,
};
use crate::feed::{AgentMode, AgentStatus, FeedItem, FeedKind, Resolution};
use crate::ledger::atomic;

/// Codex's effective hook cap. Upstream's blocking-hook deadline is shorter
/// than Claude's; this leaves a small safety margin so the bridge never holds
/// the hook past the kill window. Verify against the active Codex hook docs
/// before tightening.
const CODEX_HOOK_CAP: Duration = Duration::from_secs(60);

/// Default-install events (always wired).
const DEFAULT_EVENTS: &[&str] = &[
    "SessionStart",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "PermissionRequest",
];
/// Telemetry-install events (added when `--telemetry` is passed).
const TELEMETRY_EVENTS: &[&str] = &["UserPromptSubmit", "PreToolUse", "PostToolUse"];

/// Legacy config block written by older Rimz builds. Codex ignores this block;
/// uninstall still removes it so users can clean up stale config.
const RIMZ_BLOCK: &str = "rimz";
const HOOKS_TABLE: &str = "hooks";

#[derive(Clone, Debug, Default)]
pub struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        let feed_kind = if event_name == "PermissionRequest" {
            Some(FeedKind::Permission)
        } else {
            None
        };
        let class = if feed_kind.is_some() {
            AgentHookClass::BlockingFeed
        } else {
            match event_name {
                "SessionStart" | "SubagentStart" | "SubagentStop" | "Stop" | "UserPromptSubmit"
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
            other => Err(AgentErr::Render {
                agent: "codex",
                reason: format!("unsupported feed kind {other:?}"),
            }),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Codex permission hooks expect empty stdout on the neutral path —
        // the agent's own UI then asks the human. Per docs/internals/agent.md:
        // never emit `updatedInput` / `interrupt` for Codex permission hooks.
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        // SessionStart wires the root agent in but it has not been asked to do
        // anything yet, so it registers idle; the prompt is what moves it to
        // running. SubagentStart is different: it fires immediately before the
        // child model request, so it registers running under the child
        // `agent_id`. Stop-like events report no mode so the reducer keeps the
        // established mode.
        let (status, mode) = match event_name {
            "SessionStart" => (AgentStatus::Idle, Some(mode_from_payload(payload))),
            "SubagentStart" => (AgentStatus::Running, Some(mode_from_payload(payload))),
            "UserPromptSubmit" => (AgentStatus::Running, None),
            "SubagentStop" | "Stop" => (AgentStatus::Idle, None),
            _ => return None,
        };
        Some(AgentLifecycleObservation {
            agent_id: codex_agent_id(payload),
            status,
            agent_pid: None,
            agent_process_start: None,
            mode,
            worktree_path: optional_payload_string(payload, &["worktree_path", "cwd"]),
            worktree_branch: optional_payload_string(payload, &["worktree_branch"]),
            task: task_from_payload(event_name, payload),
            model: optional_payload_string(payload, &["model"]),
            effort: optional_payload_string(
                payload,
                &["model_reasoning_effort", "reasoning_effort", "effort"],
            ),
        })
    }

    fn install_hooks(&self, telemetry: bool) -> Result<HookInstallReport> {
        let path = codex_config_path()?;
        install_into(&path, telemetry)
    }

    fn preview_hook_install(&self, telemetry: bool) -> Result<HookInstallPreview> {
        let path = codex_config_path()?;
        preview_install_at(&path, telemetry)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = codex_config_path()?;
        uninstall_from(&path)
    }

    fn hook_cap(&self) -> Duration {
        CODEX_HOOK_CAP
    }

    fn supports_hook_install(&self) -> bool {
        true
    }

    fn hooks_installed(&self) -> bool {
        codex_config_path().is_ok_and(|path| hooks_installed_at(&path))
    }
}

/// Map Codex's `approval_policy` (or `mode`) payload field onto the
/// five-value mode pill. Bypass is observed from `--ask-for-approval never`
/// per docs/internals/agent.md:60.
fn mode_from_payload(payload: &Value) -> AgentMode {
    let policy = payload
        .get("permission_mode")
        .or_else(|| payload.get("approval_policy"))
        .or_else(|| payload.get("mode"))
        .and_then(Value::as_str);
    match policy {
        Some("never") | Some("bypass") | Some("bypassPermissions") | Some("dontAsk") => {
            AgentMode::Bypass
        }
        Some("acceptEdits") | Some("auto") | Some("auto-edit") | Some("on-failure") => {
            AgentMode::Auto
        }
        Some("plan") => AgentMode::Plan,
        Some("default") | Some("interactive") | Some("untrusted") | Some("on-request")
        | Some("ask") => AgentMode::Interactive,
        Some(_) => AgentMode::Unknown,
        None => AgentMode::Interactive,
    }
}

fn codex_agent_id(payload: &Value) -> Option<String> {
    optional_payload_string(payload, &["agent_id", "session_id"])
}

fn task_from_payload(event_name: &str, payload: &Value) -> Option<String> {
    if event_name == "SubagentStart" {
        optional_payload_string(payload, &["task", "prompt", "agent_type"])
    } else {
        optional_payload_string(payload, &["task", "prompt"])
    }
}

fn codex_config_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_CODEX_CONFIG`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    if let Some(raw) = env::var_os("RIMZ_CODEX_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AgentErr::Install {
            agent: "codex",
            reason: "$HOME is not set; cannot resolve ~/.codex/config.toml".to_owned(),
        })?;
    Ok(home.join(".codex").join("config.toml"))
}

fn install_into(path: &std::path::Path, telemetry: bool) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path, telemetry)?;
    write_table(path, &root)?;

    Ok(HookInstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        installed_events: installed,
        merged: existed,
        telemetry,
    })
}

fn preview_install_at(path: &std::path::Path, telemetry: bool) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = original_text(path)?;
    let (root, installed) = install_candidate(path, telemetry)?;
    Ok(HookInstallPreview {
        agent: "codex",
        config_path: path.to_path_buf(),
        planned_events: installed,
        original_config,
        candidate_config: render_table(&root)?,
        merged: existed,
        telemetry,
    })
}

fn install_candidate(
    path: &std::path::Path,
    telemetry: bool,
) -> Result<(toml::Table, Vec<String>)> {
    let mut root = read_existing_table(path)?;

    let mut removed = strip_rimz_hook_commands(&mut root);
    removed.extend(remove_rimz_block(&mut root));
    removed.sort();
    removed.dedup();

    let installed = if telemetry {
        DEFAULT_EVENTS
            .iter()
            .chain(TELEMETRY_EVENTS.iter())
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
    } else {
        DEFAULT_EVENTS.iter().map(|s| (*s).to_owned()).collect()
    };

    for event in &installed {
        insert_rimz_hook_group(&mut root, event);
    }

    Ok((root, installed))
}

fn uninstall_from(path: &std::path::Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    if !existed {
        return Ok(HookUninstallReport {
            agent: "codex",
            config_path: path.to_path_buf(),
            removed_events: Vec::new(),
            existed: false,
        });
    }

    let mut root = read_existing_table(path)?;
    let mut removed = strip_rimz_hook_commands(&mut root);
    removed.extend(remove_rimz_block(&mut root));
    removed.sort();
    removed.dedup();
    write_table(path, &root)?;

    Ok(HookUninstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        removed_events: removed,
        existed: true,
    })
}

fn hooks_installed_at(path: &std::path::Path) -> bool {
    let Ok(root) = read_existing_table(path) else {
        return false;
    };
    DEFAULT_EVENTS
        .iter()
        .all(|event| has_rimz_hook_command(&root, event))
}

fn read_existing_table(path: &std::path::Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(toml::Table::new()),
        Ok(text) => toml::from_str::<toml::Table>(&text).map_err(|source| AgentErr::InstallParse {
            agent: "codex",
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "codex",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_table(path: &std::path::Path, table: &toml::Table) -> Result<()> {
    let text = render_table(table)?;
    atomic::write_bytes_atomically(path, text.as_bytes())?;
    Ok(())
}

fn render_table(table: &toml::Table) -> Result<String> {
    toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "codex",
        source: Box::new(source),
    })
}

fn original_text(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "codex",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn insert_rimz_hook_group(root: &mut toml::Table, event: &str) {
    let hooks = root
        .entry(HOOKS_TABLE.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let hooks_table = match hooks {
        toml::Value::Table(table) => table,
        _ => {
            *hooks = toml::Value::Table(toml::Table::new());
            hooks.as_table_mut().expect("just inserted table")
        }
    };

    let groups = hooks_table
        .entry(event.to_owned())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let groups_array = match groups {
        toml::Value::Array(array) => array,
        _ => {
            *groups = toml::Value::Array(Vec::new());
            groups.as_array_mut().expect("just inserted array")
        }
    };

    let mut handler = toml::Table::new();
    handler.insert("type".to_owned(), toml::Value::String("command".to_owned()));
    handler.insert(
        "command".to_owned(),
        toml::Value::String(rimz_hook_command(event)),
    );
    handler.insert(
        "timeout".to_owned(),
        toml::Value::Integer(CODEX_HOOK_CAP.as_secs() as i64),
    );
    handler.insert(
        "statusMessage".to_owned(),
        toml::Value::String(format!("Routing {event} through Rimz")),
    );

    let mut group = toml::Table::new();
    if let Some(matcher) = matcher_for_event(event) {
        group.insert(
            "matcher".to_owned(),
            toml::Value::String(matcher.to_owned()),
        );
    }
    group.insert(
        "hooks".to_owned(),
        toml::Value::Array(vec![toml::Value::Table(handler)]),
    );
    groups_array.push(toml::Value::Table(group));
}

fn strip_rimz_hook_commands(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks_table) = root
        .get_mut(HOOKS_TABLE)
        .and_then(toml::Value::as_table_mut)
    else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    let event_names = hooks_table.keys().cloned().collect::<Vec<_>>();
    for event in event_names {
        let Some(groups) = hooks_table
            .get_mut(&event)
            .and_then(toml::Value::as_array_mut)
        else {
            continue;
        };

        for group in groups.iter_mut() {
            let Some(group_table) = group.as_table_mut() else {
                continue;
            };
            let Some(handlers) = group_table
                .get_mut("hooks")
                .and_then(toml::Value::as_array_mut)
            else {
                continue;
            };
            let before = handlers.len();
            handlers.retain(|handler| !is_rimz_hook_handler(handler, &event));
            if handlers.len() != before {
                removed.push(event.clone());
            }
        }

        groups.retain(|group| {
            group
                .as_table()
                .and_then(|table| table.get("hooks"))
                .and_then(toml::Value::as_array)
                .is_none_or(|handlers| !handlers.is_empty())
        });
        if groups.is_empty() {
            hooks_table.remove(&event);
        }
    }
    if hooks_table.is_empty() {
        root.remove(HOOKS_TABLE);
    }
    removed
}

fn has_rimz_hook_command(root: &toml::Table, event: &str) -> bool {
    root.get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get(event))
        .and_then(toml::Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .as_table()
                    .and_then(|table| table.get("hooks"))
                    .and_then(toml::Value::as_array)
                    .is_some_and(|handlers| {
                        handlers
                            .iter()
                            .any(|handler| is_current_rimz_hook_handler(handler, event))
                    })
            })
        })
}

fn handler_command(handler: &toml::Value) -> Option<&str> {
    handler
        .as_table()
        .and_then(|table| table.get("command"))
        .and_then(toml::Value::as_str)
}

fn is_current_rimz_hook_handler(handler: &toml::Value, event: &str) -> bool {
    handler_command(handler).is_some_and(|command| command == rimz_hook_command(event))
}

fn is_rimz_hook_handler(handler: &toml::Value, event: &str) -> bool {
    handler_command(handler).is_some_and(|command| {
        command == rimz_hook_command(event) || command == legacy_rimz_hook_command(event)
    })
}

fn rimz_hook_command(event: &str) -> String {
    format!("RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event {event}")
}

fn legacy_rimz_hook_command(event: &str) -> String {
    format!("rimz hooks feed --source codex --event {event}")
}

fn matcher_for_event(event: &str) -> Option<&'static str> {
    match event {
        "SessionStart" => Some("startup|resume|clear|compact"),
        "PermissionRequest" | "PreToolUse" | "PostToolUse" | "SubagentStart" | "SubagentStop" => {
            Some(".*")
        }
        "UserPromptSubmit" | "Stop" => None,
        _ => None,
    }
}

fn remove_rimz_block(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks_value) = root.get_mut(HOOKS_TABLE) else {
        return Vec::new();
    };
    let Some(hooks_table) = hooks_value.as_table_mut() else {
        return Vec::new();
    };
    let removed_value = hooks_table.remove(RIMZ_BLOCK);
    let removed_events = removed_value
        .as_ref()
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("events"))
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if hooks_table.is_empty() {
        root.remove(HOOKS_TABLE);
    }
    removed_events
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
            "codex",
            "agent-hook",
        )
    }

    #[test]
    fn permission_decision_has_no_reserved_keys() {
        let item = fixture(FeedKind::Permission);
        let resolution =
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
        let rendered = CodexIntegration
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
        assert!(rendered.get("updatedInput").is_none());
        assert!(rendered.get("updatedPermissions").is_none());
        assert!(rendered.get("interrupt").is_none());
    }

    #[test]
    fn permission_deny_shape_is_pinned() {
        let item = fixture(FeedKind::Permission);
        let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
        let rendered = CodexIntegration
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
    fn neutral_payload_is_empty_stdout() {
        let rendered = CodexIntegration
            .render_neutral("PermissionRequest")
            .unwrap();

        insta::assert_snapshot!(
            serde_json::to_string(&rendered).unwrap(),
            @"null"
        );
    }

    #[test]
    fn session_start_observes_idle_in_interactive_mode_by_default() {
        let obs = CodexIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "approval_policy": "ask" }),
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
        let obs = CodexIntegration
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
            )
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.task.as_deref(), Some("fix auth flow"));
        // The prompt carries no policy field, so it reports no mode: the
        // reducer keeps the mode SessionStart established.
        assert_eq!(obs.mode, None);
    }

    #[test]
    fn subagent_start_observes_child_id_and_type() {
        let obs = CodexIntegration
            .observe_lifecycle(
                "SubagentStart",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review",
                    "permission_mode": "acceptEdits",
                }),
            )
            .unwrap();

        assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.task.as_deref(), Some("review"));
        assert_eq!(obs.mode, Some(AgentMode::Auto));
    }

    #[test]
    fn subagent_stop_observes_idle_child_id() {
        let obs = CodexIntegration
            .observe_lifecycle(
                "SubagentStop",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review",
                }),
            )
            .unwrap();

        assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
        assert_eq!(obs.status, AgentStatus::Idle);
        assert_eq!(obs.task, None);
        assert_eq!(obs.mode, None);
    }

    #[test]
    fn approval_policy_never_observes_bypass_mode() {
        let obs = CodexIntegration
            .observe_lifecycle("SessionStart", &json!({ "approval_policy": "never" }))
            .unwrap();
        assert_eq!(obs.mode, Some(AgentMode::Bypass));
    }

    #[test]
    fn permission_mode_bypass_permissions_observes_bypass_mode() {
        let obs = CodexIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "permission_mode": "bypassPermissions" }),
            )
            .unwrap();
        assert_eq!(obs.mode, Some(AgentMode::Bypass));
    }

    #[test]
    fn stop_observes_idle_status() {
        let obs = CodexIntegration
            .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
            .unwrap();
        assert_eq!(obs.status, AgentStatus::Idle);
    }

    #[test]
    fn classification_unchanged_for_unknown_event() {
        let c = CodexIntegration.classify_hook("WatItIs", &Value::Null);
        assert_eq!(c.class, AgentHookClass::Unknown);
        assert!(c.feed_kind.is_none());
    }

    #[test]
    fn install_into_empty_dir_creates_documented_inline_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let report = install_into(&path, false).unwrap();
        assert!(!report.merged);
        assert_eq!(report.agent, "codex");
        assert_eq!(report.installed_events, DEFAULT_EVENTS);
        assert!(hooks_installed_at(&path));

        let text = std::fs::read_to_string(&path).unwrap();
        insta::assert_snapshot!(text, @r###"
        [[hooks.PermissionRequest]]
        matcher = ".*"

        [[hooks.PermissionRequest.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event PermissionRequest"
        statusMessage = "Routing PermissionRequest through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SessionStart]]
        matcher = "startup|resume|clear|compact"

        [[hooks.SessionStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SessionStart"
        statusMessage = "Routing SessionStart through Rimz"
        timeout = 60
        type = "command"

        [[hooks.Stop]]

        [[hooks.Stop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event Stop"
        statusMessage = "Routing Stop through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SubagentStart]]
        matcher = ".*"

        [[hooks.SubagentStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SubagentStart"
        statusMessage = "Routing SubagentStart through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SubagentStop]]
        matcher = ".*"

        [[hooks.SubagentStop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SubagentStop"
        statusMessage = "Routing SubagentStop through Rimz"
        timeout = 60
        type = "command"
        "###);
    }

    #[test]
    fn install_preserves_user_hooks_and_adds_telemetry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"model = "gpt-5.5"

[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "echo user"
"#,
        )
        .unwrap();

        let report = install_into(&path, true).unwrap();
        assert!(report.merged);
        for telemetry_event in TELEMETRY_EVENTS {
            assert!(report.installed_events.iter().any(|e| e == telemetry_event));
        }

        let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed.get("model").and_then(toml::Value::as_str),
            Some("gpt-5.5")
        );
        let pre_tool = parsed
            .get("hooks")
            .and_then(toml::Value::as_table)
            .and_then(|hooks| hooks.get("PreToolUse"))
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            pre_tool.iter().any(|group| {
                group
                    .as_table()
                    .and_then(|table| table.get("hooks"))
                    .and_then(toml::Value::as_array)
                    .is_some_and(|handlers| {
                        handlers.iter().any(|handler| {
                            handler
                                .as_table()
                                .and_then(|table| table.get("command"))
                                .and_then(toml::Value::as_str)
                                == Some("echo user")
                        })
                    })
            }),
            "user hook must survive install"
        );
        assert!(
            has_rimz_hook_command(&parsed, "PreToolUse"),
            "telemetry install wires PreToolUse"
        );
    }

    #[test]
    fn uninstall_removes_legacy_block_and_preserves_user_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model = \"o4-mini\"\n[hooks.user_custom]\ncommand = [\"echo\", \"hi\"]\n[hooks.rimz]\nevents = [\"SessionStart\", \"PermissionRequest\"]\nmanaged_by = \"rimz\"\n",
        )
        .unwrap();
        let report = uninstall_from(&path).unwrap();
        assert!(report.existed);
        assert_eq!(
            report.removed_events,
            vec!["PermissionRequest".to_owned(), "SessionStart".to_owned()]
        );
        let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed.get("model").and_then(toml::Value::as_str),
            Some("o4-mini")
        );
        let hooks = parsed.get("hooks").and_then(toml::Value::as_table).unwrap();
        assert!(hooks.contains_key("user_custom"));
        assert!(!hooks.contains_key(RIMZ_BLOCK));
    }

    #[test]
    fn uninstall_removes_rimz_hook_commands_and_preserves_user_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        install_into(&path, true).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo user stop\"\n",
                std::fs::read_to_string(&path).unwrap()
            ),
        )
        .unwrap();

        let report = uninstall_from(&path).unwrap();
        assert!(report.existed);
        assert!(report.removed_events.contains(&"SessionStart".to_owned()));
        assert!(
            report
                .removed_events
                .contains(&"PermissionRequest".to_owned())
        );
        assert!(!hooks_installed_at(&path));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("echo user stop"));
        assert!(!text.contains("rimz hooks feed --source codex"));
    }

    #[test]
    fn hooks_installed_rejects_legacy_unwrapped_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event SessionStart"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event Stop"

[[hooks.PermissionRequest]]
[[hooks.PermissionRequest.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event PermissionRequest"
"#,
        )
        .unwrap();
        assert!(
            !hooks_installed_at(&path),
            "legacy commands lack the PID wrapper and must be reinstalled"
        );
        install_into(&path, false).unwrap();
        assert!(hooks_installed_at(&path));
    }

    #[test]
    fn uninstall_on_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let report = uninstall_from(&path).unwrap();
        assert!(!report.existed);
        assert!(report.removed_events.is_empty());
    }

    #[test]
    fn codex_hook_cap_is_shorter_than_claude_default() {
        use crate::agents::ClaudeIntegration;
        assert!(CodexIntegration.hook_cap() < ClaudeIntegration.hook_cap());
    }
}
