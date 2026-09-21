//! Per-machine settings, loaded from `~/.rimz/config.toml`, `theme.toml`, and `loop.toml`. [`MachineConfigFiles`] is the ordered file registry, and [`ConfigEditor`] provides strict effective reads plus comment-preserving writes and template merges. This module also owns selectable theme-scheme lookup and validation.
//!
//! Markdown definitions under the agents home populate profiles and teams at every config load; config.toml owns machine launch preferences.
//!
//! This is the personal, never-committed tier. The project-committed tier is
//! `<root>/.rimz/config.toml`, parsed for the executable-surface hash in
//! [`crate::trust`]. Settings here are machine-wide preferences that tune how
//! RimZ drives *your* box or link *your* accounts, so they live outside the
//! repo and outside the trust hash — a clone never inherits them.
//!
//! A missing file is the default config, and unknown keys are ignored with a
//! visible warning so an older binary tolerates a newer file. Runtime entry
//! points use [`MachineConfig::load_lenient`], which degrades a broken machine
//! file to built-in defaults. A broken Markdown definition drops only that definition from read-only views and blocks launches with its source error.
//! Strict [`MachineConfig::load`] backs config inspection and reports precise errors.

use std::collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::disk::parse_cache::StampedPath;
use crate::disk::paths::{self, rimz_home};

mod accounts;
mod agents;
mod animation;
mod attention;
mod color;
mod daemon;
pub mod definitions;
mod diagnosis;
mod display;
mod edit;
pub mod effective;
mod gc;
mod glyphs;
mod harness;
mod loop_;
mod mux;
mod notifications;
mod pets;
mod remote_control;
mod resume;
mod scheme;
mod sentry;
mod sidebar;
mod skills;
mod theme;
mod web;
mod worktree;

#[cfg(test)]
pub(crate) use accounts::UsageLimitUsd;
pub use accounts::{AccountBudgetConfigError, AccountsConfig, NamedAccount};
use agents::{AgentsConfig, SubagentProfilesConfig};
pub use agents::{
    CommandsConfig, DONE_STAGE, Isolation, LaunchPlacement, Profile, ProfilesConfig, PromptSource,
    RoleBinding, SubagentsConfig, Team, TeamSignalBinding, TeamsConfig,
};
pub(crate) use agents::{FlipCompact, retired_agents_key};
use animation::validate_glyph_cells;
pub(crate) use animation::{
    AnimationColor, AnimationEffect, AnimationRole, AnimationSpec, AnimationSpeed,
    ThemeAnimationsConfig, UnreadEffect,
};
pub use attention::AttentionConfig;
pub use color::{ColorDepth, ThemeColor, ThemeMode};
pub(crate) use color::{PaletteRole, Semantic, nearest_xterm_index, parse_hex, xterm_rgb};
pub(crate) use daemon::{DaemonConfig, DaemonPane};
pub use diagnosis::ConfigFileDiagnosis;
pub(crate) use display::{
    BudgetBarConfig, BudgetBurnRateConfig, CardDensityMode, ContextBand, DisplayConfig,
    HighlightStepsConfig, ScrollbarMode,
};
pub use display::{ContextMeterConfig, PixelMode, ProviderTabsMode};
pub use edit::{ConfigEditor, MergeAction, MergeReport};
pub(crate) use gc::GcConfig;
pub use gc::parse_older_than;
pub use glyphs::{GlyphRole, ThemeGlyphsConfig};
use glyphs::{is_named_glyph_set, validate_glyph_source};
pub use harness::{CompactSeat, DayCap, HarnessConfig, IdleCompactMode};
use loop_::TaskBudgetError;
pub(crate) use loop_::WaitMeta;
pub use loop_::{CheckOn, FileMark, LoopConfig, TaskEntry, TaskTarget, Tasks, WatchSpec};
pub use mux::MultiplexerConfig;
use mux::MuxConfig;
pub(crate) use mux::{TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderStatus, ZellijConfig};
#[cfg(test)]
pub(crate) use mux::{TmuxPaneBorderLines, ZellijClipboard, ZellijForceClose};
use notifications::NotificationsConfigErr;
pub(crate) use notifications::{
    DesktopNotificationMode, NotificationSoundMode, NotifyConditionAgent, RenderMode, TemplateVars,
    render_template,
};
pub use notifications::{NotificationKind, NotificationsPrefs};
#[cfg(test)]
pub(crate) use notifications::{NotificationTrigger, NotifyCondition, NotifyHandler};
pub(crate) use pets::PetsGlyphMode;
pub use pets::{CellAspect, PetsConfig};
pub(crate) use remote_control::RemoteControlConfig;
pub(crate) use resume::DEFAULT_AUTO_CONTINUE_BACKOFF_SECS;
pub use resume::ResumeConfig;
use resume::parse_auto_redeem_min_gain;
#[cfg(test)]
pub(crate) use scheme::parse_scheme_text;
pub(crate) use scheme::{
    DEFAULT_SCHEME, ParsedScheme, explicit_scheme, parse_colors, resolve_inline_palette,
};
pub use scheme::{SchemeSwatch, scheme_swatches};
use sentry::SentryConfig;
pub use sidebar::SidebarConfig;
pub(crate) use sidebar::SidebarKeys;
pub use skills::SkillName;
pub(crate) use skills::{SkillListErr, deserialize_optional_skill_list, validate_skill_list};
pub(crate) use theme::InlinePalette;
#[cfg(test)]
pub(crate) use theme::{InlineAnsiColors, InlinePrimaryColors};
pub use theme::{ThemeConfig, ThemeProviderStyle, ThemeStyle};
use web::WebPrefs;
pub use worktree::{WorktreeBase, WorktreeConfig};

/// Default render base grid: 100ms, or 10Hz.
const DEFAULT_REFRESH_MS: u16 = 100;

/// Minimum accepted render base grid. Prevents accidental busy-spins from
/// config typos while leaving room for faster test or local tuning.
const MIN_REFRESH_MS: u16 = 16;

/// Maximum accepted render base grid. Higher values make input and overlay
/// event latency visibly worse, so keep slow data polling on `--tick-seconds`.
const MAX_REFRESH_MS: u16 = 1_000;

const CONFIG_FILE: &str = "config.toml";
const THEME_FILE: &str = "theme.toml";
const LOOP_FILE: &str = "loop.toml";
const MACHINE_CONFIG_TEMPLATE: &str = include_str!("config/templates/config.template.toml");
const MACHINE_THEME_TEMPLATE: &str = include_str!("config/templates/theme.template.toml");
const MACHINE_LOOP_TEMPLATE: &str = include_str!("config/templates/loop.template.toml");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MachineConfigFileKind {
    Core,
    Theme,
    Loop,
}

impl MachineConfigFileKind {
    const ALL: [Self; 3] = [Self::Core, Self::Theme, Self::Loop];

    fn file_name(self) -> &'static str {
        match self {
            Self::Core => CONFIG_FILE,
            Self::Theme => THEME_FILE,
            Self::Loop => LOOP_FILE,
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Core => MACHINE_CONFIG_TEMPLATE,
            Self::Theme => MACHINE_THEME_TEMPLATE,
            Self::Loop => MACHINE_LOOP_TEMPLATE,
        }
    }
}

/// One file in the ordered per-machine config set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineConfigFile {
    path: PathBuf,
    template: &'static str,
}

impl MachineConfigFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn template(&self) -> &'static str {
        self.template
    }
}

/// Canonical paths and templates for the three per-machine config files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineConfigFiles {
    core_path: PathBuf,
    agents_home: PathBuf,
}

impl MachineConfigFiles {
    /// Resolve the current machine's config roots.
    fn machine() -> Self {
        Self::from_paths(rimz_home().join(CONFIG_FILE), paths::agents_home())
    }

    /// Build an explicit config set for tests and tooling.
    fn from_paths(core_path: impl Into<PathBuf>, agents_home: impl Into<PathBuf>) -> Self {
        Self {
            core_path: core_path.into(),
            agents_home: agents_home.into(),
        }
    }

    pub fn core_path(&self) -> &Path {
        &self.core_path
    }

    fn agents_home(&self) -> &Path {
        &self.agents_home
    }

    /// Files in persistence and display order: core, theme, loop.
    pub fn ordered(&self) -> [MachineConfigFile; 3] {
        MachineConfigFileKind::ALL.map(|kind| MachineConfigFile {
            path: self.path(kind),
            template: kind.template(),
        })
    }

    fn path(&self, kind: MachineConfigFileKind) -> PathBuf {
        if kind == MachineConfigFileKind::Core {
            return self.core_path.clone();
        }
        self.core_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(kind.file_name())
    }

    fn file(&self, kind: MachineConfigFileKind) -> MachineConfigFile {
        MachineConfigFile {
            path: self.path(kind),
            template: kind.template(),
        }
    }
}

const CONFIG_STAMP_TTL: Duration = Duration::from_secs(2);
/// Re-reads allowed before a config load stops chasing an in-place rewrite and
/// holds last-known-good.
const STABLE_READ_ATTEMPTS: u8 = 3;
// ponytail: mtime quiescence; require atomic writes if config gains a RimZ writer.
const STABLE_READ_QUIET: Duration = Duration::from_millis(50);

static LOAD_MEMO: OnceLock<Mutex<Option<LoadMemo>>> = OnceLock::new();

#[derive(Debug)]
struct LoadMemo {
    stamp: ConfigStamp,
    config: Arc<MachineConfig>,
    last_verified: Instant,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigErr {
    #[error("{path}: {message}")]
    Definition { path: PathBuf, message: String },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot load {path} — the file has a TOML error")]
    Parse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
    },
    #[error("invalid per-machine agents config at {path}: {source}")]
    Agents {
        path: PathBuf,
        #[source]
        source: crate::harness::spec::LayoutErr,
    },
    #[error("invalid per-machine notifications config at {path}: {source}")]
    Notifications {
        path: PathBuf,
        #[source]
        source: NotificationsConfigErr,
    },
    #[error("invalid per-machine loop config at {path}: {source}")]
    Loop {
        path: PathBuf,
        #[source]
        source: TaskBudgetError,
    },
    #[error("invalid per-machine account budget at {path}: {source}")]
    AccountBudget {
        path: PathBuf,
        #[source]
        source: AccountBudgetConfigError,
    },
    #[error("invalid per-machine account at {path}: {source}")]
    Account {
        path: PathBuf,
        #[source]
        source: Box<crate::agents::LoginConfigErr>,
    },
    #[error(
        "removed config table in {path}: {detail} (run `rimz config init --print` for the current shape)"
    )]
    RemovedTable { path: PathBuf, detail: String },
    #[error("removed config key in {path}: {detail}")]
    RemovedKey { path: PathBuf, detail: String },
}

impl ConfigErr {
    /// The per-machine file that failed to load.
    pub fn path(&self) -> &Path {
        match self {
            Self::Io { path, .. }
            | Self::Parse { path, .. }
            | Self::Definition { path, .. }
            | Self::Agents { path, .. }
            | Self::Notifications { path, .. }
            | Self::Loop { path, .. }
            | Self::AccountBudget { path, .. }
            | Self::Account { path, .. }
            | Self::RemovedTable { path, .. }
            | Self::RemovedKey { path, .. } => path,
        }
    }

    /// The validation failure without file/location context, for callers
    /// reporting a value error rather than a broken file.
    fn validation_message(&self) -> String {
        match self {
            Self::Parse { diagnosis, .. } => diagnosis.raw_message().to_owned(),
            Self::Definition { message, .. } => message.clone(),
            Self::Agents { source, .. } => source.to_string(),
            Self::Notifications { source, .. } => source.to_string(),
            Self::Loop { source, .. } => source.to_string(),
            Self::AccountBudget { source, .. } => source.to_string(),
            Self::Account { source, .. } => source.to_string(),
            Self::Io { .. } | Self::RemovedTable { .. } | Self::RemovedKey { .. } => {
                self.to_string()
            }
        }
    }

    /// The classified TOML failure, when this error came from parsing a file.
    pub fn diagnosis(&self) -> Option<&ConfigFileDiagnosis> {
        match self {
            Self::Parse { diagnosis, .. } => Some(diagnosis),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ConfigErr>;

/// Non-fatal configuration findings retained for user-facing entry points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigNotices {
    pub unknown_keys: Vec<UnknownConfigKey>,
    pub definition_errors: Vec<DefinitionError>,
    pub failed_definitions: BTreeMap<String, BTreeSet<PathBuf>>,
}

/// A key ignored while loading a per-machine config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownConfigKey {
    pub path: PathBuf,
    pub key: String,
}

/// A Markdown definition that the lenient loader could not use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionError {
    pub path: PathBuf,
    pub message: String,
}

/// Definition files for the effective configured agent-spec catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSpecSources {
    agent_profiles: BTreeMap<String, PathBuf>,
    subagent_profiles: BTreeMap<String, PathBuf>,
    teams: BTreeMap<String, PathBuf>,
    commands: BTreeMap<String, PathBuf>,
}

impl AgentSpecSources {
    pub fn team(&self, name: &str) -> Option<&Path> {
        self.teams.get(name).map(PathBuf::as_path)
    }

    pub fn profile(&self, scope: effective::ProfileScope, name: &str) -> Option<&Path> {
        match scope {
            effective::ProfileScope::Agents => self.agent_profiles.get(name),
            effective::ProfileScope::Subagents => self.subagent_profiles.get(name),
        }
        .map(PathBuf::as_path)
    }

    pub fn command(&self, name: &str) -> Option<&Path> {
        self.commands.get(name).map(PathBuf::as_path)
    }
}

impl ConfigNotices {
    fn add_unknown_keys(&mut self, path: &Path, keys: Vec<String>) {
        self.unknown_keys
            .extend(keys.into_iter().map(|key| UnknownConfigKey {
                path: path.to_path_buf(),
                key,
            }));
    }
}

/// Per-machine configuration. Lenient on unknown keys so a newer config never
/// breaks an older binary, and every field defaults so the smallest useful file
/// is a single section.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MachineConfig {
    /// IANA time zone for displayed times and scheduling. Unset or unknown
    /// falls back to the system zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub mux: MuxConfig,
    pub accounts: AccountsConfig,
    pub remote_control: RemoteControlConfig,
    pub daemon: DaemonConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
    pub harness: HarnessConfig,
    pub gc: GcConfig,
    pub sentry: SentryConfig,
    pub web: WebPrefs,
    #[serde(skip_serializing_if = "ThemeConfig::is_unset")]
    pub theme: ThemeConfig,
    pub agents: AgentsConfig,
    pub subagents: SubagentProfilesConfig,
    #[serde(default, skip_serializing_if = "LoopConfig::is_empty")]
    pub r#loop: LoopConfig,
    #[serde(skip)]
    pub notices: ConfigNotices,
}

impl MachineConfig {
    /// The generated loop per-machine config reference.
    pub fn template_loop() -> &'static str {
        MachineConfigFileKind::Loop.template()
    }

    /// The core per-machine config path: `$RIMZ_HOME/config.toml`.
    pub fn config_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Core)
    }

    /// The loop per-machine config path: `$RIMZ_HOME/loop.toml`.
    pub fn loop_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Loop)
    }

    /// The theme per-machine config path: `$RIMZ_HOME/theme.toml`.
    pub fn theme_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Theme)
    }

    /// Load from the default per-machine paths with strict TOML validation and no memo. Missing files are defaults; broken Markdown definitions are retained in notices alongside the good definitions.
    pub fn load() -> Result<Self> {
        Self::load_with_agent_spec_sources().map(|(config, _)| config)
    }

    /// Load the strict per-machine config together with the declaring file for
    /// every configured agent profile and command.
    pub fn load_with_agent_spec_sources() -> Result<(Self, AgentSpecSources)> {
        let files = MachineConfigFiles::machine();
        Self::load_from_with_agent_spec_sources(
            files.core_path(),
            files.agents_home(),
            &crate::agents::ambient_env(),
        )
    }

    /// Strictly load only the per-machine loop task file. Missing file is the
    /// default loop config.
    pub fn load_loop() -> Result<LoopConfig> {
        let files = MachineConfigFiles::machine();
        load_optional(&files.path(MachineConfigFileKind::Loop), parse_loop_text)
            .map(|loop_| loop_.unwrap_or_default())
    }

    /// Load memoized per-machine config for a runtime entry point. Unlike [`Self::load`], broken TOML degrades to built-in defaults with a warning; both retain Markdown definition failures in notices alongside the good definitions.
    pub fn load_lenient() -> Arc<Self> {
        let files = MachineConfigFiles::machine();
        Self::load_lenient_with_memo(files.core_path(), files.agents_home())
    }

    /// Load machine files and Markdown definitions from explicit roots for tests and tooling.
    fn load_from(config_path: &Path, agents_home: &Path) -> Result<Self> {
        Self::load_from_with_agent_spec_sources(
            config_path,
            agents_home,
            &crate::agents::ambient_env(),
        )
        .map(|(config, _)| config)
    }

    /// `env` resolves provider skill roots for the sandbox skill check.
    fn load_from_with_agent_spec_sources(
        config_path: &Path,
        agents_home: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<(Self, AgentSpecSources)> {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let theme_path = files.path(MachineConfigFileKind::Theme);
        let loop_path = files.path(MachineConfigFileKind::Loop);

        let core = load_parsed_optional(files.core_path(), parse_core_text_collecting)?;
        validate_account_budgets(&core.value.accounts, files.core_path())?;
        let theme = load_parsed_optional(&theme_path, parse_theme_text_collecting)?;
        let loop_ = load_parsed_optional(&loop_path, parse_loop_text_collecting)?;

        let mut notices = ConfigNotices::default();
        notices.add_unknown_keys(files.core_path(), core.unknown_keys);
        notices.add_unknown_keys(&theme_path, theme.unknown_keys);
        notices.add_unknown_keys(&loop_path, loop_.unknown_keys);
        let mut config = Self::assemble(core.value, theme.value, loop_.value);
        validate_notifications_config(&config.notifications, files.core_path())?;
        let sources = config.load_definitions(agents_home, config_path, env, &mut notices);
        config.notices = notices;
        Ok((config, sources))
    }

    fn load_lenient_from(
        config_path: &Path,
        agents_home: &Path,
        env: &BTreeMap<String, String>,
    ) -> Self {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let theme_path = files.path(MachineConfigFileKind::Theme);
        let loop_path = files.path(MachineConfigFileKind::Loop);

        let core = recover_parsed(files.core_path(), parse_core_text_collecting);
        let theme = recover_parsed(&theme_path, parse_theme_text_collecting);
        let loop_ = recover_parsed(&loop_path, parse_loop_text_collecting);

        let mut notices = ConfigNotices::default();
        notices.add_unknown_keys(files.core_path(), core.unknown_keys);
        notices.add_unknown_keys(&theme_path, theme.unknown_keys);
        notices.add_unknown_keys(&loop_path, loop_.unknown_keys);
        let mut config = Self::assemble(core.value, theme.value, loop_.value);
        if let Err(err) = validate_notifications_config(&config.notifications, files.core_path()) {
            tracing::warn!(
                error = %err,
                "per-machine notifications config invalid; using built-in defaults",
            );
            config.notifications = NotificationsPrefs::default();
        }
        config.load_definitions(agents_home, config_path, env, &mut notices);
        config.notices = notices;
        config
    }

    pub fn parse_text(path: &Path, text: &str, agents_home: &Path) -> Result<Self> {
        Self::parse_text_with_agents_home(
            path,
            text,
            agents_home,
            false,
            &crate::agents::ambient_env(),
        )
    }

    fn parse_text_for_edit(path: &Path, text: &str, agents_home: &Path) -> Result<Self> {
        Self::parse_text_with_agents_home(
            path,
            text,
            agents_home,
            true,
            &crate::agents::ambient_env(),
        )
    }

    fn parse_text_with_agents_home(
        path: &Path,
        text: &str,
        agents_home: &Path,
        ignore_broken_definitions: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => Ok(Self::assemble(
                CoreConfig::default(),
                parse_theme_text(path, text)?,
                LoopConfig::default(),
            )),
            Some(LOOP_FILE) => Ok(Self::assemble(
                CoreConfig::default(),
                ThemeConfig::default(),
                parse_loop_text(path, text)?,
            )),
            _ => {
                let core = parse_core_text(path, text)?;
                validate_notifications_config(&core.notifications, path)?;
                validate_account_budgets(&core.accounts, path)?;
                let mut config =
                    Self::assemble(core, ThemeConfig::default(), LoopConfig::default());
                let mut notices = ConfigNotices::default();
                config.load_definitions(agents_home, path, env, &mut notices);
                if ignore_broken_definitions {
                    // An edit judges config.toml's own keys; the definition set's
                    // failures surface at launch and must not lock the editor out.
                    let own_keys = AgentsConfig {
                        profiles: ProfilesConfig::default(),
                        teams: TeamsConfig::default(),
                        ..config.agents.clone()
                    };
                    validate_agents_file(&own_keys, &SubagentProfilesConfig::default(), path)?;
                }
                config.notices = notices;
                Ok(config)
            }
        }
    }

    /// Parse one per-machine config file's text and return the key paths serde
    /// ignored, dotted, in the file's own table coordinates.
    fn parse_text_unknown_keys(path: &Path, text: &str) -> Result<Vec<String>> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => parse_unknown_keys::<ThemeFile>(path, text),
            Some(LOOP_FILE) => parse_unknown_keys::<LoopConfig>(path, text),
            _ => parse_unknown_keys::<CoreConfig>(path, text),
        }
    }

    fn assemble(core: CoreConfig, theme: ThemeConfig, loop_: LoopConfig) -> Self {
        Self {
            timezone: core.timezone,
            mux: core.mux,
            accounts: core.accounts,
            remote_control: core.remote_control,
            daemon: core.daemon,
            notifications: core.notifications,
            sidebar: core.sidebar,
            zellij: core.zellij,
            tmux: core.tmux,
            resume: core.resume,
            harness: core.harness,
            gc: core.gc,
            sentry: core.sentry,
            web: core.web,
            theme,
            agents: core.agents,
            subagents: core.subagents,
            r#loop: loop_,
            notices: ConfigNotices::default(),
        }
    }

    fn load_definitions(
        &mut self,
        agents_home: &Path,
        config_path: &Path,
        env: &BTreeMap<String, String>,
        notices: &mut ConfigNotices,
    ) -> AgentSpecSources {
        let library = agents_home.join("skills");
        let check = if self.agents.isolation == Isolation::Sandbox
            && Isolation::ambient(env) != Some(Isolation::Sandbox)
        {
            definitions::SkillCheck::Check {
                env,
                library: &library,
            }
        } else {
            definitions::SkillCheck::Skip
        };
        let loaded = definitions::load(agents_home, check, &self.agents.commands);
        self.agents.profiles = loaded.agent_profiles;
        self.subagents.profiles = loaded.subagent_profiles;
        self.agents.teams.0.extend(loaded.teams.0);
        notices.failed_definitions = loaded.failed;
        notices
            .definition_errors
            .extend(loaded.errors.into_iter().map(|error| DefinitionError {
                path: error.path,
                message: error.message,
            }));
        if let Err(error) = validate_agents_file(&self.agents, &self.subagents, agents_home) {
            notices.definition_errors.push(DefinitionError {
                path: agents_home.to_path_buf(),
                message: error.validation_message(),
            });
        }
        let mut sources = loaded.sources;
        sources.commands.extend(
            self.agents
                .commands
                .0
                .keys()
                .map(|name| (name.clone(), config_path.to_path_buf())),
        );
        sources
    }

    pub fn time_zone(&self) -> jiff::tz::TimeZone {
        resolve_time_zone(self.timezone.as_deref())
    }

    /// Failures for one unloaded definition name, including each source path.
    pub fn definition_failure_for(&self, name: &str) -> Option<String> {
        let paths = self.notices.failed_definitions.get(name)?;
        Some(
            self.notices
                .definition_errors
                .iter()
                .filter(|notice| paths.contains(&notice.path))
                .map(|notice| format!("{}: {}", notice.path.display(), notice.message))
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }

    /// Failures no definition name owns (a namespace collision, an unreadable tree): the set itself is inconsistent, so every launch refuses.
    fn unattributed_definition_failure(&self) -> Option<String> {
        let owned: std::collections::BTreeSet<&PathBuf> =
            self.notices.failed_definitions.values().flatten().collect();
        let lines: Vec<String> = self
            .notices
            .definition_errors
            .iter()
            .filter(|notice| !owned.contains(&notice.path))
            .map(|notice| format!("{}: {}", notice.path.display(), notice.message))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n\n"))
    }

    pub fn headline_spec(&self) -> crate::agents::spending::HeadlineSpec {
        crate::agents::spending::HeadlineSpec {
            mode: self.sidebar.spend_window,
            timezone: self.timezone.clone(),
        }
    }

    #[cfg(test)]
    fn load_with_memo(config_path: &Path, agents_home: &Path) -> Self {
        Self::load_lenient_with_memo(config_path, agents_home)
            .as_ref()
            .clone()
    }

    fn load_lenient_with_memo(config_path: &Path, agents_home: &Path) -> Arc<Self> {
        let now = Instant::now();
        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_ref()
            && now.duration_since(cached.last_verified) <= CONFIG_STAMP_TTL
        {
            return cached.config.clone();
        }

        let mut stamp = ConfigStamp::from_inputs(config_path, agents_home);

        if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_mut()
            && cached.stamp == stamp
        {
            cached.last_verified = now;
            return cached.config.clone();
        }

        let env = crate::agents::ambient_env();
        // A hand-edited theme.toml can be rewritten in place. A read that races
        // the editor may parse a valid prefix whose missing fields serde fills
        // with built-ins, e.g. `[theme.pets] enabled = true` without `pet`
        // becomes "rocky". Cache only after the input stamp is quiet and
        // unchanged across the read.
        for _ in 0..STABLE_READ_ATTEMPTS {
            if stamp.modified_within(STABLE_READ_QUIET) {
                std::thread::sleep(STABLE_READ_QUIET);
                stamp = ConfigStamp::from_inputs(config_path, agents_home);
                continue;
            }

            let config = Arc::new(Self::load_lenient_from(config_path, agents_home, &env));
            let after = ConfigStamp::from_inputs(config_path, agents_home);
            if after == stamp {
                if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock() {
                    *memo = Some(LoadMemo {
                        stamp,
                        config: config.clone(),
                        last_verified: now,
                    });
                }
                return config;
            }
            stamp = after;
        }

        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_ref()
        {
            return cached.config.clone();
        }
        Arc::new(Self::load_lenient_from(config_path, agents_home, &env))
    }

    pub(crate) fn load_stamp_generation() -> u64 {
        let _ = Self::load_lenient();
        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_ref()
        {
            return hash_config_stamp(&cached.stamp);
        }
        hash_config_stamp(&ConfigStamp::from_inputs(
            &Self::config_path(),
            &paths::agents_home(),
        ))
    }
}

/// Diagnose parse, I/O, and semantic failures across the per-machine config
/// files and Markdown definitions. Runtime loading remains lenient; this feeds
/// the start notice and `rimz doctor`.
pub fn broken_machine_files() -> Vec<ConfigErr> {
    broken_machine_files_in(&MachineConfigFiles::machine())
}

fn broken_machine_files_in(files: &MachineConfigFiles) -> Vec<ConfigErr> {
    let checks = [
        load_optional(files.core_path(), parse_core_text_strict).map(|_| ()),
        load_optional(&files.path(MachineConfigFileKind::Theme), parse_theme_text).map(|_| ()),
        load_optional(&files.path(MachineConfigFileKind::Loop), parse_loop_text).map(|_| ()),
    ];
    let mut errors: Vec<_> = checks.into_iter().filter_map(Result::err).collect();
    let config = MachineConfig::load_lenient_from(
        files.core_path(),
        files.agents_home(),
        &crate::agents::ambient_env(),
    );
    errors.extend(config.notices.definition_errors.into_iter().map(|error| {
        ConfigErr::Definition {
            path: error.path,
            message: error.message,
        }
    }));
    errors
}

fn hash_config_stamp(stamp: &ConfigStamp) -> u64 {
    let mut hasher = DefaultHasher::new();
    stamp.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ConfigStamp {
    core: StampedPath,
    theme: StampedPath,
    loop_: StampedPath,
    definitions: Vec<StampedPath>,
}

impl ConfigStamp {
    fn from_inputs(config_path: &Path, agents_home: &Path) -> Self {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let definitions = definitions::source_paths(agents_home)
            .into_iter()
            .chain(skill_library_paths(&agents_home.join("skills")))
            .map(|path| StampedPath::of(&path))
            .collect();
        Self {
            core: StampedPath::of(files.core_path()),
            theme: StampedPath::of(&files.path(MachineConfigFileKind::Theme)),
            loop_: StampedPath::of(&files.path(MachineConfigFileKind::Loop)),
            definitions,
        }
    }

    fn modified_within(&self, quiet: Duration) -> bool {
        let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            return true;
        };
        [&self.core, &self.theme, &self.loop_]
            .into_iter()
            .chain(self.definitions.iter())
            .any(|path| stamped_path_modified_within(path, now, quiet))
    }
}

/// The skill files a sandboxed load reads for `skills:` checks, plus the
/// directories whose listing changes when a skill is added or removed.
fn skill_library_paths(skills: &Path) -> Vec<PathBuf> {
    let mut paths = vec![skills.to_path_buf()];
    let Ok(entries) = std::fs::read_dir(skills) else {
        return paths;
    };
    let mut skills: Vec<_> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    skills.sort();
    for skill in skills {
        paths.push(skill.join("SKILL.md"));
        paths.push(skill.join("agents/openai.yaml"));
        paths.push(skill);
    }
    paths
}

fn stamped_path_modified_within(path: &StampedPath, now: Duration, quiet: Duration) -> bool {
    let stamp = path.stamp;
    if stamp.modified_secs == 0 && stamp.modified_nanos == 0 {
        return false;
    }
    let modified = Duration::new(stamp.modified_secs, stamp.modified_nanos);
    match now.checked_sub(modified) {
        Some(age) => age < quiet,
        None => true,
    }
}

/// Resolve an optional IANA name to a zone, falling back to the system zone.
pub fn resolve_time_zone(name: Option<&str>) -> jiff::tz::TimeZone {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| jiff::tz::TimeZone::get(name).ok())
        .unwrap_or_else(jiff::tz::TimeZone::system)
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CoreConfig {
    agents: AgentsConfig,
    subagents: SubagentProfilesConfig,
    timezone: Option<String>,
    mux: MuxConfig,
    accounts: AccountsConfig,
    remote_control: RemoteControlConfig,
    daemon: DaemonConfig,
    notifications: NotificationsPrefs,
    sidebar: SidebarConfig,
    zellij: ZellijConfig,
    tmux: TmuxConfig,
    resume: ResumeConfig,
    harness: HarnessConfig,
    gc: GcConfig,
    sentry: SentryConfig,
    web: WebPrefs,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    theme: ThemeConfig,
    colors: Option<InlinePalette>,
}

#[derive(Debug)]
struct Parsed<T> {
    value: T,
    unknown_keys: Vec<String>,
}

impl<T: Default> Default for Parsed<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            unknown_keys: Vec::new(),
        }
    }
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

fn load_parsed_optional<T: Default>(
    path: &Path,
    parse: fn(&Path, &str) -> Result<Parsed<T>>,
) -> Result<Parsed<T>> {
    load_optional(path, parse).map(Option::unwrap_or_default)
}

fn recover<T>(result: Result<Option<T>>) -> Option<T> {
    match result {
        Ok(opt) => opt,
        Err(err) => {
            let (config, detail) = err
                .diagnosis()
                .map(|diagnosis| (diagnosis.path().display().to_string(), diagnosis.summary()))
                .unwrap_or_default();
            tracing::warn!(
                error = %err,
                config = %config,
                detail = %detail,
                "per-machine config unreadable; using built-in defaults for this file",
            );
            None
        }
    }
}

fn recover_parsed<T: Default>(
    path: &Path,
    parse: fn(&Path, &str) -> Result<Parsed<T>>,
) -> Parsed<T> {
    recover(load_optional(path, parse)).unwrap_or_default()
}

fn parse_core_text(path: &Path, text: &str) -> Result<CoreConfig> {
    parse_core_text_collecting(path, text).map(|parsed| parsed.value)
}

fn parse_core_text_collecting(path: &Path, text: &str) -> Result<Parsed<CoreConfig>> {
    check_removed_agents_tables(path, text)?;
    parse_toml_collecting(path, text)
}

fn parse_core_text_strict(path: &Path, text: &str) -> Result<CoreConfig> {
    let core = parse_core_text(path, text)?;
    validate_account_budgets(&core.accounts, path)?;
    Ok(core)
}

fn validate_account_budgets(accounts: &AccountsConfig, path: &Path) -> Result<()> {
    accounts
        .validate_budgets()
        .map_err(|source| ConfigErr::AccountBudget {
            path: path.to_path_buf(),
            source,
        })?;
    crate::agents::LoginCatalog::from_config(accounts)
        .map(|_| ())
        .map_err(|source| ConfigErr::Account {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
}

fn parse_unknown_keys<'de, T>(path: &Path, text: &'de str) -> Result<Vec<String>>
where
    T: Deserialize<'de>,
{
    parse_toml_collecting::<T>(path, text).map(|parsed| parsed.unknown_keys)
}

fn parse_toml_collecting<'de, T>(path: &Path, text: &'de str) -> Result<Parsed<T>>
where
    T: Deserialize<'de>,
{
    let deserializer = toml::Deserializer::parse(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, text, &source)),
    })?;
    let mut ignored = Vec::new();
    let value = serde_ignored::deserialize::<_, _, T>(deserializer, |path| {
        ignored.push(path.to_string());
    })
    .map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, text, &source)),
    })?;
    Ok(Parsed {
        value,
        unknown_keys: ignored,
    })
}

fn parse_theme_text(path: &Path, text: &str) -> Result<ThemeConfig> {
    parse_theme_text_collecting(path, text).map(|parsed| parsed.value)
}

fn parse_theme_text_collecting(path: &Path, text: &str) -> Result<Parsed<ThemeConfig>> {
    let Parsed {
        mut value,
        unknown_keys,
    } = parse_toml_collecting::<ThemeFile>(path, text)?;
    value.theme.colors = value.colors;
    Ok(Parsed {
        value: value.theme,
        unknown_keys,
    })
}

fn parse_loop_text(path: &Path, text: &str) -> Result<LoopConfig> {
    parse_loop_text_collecting(path, text).map(|parsed| parsed.value)
}

fn parse_loop_text_collecting(path: &Path, text: &str) -> Result<Parsed<LoopConfig>> {
    let parsed = parse_toml_collecting::<LoopConfig>(path, text)?;
    parsed
        .value
        .validate_budgets()
        .map_err(|source| ConfigErr::Loop {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(parsed)
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
    let value = toml::Value::Table(doc.clone());
    for (table, tree) in [
        ("agents.profiles", "agents"),
        ("agents.teams", "teams"),
        ("subagents.profiles", "subagents"),
        ("profiles", "agents"),
    ] {
        if table
            .split('.')
            .try_fold(&value, |value, key| value.get(key))
            .is_some()
        {
            return Err(removed(&format!(
                "`[{table}]` is no longer read; edit <agents_home>/{tree}/<name>.md"
            )));
        }
    }
    if doc.contains_key("tab") {
        return Err(removed(
            "`[tab]` (with `[tab.keywords]`/`[tab.layouts]`) was removed — set `placement` under `[agents]` and declare teams in <agents_home>/teams/<name>.md",
        ));
    }
    if let Some(agents) = doc.get("agents").and_then(toml::Value::as_table) {
        if agents.contains_key("aliases") {
            return Err(removed(
                "`[agents.aliases]` was removed — declare agents in <agents_home>/agents/<name>.md and raw command panes under `[agents.commands]`",
            ));
        }
        if agents.contains_key("layouts") {
            return Err(removed(
                "`[agents.layouts]` was removed — declare teams in <agents_home>/teams/<name>.md",
            ));
        }
        if agents.contains_key("loop") {
            return Err(removed(
                "`[agents.loop]` moved to its own `loop.toml` — move `[agents.loop.tasks.*]` entries to `[tasks.*]` there, or re-add with `rimz loop add`",
            ));
        }
    }
    if let Some(detail) = agents::retired_agents_key(&doc) {
        return Err(ConfigErr::RemovedKey {
            path: path.to_path_buf(),
            detail,
        });
    }
    Ok(())
}

fn validate_agents_config(agents: &AgentsConfig, path: &Path) -> Result<()> {
    crate::harness::spec::validate_config(&agents.profiles, &agents.commands, &agents.teams)
        .map_err(|source| ConfigErr::Agents {
            path: path.to_path_buf(),
            source,
        })
}

/// Validate materialized definitions using the machine configuration's launch rules.
fn validate_agents_file(
    agents: &AgentsConfig,
    subagents: &SubagentProfilesConfig,
    path: &Path,
) -> Result<()> {
    validate_agents_config(agents, path)?;
    validate_subagent_profiles_config(subagents, agents, path)?;
    validate_subagent_allowlists_config(agents, subagents, path)
}

fn validate_subagent_allowlists_config(
    agents: &AgentsConfig,
    subagents: &SubagentProfilesConfig,
    path: &Path,
) -> Result<()> {
    crate::harness::spec::validate_subagent_allowlists(
        &agents.profiles,
        &subagents.profiles,
        &agents.commands,
    )
    .map_err(|source| ConfigErr::Agents {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_subagent_profiles_config(
    subagents: &SubagentProfilesConfig,
    agents: &AgentsConfig,
    path: &Path,
) -> Result<()> {
    crate::harness::spec::validate_subagent_profile_namespace(
        &subagents.profiles,
        &agents.commands,
        &agents.teams,
    )
    .and_then(|()| crate::harness::spec::validate_profile_chains(&subagents.profiles))
    .map_err(|source| ConfigErr::Agents {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_notifications_config(notifications: &NotificationsPrefs, path: &Path) -> Result<()> {
    notifications
        .validate()
        .map_err(|source| ConfigErr::Notifications {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
#[path = "config/template_tests.rs"]
mod template_tests;

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
