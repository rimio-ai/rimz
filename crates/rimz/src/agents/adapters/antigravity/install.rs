//! Antigravity hook plugin and statusline installer.
//!
//! RimZ's hooks live in a plugin directory it owns, `~/.gemini/config/plugins/rimz/`,
//! written as whole embedded files that agy discovers and enables by default
//! (observed on agy 1.5.12). The hook file holds only events with a documented
//! observer-neutral response; `PreToolUse` stays untouched because every
//! documented result changes native permission policy. Earlier releases merged
//! the same hook into the global `hooks.json`, which install and uninstall now
//! only strip: a hook name loaded from both files runs twice. agy rewrites
//! `settings.json` and drops unknown `statusLine` keys, so the statusline is
//! owned by its command and the displaced original lives in a RimZ-home record.
//! Enablement lives in agy's `config.json`, which RimZ reads and never writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, AgentSpec, HookInstallFilePreview, HookInstallPreview, HookInstallReport,
    HookPreflightErr, HookUninstallReport, ManagedIntegration, Result, agent_config_path, jsonc,
    provider_file, read_optional_file,
    settings_json::{self, PendingWrite},
};

use super::super::install_report::report_files;
use super::super::owned_statusline::{OwnedStatusLine, StateChange};
use super::{
    ANTIGRAVITY_HOOKS, HOOK_TIMEOUT_SECS, RIMZ_HOOK_MARKER, RIMZ_STATUS_LINE_MARKER,
    STATUS_LINE_COMMAND,
};

const AGENT: &str = "antigravity";
const PLUGIN_NAME: &str = "rimz";
const PLUGIN_AUTHOR: &str = "RimZ";
const MANIFEST: &str = "plugin.json";
const PLUGIN_HOOKS: &str = "hooks.json";
const ENABLE_FIX: &str = "agy plugin enable rimz";
const STACK_WITH_DEFAULT: &str = "stack_with_default";

const STATUS_LINE: OwnedStatusLine = OwnedStatusLine {
    agent: AGENT,
    command: STATUS_LINE_COMMAND,
    marker: RIMZ_STATUS_LINE_MARKER,
    retained_keys: &[STACK_WITH_DEFAULT],
    object_only: true,
};

pub(super) static MANAGED_INTEGRATION: AntigravityManagedIntegration =
    AntigravityManagedIntegration { paths: None };

/// The files the installer resolves from the process home. The plugin
/// directory and agy's `config.json` sit beside the global hooks file, so
/// the `RIMZ_ANTIGRAVITY_HOOKS` override moves all three.
#[derive(Clone, Debug)]
pub(super) struct Paths {
    pub(super) hooks: PathBuf,
    pub(super) settings: PathBuf,
    pub(super) state: PathBuf,
}

impl Paths {
    fn plugin(&self) -> PathBuf {
        self.hooks.with_file_name("plugins").join(PLUGIN_NAME)
    }

    fn config(&self) -> PathBuf {
        self.hooks.with_file_name("config.json")
    }
}

pub(super) struct AntigravityManagedIntegration {
    paths: Option<Paths>,
}

impl AntigravityManagedIntegration {
    #[cfg(test)]
    pub(super) fn at(paths: Paths) -> Self {
        Self { paths: Some(paths) }
    }

    fn paths(&self) -> Result<Paths> {
        match &self.paths {
            Some(paths) => Ok(paths.clone()),
            None => Ok(Paths {
                hooks: hooks_path()?,
                settings: settings_path()?,
                state: crate::disk::paths::rimz_home().join("antigravity-statusline.json"),
            }),
        }
    }
}

impl ManagedIntegration for AntigravityManagedIntegration {
    fn outdated_files(
        &self,
        _login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Vec<PathBuf> {
        self.paths()
            .map(|paths| outdated_files(&paths))
            .unwrap_or_default()
    }

    fn install(
        &self,
        _login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallReport> {
        install(&self.paths()?)
    }

    fn preview(
        &self,
        _login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        preview(&self.paths()?)
    }

    fn uninstall(&self, _login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall(&self.paths()?)
    }

    fn installed(&self, _login_env: &BTreeMap<String, String>) -> bool {
        self.paths().is_ok_and(|paths| {
            plugin_installed(&paths.plugin())
                && !holds_owned_hook(&paths.hooks)
                && STATUS_LINE.installed_at(&paths.settings)
        })
    }

    fn managed_artifacts_present(&self, _login_env: &BTreeMap<String, String>) -> bool {
        self.paths().is_ok_and(|paths| {
            owned_plugin_present(&paths.plugin())
                || holds_owned_hook(&paths.hooks)
                || STATUS_LINE.artifact_at(&paths.settings)
        })
    }

    fn hooks_disabled(&self, _login_env: &BTreeMap<String, String>) -> Option<HookPreflightErr> {
        let config = self.paths().ok()?.config();
        let root: Value = jsonc::from_slice(&std::fs::read(&config).ok()?).ok()?;
        (root.pointer("/plugins/rimz/enabled") == Some(&Value::Bool(false))).then(|| {
            HookPreflightErr::HooksDisabled {
                reason: format!(
                    "agy has the RimZ plugin disabled: `plugins.rimz.enabled` is false in {}",
                    config.display()
                ),
                fix: ENABLE_FIX.to_owned(),
            }
        })
    }

    fn wiring_input_paths(
        &self,
        _descriptor: &AgentSpec,
        _login_env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        let Ok(paths) = self.paths() else {
            return Vec::new();
        };
        let plugin = paths.plugin();
        plugin_files()
            .iter()
            .map(|(name, _)| plugin.join(name))
            .chain([
                paths.hooks.clone(),
                paths.config(),
                paths.settings,
                paths.state,
            ])
            .collect()
    }

    fn wrapped_status_line_command(&self, _login_env: &BTreeMap<String, String>) -> Option<String> {
        let paths = self.paths().ok()?;
        STATUS_LINE.wrapped_command_at(&paths.settings, &paths.state)
    }
}

pub(super) fn hooks_path() -> Result<PathBuf> {
    agent_config_path(
        AGENT,
        "RIMZ_ANTIGRAVITY_HOOKS",
        Path::new(".gemini/config/hooks.json"),
    )
}

pub(super) fn settings_path() -> Result<PathBuf> {
    agent_config_path(
        AGENT,
        "RIMZ_ANTIGRAVITY_SETTINGS",
        Path::new(".gemini/antigravity-cli/settings.json"),
    )
}

/// The plugin's files, hooks first so agy never discovers a manifest whose
/// hooks are not yet on disk. The manifest's version moves only with the
/// plugin's content.
fn plugin_files() -> [(&'static str, Vec<u8>); 2] {
    let mut hooks = Map::new();
    hooks.insert(PLUGIN_NAME.to_owned(), Value::Object(rimz_hook()));
    let manifest = json!({
        "name": PLUGIN_NAME,
        "version": "1.0.0",
        "description": "RimZ session and attention hooks",
        "author": PLUGIN_AUTHOR,
    });
    // Both values hold only string keys and serializable primitives.
    [
        (
            PLUGIN_HOOKS,
            serde_json::to_vec_pretty(&hooks).expect("embedded hooks serialize"),
        ),
        (
            MANIFEST,
            serde_json::to_vec_pretty(&manifest).expect("embedded manifest serializes"),
        ),
    ]
}

fn rimz_hook() -> Map<String, Value> {
    let mut rimz = Map::new();
    for hook in &ANTIGRAVITY_HOOKS {
        let handler = json!({
            "type": "command",
            "command": hook.command,
            "timeout": HOOK_TIMEOUT_SECS,
        });
        let entry = match hook.config_matcher {
            Some(matcher) => json!({"matcher": matcher, "hooks": [handler]}),
            None => handler,
        };
        if let Value::Array(entries) = rimz
            .entry(hook.config_event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            entries.push(entry);
        }
    }
    rimz
}

/// Refuses a plugin path RimZ does not own: a file, a symlink, a plugin file
/// that is not a regular file, a manifest another author wrote, or a
/// manifest-less directory holding anything but a hook file with RimZ's
/// handlers. An absent or empty directory is RimZ's to fill.
fn check_plugin(dir: &Path) -> Result<()> {
    let io_err = |source| AgentErr::InstallIo {
        agent: AGENT,
        path: dir.to_path_buf(),
        source,
    };
    match std::fs::symlink_metadata(dir) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_err(source)),
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(not_owned(dir)),
    }
    for name in [PLUGIN_HOOKS, MANIFEST] {
        let file = dir.join(name);
        if std::fs::symlink_metadata(&file).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(AgentErr::Install {
                agent: AGENT,
                reason: format!(
                    "{} is not a regular file; move it aside before installing RimZ",
                    file.display()
                ),
            });
        }
    }
    let hooks = dir.join(PLUGIN_HOOKS);
    let owned = match settings_json::read_optional_bytes(AGENT, &dir.join(MANIFEST))? {
        Some(bytes) => jsonc::from_slice::<Value>(&bytes).is_ok_and(|manifest| {
            manifest["name"] == PLUGIN_NAME && manifest["author"] == PLUGIN_AUTHOR
        }),
        None => {
            std::fs::read_dir(dir).map_err(io_err)?.all(|entry| {
                entry.is_ok_and(|entry| {
                    entry.file_name() == PLUGIN_HOOKS
                        && entry.file_type().is_ok_and(|kind| kind.is_file())
                })
            }) && (!hooks.exists() || holds_owned_hook(&hooks))
        }
    };
    if owned { Ok(()) } else { Err(not_owned(dir)) }
}

fn not_owned(dir: &Path) -> AgentErr {
    AgentErr::Install {
        agent: AGENT,
        reason: format!(
            "{} is not a RimZ-owned agy plugin; move it aside before installing RimZ",
            dir.display()
        ),
    }
}

fn owned_plugin_present(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok() && check_plugin(dir).is_ok()
}

fn plugin_installed(dir: &Path) -> bool {
    owned_plugin_present(dir)
        && plugin_files().iter().all(|(name, bytes)| {
            let file = dir.join(name);
            std::fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.is_file())
                && std::fs::read(file).is_ok_and(|content| content == *bytes)
        })
}

fn outdated_files(paths: &Paths) -> Vec<PathBuf> {
    let plugin = paths.plugin();
    let mut files = Vec::new();
    if check_plugin(&plugin).is_ok() {
        let entries = plugin_files().map(|(name, bytes)| {
            let file = plugin.join(name);
            let current = std::fs::symlink_metadata(&file)
                .is_ok_and(|metadata| metadata.is_file())
                .then(|| std::fs::read(&file).ok())
                .flatten();
            (
                file,
                current.as_deref() == Some(bytes.as_slice()),
                current.is_some(),
            )
        });
        if entries.iter().any(|(_, _, present)| *present) {
            files.extend(
                entries
                    .into_iter()
                    .filter(|(_, current, _)| !current)
                    .map(|(file, ..)| file),
            );
        }
    }
    if holds_owned_hook(&paths.hooks) {
        files.push(paths.hooks.clone());
    }
    if STATUS_LINE.outdated_at(&paths.settings) {
        files.push(paths.settings.clone());
    }
    files
}

/// The plugin is published last: a failed record or settings pair leaves any
/// legacy global handlers as the one hook set, never beside the plugin.
fn install(paths: &Paths) -> Result<HookInstallReport> {
    let plugin = paths.plugin();
    check_plugin(&plugin)?;
    let settings = Settings::plan(paths)?;
    let global = stripped_global(&paths.hooks)?;
    let hooks_original = global
        .is_some()
        .then(|| settings_json::read_optional_bytes(AGENT, &paths.hooks))
        .transpose()?
        .flatten();
    let hooks_candidate = global
        .as_ref()
        .map(|root| settings_json::render_json(AGENT, root))
        .transpose()?;
    let state_existed = paths.state.exists();
    STATUS_LINE.apply_state(&paths.state, &settings.state)?;
    settings_json::commit_pair(
        AGENT,
        PendingWrite::optional(&paths.settings, settings.write.as_deref()),
        PendingWrite::optional(&paths.hooks, hooks_candidate.as_deref()),
        settings.original.as_deref(),
        hooks_original.as_deref(),
    )?;
    let mut files = Vec::new();
    for (name, candidate) in plugin_files() {
        let file = plugin.join(name);
        let original = settings_json::read_optional_bytes(AGENT, &file)?;
        if original.as_deref() != Some(candidate.as_slice()) {
            provider_file::write_bytes(&file, &candidate)?;
        }
        files.push((file, original.is_some()));
    }
    if global.is_some() {
        files.push((paths.hooks.clone(), true));
    }
    files.push((paths.settings.clone(), settings.original.is_some()));
    if !matches!(settings.state, StateChange::Keep) {
        files.push((paths.state.clone(), state_existed));
    }
    Ok(HookInstallReport {
        plugin_dir: None,
        agent: AGENT,
        files: report_files(
            files
                .iter()
                .map(|(path, existed)| (path.as_path(), *existed)),
        ),
        installed_events: installed_event_names(),
    })
}

fn preview(paths: &Paths) -> Result<HookInstallPreview> {
    let plugin = paths.plugin();
    check_plugin(&plugin)?;
    let settings = Settings::plan(paths)?;
    let mut files = Vec::new();
    for (name, candidate) in plugin_files() {
        let file = plugin.join(name);
        let original = read_optional_file(AGENT, &file)?;
        files.push(HookInstallFilePreview {
            path: file,
            existed: original.is_some(),
            original,
            candidate: String::from_utf8(candidate).expect("serialized JSON is UTF-8"),
        });
    }
    if let Some(root) = stripped_global(&paths.hooks)? {
        files.push(HookInstallFilePreview {
            path: paths.hooks.clone(),
            existed: true,
            original: read_optional_file(AGENT, &paths.hooks)?,
            candidate: settings_json::render_json(AGENT, &root)?,
        });
    }
    let settings_original = read_optional_file(AGENT, &paths.settings)?;
    files.push(HookInstallFilePreview {
        path: paths.settings.clone(),
        existed: settings_original.is_some(),
        candidate: settings
            .write
            .clone()
            .or_else(|| settings_original.clone())
            .unwrap_or_default(),
        original: settings_original,
    });
    files.extend(STATUS_LINE.state_preview(&paths.state, &settings.state)?);
    Ok(HookInstallPreview {
        agent: AGENT,
        files,
        planned_events: installed_event_names(),
        status_line_change: settings.change,
        subagent_status_line_change: None,
        plugin_dir: None,
    })
}

/// A plugin path RimZ does not own stays; the global file's owned handlers and
/// the owned statusline are still removed.
fn uninstall(paths: &Paths) -> Result<HookUninstallReport> {
    let plugin = paths.plugin();
    let mut files = Vec::new();
    let mut removed_hooks = false;
    if owned_plugin_present(&plugin) {
        removed_hooks = holds_owned_hook(&plugin.join(PLUGIN_HOOKS));
        std::fs::remove_dir_all(&plugin).map_err(|source| AgentErr::InstallIo {
            agent: AGENT,
            path: plugin.clone(),
            source,
        })?;
        files.push((plugin, true));
    }
    if let Some(root) = stripped_global(&paths.hooks)? {
        settings_json::write_json(AGENT, &paths.hooks, &root)?;
        files.push((paths.hooks.clone(), true));
        removed_hooks = true;
    }
    if paths.settings.exists() {
        let mut root = settings_json::read_json_object(AGENT, &paths.settings)?;
        let before = root.clone();
        STATUS_LINE.strip(&mut root, &paths.state)?;
        if root != before {
            settings_json::write_json(AGENT, &paths.settings, &root)?;
            files.push((paths.settings.clone(), true));
        }
    }
    if paths.state.exists() {
        STATUS_LINE.remove_state(&paths.state)?;
        files.push((paths.state.clone(), true));
    }
    Ok(HookUninstallReport {
        agent: AGENT,
        files: report_files(
            files
                .iter()
                .map(|(path, existed)| (path.as_path(), *existed)),
        ),
        removed_events: if removed_hooks {
            installed_event_names()
        } else {
            Vec::new()
        },
    })
}

/// What install does to `settings.json`: the bytes to write when the
/// candidate differs from the file, and what it does to the record.
struct Settings {
    original: Option<Vec<u8>>,
    write: Option<String>,
    state: StateChange,
    change: Option<crate::agents::StatusLineChange>,
}

impl Settings {
    fn plan(paths: &Paths) -> Result<Self> {
        let path = &paths.settings;
        let existing = settings_json::read_json_object(AGENT, path)?;
        let (mut candidate, displaced) = STATUS_LINE.install_candidate(path)?;
        if !existing.contains_key("statusLine")
            && let Some(Value::Object(statusline)) = candidate.get_mut("statusLine")
        {
            statusline.insert(STACK_WITH_DEFAULT.to_owned(), Value::Bool(true));
        }
        let write = (candidate != existing)
            .then(|| settings_json::render_json(AGENT, &candidate))
            .transpose()?;
        Ok(Self {
            original: settings_json::read_optional_bytes(AGENT, path)?,
            change: STATUS_LINE.classify(&existing),
            state: STATUS_LINE.state_change(path, &paths.state, displaced.as_ref())?,
            write,
        })
    }
}

fn installed_event_names() -> Vec<String> {
    ANTIGRAVITY_HOOKS
        .iter()
        .map(|entry| entry.hook.event.to_owned())
        .collect()
}

/// The global hooks file with RimZ's handlers removed, when it holds any.
fn stripped_global(path: &Path) -> Result<Option<Map<String, Value>>> {
    let mut root = settings_json::read_json_object(AGENT, path)?;
    Ok(strip_owned_hooks(&mut root).then_some(root))
}

fn holds_owned_hook(path: &Path) -> bool {
    settings_json::read_json_object(AGENT, path)
        .is_ok_and(|root| value_has_owned_hook(&Value::Object(root)))
}

fn value_has_owned_hook(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (key == "command"
                && value
                    .as_str()
                    .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER)))
                || value_has_owned_hook(value)
        }),
        Value::Array(values) => values.iter().any(value_has_owned_hook),
        _ => false,
    }
}

/// Removes RimZ-owned handlers, and each entry, event, or hook they leave
/// empty; a container the user left empty stays.
fn strip_owned_hooks(root: &mut Map<String, Value>) -> bool {
    let before = value_has_owned_hook(&Value::Object(root.clone()));
    root.retain(|_, hook| {
        let Some(events) = hook.as_object_mut() else {
            return true;
        };
        let event_count = events.len();
        events.retain(|_, entries| {
            let Some(entries) = entries.as_array_mut() else {
                return true;
            };
            let entry_count = entries.len();
            entries.retain_mut(|entry| {
                let Some(object) = entry.as_object_mut() else {
                    return true;
                };
                if object
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
                {
                    return false;
                }
                if let Some(hooks) = object.get_mut("hooks").and_then(Value::as_array_mut) {
                    let handler_count = hooks.len();
                    hooks.retain(|handler| {
                        !handler
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
                    });
                    if hooks.is_empty() && hooks.len() != handler_count {
                        return false;
                    }
                }
                true
            });
            !entries.is_empty() || entries.len() == entry_count
        });
        !events.is_empty() || events.len() == event_count
    });
    before
}
