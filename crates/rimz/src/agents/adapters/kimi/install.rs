//! Non-destructive Kimi `[[hooks]]` config merge.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::provider_file;
use crate::agents::version::{CliVersion, probe_cli_version};
use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, ManagedIntegration,
    Result, read_optional_file,
};

use super::KIMI_HOOKS;

pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source kimi";
const RIMZ_COMMAND_MARKER: &str = "rimz hooks feed --source kimi";

pub(super) static MANAGED_INTEGRATION: KimiManagedIntegration = KimiManagedIntegration;

pub(super) struct KimiManagedIntegration;

impl ManagedIntegration for KimiManagedIntegration {
    fn install(
        &self,
        _login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallReport> {
        install(&config_path()?, installed_cli_version())
    }

    fn preview(
        &self,
        _login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        preview(&config_path()?, installed_cli_version())
    }

    fn uninstall(&self, _login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall(&config_path()?)
    }

    fn installed(&self, _login_env: &BTreeMap<String, String>) -> bool {
        config_path().is_ok_and(|path| installed(&path))
    }

    fn managed_artifacts_present(&self, _login_env: &BTreeMap<String, String>) -> bool {
        config_path().is_ok_and(|path| managed(&path))
    }

    fn install_blocker(&self, _login_env: &BTreeMap<String, String>) -> Option<String> {
        version_blocker(installed_cli_version())
    }

    fn wiring_input_paths(
        &self,
        _descriptor: &super::super::AgentSpec,
        _login_env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        config_path().into_iter().collect()
    }
}

pub(super) fn config_path() -> Result<PathBuf> {
    Ok(std::env::var_os("RIMZ_KIMI_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| super::wire::kimi_home().join("config.toml")))
}

fn read_table(path: &Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(toml::Table::new()),
        Ok(text) => toml::from_str(&text).map_err(|source| AgentErr::InstallParse {
            agent: "kimi",
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "kimi",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn candidate(path: &Path) -> Result<(toml::Table, Vec<String>)> {
    let mut root = read_table(path)?;
    validate_hooks(path, &root)?;
    strip_managed(&mut root);
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    if !hooks.is_array() {
        return Err(AgentErr::Install {
            agent: "kimi",
            reason: "`hooks` must be an array of tables".to_owned(),
        });
    }
    let array = hooks.as_array_mut().expect("array checked above");
    for hook_record in KIMI_HOOKS {
        let mut hook = toml::Table::new();
        hook.insert(
            "event".to_owned(),
            toml::Value::String(hook_record.event.to_owned()),
        );
        if let Some(matcher) = hook_record.matcher {
            hook.insert(
                "matcher".to_owned(),
                toml::Value::String(matcher.to_owned()),
            );
        }
        hook.insert(
            "command".to_owned(),
            toml::Value::String(RIMZ_HOOK_COMMAND.to_owned()),
        );
        hook.insert(
            "timeout".to_owned(),
            toml::Value::Integer(if hook_record.event == "SessionEnd" {
                4
            } else {
                10
            }),
        );
        array.push(toml::Value::Table(hook));
    }
    Ok((
        root,
        KIMI_HOOKS
            .iter()
            .map(|hook| hook.event.to_owned())
            .collect(),
    ))
}

fn strip_managed(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks) = root.get_mut("hooks").and_then(toml::Value::as_array_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    hooks.retain(|value| {
        let managed = value
            .as_table()
            .and_then(|hook| hook.get("command"))
            .and_then(toml::Value::as_str)
            .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER));
        if managed
            && let Some(event) = value
                .as_table()
                .and_then(|hook| hook.get("event"))
                .and_then(toml::Value::as_str)
        {
            removed.push(event.to_owned());
        }
        !managed
    });
    if hooks.is_empty() {
        root.remove("hooks");
    }
    removed
}

fn render(table: &toml::Table) -> Result<String> {
    toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "kimi",
        source: Box::new(source),
    })
}

fn write(path: &Path, table: &toml::Table) -> Result<()> {
    provider_file::write_bytes(path, render(table)?.as_bytes())?;
    Ok(())
}

pub(super) fn install(path: &Path, version: Option<CliVersion>) -> Result<HookInstallReport> {
    refuse_old_cli(version)?;
    let existed = path.exists();
    let (table, installed_events) = candidate(path)?;
    write(path, &table)?;
    Ok(HookInstallReport {
        agent: "kimi",
        files: vec![crate::agents::HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        installed_events,
    })
}

pub(super) fn preview(path: &Path, version: Option<CliVersion>) -> Result<HookInstallPreview> {
    refuse_old_cli(version)?;
    let existed = path.exists();
    let original_config = read_optional_file("kimi", path)?;
    let (table, planned_events) = candidate(path)?;
    Ok(HookInstallPreview {
        agent: "kimi",
        files: vec![crate::agents::HookInstallFilePreview {
            path: path.to_path_buf(),
            original: original_config,
            candidate: render(&table)?,
            existed,
        }],
        planned_events,
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall(path: &Path) -> Result<HookUninstallReport> {
    if !path.exists() {
        return Ok(HookUninstallReport {
            agent: "kimi",
            files: vec![crate::agents::HookInstallFileReport {
                path: path.to_path_buf(),
                existed: false,
            }],
            removed_events: Vec::new(),
        });
    }
    let mut table = read_table(path)?;
    let mut removed_events = strip_managed(&mut table);
    removed_events.sort();
    removed_events.dedup();
    write(path, &table)?;
    Ok(HookUninstallReport {
        agent: "kimi",
        files: vec![crate::agents::HookInstallFileReport {
            path: path.to_path_buf(),
            existed: true,
        }],
        removed_events,
    })
}

pub(super) fn installed(path: &Path) -> bool {
    let Ok(table) = read_table(path) else {
        return false;
    };
    if validate_hooks(path, &table).is_err() {
        return false;
    }
    let Some(hooks) = table.get("hooks").and_then(toml::Value::as_array) else {
        return false;
    };
    KIMI_HOOKS.iter().all(|hook_record| {
        hooks.iter().any(|value| {
            let Some(hook) = value.as_table() else {
                return false;
            };
            hook.get("event").and_then(toml::Value::as_str) == Some(hook_record.event)
                && hook
                    .get("command")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER))
        })
    })
}

pub(super) fn managed(path: &Path) -> bool {
    read_table(path).is_ok_and(|table| {
        table
            .get("hooks")
            .and_then(toml::Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|value| {
                    value
                        .as_table()
                        .and_then(|hook| hook.get("command"))
                        .and_then(toml::Value::as_str)
                        .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER))
                })
            })
    })
}

fn installed_cli_version() -> Option<CliVersion> {
    probe_cli_version("kimi")?.parse().ok()
}

fn version_blocker(version: Option<CliVersion>) -> Option<String> {
    let found = version?;
    (found < CliVersion::new(2, 1, 1)).then(|| {
        format!(
            "Kimi Code {found} is unsupported; upgrade to 2.1.1 or later with `kimi upgrade -y`"
        )
    })
}

fn refuse_old_cli(version: Option<CliVersion>) -> Result<()> {
    match version_blocker(version) {
        Some(reason) => Err(AgentErr::Install {
            agent: "kimi",
            reason,
        }),
        None => Ok(()),
    }
}

fn validate_hooks(path: &Path, root: &toml::Table) -> Result<()> {
    let Some(value) = root.get("hooks") else {
        return Ok(());
    };
    let invalid = |entry: String, problem: String| AgentErr::Install {
        agent: "kimi",
        reason: format!(
            "{}: {entry}: {problem}; Kimi Code ignores every hook while one entry is invalid; fix or remove that entry",
            path.display()
        ),
    };
    let hooks = value
        .as_array()
        .ok_or_else(|| invalid("hooks".to_owned(), "must be an array of tables".to_owned()))?;
    for (index, value) in hooks.iter().enumerate() {
        let mut entry = format!("hooks entry {}", index + 1);
        let hook = value
            .as_table()
            .ok_or_else(|| invalid(entry.clone(), "must be a table".to_owned()))?;
        if let Some(event) = hook.get("event") {
            entry.push_str(&format!(" (event {event})"));
        }
        if let Some(key) = hook
            .keys()
            .find(|key| !matches!(key.as_str(), "event" | "matcher" | "command" | "timeout"))
        {
            return Err(invalid(entry, format!("unknown key `{key}`")));
        }
        if !matches!(
            hook.get("event").and_then(toml::Value::as_str),
            Some(
                "SessionStart"
                    | "SessionEnd"
                    | "SessionHeartbeat"
                    | "UserPromptSubmit"
                    | "UserPromptQueued"
                    | "TurnStarted"
                    | "PreToolUse"
                    | "PostToolUse"
                    | "PostToolUseFailure"
                    | "PermissionRequest"
                    | "PermissionResult"
                    | "Stop"
                    | "StopFailure"
                    | "Interrupt"
                    | "SubagentStart"
                    | "SubagentStop"
                    | "TaskStarted"
                    | "PreCompact"
                    | "PostCompact"
                    | "Notification"
            )
        ) {
            return Err(invalid(entry, "missing or unknown event".to_owned()));
        }
        if hook.get("matcher").is_some_and(|value| !value.is_str()) {
            return Err(invalid(entry, "matcher must be a string".to_owned()));
        }
        if !hook
            .get("command")
            .and_then(toml::Value::as_str)
            .is_some_and(|command| !command.is_empty())
        {
            return Err(invalid(
                entry,
                "command must be a nonempty string".to_owned(),
            ));
        }
        // Kimi's TOML parser yields a JS number, so `10.0` passes its `.int()` check.
        if hook.get("timeout").is_some_and(|value| {
            !value
                .as_integer()
                .map(|timeout| timeout as f64)
                .or_else(|| value.as_float())
                .is_some_and(|timeout| timeout.fract() == 0.0 && (1.0..=600.0).contains(&timeout))
        }) {
            return Err(invalid(
                entry,
                "timeout must be an integer in 1..=600".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_floor_refuses_old_but_allows_current_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "default_model = 'keep'\n";
        std::fs::write(&path, original).unwrap();
        let old = Some(CliVersion::new(2, 1, 0));
        for result in [
            install(&path, old).map(|_| ()),
            preview(&path, old).map(|_| ()),
        ] {
            assert!(
                result.is_err(),
                "Kimi 2.1.0 must refuse installation and preview"
            );
            let error = result.unwrap_err().to_string();
            for expected in ["2.1.0", "2.1.1", "kimi upgrade -y"] {
                assert!(error.contains(expected), "{error}");
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        assert!(version_blocker(old).unwrap().contains("kimi upgrade -y"));
        for version in [Some(CliVersion::new(2, 1, 1)), None] {
            assert!(version_blocker(version).is_none());
            preview(&path, version).unwrap();
            install(&path, version).unwrap();
            assert!(installed(&path));
        }
    }

    #[test]
    fn invalid_hook_entries_refuse_without_rewriting_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (entry, problem) in [
            (
                "event='TaskStarted'\ncommand='foreign'\nextra=true",
                "unknown key",
            ),
            ("event='FutureEvent'\ncommand='foreign'", "event"),
            (
                "event='TaskStarted'\ncommand='foreign'\nmatcher=42",
                "matcher",
            ),
            ("event='TaskStarted'", "command"),
            ("event='TaskStarted'\ncommand=''", "command"),
            ("event='TaskStarted'\ncommand=42", "command"),
            (
                "event='TaskStarted'\ncommand='foreign'\ntimeout=0",
                "timeout",
            ),
            (
                "event='TaskStarted'\ncommand='foreign'\ntimeout=601",
                "timeout",
            ),
            (
                "event='TaskStarted'\ncommand='foreign'\ntimeout=1.5",
                "timeout",
            ),
            (
                "event='TaskStarted'\ncommand='foreign'\ntimeout='10'",
                "timeout",
            ),
        ] {
            let original = format!("[[hooks]]\n{entry}\n");
            std::fs::write(&path, &original).unwrap();
            for result in [
                install(&path, None).map(|_| ()),
                preview(&path, None).map(|_| ()),
            ] {
                assert!(result.is_err(), "invalid {problem} must refuse: {entry}");
                let error = result.unwrap_err().to_string();
                for expected in [
                    problem,
                    "entry 1",
                    path.to_str().unwrap(),
                    "fix or remove",
                    "ignores every hook",
                ] {
                    assert!(error.contains(expected), "{error}");
                }
                assert!(error.contains(if entry.contains("FutureEvent") {
                    "FutureEvent"
                } else {
                    "TaskStarted"
                }));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            }
        }
    }

    #[test]
    fn installed_rejects_invalid_foreign_entries_and_preserves_valid_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        install(&path, None).unwrap();
        let broken = format!(
            "{}\n[[hooks]]\nevent='TaskStarted'\ncommand='foreign'\nextra=true\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, broken).unwrap();
        assert!(
            !installed(&path),
            "one invalid foreign entry disables all hooks"
        );
        std::fs::write(&path, "[[hooks]]\nevent='TaskStarted'\ncommand='foreign'\ntimeout=600\n[[hooks]]\nevent='SessionHeartbeat'\ncommand='heartbeat'\ntimeout=10.0\n").unwrap();
        install(&path, None).unwrap();
        install(&path, None).unwrap();
        assert!(installed(&path));
        let table = read_table(&path).unwrap();
        let hooks = table["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), KIMI_HOOKS.len() + 2);
        for command in ["foreign", "heartbeat"] {
            assert!(
                hooks
                    .iter()
                    .any(|hook| hook["command"].as_str() == Some(command))
            );
        }
    }
}
