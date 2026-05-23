//! Codex hook adapter.
//!
//! Classifies `PermissionRequest` (blocking) and the lifecycle events
//! (`SessionStart` / `Stop`); renders the Codex-shaped
//! `{"decision":"allow"|"deny"}` decision payload (neutral is empty stdout).
//! `approval_policy` from the agent payload drives the mode pill.
//!
//! Owns hook install / uninstall through a non-destructive merge into
//! `~/.codex/config.toml` under a Rimz-managed `[hooks.rimz]` namespace.
//! Blocking decision hooks are marked `sync = true` (see [`BLOCKING_EVENTS`]
//! and `docs/internals/agent.md`).

use std::env;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentErr, AgentHookClass, AgentIntegration, AgentLifecycleObservation, ClassifiedHook,
    HookInstallReport, HookUninstallReport, Result, choice_is_allow,
};
use crate::feed::{AgentMode, AgentStatus, FeedItem, FeedKind, Resolution};
use crate::ledger::atomic;

/// Default-install events (always wired).
const DEFAULT_EVENTS: &[&str] = &["SessionStart", "Stop", "PermissionRequest"];
/// Telemetry-install events (added when `--telemetry` is passed).
const TELEMETRY_EVENTS: &[&str] = &["UserPromptSubmit", "PreToolUse", "PostToolUse"];
/// Events that must be installed `sync = true` because the hook must hold the
/// agent open while the bridge waits for a resolver answer. Installing a
/// blocking event as async is a hard error per docs/internals/agent.md:42.
const BLOCKING_EVENTS: &[&str] = &["PermissionRequest"];

/// Top-level key under which Rimz writes its hook block. Sits next to any
/// user-managed `[hooks.<other>]` table so a clean merge round-trip is
/// possible.
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
                "SessionStart" | "Stop" => AgentHookClass::Lifecycle,
                "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => AgentHookClass::Telemetry,
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
                "decision": if choice_is_allow(resolution) { "allow" } else { "deny" }
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
        let (status, mode) = match event_name {
            "SessionStart" => (AgentStatus::Running, mode_from_payload(payload)),
            "Stop" => (AgentStatus::Idle, mode_from_payload(payload)),
            _ => return None,
        };
        Some(AgentLifecycleObservation {
            agent_id: payload
                .get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            status,
            mode,
            worktree_branch: payload
                .get("worktree_branch")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }

    fn install_hooks(&self, telemetry: bool) -> Result<HookInstallReport> {
        let path = codex_config_path()?;
        install_into(&path, telemetry)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        let path = codex_config_path()?;
        uninstall_from(&path)
    }
}

/// Map Codex's `approval_policy` (or `mode`) payload field onto the
/// five-value mode pill. Bypass is observed from `--ask-for-approval never`
/// per docs/internals/agent.md:60.
fn mode_from_payload(payload: &Value) -> AgentMode {
    let policy = payload
        .get("approval_policy")
        .or_else(|| payload.get("mode"))
        .and_then(Value::as_str);
    match policy {
        Some("never") | Some("bypass") => AgentMode::Bypass,
        Some("auto") | Some("auto-edit") | Some("on-failure") => AgentMode::Auto,
        Some("plan") => AgentMode::Plan,
        Some("interactive") | Some("untrusted") | Some("on-request") | Some("ask") => {
            AgentMode::Interactive
        }
        Some(_) => AgentMode::Unknown,
        None => AgentMode::Interactive,
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
    let mut root = read_existing_table(path)?;

    // Hard error if the pre-existing config marks any event we know must
    // block as `sync = false`. The source of truth for "must block" is our
    // own `BLOCKING_EVENTS` constant — never the on-disk `blocking_events`
    // array, which could be stripped by a tampered or stale Rimz write.
    if let Some(rimz_block) = rimz_block(&root) {
        for name in BLOCKING_EVENTS {
            let Some(table) = rimz_block.get(*name).and_then(toml::Value::as_table) else {
                continue;
            };
            let sync = table
                .get("sync")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if !sync {
                return Err(AgentErr::Install {
                    agent: "codex",
                    reason: format!(
                        "existing config marks blocking hook `{name}` as async; refusing to install"
                    ),
                });
            }
        }
    }

    let installed = if telemetry {
        DEFAULT_EVENTS
            .iter()
            .chain(TELEMETRY_EVENTS.iter())
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
    } else {
        DEFAULT_EVENTS.iter().map(|s| (*s).to_owned()).collect()
    };

    let rimz_block = build_rimz_block(&installed, telemetry);
    insert_rimz_block(&mut root, rimz_block);

    write_table(path, &root)?;

    Ok(HookInstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        installed_events: installed,
        merged: existed,
        telemetry,
    })
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
    let removed = remove_rimz_block(&mut root);
    write_table(path, &root)?;

    Ok(HookUninstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        removed_events: removed,
        existed: true,
    })
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
    let text = toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "codex",
        source: Box::new(source),
    })?;
    atomic::write_bytes_atomically(path, text.as_bytes())?;
    Ok(())
}

fn rimz_block(root: &toml::Table) -> Option<&toml::Table> {
    root.get(HOOKS_TABLE)?
        .as_table()?
        .get(RIMZ_BLOCK)?
        .as_table()
}

fn build_rimz_block(events: &[String], telemetry: bool) -> toml::Table {
    let mut block = toml::Table::new();
    block.insert(
        "managed_by".to_owned(),
        toml::Value::String("rimz".to_owned()),
    );
    block.insert("config_version".to_owned(), toml::Value::Integer(1));
    block.insert("telemetry".to_owned(), toml::Value::Boolean(telemetry));
    block.insert(
        "events".to_owned(),
        toml::Value::Array(
            events
                .iter()
                .map(|e| toml::Value::String(e.clone()))
                .collect(),
        ),
    );
    let blocking: Vec<toml::Value> = BLOCKING_EVENTS
        .iter()
        .filter(|b| events.iter().any(|e| e == *b))
        .map(|b| toml::Value::String((*b).to_owned()))
        .collect();
    block.insert("blocking_events".to_owned(), toml::Value::Array(blocking));

    for event in events {
        let mut entry = toml::Table::new();
        let argv = vec![
            toml::Value::String("rimz".to_owned()),
            toml::Value::String("hooks".to_owned()),
            toml::Value::String("feed".to_owned()),
            toml::Value::String("--source".to_owned()),
            toml::Value::String("codex".to_owned()),
            toml::Value::String("--event".to_owned()),
            toml::Value::String(event.clone()),
        ];
        entry.insert("command".to_owned(), toml::Value::Array(argv));
        let is_blocking = BLOCKING_EVENTS.contains(&event.as_str());
        entry.insert("sync".to_owned(), toml::Value::Boolean(is_blocking));
        block.insert(event.clone(), toml::Value::Table(entry));
    }

    block
}

fn insert_rimz_block(root: &mut toml::Table, block: toml::Table) {
    let hooks = root
        .entry(HOOKS_TABLE.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(hooks_table) = hooks.as_table_mut() else {
        // User had a non-table `hooks` value; replace with a fresh table that
        // hosts only Rimz's block. This is the safest move — coercing a
        // string/array into a table would lose meaning.
        *hooks = toml::Value::Table(toml::Table::new());
        hooks
            .as_table_mut()
            .expect("just inserted a table")
            .insert(RIMZ_BLOCK.to_owned(), toml::Value::Table(block));
        return;
    };
    hooks_table.insert(RIMZ_BLOCK.to_owned(), toml::Value::Table(block));
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

    #[test]
    fn permission_decision_has_no_reserved_keys() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
        let item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "allow?",
            "codex",
            "agent-hook",
        );
        let resolution =
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
        let rendered = CodexIntegration
            .render_decision(&item, &resolution)
            .unwrap();
        insta::assert_json_snapshot!(rendered, @r###"
        {
          "decision": "allow"
        }
        "###);
        assert_eq!(rendered, json!({ "decision": "allow" }));
        assert!(rendered.get("updatedInput").is_none());
        assert!(rendered.get("interrupt").is_none());
    }

    #[test]
    fn permission_deny_shape_is_pinned() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
        let item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "allow?",
            "codex",
            "agent-hook",
        );
        let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
        let rendered = CodexIntegration
            .render_decision(&item, &resolution)
            .unwrap();

        insta::assert_json_snapshot!(rendered, @r###"
        {
          "decision": "deny"
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
    fn session_start_observes_interactive_mode_by_default() {
        let obs = CodexIntegration
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "approval_policy": "ask" }),
            )
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.status, AgentStatus::Running);
        assert_eq!(obs.mode, AgentMode::Interactive);
    }

    #[test]
    fn approval_policy_never_observes_bypass_mode() {
        let obs = CodexIntegration
            .observe_lifecycle("SessionStart", &json!({ "approval_policy": "never" }))
            .unwrap();
        assert_eq!(obs.mode, AgentMode::Bypass);
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
    fn install_into_empty_dir_creates_marker_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let report = install_into(&path, false).unwrap();
        assert!(!report.merged);
        assert_eq!(report.agent, "codex");
        assert_eq!(report.installed_events, DEFAULT_EVENTS);
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        let block = rimz_block(&parsed).expect("rimz block present");
        assert_eq!(
            block.get("managed_by").and_then(toml::Value::as_str),
            Some("rimz")
        );
        assert!(
            block
                .get("PermissionRequest")
                .and_then(toml::Value::as_table)
                .and_then(|t| t.get("sync"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
            "PermissionRequest must be installed as sync"
        );
    }

    #[test]
    fn install_preserves_user_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model = \"o4-mini\"\n[hooks.user_custom]\ncommand = [\"echo\", \"hi\"]\n",
        )
        .unwrap();
        let report = install_into(&path, false).unwrap();
        assert!(report.merged);
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed.get("model").and_then(toml::Value::as_str),
            Some("o4-mini")
        );
        let hooks = parsed.get("hooks").and_then(toml::Value::as_table).unwrap();
        assert!(hooks.contains_key("user_custom"));
        assert!(hooks.contains_key(RIMZ_BLOCK));
    }

    #[test]
    fn telemetry_install_adds_additional_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let report = install_into(&path, true).unwrap();
        assert!(report.telemetry);
        for telemetry_event in TELEMETRY_EVENTS {
            assert!(report.installed_events.iter().any(|e| e == telemetry_event));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        let block = rimz_block(&parsed).unwrap();
        for event in TELEMETRY_EVENTS {
            let entry = block.get(*event).and_then(toml::Value::as_table).unwrap();
            // Telemetry hooks are non-blocking: sync = false.
            assert_eq!(
                entry.get("sync").and_then(toml::Value::as_bool),
                Some(false)
            );
        }
    }

    #[test]
    fn uninstall_removes_block_and_preserves_user_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model = \"o4-mini\"\n[hooks.user_custom]\ncommand = [\"echo\", \"hi\"]\n",
        )
        .unwrap();
        install_into(&path, true).unwrap();
        let report = uninstall_from(&path).unwrap();
        assert!(report.existed);
        assert!(!report.removed_events.is_empty());
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
    fn uninstall_on_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let report = uninstall_from(&path).unwrap();
        assert!(!report.existed);
        assert!(report.removed_events.is_empty());
    }

    #[test]
    fn install_rejects_async_blocking_hook_in_existing_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[hooks.rimz]\nblocking_events = [\"PermissionRequest\"]\n[hooks.rimz.PermissionRequest]\nsync = false\ncommand = [\"x\"]\n",
        )
        .unwrap();
        let err = install_into(&path, false).unwrap_err();
        assert!(matches!(err, AgentErr::Install { agent: "codex", .. }));
    }

    #[test]
    fn install_rejects_async_blocking_hook_even_without_blocking_events_list() {
        // Tampered or stale Rimz write: the [hooks.rimz] block has the
        // PermissionRequest sub-table marked sync = false but the
        // `blocking_events` array is missing. The installer must still
        // reject — BLOCKING_EVENTS is the source of truth.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[hooks.rimz.PermissionRequest]\nsync = false\ncommand = [\"x\"]\n",
        )
        .unwrap();
        let err = install_into(&path, false).unwrap_err();
        assert!(matches!(err, AgentErr::Install { agent: "codex", .. }));
    }
}
