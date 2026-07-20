//! Per-machine settings, loaded from `~/.config/rimz/config.toml`, `theme.toml`, `agents.toml`, and `loop.toml`. [`MachineConfigFiles`] is the ordered file registry, and [`ConfigEditor`] provides strict effective reads plus comment-preserving writes and template merges. This module also owns selectable theme-scheme lookup and validation.
//!
//! Agent and team fragments discovered under `~/.agents/{profiles,teams}` are the base layer for `agents.toml`, whose entries take precedence on name clashes. Strict and lenient load paths merge fragments before validating the agents view.
//!
//! This is the personal, never-committed tier. The project-committed tier is
//! `<root>/.rimz/config.toml`, parsed for the executable-surface hash in
//! [`crate::trust`]. Settings here are machine-wide preferences that tune how
//! RimZ drives *your* box or link *your* accounts, so they live outside the
//! repo and outside the trust hash — a clone never inherits them.
//!
//! A missing file is the default config, and unknown keys are ignored so an
//! older binary tolerates a newer file. Runtime entry points use
//! [`MachineConfig::load_lenient`], which degrades a broken file to built-in
//! defaults with a warning. Room start adds one narrow strict preflight for an
//! unenforceable account-day cap; strict [`MachineConfig::load`] and
//! [`MachineConfig::load_from`] otherwise back `rimz config` and `rimz doctor`,
//! which report the precise error.

use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::store::parse_cache::StampedPath;
use crate::store::paths::{self, config_home};

mod accounts;
mod agents;
mod animation;
mod attention;
mod color;
mod daemon;
mod display;
mod edit;
pub mod effective;
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
mod theme;
mod web;
mod worktree;

pub use accounts::{AccountBudgetConfigError, AccountsConfig, UsageLimitUsd};
pub use agents::{
    AgentsConfig, CommandsConfig, LaunchPlacement, Profile, ProfilesConfig, RoleBinding, Team,
    TeamsConfig,
};
pub use animation::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationRole, AnimationSpec, AnimationSpeed,
    ThemeAnimationsConfig, UnreadEffect, validate_glyph_cells, validate_single_cell,
};
pub use attention::AttentionConfig;
pub(crate) use color::xterm_rgb;
pub use color::{
    ColorDepth, PaletteRole, Semantic, ThemeColor, ThemeMode, nearest_xterm_index, parse_hex,
};
pub use daemon::{DaemonConfig, DaemonPane};
pub use display::{
    BudgetBarConfig, BudgetBurnRateConfig, CardDensityMode, ContextBand, ContextMeterConfig,
    DisplayConfig, HighlightStepsConfig, PixelMode, ProviderTabsMode, ScrollbarMode,
};
pub use edit::{
    ConfigEditErr, ConfigEditor, FileMergeOutcome, MergeAction, MergeReport, SkippedKey,
};
pub use glyphs::{
    GlyphOverrides, GlyphRole, ThemeGlyphsConfig, glyph_lookup_hint, is_named_glyph_set,
    validate_glyph_source,
};
pub use harness::{DayCap, DayCapParseError, HarnessConfig, RtkMode};
pub use loop_::{CheckOn, LoopConfig, TaskBudgetError, TaskEntry, TaskTarget, Tasks};
pub use mux::{
    MultiplexerConfig, MuxConfig, TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderLines,
    TmuxPaneBorderStatus, TmuxSetClipboard, ZellijClipboard, ZellijConfig, ZellijForceClose,
};
pub use notifications::{
    DesktopNotificationMode, NotificationKind, NotificationSoundMode, NotificationTrigger,
    NotificationsConfigErr, NotificationsPrefs, NotifyCondition, NotifyConditionAgent,
    NotifyHandler, RenderMode, TemplateVars, render_template,
};
pub use pets::{CellAspect, PetsConfig, PetsGlyphMode};
pub use remote_control::RemoteControlConfig;
pub(crate) use resume::parse_auto_redeem_min_gain;
pub use resume::{DEFAULT_AUTO_CONTINUE_BACKOFF_SECS, DEFAULT_AUTO_REDEEM_MIN_GAIN, ResumeConfig};
#[cfg(test)]
pub(crate) use scheme::parse_scheme_text;
pub(crate) use scheme::{DEFAULT_SCHEME, ParsedScheme, explicit_scheme, parsed_inline_palette};
pub use scheme::{available_scheme_names, resolve_inline_palette, theme_lookup_hint};
pub use sentry::SentryConfig;
pub use sidebar::{DEFAULT_AFK_AFTER_SECS, SidebarConfig, SidebarKeys};
pub use theme::{
    InlineAnsiColors, InlineCursorColors, InlinePalette, InlinePrimaryColors,
    InlineSelectionColors, ThemeConfig, ThemeProviderStyle, ThemeStyle,
};
pub use web::{WebPrefs, ZellijWebPrefs};
pub use worktree::{WorktreeBase, WorktreeBaseParseError, WorktreeConfig};

const CONFIG_FILE: &str = "config.toml";
const THEME_FILE: &str = "theme.toml";
const AGENTS_FILE: &str = "agents.toml";
const LOOP_FILE: &str = "loop.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";
const AGENTS_HOME_PROFILES_SUBDIR: &str = "profiles";
const AGENTS_HOME_TEAMS_SUBDIR: &str = "teams";
const AGENT_FRAGMENT_FILE: &str = "agent.toml";
const TEAM_FRAGMENT_FILE: &str = "team.toml";
pub const MACHINE_CONFIG_TEMPLATE: &str = include_str!("config/templates/config.template.toml");
pub const MACHINE_THEME_TEMPLATE: &str = include_str!("config/templates/theme.template.toml");
pub const MACHINE_AGENTS_TEMPLATE: &str = include_str!("config/templates/agents.template.toml");
pub const MACHINE_LOOP_TEMPLATE: &str = include_str!("config/templates/loop.template.toml");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MachineConfigFileKind {
    Core,
    Theme,
    Agents,
    Loop,
}

impl MachineConfigFileKind {
    const ALL: [Self; 4] = [Self::Core, Self::Theme, Self::Agents, Self::Loop];

    fn file_name(self) -> &'static str {
        match self {
            Self::Core => CONFIG_FILE,
            Self::Theme => THEME_FILE,
            Self::Agents => AGENTS_FILE,
            Self::Loop => LOOP_FILE,
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Core => MACHINE_CONFIG_TEMPLATE,
            Self::Theme => MACHINE_THEME_TEMPLATE,
            Self::Agents => MACHINE_AGENTS_TEMPLATE,
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

/// Canonical paths and templates for the four per-machine config files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineConfigFiles {
    core_path: PathBuf,
    agents_home: PathBuf,
}

impl MachineConfigFiles {
    /// Resolve the current machine's config roots.
    pub fn machine() -> Self {
        Self::from_paths(
            config_home().join(RIMZ_CONFIG_SUBDIR).join(CONFIG_FILE),
            paths::agents_home(),
        )
    }

    /// Build an explicit config set for tests and tooling.
    pub fn from_paths(core_path: impl Into<PathBuf>, agents_home: impl Into<PathBuf>) -> Self {
        Self {
            core_path: core_path.into(),
            agents_home: agents_home.into(),
        }
    }

    pub fn core_path(&self) -> &Path {
        &self.core_path
    }

    pub fn agents_home(&self) -> &Path {
        &self.agents_home
    }

    /// Files in persistence and display order: core, theme, agents, loop.
    pub fn ordered(&self) -> [MachineConfigFile; 4] {
        MachineConfigFileKind::ALL.map(|kind| MachineConfigFile {
            path: self.path(kind),
            template: kind.template(),
        })
    }

    pub fn ordered_paths(&self) -> [PathBuf; 4] {
        MachineConfigFileKind::ALL.map(|kind| self.path(kind))
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
    #[error("cannot access {path}: {source}")]
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
    #[error(
        "removed config table in {path}: {detail} (run `rimz config init --print` for the current shape)"
    )]
    RemovedTable { path: PathBuf, detail: String },
}

impl ConfigErr {
    /// The per-machine file that failed to load.
    pub fn path(&self) -> &Path {
        match self {
            Self::Io { path, .. }
            | Self::Parse { path, .. }
            | Self::Agents { path, .. }
            | Self::Notifications { path, .. }
            | Self::Loop { path, .. }
            | Self::AccountBudget { path, .. }
            | Self::RemovedTable { path, .. } => path,
        }
    }

    /// The validation failure without file/location context, for callers
    /// reporting a value error rather than a broken file.
    pub fn validation_message(&self) -> String {
        match self {
            Self::Parse { source, .. } => source.message().to_owned(),
            Self::Agents { source, .. } => source.to_string(),
            Self::Notifications { source, .. } => source.to_string(),
            Self::Loop { source, .. } => source.to_string(),
            Self::AccountBudget { source, .. } => source.to_string(),
            Self::Io { .. } | Self::RemovedTable { .. } => self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, ConfigErr>;

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
    pub sentry: SentryConfig,
    pub web: WebPrefs,
    #[serde(skip_serializing_if = "ThemeConfig::is_unset")]
    pub theme: ThemeConfig,
    pub agents: AgentsConfig,
    #[serde(default, skip_serializing_if = "LoopConfig::is_empty")]
    pub r#loop: LoopConfig,
}

impl MachineConfig {
    /// The generated core per-machine config reference.
    pub fn template_core() -> &'static str {
        MachineConfigFileKind::Core.template()
    }

    /// The generated theme per-machine config reference.
    pub fn template_theme() -> &'static str {
        MachineConfigFileKind::Theme.template()
    }

    /// The generated agents per-machine config reference.
    pub fn template_agents() -> &'static str {
        MachineConfigFileKind::Agents.template()
    }

    /// The generated loop per-machine config reference.
    pub fn template_loop() -> &'static str {
        MachineConfigFileKind::Loop.template()
    }

    /// The core per-machine config path: `$XDG_CONFIG_HOME/rimz/config.toml`.
    pub fn config_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Core)
    }

    /// The theme per-machine config path: `$XDG_CONFIG_HOME/rimz/theme.toml`.
    pub fn theme_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Theme)
    }

    /// The agents per-machine config path: `$XDG_CONFIG_HOME/rimz/agents.toml`.
    pub fn agents_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Agents)
    }

    /// The loop per-machine config path: `$XDG_CONFIG_HOME/rimz/loop.toml`.
    pub fn loop_path() -> PathBuf {
        MachineConfigFiles::machine().path(MachineConfigFileKind::Loop)
    }

    /// Load from the default per-machine paths. Missing files are defaults —
    /// never an error.
    pub fn load() -> Result<Self> {
        let files = MachineConfigFiles::machine();
        Self::load_from(files.core_path(), files.agents_home())
    }

    /// Strictly load only the per-machine loop task file. Missing file is the
    /// default loop config.
    pub fn load_loop() -> Result<LoopConfig> {
        let files = MachineConfigFiles::machine();
        load_optional(&files.path(MachineConfigFileKind::Loop), parse_loop_text)
            .map(|loop_| loop_.unwrap_or_default())
    }

    /// Load per-machine config for a runtime entry point. A file that fails to
    /// load degrades to its built-in defaults with a warning instead of
    /// aborting the room; the strict [`Self::load`] and [`Self::load_from`]
    /// report the precise error for `rimz config` and `rimz doctor`.
    pub fn load_lenient() -> Arc<Self> {
        let files = MachineConfigFiles::machine();
        Self::load_lenient_with_memo(files.core_path(), files.agents_home())
    }

    /// Load from an explicit config.toml path and its sibling theme.toml,
    /// agents.toml, and loop.toml files, merging fragments from the explicit
    /// agents-home root before validation — the test and tooling seam. A
    /// nonexistent fragment root means no fragments.
    pub fn load_from(config_path: &Path, agents_home: &Path) -> Result<Self> {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let theme_path = files.path(MachineConfigFileKind::Theme);
        let agents_path = files.path(MachineConfigFileKind::Agents);
        let loop_path = files.path(MachineConfigFileKind::Loop);

        let core = load_optional(files.core_path(), parse_core_text)?.unwrap_or_default();
        validate_account_budgets(&core.accounts, files.core_path())?;
        let theme = load_optional(&theme_path, parse_theme_text)?.unwrap_or_default();
        let agents = load_optional(&agents_path, parse_agents_text)?.unwrap_or_default();
        let loop_ = load_optional(&loop_path, parse_loop_text)?.unwrap_or_default();

        let mut config = Self::assemble(core, theme, agents, loop_);
        validate_notifications_config(&config.notifications, files.core_path())?;
        apply_agents_home(&mut config.agents, files.agents_home(), &agents_path)?;
        Ok(config)
    }

    fn load_lenient_from(config_path: &Path, agents_home: &Path) -> Self {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let theme_path = files.path(MachineConfigFileKind::Theme);
        let agents_path = files.path(MachineConfigFileKind::Agents);
        let loop_path = files.path(MachineConfigFileKind::Loop);

        let core = recover(load_optional(files.core_path(), parse_core_text)).unwrap_or_default();
        let theme = recover(load_optional(&theme_path, parse_theme_text)).unwrap_or_default();
        let agents = recover(load_optional(&agents_path, parse_agents_text)).unwrap_or_default();
        let loop_ = recover(load_optional(&loop_path, parse_loop_text)).unwrap_or_default();

        let mut config = Self::assemble(core, theme, agents, loop_);
        if let Err(err) = validate_notifications_config(&config.notifications, files.core_path()) {
            tracing::warn!(
                error = %err,
                "per-machine notifications config invalid; using built-in defaults",
            );
            config.notifications = NotificationsPrefs::default();
        }
        let fragment = match discover_agents_home(files.agents_home()) {
            Ok(fragment) => fragment,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "~/.agents discovery failed; using per-machine agents config only",
                );
                AgentsFragment::default()
            }
        };
        let mut merged = config.agents.clone();
        overlay_agents_fragment_under(&mut merged, &fragment);
        match validate_agents_config(&merged, &agents_path) {
            Ok(()) => config.agents = merged,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "per-machine agents config invalid; using built-in defaults",
                );
                let mut fallback = AgentsConfig::default();
                overlay_agents_fragment_under(&mut fallback, &fragment);
                match validate_agents_config(&fallback, &agents_path) {
                    Ok(()) => config.agents = fallback,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "~/.agents fragments invalid; using built-in defaults",
                        );
                        config.agents = AgentsConfig::default();
                    }
                }
            }
        }
        config
    }

    pub fn parse_text(path: &Path, text: &str, agents_home: &Path) -> Result<Self> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => Ok(Self::assemble(
                CoreConfig::default(),
                parse_theme_text(path, text)?,
                AgentsConfig::default(),
                LoopConfig::default(),
            )),
            Some(AGENTS_FILE) => {
                let mut agents = parse_agents_text(path, text)?;
                apply_agents_home(&mut agents, agents_home, path)?;
                Ok(Self::assemble(
                    CoreConfig::default(),
                    ThemeConfig::default(),
                    agents,
                    LoopConfig::default(),
                ))
            }
            Some(LOOP_FILE) => Ok(Self::assemble(
                CoreConfig::default(),
                ThemeConfig::default(),
                AgentsConfig::default(),
                parse_loop_text(path, text)?,
            )),
            _ => {
                let core = parse_core_text(path, text)?;
                validate_notifications_config(&core.notifications, path)?;
                validate_account_budgets(&core.accounts, path)?;
                Ok(Self::assemble(
                    core,
                    ThemeConfig::default(),
                    AgentsConfig::default(),
                    LoopConfig::default(),
                ))
            }
        }
    }

    /// Parse one per-machine config file's text and return the key paths serde
    /// ignored, dotted, in the file's own table coordinates.
    fn parse_text_unknown_keys(path: &Path, text: &str) -> Result<Vec<String>> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => parse_unknown_keys::<ThemeFile>(path, text),
            Some(AGENTS_FILE) => parse_unknown_keys::<AgentsFile>(path, text),
            Some(LOOP_FILE) => parse_unknown_keys::<LoopConfig>(path, text),
            _ => parse_unknown_keys::<CoreConfig>(path, text),
        }
    }

    fn assemble(
        core: CoreConfig,
        theme: ThemeConfig,
        agents: AgentsConfig,
        loop_: LoopConfig,
    ) -> Self {
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
            sentry: core.sentry,
            web: core.web,
            theme,
            agents,
            r#loop: loop_,
        }
    }

    pub fn time_zone(&self) -> jiff::tz::TimeZone {
        resolve_time_zone(self.timezone.as_deref())
    }

    /// Serialize the effective config into a traversable TOML value.
    pub fn to_toml_value(&self) -> std::result::Result<toml::Value, toml::ser::Error> {
        toml::Value::try_from(self)
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

        let Ok(mut stamp) = ConfigStamp::from_inputs(config_path, agents_home) else {
            // A fragment dir can vanish mid-scan. Read without caching so the
            // next tick re-derives from a settled tree.
            return Arc::new(Self::load_lenient_from(config_path, agents_home));
        };

        if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_mut()
            && cached.stamp == stamp
        {
            cached.last_verified = now;
            return cached.config.clone();
        }

        // A hand-edited theme.toml can be rewritten in place. A read that races
        // the editor may parse a valid prefix whose missing fields serde fills
        // with built-ins, e.g. `[theme.pets] enabled = true` without `pet`
        // becomes "rocky". Cache only after the input stamp is quiet and
        // unchanged across the read.
        for _ in 0..STABLE_READ_ATTEMPTS {
            if stamp.modified_within(STABLE_READ_QUIET) {
                std::thread::sleep(STABLE_READ_QUIET);
                match ConfigStamp::from_inputs(config_path, agents_home) {
                    Ok(after) => {
                        stamp = after;
                        continue;
                    }
                    Err(_) => {
                        return Arc::new(Self::load_lenient_from(config_path, agents_home));
                    }
                }
            }

            let config = Arc::new(Self::load_lenient_from(config_path, agents_home));
            match ConfigStamp::from_inputs(config_path, agents_home) {
                Ok(after) if after == stamp => {
                    if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock() {
                        *memo = Some(LoadMemo {
                            stamp,
                            config: config.clone(),
                            last_verified: now,
                        });
                    }
                    return config;
                }
                Ok(after) => stamp = after,
                Err(_) => return config,
            }
        }

        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_ref()
        {
            return cached.config.clone();
        }
        Arc::new(Self::load_lenient_from(config_path, agents_home))
    }

    pub(crate) fn load_stamp_generation() -> u64 {
        let _ = Self::load_lenient();
        if let Ok(memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
            && let Some(cached) = memo.as_ref()
        {
            return hash_config_stamp(&cached.stamp);
        }
        ConfigStamp::from_inputs(&Self::config_path(), &paths::agents_home())
            .map(|stamp| hash_config_stamp(&stamp))
            .unwrap_or(0)
    }
}

/// Strictly parse the four per-machine config files, returning one error per
/// file that exists but cannot load. Runtime loading remains lenient; this
/// feeds the start notice and `rimz doctor`.
pub fn broken_machine_files() -> Vec<ConfigErr> {
    broken_machine_files_in(&MachineConfigFiles::machine())
}

fn broken_machine_files_in(files: &MachineConfigFiles) -> Vec<ConfigErr> {
    let checks = [
        load_optional(files.core_path(), parse_core_text_strict).map(|_| ()),
        load_optional(&files.path(MachineConfigFileKind::Theme), parse_theme_text).map(|_| ()),
        load_optional(
            &files.path(MachineConfigFileKind::Agents),
            parse_agents_text,
        )
        .map(|_| ()),
        load_optional(&files.path(MachineConfigFileKind::Loop), parse_loop_text).map(|_| ()),
    ];
    checks.into_iter().filter_map(Result::err).collect()
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
    agents: StampedPath,
    loop_: StampedPath,
    fragments: Vec<StampedPath>,
}

impl ConfigStamp {
    fn from_inputs(config_path: &Path, agents_home: &Path) -> Result<Self> {
        let files = MachineConfigFiles::from_paths(config_path, agents_home);
        let mut fragments = Vec::new();
        collect_agents_home_fragment_stamps(
            agents_home,
            AGENTS_HOME_PROFILES_SUBDIR,
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
            core: StampedPath::of(files.core_path()),
            theme: StampedPath::of(&files.path(MachineConfigFileKind::Theme)),
            agents: StampedPath::of(&files.path(MachineConfigFileKind::Agents)),
            loop_: StampedPath::of(&files.path(MachineConfigFileKind::Loop)),
            fragments,
        })
    }

    fn modified_within(&self, quiet: Duration) -> bool {
        let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            return true;
        };
        [&self.core, &self.theme, &self.agents, &self.loop_]
            .into_iter()
            .chain(self.fragments.iter())
            .any(|path| stamped_path_modified_within(path, now, quiet))
    }
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

fn collect_agents_home_fragment_stamps(
    root: &Path,
    subdir: &str,
    fragment_file: &str,
    out: &mut Vec<StampedPath>,
) -> Result<()> {
    for dir in child_dirs(&root.join(subdir))? {
        out.push(StampedPath::of(&dir.join(fragment_file)));
    }
    Ok(())
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
    sentry: SentryConfig,
    web: WebPrefs,
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
        })
}

fn parse_unknown_keys<'de, T>(path: &Path, text: &'de str) -> Result<Vec<String>>
where
    T: Deserialize<'de>,
{
    let deserializer = toml::Deserializer::parse(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let mut ignored = Vec::new();
    let _ = serde_ignored::deserialize::<_, _, T>(deserializer, |path| {
        ignored.push(path.to_string());
    })
    .map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(ignored)
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
    let mut file: AgentsFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    resolve_agents_prompt_paths(&mut file.agents.profiles, &mut file.agents.teams, path);
    Ok(file.agents)
}

fn parse_loop_text(path: &Path, text: &str) -> Result<LoopConfig> {
    let loop_: LoopConfig = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    loop_.validate_budgets().map_err(|source| ConfigErr::Loop {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(loop_)
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
        if agents.contains_key("loop") {
            return Err(removed(
                "`[agents.loop]` moved to its own `loop.toml` — move `[agents.loop.tasks.*]` entries to `[tasks.*]` there, or re-add with `rimz loop add`",
            ));
        }
    }
    Ok(())
}

fn parse_agents_fragment_text(path: &Path, text: &str) -> Result<AgentsFragment> {
    let mut file: AgentsFragmentFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    resolve_agents_prompt_paths(&mut file.agents.profiles, &mut file.agents.teams, path);
    Ok(file.agents)
}

fn resolve_agents_prompt_paths(
    profiles: &mut ProfilesConfig,
    teams: &mut TeamsConfig,
    source_path: &Path,
) {
    let source_dir = source_path.parent().unwrap_or_else(|| Path::new("."));
    crate::harness::spec::resolve_prompt_paths(profiles, teams, source_dir);
}

fn discover_agents_home(root: &Path) -> Result<AgentsFragment> {
    let mut fragment = AgentsFragment::default();
    discover_agents_home_subdir(
        root,
        AGENTS_HOME_PROFILES_SUBDIR,
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
        let Some(fragment) = load_optional(&path, parse_agents_fragment_text)? else {
            continue;
        };
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
    overlay_agents_fragment_under(&mut merged, &fragment);
    validate_agents_config(&merged, agents_path)?;
    *agents = merged;
    Ok(())
}

fn overlay_agents_fragment_under(agents: &mut AgentsConfig, fragment: &AgentsFragment) {
    overlay_under(&mut agents.profiles.0, fragment.profiles.0.clone());
    overlay_under(&mut agents.teams.0, fragment.teams.0.clone());
    overlay_under(&mut agents.commands.0, fragment.commands.0.clone());
}

fn overlay_under<V>(file: &mut BTreeMap<String, V>, fragment: BTreeMap<String, V>) {
    for (key, value) in fragment {
        file.entry(key).or_insert(value);
    }
}

fn validate_agents_config(agents: &AgentsConfig, path: &Path) -> Result<()> {
    crate::harness::spec::validate_config(&agents.profiles, &agents.commands, &agents.teams)
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
