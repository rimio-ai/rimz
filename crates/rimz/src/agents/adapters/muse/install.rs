//! Two-file managed-hook transaction; ownership is structural, never a JSON marker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use super::{MUSE_HOOKS, MuseAdapter};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::hook_types::{HOOK_ENV_EXTRA_NAMES, HOOK_ENV_NAMES};
use crate::agents::settings_json::{self, PendingWrite};
use crate::agents::version::{CliVersion, probe_cli_version_with};
use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, Result, read_optional_file,
};

const AGENT: &str = "muse";
pub(super) const MIN_SUPPORTED_CLI: CliVersion = CliVersion::new(1, 4, 9);
const HOOK_TIMEOUT: i64 = 4;

pub(super) struct MuseManagedIntegration;

impl ManagedIntegration for MuseManagedIntegration {
    fn install(&self, env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        install_at(&home(env)?, Some(installed_cli_version()?))
    }

    fn preview(&self, env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        preview_at(&home(env)?, Some(installed_cli_version()?))
    }

    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall_at(&home(env)?)
    }

    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        let Ok(home) = home(env) else {
            return false;
        };
        let Ok(settings) = settings_json::read_json_object(AGENT, &home.join("settings.json"))
        else {
            return false;
        };
        settings.get("schema_version") == Some(&json!(1))
            && owns_settings(&settings, &home)
            && settings
                .get("managed_hooks_env_vars")
                .and_then(Value::as_array)
                .is_some_and(|names| {
                    let names: BTreeSet<_> = names.iter().filter_map(Value::as_str).collect();
                    passthrough_names().iter().all(|name| names.contains(name))
                })
            && render_hooks().is_ok_and(|canonical| {
                std::fs::read_to_string(home.join("rimz-hooks.json"))
                    .is_ok_and(|text| text == canonical)
            })
    }

    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        home(env).is_ok_and(|home| {
            home.join("rimz-hooks.json").exists()
                || settings_json::read_json_object(AGENT, &home.join("settings.json"))
                    .is_ok_and(|settings| owns_settings(&settings, &home))
        })
    }

    fn upgrade_available(&self, env: &BTreeMap<String, String>) -> bool {
        self.managed_artifacts_present(env) && !self.installed(env)
    }

    fn install_blocker(&self, env: &BTreeMap<String, String>) -> Option<String> {
        self.preview(env).err().map(|error| error.to_string())
    }

    fn wiring_input_paths(
        &self,
        _spec: &crate::agents::AgentSpec,
        env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        home(env)
            .map(|home| vec![home.join("rimz-hooks.json"), home.join("settings.json")])
            .unwrap_or_default()
    }
}

fn home(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    MuseAdapter
        .config_home(env)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| AgentErr::Install {
            agent: AGENT,
            reason: "set an absolute XDG_CONFIG_HOME or HOME to resolve Muse settings.json"
                .to_owned(),
        })
}

fn version_error(found: &str) -> AgentErr {
    AgentErr::Install {
        agent: AGENT,
        reason: format!(
            "Muse Code {MIN_SUPPORTED_CLI} or later is required (found {found}); update Muse to {MIN_SUPPORTED_CLI} or later"
        ),
    }
}

fn installed_cli_version() -> Result<CliVersion> {
    let found = probe_cli_version_with("muse", |stdout, stderr| {
        [stdout, stderr]
            .into_iter()
            .map(str::trim)
            .find(|text| !text.is_empty())
            .map(|text| {
                text.strip_prefix("Muse Code ")
                    .unwrap_or(text)
                    .split_whitespace()
                    .next()
                    .unwrap_or(text)
                    .to_owned()
            })
    });
    let version = found
        .as_deref()
        .and_then(|found| found.parse::<CliVersion>().ok());
    version
        .filter(|version| *version >= MIN_SUPPORTED_CLI)
        .ok_or_else(|| version_error(found.as_deref().unwrap_or("unavailable or unparseable")))
}

fn refuse_old_cli(version: Option<CliVersion>) -> Result<()> {
    match version {
        Some(version) if version >= MIN_SUPPORTED_CLI => Ok(()),
        Some(version) => Err(version_error(&version.to_string())),
        None => Err(version_error("unavailable or unparseable")),
    }
}

pub(super) fn passthrough_names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: BTreeSet<_> = HOOK_ENV_NAMES
            .iter()
            .chain(HOOK_ENV_EXTRA_NAMES)
            .copied()
            .collect();
        names.extend(["MUSE_SESSION_ID", "MUSE_THREAD_ID"]);
        names.remove("RIMZ_AGENT_PID");
        names.remove("RIMZ_HOOK_OWNER_KIND");
        names.into_iter().collect()
    })
}

fn render_hooks() -> Result<String> {
    let mut hooks = Map::new();
    for event in MUSE_HOOKS {
        let mut handler = json!({"type": "command", "command": "rimz hooks feed --source muse", "timeout": HOOK_TIMEOUT});
        if event.event == "Interrupt" {
            handler["async"] = json!(true);
        }
        hooks.insert(event.event.to_owned(), json!([{"hooks": [handler]}]));
    }
    settings_json::render_json(
        AGENT,
        &Map::from_iter([("hooks".to_owned(), Value::Object(hooks))]),
    )
}

fn owns_settings(settings: &Map<String, Value>, home: &Path) -> bool {
    settings
        .get("managed_hooks_path")
        .and_then(Value::as_str)
        .is_some_and(|path| Path::new(path) == home.join("rimz-hooks.json"))
}

fn settings_candidate(home: &Path) -> Result<Map<String, Value>> {
    let path = home.join("settings.json");
    let exists = settings_json::read_optional_bytes(AGENT, &path)?.is_some();
    let mut settings = settings_json::read_json_object(AGENT, &path)?;
    if exists && settings.get("schema_version") != Some(&json!(1)) {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "{} requires schema_version: 1; Muse refuses it as it stands; fix the settings file before installing",
                path.display()
            ),
        });
    }
    if settings
        .get("managed_hooks_path")
        .is_some_and(|path| !path.is_null())
        && !owns_settings(&settings, home)
    {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "{} names managed_hooks_path {}; RimZ will not replace another managed hooks source; remove that setting to use RimZ hooks",
                path.display(),
                settings["managed_hooks_path"]
            ),
        });
    }
    settings.insert("schema_version".to_owned(), json!(1));
    settings.insert(
        "managed_hooks_path".to_owned(),
        json!(home.join("rimz-hooks.json")),
    );
    settings.insert(
        "managed_hooks_env_vars".to_owned(),
        json!(passthrough_names()),
    );
    Ok(settings)
}

pub(super) fn preview_at(home: &Path, version: Option<CliVersion>) -> Result<HookInstallPreview> {
    refuse_old_cli(version)?;
    let settings = settings_candidate(home)?;
    let mut files = Vec::new();
    for (path, candidate) in [
        (home.join("rimz-hooks.json"), render_hooks()?),
        (
            home.join("settings.json"),
            settings_json::render_json(AGENT, &settings)?,
        ),
    ] {
        let original = read_optional_file(AGENT, &path)?;
        files.push(HookInstallFilePreview {
            path,
            existed: original.is_some(),
            original,
            candidate,
        });
    }
    Ok(HookInstallPreview {
        agent: AGENT,
        files,
        planned_events: event_names(),
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

pub(super) fn install_at(home: &Path, version: Option<CliVersion>) -> Result<HookInstallReport> {
    let preview = preview_at(home, version)?;
    let hooks = &preview.files[0];
    let settings = &preview.files[1];
    settings_json::commit_pair(
        AGENT,
        PendingWrite::required(&hooks.path, &hooks.candidate),
        PendingWrite::required(&settings.path, &settings.candidate),
        hooks.original.as_deref().map(str::as_bytes),
        settings.original.as_deref().map(str::as_bytes),
    )?;
    Ok(HookInstallReport {
        agent: AGENT,
        files: preview
            .files
            .into_iter()
            .map(|file| HookInstallFileReport {
                path: file.path,
                existed: file.existed,
            })
            .collect(),
        installed_events: preview.planned_events,
    })
}

pub(super) fn uninstall_at(home: &Path) -> Result<HookUninstallReport> {
    let path = home.join("settings.json");
    let hooks_path = home.join("rimz-hooks.json");
    let settings_existed = settings_json::read_optional_bytes(AGENT, &path)?.is_some();
    let mut settings = settings_json::read_json_object(AGENT, &path)?;
    if owns_settings(&settings, home) {
        settings.remove("managed_hooks_path");
        settings.remove("managed_hooks_env_vars");
        settings_json::write_json(AGENT, &path, &settings)?;
    }
    let hooks_existed = match std::fs::remove_file(&hooks_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(AgentErr::InstallIo {
                agent: AGENT,
                path: hooks_path,
                source,
            });
        }
    };
    Ok(HookUninstallReport {
        agent: AGENT,
        files: vec![
            HookInstallFileReport {
                path: hooks_path,
                existed: hooks_existed,
            },
            HookInstallFileReport {
                path,
                existed: settings_existed,
            },
        ],
        removed_events: if hooks_existed {
            event_names()
        } else {
            Vec::new()
        },
    })
}

fn event_names() -> Vec<String> {
    MUSE_HOOKS
        .iter()
        .map(|event| event.event.to_owned())
        .collect()
}
