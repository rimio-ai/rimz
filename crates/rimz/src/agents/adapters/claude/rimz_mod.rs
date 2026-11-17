//! RimZ's Claude Code mod: the embedded plugin folder hook install writes under
//! the RimZ home, and the settings entry that loads it in every session.
//!
//! The folder's content is stable across RimZ releases and carries its own
//! [`MOD_VERSION`]: every rewrite of an installed file prints a reload line in
//! each open session of the account, so install writes only a file whose bytes
//! differ. Claude also lays its own `.claude-plugin/types/` beside the files at
//! load; only the files listed here are RimZ's, so only they count as drift.
//!
//! The mod reports through `rimz mod feed --source claude` with one envelope
//! on stdin, `$.agent.list()` inside it as Claude returned it; [`observe`]
//! maps that list onto the child sidecar fields here, so the mod stays a thin
//! forwarder. An unknown field or event is a newer mod's and is ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::subagent_statusline::value_as_timestamp;
use crate::agents::context::{ModReport, SubagentContext, SubagentObservation};
use crate::agents::managed_env_list::ManagedEnvPath;
use crate::agents::{AgentErr, HookInstallFilePreview, HookInstallFileReport, Result};

/// The mod's own version, bumped by hand whenever a file below changes.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only the unit test reads it, pinning the manifest and module to it"
    )
)]
pub(super) const MOD_VERSION: &str = "1.0.0";

/// The settings `env` variable Claude reads its plugin folders from.
pub(super) const PLUGIN_DIRS_KEY: &str = "CLAUDE_CODE_PLUGIN_DIRS";

const FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("rimz_mod/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("rimz_mod/hooks.json")),
    ("hooks/rimz.ts", include_str!("rimz_mod/rimz.ts")),
];

/// The mod folder for this login, shared by every Claude account.
pub(super) fn dir(login_env: &BTreeMap<String, String>) -> PathBuf {
    crate::disk::paths::claude_mod_dir_in(login_env)
}

/// The settings entry that lists `dir` in `env.CLAUDE_CODE_PLUGIN_DIRS`.
pub(super) fn env_path(dir: &Path) -> Result<ManagedEnvPath<'_>> {
    let path = dir.to_str().ok_or_else(|| AgentErr::Install {
        agent: "claude",
        reason: format!(
            "the RimZ mod folder {} is not a UTF-8 path; Claude cannot list it",
            dir.display()
        ),
    })?;
    Ok(ManagedEnvPath {
        key: PLUGIN_DIRS_KEY,
        path,
    })
}

/// Each mod file under `dir` whose content differs from the embedded one, with
/// what is there now (`None` when missing).
pub(super) fn previews(dir: &Path) -> Result<Vec<HookInstallFilePreview>> {
    let mut previews = Vec::new();
    for (relative, content) in FILES {
        let path = dir.join(relative);
        let original = crate::agents::read_optional_file("claude", &path)?;
        if original.as_deref() == Some(*content) {
            continue;
        }
        previews.push(HookInstallFilePreview {
            existed: original.is_some(),
            original,
            candidate: (*content).to_owned(),
            path,
        });
    }
    Ok(previews)
}

/// Write each mod file whose content differs, reporting the files written.
pub(super) fn write(dir: &Path) -> Result<Vec<HookInstallFileReport>> {
    let mut written = Vec::new();
    for preview in previews(dir)? {
        crate::agents::provider_file::write_bytes(&preview.path, preview.candidate.as_bytes())?;
        written.push(HookInstallFileReport {
            path: preview.path,
            existed: preview.existed,
        });
    }
    Ok(written)
}

/// Mod files missing, unreadable, or different from the embedded ones. Reads
/// only.
pub(super) fn outdated(dir: &Path) -> Vec<PathBuf> {
    FILES
        .iter()
        .map(|(relative, expected)| (dir.join(relative), *expected))
        .filter(|(path, expected)| {
            std::fs::read(path).map_or(true, |content| content != expected.as_bytes())
        })
        .map(|(path, _)| path)
        .collect()
}

/// The events the mod reports at.
const EVENTS: &[&str] = &["session.start", "agent.spawn", "tool.call", "turn.complete"];

/// The envelope the mod writes. Every field but `v`, `event` and
/// `session_id` may be absent.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Envelope {
    v: Option<String>,
    event: Option<String>,
    session_id: Option<String>,
    claude_version: Option<String>,
    agents: Option<Vec<Value>>,
}

/// One mod report, or `None` for an envelope not to act on. An agent entry
/// without a usable id drops alone.
pub(super) fn observe(payload: &Value, observed_at: Timestamp) -> Option<ModReport> {
    let envelope = Envelope::deserialize(payload).ok()?;
    let event = envelope.event?;
    if !EVENTS.contains(&event.as_str()) {
        return None;
    }
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let subagents = envelope
        .agents
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| observe_agent(entry, observed_at))
        .collect();
    Some(ModReport {
        session_id: non_empty(envelope.session_id)?,
        mod_version: non_empty(envelope.v)?,
        provider_version: non_empty(envelope.claude_version),
        subagents,
    })
}

/// One `$.agent.list()` entry (Claude's `AgentInfo`), read field by field: a
/// field of a shape RimZ does not expect drops alone, and only an entry
/// without a usable `id` drops whole.
fn observe_agent(entry: &Value, observed_at: Timestamp) -> Option<SubagentObservation> {
    let text = |field: &str| {
        entry
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    // Claude types an effort as a level name or a number.
    let effort = |holder: Option<&Value>| match holder?.get("effort")? {
        Value::String(level) if !level.is_empty() => Some(level.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    Some(SubagentObservation {
        agent_id: text("id")?,
        context: SubagentContext {
            usage: None,
            agent_type: text("type"),
            model: text("model"),
            // The configured effort, absent when the child runs on the
            // session's, wins over the one its newest request carried.
            effort: effort(Some(entry)).or_else(|| effort(entry.get("lastRequest"))),
            description: text("description"),
            cost_usd: None,
            started_at: entry.get("startTime").and_then(value_as_timestamp),
            observed_at,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at() -> Timestamp {
        Timestamp::from_second(1_794_946_800).unwrap()
    }

    #[test]
    fn observe_maps_the_agent_list_onto_child_fields() {
        let report = observe(
            &json!({
                "v": MOD_VERSION,
                "event": "turn.complete",
                "session_id": "sess-1",
                "transcript_path": "/t/sess-1.jsonl",
                "claude_version": "2.1.326",
                "agent_id": "aaf8677e2225c6981",
                "future_field": {"ignored": true},
                "agents": [
                    {"id": "aaf8677e2225c6981", "description": "Run printf child-ok",
                     "type": "general-purpose", "status": "completed", "kind": "local_agent",
                     "startTime": 1_794_946_718_622_i64, "model": "claude-haiku-5-5",
                     "cost": {"usd": 0.01},
                     "lastRequest": {"at": 1, "model": "claude-haiku-5-5", "effort": "low"}},
                    {"id": "b2", "type": "Explore", "effort": "high",
                     "lastRequest": {"effort": "low"}},
                    {"description": "no id"},
                    "not an object"
                ]
            }),
            at(),
        )
        .unwrap();
        assert_eq!(report.session_id, "sess-1");
        assert_eq!(report.mod_version, MOD_VERSION);
        assert_eq!(report.provider_version.as_deref(), Some("2.1.326"));
        assert_eq!(
            report.subagents,
            vec![
                SubagentObservation {
                    agent_id: "aaf8677e2225c6981".into(),
                    context: SubagentContext {
                        usage: None,
                        agent_type: Some("general-purpose".into()),
                        model: Some("claude-haiku-5-5".into()),
                        effort: Some("low".into()),
                        description: Some("Run printf child-ok".into()),
                        cost_usd: None,
                        started_at: Some(Timestamp::from_millisecond(1_794_946_718_622).unwrap()),
                        observed_at: at(),
                    },
                },
                SubagentObservation {
                    agent_id: "b2".into(),
                    context: SubagentContext {
                        usage: None,
                        agent_type: Some("Explore".into()),
                        model: None,
                        effort: Some("high".into()),
                        description: None,
                        cost_usd: None,
                        started_at: None,
                        observed_at: at(),
                    },
                },
            ],
            "the configured effort wins over the last request's"
        );
    }

    #[test]
    fn observe_keeps_a_child_whose_optional_fields_surprise() {
        let report = observe(
            &json!({
                "v": MOD_VERSION,
                "event": "turn.complete",
                "session_id": "s",
                "agents": [
                    {"id": "configured", "type": "Explore", "model": "m", "effort": 32000},
                    {"id": "requested", "type": "Explore", "model": "m",
                     "lastRequest": {"effort": 2.5}},
                    {"id": "odd", "type": "Explore", "model": "m", "effort": true,
                     "description": {"text": "x"}, "startTime": [1], "lastRequest": "x"},
                    {"id": 7, "type": "Explore"}
                ]
            }),
            at(),
        )
        .unwrap();
        let children: Vec<_> = report
            .subagents
            .iter()
            .map(|child| {
                (
                    child.agent_id.as_str(),
                    child.context.agent_type.as_deref(),
                    child.context.model.as_deref(),
                    child.context.effort.as_deref(),
                    child.context.description.as_deref(),
                    child.context.started_at,
                )
            })
            .collect();
        assert_eq!(
            children,
            [
                (
                    "configured",
                    Some("Explore"),
                    Some("m"),
                    Some("32000"),
                    None,
                    None
                ),
                (
                    "requested",
                    Some("Explore"),
                    Some("m"),
                    Some("2.5"),
                    None,
                    None
                ),
                ("odd", Some("Explore"), Some("m"), None, None, None),
            ],
            "a surprising field drops alone; only an entry without a usable id drops"
        );
    }

    #[test]
    fn observe_refuses_envelopes_not_to_act_on() {
        for payload in [
            json!(null),
            json!([]),
            json!({}),
            json!({"v": "1.0.0", "event": "session.measure", "session_id": "s"}),
            json!({"v": "1.0.0", "event": "session.start"}),
            json!({"v": "1.0.0", "event": "session.start", "session_id": ""}),
            json!({"event": "session.start", "session_id": "s"}),
            json!({"v": 1, "event": "session.start", "session_id": "s"}),
        ] {
            assert_eq!(observe(&payload, at()), None, "{payload}");
        }
        let listless = observe(
            &json!({"v": "1.0.0", "event": "session.start", "session_id": "s", "agents": "x"}),
            at(),
        );
        assert_eq!(listless, None, "a malformed list is a malformed envelope");
        let bare = observe(
            &json!({"v": "1.0.0", "event": "session.start", "session_id": "s"}),
            at(),
        )
        .unwrap();
        assert!(bare.subagents.is_empty());
        assert_eq!(bare.provider_version, None);
    }

    #[test]
    fn manifest_and_module_carry_the_mod_version() {
        let manifest: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
        assert_eq!(manifest["version"], MOD_VERSION);
        assert_eq!(manifest["name"], "rimz");
        let hooks: serde_json::Value = serde_json::from_str(FILES[1].1).unwrap();
        assert_eq!(hooks["modules"], serde_json::json!(["./rimz.ts"]));
        assert!(
            FILES[2]
                .1
                .contains(&format!("const MOD_VERSION = '{MOD_VERSION}'")),
            "the hooks module reports the version the manifest names"
        );
    }

    #[test]
    fn write_touches_only_files_that_differ() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("mods/claude");
        assert_eq!(
            outdated(&dir).len(),
            FILES.len(),
            "a missing folder is drift"
        );
        let first = write(&dir).unwrap();
        assert_eq!(first.len(), FILES.len());
        assert!(first.iter().all(|file| !file.existed));
        assert!(outdated(&dir).is_empty());
        assert!(
            write(&dir).unwrap().is_empty(),
            "a reinstall writes nothing"
        );

        let module = dir.join("hooks/rimz.ts");
        std::fs::write(&module, "stale").unwrap();
        std::fs::create_dir_all(dir.join(".claude-plugin/types")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/types/index.d.ts"),
            "laid by Claude",
        )
        .unwrap();
        assert_eq!(outdated(&dir), vec![module.clone()]);
        let refreshed = write(&dir).unwrap();
        assert_eq!(
            refreshed,
            vec![HookInstallFileReport {
                path: module.clone(),
                existed: true
            }]
        );
        assert!(dir.join(".claude-plugin/types/index.d.ts").is_file());

        std::fs::remove_file(&module).unwrap();
        std::fs::create_dir(&module).unwrap();
        assert_eq!(
            outdated(&dir),
            vec![module],
            "a mod file that cannot be read is stale"
        );
    }
}
