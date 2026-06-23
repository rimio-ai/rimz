//! Per-machine settings, loaded from `~/.config/rimz/config.toml`,
//! `theme.toml`, and `agents.toml`. Agent and team fragments discovered under
//! `~/.agents/{agents,teams}` are the base layer for `agents.toml`, whose
//! entries take precedence on name clashes.
//!
//! This is the personal, never-committed tier. The project-committed tier is
//! `<root>/.rimz/config.toml`, parsed for the executable-surface hash in
//! [`crate::trust`]. Settings here are machine-wide preferences that tune how
//! Rimz drives *your* box or link *your* accounts, so they live outside the
//! repo and outside the trust hash — a clone never inherits them.
//!
//! A missing file is the default config, and unknown keys are ignored so an
//! older binary tolerates a newer file. Runtime entry points use
//! [`MachineConfig::load_lenient`], which degrades a broken file to built-in
//! defaults with a warning so a bad per-machine config does not block the room;
//! strict [`MachineConfig::load`] and [`MachineConfig::load_from`] back
//! `rimz config` and `rimz doctor`, which report the precise error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::ledger::paths::{self, config_home};

mod accounts;
mod agents;
mod animation;
mod attention;
mod color;
mod display;
pub mod effective;
mod glyphs;
mod harness;
mod loop_;
mod mux;
mod notifications;
mod pets;
mod remote_control;
mod resume;
mod sentry;
mod sidebar;
mod theme;
mod worktree;

pub use accounts::{AccountsConfig, UsageLimitUsd};
pub use agents::{
    AgentsConfig, CommandsConfig, LaunchPlacement, Profile, ProfilesConfig, RoleBinding, Team,
    TeamsConfig,
};
pub use animation::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationSpec, AnimationSpeed,
    ThemeAnimationsConfig, UnreadEffect, validate_glyph_cells, validate_single_cell,
};
pub use attention::AttentionConfig;
pub(crate) use color::xterm_rgb;
pub use color::{
    ColorDepth, PaletteRole, Semantic, ThemeColor, ThemeMode, nearest_xterm_index, parse_hex,
};
pub use display::{
    BudgetBarConfig, BudgetBurnRateConfig, CardDensityMode, ContextBand, ContextMeterConfig,
    DisplayConfig, GlowMode, ProviderTabsMode, ScrollbarMode,
};
pub use glyphs::{GlyphGroup, GlyphNamespaces, GlyphRole, ThemeGlyphsConfig, is_named_glyph_set};
pub use harness::{HarnessConfig, RtkMode};
pub use loop_::{LoopConfig, TaskEntry, TaskTarget, Tasks};
pub use mux::{
    MultiplexerConfig, TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderLines,
    TmuxPaneBorderStatus, TmuxSetClipboard, ZellijClipboard, ZellijConfig, ZellijForceClose,
};
pub use notifications::{
    DesktopNotificationMode, NotificationSoundMode, NotificationTrigger, NotificationsPrefs,
};
pub use pets::{PetsConfig, PetsGlyphMode, PetsSize};
pub use remote_control::RemoteControlConfig;
pub use resume::{DEFAULT_OVERLOAD_BACKOFF_SECS, ResumeConfig};
pub use sentry::SentryConfig;
pub use sidebar::SidebarConfig;
pub use theme::{
    InlineAnsiColors, InlinePalette, InlinePrimaryColors, InlineSelectionColors, ThemeConfig,
    ThemeProviderStyle, ThemeStyle,
};
pub use worktree::{WorktreeBase, WorktreeConfig};

const CONFIG_FILE: &str = "config.toml";
const THEME_FILE: &str = "theme.toml";
const AGENTS_FILE: &str = "agents.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";
const AGENTS_HOME_AGENTS_SUBDIR: &str = "agents";
const AGENTS_HOME_TEAMS_SUBDIR: &str = "teams";
const AGENT_FRAGMENT_FILE: &str = "agent.toml";
const TEAM_FRAGMENT_FILE: &str = "team.toml";
pub const MACHINE_CONFIG_TEMPLATE: &str = include_str!("config/templates/config.template.toml");
pub const MACHINE_THEME_TEMPLATE: &str = include_str!("config/templates/theme.template.toml");
pub const MACHINE_AGENTS_TEMPLATE: &str = include_str!("config/templates/agents.template.toml");

static LOAD_MEMO: OnceLock<Mutex<Option<(ConfigStamp, MachineConfig)>>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum ConfigErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing per-machine config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid per-machine agents config at {path}: {source}")]
    Agents {
        path: PathBuf,
        #[source]
        source: crate::agents_spec::LayoutErr,
    },
    #[error(
        "removed config table in {path}: {detail} (run `rimz config init --print` for the current shape)"
    )]
    RemovedTable { path: PathBuf, detail: String },
}

pub type Result<T> = std::result::Result<T, ConfigErr>;

/// Per-machine configuration. Lenient on unknown keys so a newer config never
/// breaks an older binary, and every field defaults so the smallest useful file
/// is a single section.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MachineConfig {
    pub accounts: AccountsConfig,
    pub remote_control: RemoteControlConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
    pub harness: HarnessConfig,
    pub sentry: SentryConfig,
    #[serde(skip_serializing_if = "ThemeConfig::is_unset")]
    pub theme: ThemeConfig,
    pub agents: AgentsConfig,
}

impl MachineConfig {
    /// The generated core per-machine config reference.
    pub fn template_core() -> &'static str {
        MACHINE_CONFIG_TEMPLATE
    }

    /// The generated theme per-machine config reference.
    pub fn template_theme() -> &'static str {
        MACHINE_THEME_TEMPLATE
    }

    /// The generated agents per-machine config reference.
    pub fn template_agents() -> &'static str {
        MACHINE_AGENTS_TEMPLATE
    }

    /// The core per-machine config path: `$XDG_CONFIG_HOME/rimz/config.toml`.
    pub fn config_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(CONFIG_FILE)
    }

    /// The theme per-machine config path: `$XDG_CONFIG_HOME/rimz/theme.toml`.
    pub fn theme_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(THEME_FILE)
    }

    /// The agents per-machine config path: `$XDG_CONFIG_HOME/rimz/agents.toml`.
    pub fn agents_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(AGENTS_FILE)
    }

    /// Load from the default per-machine paths. Missing files are defaults —
    /// never an error.
    pub fn load() -> Result<Self> {
        Self::load_with_memo(&Self::config_path(), &paths::agents_home())
    }

    /// Load per-machine config for a runtime entry point. A file that fails to
    /// load degrades to its built-in defaults with a warning instead of
    /// aborting the room; the strict [`Self::load`] and [`Self::load_from`]
    /// report the precise error for `rimz config` and `rimz doctor`.
    pub fn load_lenient() -> Self {
        let mut config = Self::load_lenient_from(&Self::config_path());
        if let Err(err) = apply_agents_home(
            &mut config.agents,
            &paths::agents_home(),
            &Self::agents_path(),
        ) {
            tracing::warn!(
                error = %err,
                "~/.agents discovery failed; using per-machine agents config only",
            );
        }
        config
    }

    /// Load from an explicit config.toml path and its sibling theme.toml and
    /// agents.toml files — the test and tooling seam.
    pub fn load_from(config_path: &Path) -> Result<Self> {
        let dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let theme_path = dir.join(THEME_FILE);
        let agents_path = dir.join(AGENTS_FILE);

        let core = load_optional(config_path, parse_core_text)?.unwrap_or_default();
        let theme = load_optional(&theme_path, parse_theme_text)?.unwrap_or_default();
        let agents = load_optional(&agents_path, parse_agents_text)?.unwrap_or_default();

        let mut config = Self::assemble(core, theme, agents);
        validate_agents_config(&mut config.agents, &agents_path)?;
        Ok(config)
    }

    fn load_lenient_from(config_path: &Path) -> Self {
        let dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let theme_path = dir.join(THEME_FILE);
        let agents_path = dir.join(AGENTS_FILE);

        let core = recover(load_optional(config_path, parse_core_text)).unwrap_or_default();
        let theme = recover(load_optional(&theme_path, parse_theme_text)).unwrap_or_default();
        let agents = recover(load_optional(&agents_path, parse_agents_text)).unwrap_or_default();

        let mut config = Self::assemble(core, theme, agents);
        if let Err(err) = validate_agents_config(&mut config.agents, &agents_path) {
            tracing::warn!(
                error = %err,
                "per-machine agents config invalid; using built-in defaults",
            );
            config.agents = AgentsConfig::default();
        }
        config
    }

    pub fn parse_text(path: &Path, text: &str) -> Result<Self> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => Ok(Self::assemble(
                CoreConfig::default(),
                parse_theme_text(path, text)?,
                AgentsConfig::default(),
            )),
            Some(AGENTS_FILE) => {
                let mut agents = parse_agents_text(path, text)?;
                validate_agents_config(&mut agents, path)?;
                Ok(Self::assemble(
                    CoreConfig::default(),
                    ThemeConfig::default(),
                    agents,
                ))
            }
            _ => {
                let core = parse_core_text(path, text)?;
                Ok(Self::assemble(
                    core,
                    ThemeConfig::default(),
                    AgentsConfig::default(),
                ))
            }
        }
    }

    fn assemble(core: CoreConfig, theme: ThemeConfig, agents: AgentsConfig) -> Self {
        Self {
            accounts: core.accounts,
            remote_control: core.remote_control,
            notifications: core.notifications,
            sidebar: core.sidebar,
            zellij: core.zellij,
            tmux: core.tmux,
            resume: core.resume,
            harness: core.harness,
            sentry: core.sentry,
            theme,
            agents,
        }
    }

    fn load_with_memo(config_path: &Path, agents_home: &Path) -> Result<Self> {
        let stamp = ConfigStamp::from_inputs(config_path, agents_home)?;
        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some((cached_stamp, cached)) = memo.as_ref()
            && cached_stamp == &stamp
        {
            return Ok(cached.clone());
        }

        let mut config = Self::load_from(config_path)?;
        let agents_path = sibling_path(config_path, AGENTS_FILE);
        apply_agents_home(&mut config.agents, agents_home, &agents_path)?;
        if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock() {
            *memo = Some((stamp, config.clone()));
        }
        Ok(config)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigStamp {
    core: StampedPath,
    theme: StampedPath,
    agents: StampedPath,
    fragments: Vec<StampedPath>,
}

impl ConfigStamp {
    fn from_inputs(config_path: &Path, agents_home: &Path) -> Result<Self> {
        let mut fragments = Vec::new();
        collect_agents_home_fragment_stamps(
            agents_home,
            AGENTS_HOME_AGENTS_SUBDIR,
            AGENT_FRAGMENT_FILE,
            &mut fragments,
        )?;
        collect_agents_home_fragment_stamps(
            agents_home,
            AGENTS_HOME_TEAMS_SUBDIR,
            TEAM_FRAGMENT_FILE,
            &mut fragments,
        )?;
        fragments.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self {
            core: stamped_path(config_path),
            theme: stamped_path(&sibling_path(config_path, THEME_FILE)),
            agents: stamped_path(&sibling_path(config_path, AGENTS_FILE)),
            fragments,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StampedPath {
    path: PathBuf,
    stamp: FileStamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

fn sibling_path(path: &Path, file: &str) -> PathBuf {
    path.parent().unwrap_or_else(|| Path::new(".")).join(file)
}

fn stamped_path(path: &Path) -> StampedPath {
    StampedPath {
        path: path.to_path_buf(),
        stamp: file_stamp(path),
    }
}

fn file_stamp(path: &Path) -> FileStamp {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileStamp {
            len: 0,
            modified_secs: 0,
            modified_nanos: 0,
        };
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
    FileStamp {
        len: meta.len(),
        modified_secs: modified.as_ref().map_or(0, |duration| duration.as_secs()),
        modified_nanos: modified.map_or(0, |duration| duration.subsec_nanos()),
    }
}

fn collect_agents_home_fragment_stamps(
    root: &Path,
    subdir: &str,
    fragment_file: &str,
    out: &mut Vec<StampedPath>,
) -> Result<()> {
    for dir in child_dirs(&root.join(subdir))? {
        out.push(stamped_path(&dir.join(fragment_file)));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CoreConfig {
    accounts: AccountsConfig,
    remote_control: RemoteControlConfig,
    notifications: NotificationsPrefs,
    sidebar: SidebarConfig,
    zellij: ZellijConfig,
    tmux: TmuxConfig,
    resume: ResumeConfig,
    harness: HarnessConfig,
    sentry: SentryConfig,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    theme: ThemeConfig,
    colors: Option<InlinePalette>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AgentsFile {
    agents: AgentsConfig,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AgentsFragmentFile {
    agents: AgentsFragment,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AgentsFragment {
    profiles: ProfilesConfig,
    teams: TeamsConfig,
    commands: CommandsConfig,
}

fn load_optional<T>(path: &Path, parse: fn(&Path, &str) -> Result<T>) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn recover<T>(result: Result<Option<T>>) -> Option<T> {
    match result {
        Ok(opt) => opt,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "per-machine config unreadable; using built-in defaults for this file",
            );
            None
        }
    }
}

fn parse_core_text(path: &Path, text: &str) -> Result<CoreConfig> {
    toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_theme_text(path: &Path, text: &str) -> Result<ThemeConfig> {
    let mut file: ThemeFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    file.theme.colors = file.colors;
    Ok(file.theme)
}

fn parse_agents_text(path: &Path, text: &str) -> Result<AgentsConfig> {
    check_removed_agents_tables(path, text)?;
    let file: AgentsFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file.agents)
}

/// Tables the `[agents]` redesign removed. Serde tolerates unknown keys so a
/// newer config never breaks an older binary, but a *renamed* table is not a
/// forward-compatible unknown — silently dropping it would launch a surface the
/// user never declared. Fail fast naming the rename instead. A genuine syntax
/// error is left to the typed parse to report.
fn check_removed_agents_tables(path: &Path, text: &str) -> Result<()> {
    let Ok(doc) = toml::from_str::<toml::Table>(text) else {
        return Ok(());
    };
    let removed = |detail: &str| ConfigErr::RemovedTable {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    };
    if doc.contains_key("tab") {
        return Err(removed(
            "`[tab]` (with `[tab.keywords]`/`[tab.layouts]`) was removed — set `placement` under `[agents]` and declare layouts as `[agents.teams]`",
        ));
    }
    if let Some(agents) = doc.get("agents").and_then(toml::Value::as_table) {
        if agents.contains_key("aliases") {
            return Err(removed(
                "`[agents.aliases]` was split into `[agents.profiles]` (agent presets) and `[agents.commands]` (raw command panes)",
            ));
        }
        if agents.contains_key("layouts") {
            return Err(removed(
                "`[agents.layouts]` was renamed to `[agents.teams]`",
            ));
        }
    }
    Ok(())
}

fn parse_agents_fragment_text(path: &Path, text: &str) -> Result<AgentsFragment> {
    let file: AgentsFragmentFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file.agents)
}

fn discover_agents_home(root: &Path) -> Result<AgentsFragment> {
    let mut fragment = AgentsFragment::default();
    discover_agents_home_subdir(
        root,
        AGENTS_HOME_AGENTS_SUBDIR,
        AGENT_FRAGMENT_FILE,
        &mut fragment,
    )?;
    discover_agents_home_subdir(
        root,
        AGENTS_HOME_TEAMS_SUBDIR,
        TEAM_FRAGMENT_FILE,
        &mut fragment,
    )?;
    Ok(fragment)
}

fn discover_agents_home_subdir(
    root: &Path,
    subdir: &str,
    fragment_file: &str,
    out: &mut AgentsFragment,
) -> Result<()> {
    let mut dirs = child_dirs(&root.join(subdir))?;
    dirs.sort();
    for dir in dirs {
        let path = dir.join(fragment_file);
        let Some(mut fragment) = load_optional(&path, parse_agents_fragment_text)? else {
            continue;
        };
        crate::agents_spec::resolve_profile_prompt_paths(&mut fragment.profiles, &dir);
        crate::agents_spec::resolve_team_prompt_paths(&mut fragment.teams, &dir);
        out.profiles.0.extend(fragment.profiles.0);
        out.teams.0.extend(fragment.teams.0);
        out.commands.0.extend(fragment.commands.0);
    }
    Ok(())
}

fn child_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ConfigErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();
        let file_type = entry.file_type().map_err(|source| ConfigErr::Io {
            path: entry_path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            dirs.push(entry_path);
        }
    }
    Ok(dirs)
}

fn apply_agents_home(agents: &mut AgentsConfig, root: &Path, agents_path: &Path) -> Result<()> {
    let mut merged = agents.clone();
    let fragment = discover_agents_home(root)?;
    overlay_under(&mut merged.profiles.0, fragment.profiles.0);
    overlay_under(&mut merged.teams.0, fragment.teams.0);
    overlay_under(&mut merged.commands.0, fragment.commands.0);
    validate_agents_config(&mut merged, agents_path)?;
    *agents = merged;
    Ok(())
}

fn overlay_under<V>(file: &mut BTreeMap<String, V>, fragment: BTreeMap<String, V>) {
    for (key, value) in fragment {
        file.entry(key).or_insert(value);
    }
}

fn validate_agents_config(agents: &mut AgentsConfig, path: &Path) -> Result<()> {
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    crate::agents_spec::resolve_profile_prompt_paths(&mut agents.profiles, config_dir);
    crate::agents_spec::resolve_team_prompt_paths(&mut agents.teams, config_dir);
    crate::agents_spec::validate_config(&agents.profiles, &agents.commands, &agents.teams).map_err(
        |source| ConfigErr::Agents {
            path: path.to_path_buf(),
            source,
        },
    )
}

#[cfg(test)]
#[path = "config/template_tests.rs"]
mod template_tests;

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
