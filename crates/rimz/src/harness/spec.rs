//! Backend-neutral agent layout IR plus team/profile/command resolution.
//!
//! Commas split columns, plus signs tile rows within a column, slashes stack
//! rows within a column, and each cell is a profile, a configured command, a
//! PATH executable, or a built-in cell. Agent cells optionally carry a `:role`
//! suffix for an ad-hoc role handle. Named teams compile to one column per role
//! unless they declare an explicit role-first layout shape. Built-ins provide
//! `term`, registered runtimes, and `<kind>-<mode>` permission cells;
//! per-machine `[agents.profiles]` entries can specialize agent cells and
//! `[agents.commands]` entries provide raw command panes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::agents::PermissionMode;
use crate::agents::petname;
use crate::config::{
    CommandsConfig, Profile, ProfilesConfig, RoleBinding, Selection, Team, TeamsConfig,
};
use crate::harness::team_prompt::TeamPrompt;
use crate::ids::AgentKind;

const PERMISSION_MODE_NAMES: &[&str] = &["plan", "ask", "auto", "yolo"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSpec {
    pub columns: Vec<Column>,
}

impl LayoutSpec {
    pub fn single(cell: Cell) -> Self {
        Self {
            columns: vec![Column {
                rows: vec![cell],
                stacked: false,
            }],
        }
    }

    /// Every agent cell, in layout order (duplicates included).
    pub fn agent_cells(&self) -> impl Iterator<Item = &AgentCell> {
        self.columns
            .iter()
            .flat_map(|column| column.rows.iter())
            .filter_map(|cell| match cell {
                Cell::Agent(agent) => Some(agent),
                Cell::Command { .. } => None,
            })
    }

    /// Every mutable agent cell, in layout order (duplicates included).
    pub(super) fn agent_cells_mut(&mut self) -> impl Iterator<Item = &mut AgentCell> {
        self.columns
            .iter_mut()
            .flat_map(|column| column.rows.iter_mut())
            .filter_map(|cell| match cell {
                Cell::Agent(agent) => Some(agent),
                Cell::Command { .. } => None,
            })
    }

    /// Every agent cell's kind, in layout order (duplicates included).
    pub fn agent_kinds(&self) -> impl Iterator<Item = &str> {
        self.agent_cells().map(|cell| cell.kind.as_str())
    }
}

/// Borrowed structural layout grammar before cells acquire launch semantics.
pub(crate) struct RawLayout<'a> {
    pub(crate) columns: Vec<RawColumn<'a>>,
}

pub(crate) struct RawColumn<'a> {
    pub(crate) cells: Vec<&'a str>,
    pub(crate) stacked: bool,
}

impl<'a> RawLayout<'a> {
    pub(crate) fn cells(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.columns
            .iter()
            .flat_map(|column| column.cells.iter().copied())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub rows: Vec<Cell>,
    pub stacked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellNotice {
    DroppedModel { model: String },
    DroppedTools { tools: Vec<String> },
    ToolRestrictions { tools: Vec<String> },
    ToolsNotApplied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCell {
    pub notices: Vec<CellNotice>,
    pub empty_dir: Option<bool>,
    /// The subagent definition's run timeout, read by the launcher's deadline ladder.
    pub timeout: Option<std::time::Duration>,
    pub resume_model_override: bool,
    pub isolation_default: Option<crate::config::Isolation>,
    pub kind: AgentKind,
    pub args: Vec<String>,
    pub auto_compact: Option<String>,
    /// Where the cell's effort, fast, code mode, and auto-compact came from.
    pub tuning: TuningSources,
    pub system_prompt_file: Option<crate::config::PromptSource>,
    /// Ordered prompt fragments composed into the replacement system prompt.
    pub append_system_prompt_files: Vec<crate::config::PromptSource>,
    /// The team layer composed after the role's own prompt; set only on a role
    /// cell of a staged team whose prompt has a base.
    pub team_prompt: Option<crate::harness::team_prompt::TeamPrompt>,
    /// Whether this cell holds its team's `leader` seat, set by team compilation on exactly one
    /// cell of a team's layout and read by [`prompt_leader`].
    pub(crate) leads_team: bool,
    pub skills: Option<Vec<crate::config::SkillName>>,
    pub seat_consensus_skill: bool,
    pub mcps: Option<Vec<crate::config::McpName>>,
    pub no_skill_tool: bool,
    /// The RimZ grant of a `capabilities:` list, from the tools value it rides on.
    pub rimz_capabilities: Option<Vec<crate::config::RimzEntry>>,
    pub allowed_tools: Option<Vec<crate::config::ToolRule>>,
    /// The definition's tools list as canonical base names, for a kind that writes it into a
    /// provider file rather than argv.
    pub tools: Option<Vec<String>>,
    /// Canonical shared launch identity selected by profiles, roles, and CLI overlays.
    pub launch: crate::agents::LaunchParams,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "layout IR keeps agent cells inline and avoids one allocation per launch"
)]
pub enum Cell {
    Agent(AgentCell),
    Command { argv: Vec<String> },
}

impl Cell {
    #[cfg(test)]
    pub(crate) fn agent(kind: AgentKind) -> Self {
        Self::Agent(AgentCell {
            notices: Vec::new(),
            empty_dir: None,
            timeout: None,
            resume_model_override: false,
            kind,
            args: Vec::new(),
            auto_compact: None,
            tuning: TuningSources::default(),
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            leads_team: false,
            skills: None,
            seat_consensus_skill: false,
            mcps: None,
            no_skill_tool: false,
            rimz_capabilities: None,
            allowed_tools: None,
            tools: None,
            isolation_default: None,
            launch: crate::agents::LaunchParams::default(),
        })
    }

    pub(super) fn shell() -> Self {
        Self::Command { argv: Vec::new() }
    }

    pub(super) fn pane_name(&self) -> String {
        match self {
            Self::Agent(cell) => {
                crate::agents::pane_title::short_label(cell.launch.profile.as_deref(), &cell.kind)
                    .to_owned()
            }
            Self::Command { argv } => {
                argv.first()
                    .map_or_else(crate::proc::shell_pane_name, |program| {
                        Path::new(program).file_name().map_or_else(
                            || program.clone(),
                            |name| name.to_string_lossy().into_owned(),
                        )
                    })
            }
        }
    }
}

/// A kind base's tuning, held apart from the chain's written values: the kind-defaults
/// step fills from it what nothing nearer wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct KindBase {
    effort: Option<String>,
    auto_compact: Option<String>,
    fast: Option<crate::agents::Toggle>,
    code_mode: Option<crate::agents::Toggle>,
    built_in: bool,
}

/// Where a launch tuning value came from, as the resolution that set it records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuningSource {
    /// The profile layer that writes it.
    Profile(String),
    /// The team role that writes it.
    Role(String),
    /// The final kind's base definition, below every written value.
    KindBase(AgentKind),
    /// The kind base RimZ ships for the final kind, standing where no home or pack base does.
    BuiltInKindBase(AgentKind),
    /// The built-in effort a definition profile takes for its kind and model.
    BuiltIn,
    /// The machine's `[harness] auto_compact`.
    MachineDefault,
    /// The launch flag that set it.
    Flag(&'static str),
    /// The `<base>-fast` target that stamps fast on.
    Target(String),
    /// The launch record a relaunch replays.
    Recorded,
}

impl TuningSource {
    /// The kind-defaults step's own fill, cleared when a role override or `--agent` override fills again.
    fn is_kind_default(source: &Option<Self>) -> bool {
        matches!(
            source,
            Some(Self::KindBase(_) | Self::BuiltInKindBase(_) | Self::BuiltIn)
        )
    }
}

impl std::fmt::Display for TuningSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Profile(name) => write!(f, "profile {name}"),
            Self::Role(role) => write!(f, "role {role}"),
            Self::KindBase(kind) => write!(f, "kind base {kind}"),
            Self::BuiltInKindBase(kind) => write!(f, "built-in kind base {kind}"),
            Self::BuiltIn => f.write_str("built-in default"),
            Self::MachineDefault => f.write_str("machine default"),
            Self::Flag(flag) => f.write_str(flag),
            Self::Target(target) => f.write_str(target),
            Self::Recorded => f.write_str("recorded"),
        }
    }
}

/// The source of each tuning value a cell plans; a value the cell leaves unset has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TuningSources {
    pub effort: Option<TuningSource>,
    pub fast: Option<TuningSource>,
    pub code_mode: Option<TuningSource>,
    pub auto_compact: Option<TuningSource>,
}

/// A profile chain flattened to the concrete adapter kind that can be executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    notices: Vec<CellNotice>,
    kind_defaults: bool,
    kind_base: KindBase,
    sources: TuningSources,
    pub empty_dir: Option<bool>,
    pub timeout: Option<std::time::Duration>,
    pub isolation_default: Option<crate::config::Isolation>,
    pub kind: AgentKind,
    pub(crate) variant: Option<&'static crate::agents::Variant>,
    pub layers: Vec<String>,
    pub launch: crate::agents::LaunchParams,
    pub auto_compact: Option<String>,
    pub auto_compact_default: Option<crate::config::AutoCompactDefault>,
    /// Fast mode as the chain set it. A nearer layer's off or `auto` wins
    /// over a farther on; on forces the switch, off forces it off, and auto
    /// or absent leaves the provider's default.
    pub fast: Option<crate::agents::Toggle>,
    /// Code mode as the chain set it, under the same rule as `fast`.
    pub code_mode: Option<crate::agents::Toggle>,
    pub system_prompt_file: Option<crate::config::PromptSource>,
    pub append_system_prompt_files: Vec<crate::config::PromptSource>,
    pub skills: Option<Vec<crate::config::SkillName>>,
    pub seat_consensus_skill: bool,
    pub mcps: Option<Vec<crate::config::McpName>>,
    pub tools: Option<crate::agents::ToolSet>,
    pub allowed_tools: Option<Vec<crate::config::ToolRule>>,
    pub args: Option<String>,
}

impl ResolvedProfile {
    /// Consume the resolved profile into its effective display chain.
    pub fn into_chain(mut self) -> Vec<String> {
        self.layers.push(self.kind.to_string());
        dedup_first(&mut self.layers);
        self.layers
    }

    fn bare(kind: &str) -> Self {
        Self {
            notices: Vec::new(),
            kind_defaults: false,
            kind_base: KindBase::default(),
            sources: TuningSources::default(),
            kind: AgentKind::new_unchecked(kind),
            variant: None,
            empty_dir: None,
            timeout: None,
            layers: Vec::new(),
            launch: crate::agents::LaunchParams::default(),
            auto_compact: None,
            auto_compact_default: None,
            fast: None,
            code_mode: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            seat_consensus_skill: false,
            mcps: None,
            tools: None,
            allowed_tools: None,
            isolation_default: None,
            args: None,
        }
    }

    fn fill_missing(&mut self, layer: &Profile, name: &str) {
        if layer.kind_base {
            self.kind_base = KindBase {
                effort: layer.effort.clone(),
                auto_compact: layer.auto_compact.clone(),
                fast: layer.fast,
                code_mode: layer.code_mode,
                built_in: layer.built_in_base,
            };
        }
        let written = !layer.kind_base;
        let writers = layer.definition.as_ref().map(|def| def.writers());
        let source = |writer: Option<&String>| {
            Some(TuningSource::Profile(
                writer.cloned().unwrap_or_else(|| name.to_owned()),
            ))
        };
        self.variant = self.variant.or(layer.variant);
        self.kind_defaults |= layer.kind_defaults;
        self.empty_dir = self.empty_dir.or(layer.empty_dir);
        self.timeout = self.timeout.or(layer.timeout);
        self.isolation_default = self.isolation_default.or(layer.isolation);
        if self.skills.is_none() {
            self.skills.clone_from(&layer.skills);
            self.seat_consensus_skill = layer.seat_consensus_skill;
        }
        if self.mcps.is_none() {
            self.mcps.clone_from(&layer.mcps);
        }
        if self.tools.is_none() {
            self.tools.clone_from(&layer.tools);
        }
        if self.allowed_tools.is_none() {
            self.allowed_tools.clone_from(&layer.allowed_tools);
        }
        self.launch.mode = self.launch.mode.or(layer.mode);
        if self.launch.model.is_none() {
            self.launch.model.clone_from(&layer.model);
            self.launch.tier.clone_from(&layer.tier_stamp);
        }
        if written && self.launch.effort.is_none() && layer.effort.is_some() {
            self.launch.effort.clone_from(&layer.effort);
            self.sources.effort = source(writers.and_then(|w| w.effort.as_ref()));
        }
        if self.launch.budget.is_none() {
            self.launch.budget.clone_from(&layer.budget);
        }
        if written && self.auto_compact.is_none() && layer.auto_compact.is_some() {
            self.auto_compact.clone_from(&layer.auto_compact);
            self.sources.auto_compact = source(writers.and_then(|w| w.auto_compact.as_ref()));
        }
        if self.auto_compact_default.is_none() {
            self.auto_compact_default = layer.auto_compact_default;
        }
        if written && self.fast.is_none() && layer.fast.is_some() {
            self.fast = layer.fast;
            self.sources.fast = source(writers.and_then(|w| w.fast.as_ref()));
        }
        if written && self.code_mode.is_none() && layer.code_mode.is_some() {
            self.code_mode = layer.code_mode;
            self.sources.code_mode = source(writers.and_then(|w| w.code_mode.as_ref()));
        }
        if self.system_prompt_file.is_none() {
            self.system_prompt_file
                .clone_from(&layer.system_prompt_file);
        }
        if !layer.append_system_prompt_files.is_empty() {
            let mut fragments = layer.append_system_prompt_files.clone();
            fragments.append(&mut self.append_system_prompt_files);
            self.append_system_prompt_files = fragments;
        }
        if self.args.is_none() {
            self.args.clone_from(&layer.args);
        }
    }

    /// Fill what nothing wrote: the kind base's tuning first, then the built-in effort a
    /// definition profile takes for its final kind and model.
    fn fill_kind_defaults(&mut self) {
        let base = &self.kind_base;
        let from_base = || {
            let kind = self.kind.clone();
            Some(if base.built_in {
                TuningSource::BuiltInKindBase(kind)
            } else {
                TuningSource::KindBase(kind)
            })
        };
        if self.fast.is_none() && base.fast.is_some() {
            self.fast = base.fast;
            self.sources.fast = from_base();
        }
        if self.code_mode.is_none() && base.code_mode.is_some() {
            self.code_mode = base.code_mode;
            self.sources.code_mode = from_base();
        }
        if self.auto_compact.is_none() && base.auto_compact.is_some() {
            self.auto_compact.clone_from(&base.auto_compact);
            self.sources.auto_compact = from_base();
        }
        if self.launch.effort.is_none() && base.effort.is_some() {
            self.launch.effort.clone_from(&base.effort);
            self.sources.effort = from_base();
        }
        if !self.kind_defaults || self.launch.effort.is_some() {
            return;
        }
        let defaults = crate::agents::definition_defaults(&self.kind, self.launch.model.as_deref());
        self.launch.effort = defaults.effort.map(str::to_owned);
        self.sources.effort = self.launch.effort.as_ref().map(|_| TuningSource::BuiltIn);
    }

    fn clear_kind_defaults(&mut self) {
        let sources = &mut self.sources;
        if TuningSource::is_kind_default(&sources.effort) {
            self.launch.effort = None;
            sources.effort = None;
        }
        if TuningSource::is_kind_default(&sources.auto_compact) {
            self.auto_compact = None;
            sources.auto_compact = None;
        }
        if TuningSource::is_kind_default(&sources.fast) {
            self.fast = None;
            sources.fast = None;
        }
        if TuningSource::is_kind_default(&sources.code_mode) {
            self.code_mode = None;
            sources.code_mode = None;
        }
    }

    /// A role's own `fast`, on, off, or `auto`, over everything the chain and an `--agent` override set.
    fn apply_role_fast(&mut self, binding: &RoleBinding) {
        if binding.fast.is_some() {
            self.fast = binding.fast;
            self.sources.fast = Some(TuningSource::Role(binding.role.clone()));
        }
    }

    fn effective_auto_compact(&self) -> Option<String> {
        self.auto_compact.clone().or_else(|| {
            self.auto_compact_default?
                .resolve_for_kind(&self.kind)
                .map(|tokens| tokens.to_string())
        })
    }

    fn apply_role(&mut self, binding: &RoleBinding) {
        self.clear_kind_defaults();
        if binding.model.is_some() {
            self.launch.tier = None;
        }
        if let Some(mode) = binding.mode {
            self.launch.mode = Some(mode);
        }
        self.apply_role_fast(binding);
        let role = || Some(TuningSource::Role(binding.role.clone()));
        if binding.effort.is_some() {
            self.launch.effort.clone_from(&binding.effort);
            self.sources.effort = role();
        }
        if binding.auto_compact.is_some() {
            self.auto_compact.clone_from(&binding.auto_compact);
            self.sources.auto_compact = role();
        }
        for (target, source) in [
            (&mut self.launch.model, &binding.model),
            (&mut self.launch.budget, &binding.budget),
        ] {
            if source.is_some() {
                target.clone_from(source);
            }
        }
        if binding.system_prompt_file.is_some() {
            self.system_prompt_file
                .clone_from(&binding.system_prompt_file);
        }
        self.append_system_prompt_files
            .extend(binding.append_system_prompt_files.iter().cloned());
        if binding.args.is_some() {
            self.args.clone_from(&binding.args);
        }
        self.fill_kind_defaults();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSpec {
    pub word: String,
    pub candidates: Vec<SpecCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecCandidate {
    pub name: String,
    pub kind: SpecKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecKind {
    /// The profile as a listing shows it: its effort and fast as [`listed_tuning`] gives them.
    Profile(Box<Profile>),
    Command,
    Team(Option<String>),
    Kind,
    Cell,
    Role,
}

impl UnknownSpec {
    fn closest(&self) -> Vec<&SpecCandidate> {
        crate::utils::suggest::nearest_names(
            &self.word,
            self.candidates
                .iter()
                .map(|candidate| candidate.name.as_str()),
        )
        .into_iter()
        .filter_map(|name| {
            self.candidates
                .iter()
                .find(|candidate| candidate.name == name)
        })
        .collect()
    }

    fn suggestions(&self) -> String {
        let closest = self.closest();
        if closest.is_empty() {
            return self.choices();
        }
        format!(
            "did you mean: {}",
            closest
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    pub fn choices(&self) -> String {
        type KindPredicate = fn(&SpecKind) -> bool;

        let mut lines = Vec::new();
        let groups: [(&str, KindPredicate); 3] = [
            ("teams:", |kind| matches!(kind, SpecKind::Team(_))),
            ("profiles:", |kind| {
                matches!(kind, SpecKind::Profile(_) | SpecKind::Command)
            }),
            ("kinds:", |kind| matches!(kind, SpecKind::Kind)),
        ];
        for (label, predicate) in groups {
            let mut names = self
                .candidates
                .iter()
                .filter(|candidate| predicate(&candidate.kind))
                .map(|candidate| candidate.name.as_str())
                .peekable();
            if names.peek().is_none() {
                continue;
            }
            lines.push(wrap_choices(label, names));
        }
        let modes = PERMISSION_MODE_NAMES
            .iter()
            .map(|mode| format!("-{mode}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "              (each also as <kind>{modes}, -fast where supported)"
        ));
        lines.join("\n")
    }
}

impl std::fmt::Display for UnknownSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no team, profile, or command named `{}`\n{}",
            self.word,
            self.suggestions()
        )
    }
}

pub fn team_summary(team: &Team, profiles: &ProfilesConfig) -> Option<String> {
    if let Some(layout) = &team.layout {
        return Some(layout.clone());
    }
    let mut kinds = Vec::new();
    for binding in &team.roles {
        let kind = resolve_profile(&binding.profile, profiles)
            .ok()?
            .kind
            .to_string();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    (!kinds.is_empty()).then(|| kinds.join(","))
}

fn subagent_profile_hint(name: &str, names_flag: bool) -> String {
    if names_flag {
        format!("launch it with `rimz agents {name} --subagent-profile`, or ")
    } else {
        String::new()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutErr {
    #[error("{definition} keep-warm while `{clause}`: {reason}")]
    KeepWarm {
        definition: String,
        clause: String,
        reason: String,
    },
    #[error("layout spec is empty")]
    Empty,
    /// The engine module could not choose an agent cell's engine under the launch's flags.
    #[error("{0}")]
    Engine(String),
    /// A resumed profile now runs on another kind and does not build under the session's.
    #[error("now resolves to {now}, but this session is {was}")]
    KindChanged { was: String, now: String },
    #[error("empty layout cell in `{0}`")]
    EmptyCell(String),
    #[error("a layout column uses `+` (tile) or `/` (stack), not both: `{column}`")]
    MixedRowOperators { column: String },
    #[error("unknown layout cell `{}`\n{}", .0.word, .0.suggestions())]
    UnknownCell(Box<UnknownSpec>),
    #[error("{0}")]
    UnknownSpec(Box<UnknownSpec>),
    #[error(
        "profile `{name}` is defined under [{defined_in}.profiles], but {command} resolves [{expected_in}.profiles]; {}move or copy the profile to the expected section",
        subagent_profile_hint(.name, *.names_flag)
    )]
    ProfileWrongDoorway {
        name: String,
        defined_in: &'static str,
        command: &'static str,
        expected_in: &'static str,
        /// The refusal names `rimz agents <name> --subagent-profile`.
        names_flag: bool,
    },
    #[error("team `{team}` has no role `{role}`; declared roles: {valid_roles}{suggestion}")]
    UnknownRoleInTeam {
        team: String,
        role: String,
        valid_roles: String,
        suggestion: Box<str>,
    },
    #[error(
        "team `{team}` leader `{leader}` is not a declared role; declared roles: {valid_roles}"
    )]
    UnknownLeaderRole {
        team: String,
        leader: String,
        valid_roles: String,
    },
    #[error("team `{team}` must declare a `leader`; {fix}")]
    MissingLeader { team: String, fix: String },
    #[error(
        "the launch prompt leader `{token}` is ambiguous because this layout has several `{token}` cells; give the first cell an inline role (`{token}:lead,{token}`), use a unique profile plus `leader` on a named team, or launch without a prompt and `rimz message` the right agent"
    )]
    AmbiguousPromptLeader { token: String },
    #[error("this layout has no agent cell to receive a prompt")]
    NoPromptTarget,
    #[error("invalid team name `{name}`; team names cannot contain `.` or `/`")]
    InvalidTeamName { name: String },
    #[error(
        "team name `{0}` is reserved for an inline profile/command cell; choose another team name"
    )]
    ReservedTeamName(String),
    #[error(
        "`{spec}` is ambiguous in `#{channel}`: team `{team}` declares role `{spec}` and a `{spec}` profile or command resolves to a different agent; launch `{team}.{spec}` for the role, or rename one of them"
    )]
    AmbiguousInChannel {
        spec: String,
        channel: String,
        team: String,
    },
    #[error("team `{team}` must declare at least one role")]
    EmptyTeam { team: String },
    #[error("team `{team}` role `{role}` signal binding {index}: {source}")]
    InvalidTeamSignal {
        team: String,
        role: String,
        index: usize,
        source: Box<crate::harness::signal::SelectorErr>,
    },
    #[error(
        "team `{team}` role `{role}` signal binding {index} (`{signal}`) requires match.handle or match.session"
    )]
    UnscopedTeamAgentSignal {
        team: String,
        role: String,
        index: usize,
        signal: String,
    },
    #[error("team `{team}` role `{role}` references unknown profile `{profile}`")]
    UnknownRoleProfile {
        team: String,
        role: String,
        profile: String,
    },
    #[error("team profile `{definition}` sets empty-dir; teams share the worktree")]
    TeamEmptyDir { definition: String },
    #[error(
        "invalid role name `{name}` in team `{team}`; roles cannot be empty or contain whitespace, `,`, `+`, `/`, `:`, or `#`; stage owners must also omit control characters and parentheses"
    )]
    InvalidRoleName { team: String, name: String },
    #[error("duplicate role `{role}` in team `{team}`")]
    DuplicateRole { team: String, role: String },
    #[error("team `{team}` layout token `{role}` is not a declared role or valid roleless cell")]
    UnknownRoleInLayout { team: String, role: String },
    #[error("team `{team}` layout does not place role `{role}`")]
    RoleNotPlaced { team: String, role: String },
    #[error("team `{team}` layout places role `{role}` more than once")]
    DuplicateRoleInLayout { team: String, role: String },
    #[error(
        "team `{team}` sets consensus-file, consensus, or append-system-prompt-files but declares no stages; add stages (or owns) or remove the team prompt fields"
    )]
    TeamPromptWithoutStages { team: String },
    #[error(
        "team `{team}` sets consensus-file, consensus, or append-system-prompt-files but role `{role}` ({kind}) has no base system prompt to compose them onto; add system-prompt-file to role {role} or its profile{kind_base_fix}",
        kind_base_fix = team_kind_base_fix(.kind)
    )]
    TeamPromptWithoutBase {
        team: String,
        role: String,
        kind: AgentKind,
    },
    #[error("invalid scratch-files pattern {pattern:?} in team `{team}`: {reason}")]
    InvalidScratchPattern {
        team: String,
        pattern: String,
        reason: &'static str,
    },
    #[error(
        "invalid stage name {name:?} in team `{team}`; use a nonempty name without surrounding whitespace or control characters"
    )]
    InvalidStageName { team: String, name: String },
    #[error("duplicate stage `{name}` in team `{team}`")]
    DuplicateStage { team: String, name: String },
    #[error(
        "reserved stage `{name}` in team `{team}`; `Done` is implicit and always last; remove it from stages and owns"
    )]
    ReservedStage { team: String, name: String },
    #[error(
        "duplicate owner for stage `{stage}` in team `{team}`: {roles:?}; keep exactly one owns entry"
    )]
    DuplicateStageOwner {
        team: String,
        stage: String,
        roles: [String; 2],
    },
    #[error(
        "unknown owned stage `{stage}` for role `{role}` in team `{team}`; add \"{stage}\" to stages or drop it from owns"
    )]
    UnknownOwnedStage {
        team: String,
        role: String,
        stage: String,
    },
    #[error(
        "role `{role}` in team `{team}` enables flip-compact but agent kind `{kind}` has no compact command; set flip-compact = \"off\" on role {role}, set [harness] flip_compact = \"off\", or choose a supporting agent"
    )]
    FlipCompactUnsupported {
        team: String,
        role: String,
        kind: AgentKind,
    },
    #[error("invalid profile `{profile}`: {reason}")]
    InvalidProfile { profile: String, reason: String },
    /// The engine chosen for a cell does not build it.
    #[error("invalid profile `{profile}`: cannot run on {kind}: {reason}")]
    CannotRun {
        profile: String,
        kind: String,
        reason: String,
    },
    #[error("{0}")]
    FailedProfile(String),
    #[error(
        "unknown agent profile or kind `{value}` for --agent; valid profiles and kinds: {valid}"
    )]
    UnknownAgentOverride { value: String, valid: String },
    #[error("invalid command `{command}`: {reason}")]
    InvalidCommand { command: String, reason: String },
    #[error("profile `{profile}` references unknown base `{base}`")]
    UnknownProfileBase { profile: String, base: String },
    #[error(
        "repo profile `{profile}` references machine-only profile `{base}`; repo profiles may inherit only repo profiles or built-in agent kinds"
    )]
    RepoProfileEscapesTrust { profile: String, base: String },
    #[error(
        "repo profile `{profile}` sets isolation; declare it in ~/.rimz/agents/<name>.md or pass --isolation"
    )]
    RepoProfileSetsIsolation { profile: String },
    #[error(
        "invalid profile name `{name}`; profiles cannot be empty or contain whitespace, `,`, `+`, `/`, `(`, or `)`"
    )]
    InvalidProfileName { name: String },
    #[error("profile name `{name}` is reserved for `rimz agents`")]
    ReservedProfileName { name: String },
    #[error(
        "profile name `{name}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    ProfileShadowsAddress { name: String, reason: &'static str },
    #[error(
        "role name `{name}` in team `{team}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    RoleShadowsAddress {
        team: String,
        name: String,
        reason: &'static str,
    },
    #[error(
        "invalid inline role name `{name}`; roles cannot be empty or contain whitespace, `,`, `+`, `/`, `:`, or `#`"
    )]
    InvalidInlineRole { name: String },
    #[error(
        "inline role name `{name}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    InlineRoleShadowsAddress { name: String, reason: &'static str },
    #[error("duplicate inline role `{role}` in layout spec")]
    DuplicateInlineRole { role: String },
    #[error(
        "layout cell `{cell}` cannot have inline role `{role}`; roles apply only to agent cells"
    )]
    RoleOnCommandCell { cell: String, role: String },
    #[error(
        "invalid command name `{name}`; commands cannot be empty or contain whitespace, `,`, `+`, or `/`"
    )]
    InvalidCommandName { name: String },
    #[error("command name `{name}` is reserved for `rimz agents`")]
    ReservedCommandName { name: String },
    #[error(
        "`[subagents.profiles.{profile}]` sets `subagents`, but a subagent cannot launch; remove the field"
    )]
    SubagentAllowlistOnSubagentProfile { profile: String },
    #[error(
        "profile `{profile}` allows subagent `{entry}`, which is not a subagent profile, a command, or an agent kind; valid: {valid}"
    )]
    UnknownSubagentAllowlistEntry {
        profile: String,
        entry: String,
        valid: String,
    },
    #[error(
        "profile `{profile}` allows subagent `{entry}`: `+` entries add to an inherited list in Markdown definitions only; list the bare name"
    )]
    PlusSubagentEntry { profile: String, entry: String },
    #[error(
        "launch refused: profile `{caller_profile}` may only launch subagent profiles {allowed}; `{profile}` is not one of them. Pick an allowed profile or ask the user to widen the `subagents` list of profile `{caller_profile}`."
    )]
    SubagentProfileNotAllowed {
        caller_profile: String,
        profile: String,
        allowed: String,
    },
}

pub(crate) type Result<T> = std::result::Result<T, LayoutErr>;

/// Resolve agent prompt files against the directory of the config source that
/// declared them. `~` expands to the home directory and a relative path roots
/// at that source directory, not the agent's launch cwd. Call this before
/// combining config sources so a drop-in `team.toml` keeps paths relative to
/// its own directory. Pure — the file's existence is checked at the launch
/// entry point, so a moved prompt never breaks an unrelated config read.
pub(crate) fn resolve_prompt_paths(
    profiles: &mut ProfilesConfig,
    teams: &mut TeamsConfig,
    source_dir: &Path,
) {
    resolve_profile_prompt_paths(profiles, source_dir);
    for team in teams.0.values_mut() {
        for binding in &mut team.roles {
            if let Some(crate::config::PromptSource::File(path)) =
                binding.system_prompt_file.as_mut()
            {
                *path = resolve_prompt_path(path, source_dir);
            }
            for source in &mut binding.append_system_prompt_files {
                if let crate::config::PromptSource::File(path) = source {
                    *path = resolve_prompt_path(path, source_dir);
                }
            }
        }
        if let Some(path) = team.consensus_file.as_mut() {
            *path = resolve_prompt_path(path, source_dir);
        }
        for source in &mut team.append_system_prompt_files {
            if let crate::config::PromptSource::File(path) = source {
                *path = resolve_prompt_path(path, source_dir);
            }
        }
    }
}

/// Resolve prompt files declared by profiles against their config source.
pub(crate) fn resolve_profile_prompt_paths(profiles: &mut ProfilesConfig, source_dir: &Path) {
    for profile in profiles.0.values_mut() {
        if let Some(crate::config::PromptSource::File(path)) = profile.system_prompt_file.as_mut() {
            *path = resolve_prompt_path(path, source_dir);
        }
        for source in &mut profile.append_system_prompt_files {
            if let crate::config::PromptSource::File(path) = source {
                *path = resolve_prompt_path(path, source_dir);
            }
        }
    }
}

fn resolve_prompt_path(path: &Path, config_dir: &Path) -> PathBuf {
    let expanded = crate::agents::transcript_fs::expand_tilde(&path.to_string_lossy());
    if expanded.is_absolute() {
        expanded
    } else {
        config_dir.join(expanded)
    }
}

pub(crate) fn validate_config(
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<()> {
    validate_profile_namespace(profiles, commands, teams)?;
    for (name, team) in &teams.0 {
        validate_team(name, team, profiles, commands)?;
    }
    Ok(())
}

/// Validate team structure that does not depend on resolving seat profiles.
pub(crate) fn validate_team_structure(
    name: &str,
    team: &Team,
    declared_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    validate_team_names(std::iter::once(name))?;
    if is_cell_word(name, declared_profiles, commands) {
        return Err(LayoutErr::ReservedTeamName(name.to_owned()));
    }
    validate_team_header(name, team)?;
    if let Some(raw) = team.layout.as_deref() {
        let structure = parse_layout_structure(raw)?;
        let mut placements: BTreeMap<&str, usize> = team
            .roles
            .iter()
            .map(|binding| (binding.role.as_str(), 0usize))
            .collect();
        for cell_name in structure.cells() {
            if let Some(count) = placements.get_mut(cell_name) {
                *count += 1;
                continue;
            }
            if !is_cell_word(cell_name, declared_profiles, commands)
                && path_command_cell(cell_name).is_none()
            {
                return Err(LayoutErr::UnknownRoleInLayout {
                    team: name.to_owned(),
                    role: cell_name.to_owned(),
                });
            }
        }
        check_role_placements(name, placements)?;
    }
    Ok(())
}

fn check_role_placements<'a>(
    team_name: &str,
    placements: impl IntoIterator<Item = (&'a str, usize)>,
) -> Result<()> {
    for (role, count) in placements {
        match count {
            0 => {
                return Err(LayoutErr::RoleNotPlaced {
                    team: team_name.to_owned(),
                    role: role.to_owned(),
                });
            }
            1 => {}
            _ => {
                return Err(LayoutErr::DuplicateRoleInLayout {
                    team: team_name.to_owned(),
                    role: role.to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Validate one team before its seats and name enter the merged namespace.
pub(crate) fn validate_team(
    name: &str,
    team: &Team,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    validate_team_name(name, profiles, commands)?;
    validate_team_roster(name, team, profiles, commands)
}

/// The checks of a team's name alone, which the namespace check makes for every team.
pub(crate) fn validate_team_name(
    name: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    validate_team_names(std::iter::once(name))?;
    if is_cell_word(name, profiles, commands) {
        return Err(LayoutErr::ReservedTeamName(name.to_owned()));
    }
    Ok(())
}

/// Validate one team past its name: its roles, seats, and layout.
pub(crate) fn validate_team_roster(
    name: &str,
    team: &Team,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    let prepared = prepare_team(name, team, profiles, commands, CellBase::none(), None)?;
    if team.layout.is_some() {
        compile_team(name, prepared, profiles, commands, CellBase::none())?;
    }
    Ok(())
}

/// Validate profile names and cell-name collisions without resolving every
/// profile chain or requiring team role bindings to close over the namespace.
pub(crate) fn validate_profile_namespace(
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<()> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_team_names(teams.0.keys().map(String::as_str))?;
    for name in teams.0.keys() {
        if is_cell_word(name, profiles, commands) {
            return Err(LayoutErr::ReservedTeamName(name.clone()));
        }
    }
    Ok(())
}

/// Validate the profile namespace dedicated to the `rimz subagents` doorway.
pub(crate) fn validate_subagent_profile_namespace(
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<()> {
    validate_profile_namespace(profiles, commands, teams)?;
    if let Some((profile, _)) = profiles
        .0
        .iter()
        .find(|(_, profile)| profile.subagents.is_some())
    {
        return Err(LayoutErr::SubagentAllowlistOnSubagentProfile {
            profile: profile.clone(),
        });
    }
    Ok(())
}

pub(crate) fn validate_subagent_allowlists(
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    let valid = valid_cells(subagent_profiles, commands);
    for (profile_name, profile) in &profiles.0 {
        let Some(allowed) = profile.subagents.as_ref() else {
            continue;
        };
        for entry in allowed {
            if is_cell_word(entry, subagent_profiles, commands) {
                continue;
            }
            if entry.trim_start().starts_with('+') {
                return Err(LayoutErr::PlusSubagentEntry {
                    profile: profile_name.clone(),
                    entry: entry.clone(),
                });
            }
            return Err(LayoutErr::UnknownSubagentAllowlistEntry {
                profile: profile_name.clone(),
                entry: entry.clone(),
                valid: valid.clone(),
            });
        }
    }
    Ok(())
}

/// Resolve every profile chain in a standalone profile namespace.
pub(crate) fn validate_profile_chains(profiles: &ProfilesConfig) -> Result<()> {
    for name in profiles.0.keys() {
        resolve_profile(name, profiles)?;
    }
    Ok(())
}

/// The team a launch spec names, for the whole-team form (`forge`) and the
/// single-role form (`forge.planner`). `None` when the spec names no team.
pub(super) fn spec_team<'a>(spec: &'a str, teams: &TeamsConfig) -> Option<&'a str> {
    let spec = spec.trim();
    if teams.0.contains_key(spec) {
        return Some(spec);
    }
    let (team, _) = spec.split_once('.')?;
    teams.0.contains_key(team).then_some(team)
}

/// Qualify a bare role spec with the team that owns the current lane.
///
/// Inside `#forge`, `reviewer` means `forge.reviewer`. Returns the spec
/// unchanged when no team owns the lane, when the spec already names a team or
/// a multi-cell inline layout, or when the team declares no such role.
///
/// A role bound to its same-named profile refines that profile — same agent,
/// plus role and team identity — so it qualifies silently. A role bound to a
/// different profile than a same-named global cell is a genuine collision and
/// refuses.
pub fn qualify_spec_in_channel<'a>(
    raw: &'a str,
    channel: &str,
    channel_team: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<Cow<'a, str>> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains(['.', ',', '+', '/', ':'])
        || teams.0.contains_key(trimmed)
    {
        return Ok(Cow::Borrowed(raw));
    }
    // A stale stamp can name a team the config no longer declares.
    let Some(team) = teams.0.get(channel_team) else {
        return Ok(Cow::Borrowed(raw));
    };
    let Some(binding) = team.roles.iter().find(|binding| binding.role == trimmed) else {
        return Ok(Cow::Borrowed(raw));
    };
    if is_cell_word(trimmed, profiles, commands) && binding.profile != trimmed {
        return Err(LayoutErr::AmbiguousInChannel {
            spec: trimmed.to_owned(),
            channel: channel.to_owned(),
            team: channel_team.to_owned(),
        });
    }
    Ok(Cow::Owned(format!("{channel_team}.{trimmed}")))
}

pub fn parse_layout_spec(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    parse_layout_spec_validated(raw, profiles, commands, CellBase::none())
}

#[cfg(test)]
fn resolve_spec(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<LayoutSpec> {
    resolve_spec_with_agent_override(arg, profiles, profiles, commands, teams, None)
}

#[cfg(test)]
pub(crate) fn resolve_spec_with_agent_override(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    team_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    agent_override: Option<&str>,
) -> Result<LayoutSpec> {
    resolve_spec_in_scope(
        arg,
        profiles,
        team_profiles,
        None,
        commands,
        teams,
        &Selection::default().agent(agent_override),
    )
}

pub(super) fn resolve_spec_in_scope(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    team_profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    selection: &Selection,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_team_names(teams.0.keys().map(String::as_str))?;
    let base_override = resolve_agent_override(selection.agent_name(), profiles, fallback)?;
    crate::config::engine::check_flags(
        base_override.as_ref().map(x_side).as_ref(),
        selection.model_flag(),
        selection.tier_flag(),
    )
    .map_err(|error| LayoutErr::Engine(error.to_string()))?;
    let base = CellBase {
        x: base_override.as_ref(),
        selection,
    };
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::shell()));
    };
    if teams.0.contains_key(raw) {
        if !commands.0.contains_key(raw)
            && let Some(detail) = profiles.failed(raw)
        {
            return Err(LayoutErr::FailedProfile(detail.to_owned()));
        }
        if is_cell_word(raw, profiles, commands) {
            return Err(LayoutErr::ReservedTeamName(raw.to_owned()));
        }
        return resolve_team_with_base_override(raw, teams, team_profiles, commands, base);
    }
    if let Some((team, role)) = teams.role_spec(raw) {
        return resolve_team_role(team, role, teams, team_profiles, commands, base);
    }
    if is_inline_spec(raw, profiles, commands) {
        return parse_layout_spec_validated(raw, profiles, commands, base);
    }
    if let Some(cell) = path_command_cell(raw) {
        return Ok(LayoutSpec::single(cell));
    }
    let (cell, role) = split_inline_role(raw, profiles, commands);
    if let Some(role) = role.filter(|_| path_command_cell(cell).is_some()) {
        return Err(LayoutErr::RoleOnCommandCell {
            cell: cell.to_owned(),
            role: role.to_owned(),
        });
    }
    Err(LayoutErr::UnknownSpec(Box::new(UnknownSpec {
        word: raw.to_owned(),
        candidates: spec_candidates(profiles, team_profiles, commands, teams),
    })))
}

pub fn resolve_team(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    resolve_team_under(name, teams, profiles, commands, no_flags())
}

/// [`resolve_team`] under a launch's selection, which reaches every agent cell of the team.
pub(crate) fn resolve_team_under(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    selection: &Selection,
) -> Result<LayoutSpec> {
    let base_override = resolve_agent_override(selection.agent_name(), profiles, None)?;
    resolve_team_with_base_override(
        name,
        teams,
        profiles,
        commands,
        CellBase {
            x: base_override.as_ref(),
            selection,
        },
    )
}

fn resolve_team_with_base_override(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<LayoutSpec> {
    let team = teams
        .0
        .get(name)
        .expect("team resolution called with a known team name");
    let mut prepared = prepare_team(name, team, profiles, commands, base, None)?;
    resolve_team_prompts(name, &mut prepared)?;
    validate_flip_compact(
        name,
        &prepared,
        crate::config::MachineConfig::load_lenient()
            .harness
            .flip_compact,
    )?;
    Ok(compile_team(name, prepared, profiles, commands, base)?.layout)
}

/// Resolve the one agent cell that receives a trailing launch prompt: a team's leader seat, the
/// cell team compilation marks `leads_team`, else the first agent cell when no other carries its
/// token.
pub fn prompt_leader(layout: &LayoutSpec) -> Result<usize> {
    let agent_cells = layout.agent_cells().collect::<Vec<_>>();
    match agent_cells.as_slice() {
        [] => return Err(LayoutErr::NoPromptTarget),
        [_] => return Ok(0),
        _ => {}
    }
    if let Some(leader) = agent_cells.iter().position(|cell| cell.leads_team) {
        return Ok(leader);
    }

    let first = agent_cells[0];
    if first.launch.role.is_some() {
        return Ok(0);
    }
    let token = prompt_leader_token(first).expect("agent cells have a profile or kind token");
    if agent_cells
        .iter()
        .skip(1)
        .any(|cell| prompt_leader_token(cell) == Some(token))
    {
        return Err(LayoutErr::AmbiguousPromptLeader {
            token: token.to_owned(),
        });
    }
    Ok(0)
}

fn prompt_leader_token(cell: &AgentCell) -> Option<&str> {
    Some(cell.launch.profile.as_deref().unwrap_or(cell.kind.as_str()))
}

/// A layout word's token as written: the profile of the cell it builds, else its kind, read with
/// no `--agent`, `--tier`, `--model` or tier fallback, so that a roleless team's `leader` names
/// the cell the layout writes, whatever a launch routes it to. A command cell has none.
fn written_token(
    word: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<Option<String>> {
    Ok(
        match parse_cell(word, profiles, commands, CellBase::none())? {
            Cell::Agent(cell) => prompt_leader_token(&cell).map(str::to_owned),
            Cell::Command { .. } => None,
        },
    )
}

/// Mark the one cell of `layout` that holds team `team_name`'s leader seat
/// ([`AgentCell::leads_team`]): the cell of the role `leader` names, or in a roleless team the
/// cell whose [`written_token`] is `leader`.
fn mark_team_leader(
    team_name: &str,
    team: &Team,
    leader: &str,
    layout: &mut LayoutSpec,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    let seats = if team.roles.is_empty() {
        // `validate_team_header` refuses a team with neither roles nor a layout.
        parse_layout_structure(team.layout.as_deref().unwrap_or_default())?
            .cells()
            .map(|word| Ok(written_token(word, profiles, commands)?.as_deref() == Some(leader)))
            .collect::<Result<Vec<_>>>()?
    } else {
        layout
            .columns
            .iter()
            .flat_map(|column| &column.rows)
            .map(|cell| {
                matches!(cell, Cell::Agent(cell) if cell.launch.role.as_deref() == Some(leader))
            })
            .collect()
    };
    match seats.iter().filter(|seat| **seat).count() {
        1 => {}
        0 => {
            return Err(LayoutErr::UnknownLeaderRole {
                team: team_name.to_owned(),
                leader: leader.to_owned(),
                valid_roles: valid_team_roles(team),
            });
        }
        _ => {
            return Err(LayoutErr::AmbiguousPromptLeader {
                token: leader.to_owned(),
            });
        }
    }
    for (cell, seat) in layout
        .columns
        .iter_mut()
        .flat_map(|column| &mut column.rows)
        .zip(seats)
    {
        if let Cell::Agent(cell) = cell {
            cell.leads_team = seat;
        }
    }
    Ok(())
}

fn resolve_team_role(
    team_name: &str,
    role_name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<LayoutSpec> {
    let team = teams
        .0
        .get(team_name)
        .expect("team role resolution called with a known team name");
    let mut prepared = prepare_team(team_name, team, profiles, commands, base, Some(role_name))?;
    resolve_team_prompts(team_name, &mut prepared)?;
    validate_flip_compact(
        team_name,
        &prepared,
        crate::config::MachineConfig::load_lenient()
            .harness
            .flip_compact,
    )?;
    // The layout-only cells are built but not started, so no flag reaches them.
    let unlaunched = base.selection.without_flags();
    let compiled = compile_team(
        team_name,
        prepared,
        profiles,
        commands,
        CellBase {
            x: None,
            selection: &unlaunched,
        },
    )?;
    let Some(cell) = compiled.roles.get(role_name) else {
        return Err(LayoutErr::UnknownRoleInTeam {
            team: team_name.to_owned(),
            role: role_name.to_owned(),
            valid_roles: valid_team_roles(team),
            suggestion: crate::utils::suggest::nearest_name(
                role_name,
                team.roles.iter().map(|binding| binding.role.as_str()),
            )
            .map_or_else(String::new, |role| {
                format!("\ndid you mean: {team_name}.{role}")
            })
            .into_boxed_str(),
        });
    };
    Ok(LayoutSpec::single(Cell::Agent(cell.clone())))
}

struct PreparedRole {
    role: String,
    profile: String,
    resolved: ResolvedProfile,
    rendered: (Vec<String>, Vec<CellNotice>),
    team_prompt: Option<TeamPrompt>,
}

struct PreparedTeam<'a> {
    team: &'a Team,
    roles: Vec<PreparedRole>,
}

struct CompiledTeam {
    layout: LayoutSpec,
    roles: BTreeMap<String, AgentCell>,
}

fn compile_team(
    team_name: &str,
    prepared: PreparedTeam<'_>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<CompiledTeam> {
    let mut role_cells = BTreeMap::new();
    for mut role in prepared.roles {
        normalize_budget(&mut role.resolved.launch.budget, &role.profile)?;
        let mut cell = agent_cell_from(
            &role.resolved,
            role.rendered,
            Some(role.profile),
            Some(role.role.clone()),
            role.resolved.launch.mode,
        );
        cell.team_prompt = role.team_prompt;
        role_cells.insert(role.role, cell);
    }
    let mut layout = if let Some(raw) = prepared.team.layout.as_deref() {
        compile_team_layout(team_name, raw, &role_cells, profiles, commands, base)?
    } else {
        LayoutSpec {
            columns: prepared
                .team
                .roles
                .iter()
                .map(|binding| Column {
                    rows: vec![Cell::Agent(
                        role_cells
                            .get(&binding.role)
                            .expect("prepared role has a compiled cell")
                            .clone(),
                    )],
                    stacked: false,
                })
                .collect(),
        }
    };
    if let Some(leader) = prepared.team.leader.as_deref() {
        mark_team_leader(
            team_name,
            prepared.team,
            leader,
            &mut layout,
            profiles,
            commands,
        )?;
    }
    Ok(CompiledTeam {
        layout,
        roles: role_cells,
    })
}

fn compile_team_layout(
    team_name: &str,
    raw: &str,
    role_cells: &BTreeMap<String, AgentCell>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<LayoutSpec> {
    let structure = parse_layout_structure(raw)?;
    let mut placements: BTreeMap<&str, usize> = role_cells
        .keys()
        .map(|role| (role.as_str(), 0usize))
        .collect();
    let layout = fold_layout(structure, |cell_name| {
        if let Some(cell) = role_cells.get(cell_name) {
            *placements
                .get_mut(cell_name)
                .expect("placement map mirrors role cells") += 1;
            return Ok(Cell::Agent(cell.clone()));
        }
        parse_cell(cell_name, profiles, commands, base).map_err(|err| match err {
            LayoutErr::UnknownCell(_) => LayoutErr::UnknownRoleInLayout {
                team: team_name.to_owned(),
                role: cell_name.to_owned(),
            },
            err => err,
        })
    })?;

    check_role_placements(team_name, placements)?;

    Ok(layout)
}

/// Resolve a role binding: its profile chain, then the binding's own overrides on top.
pub fn resolve_role(binding: &RoleBinding, profiles: &ProfilesConfig) -> Result<ResolvedProfile> {
    let mut resolved = resolve_profile(&binding.profile, profiles)?;
    if resolved.empty_dir == Some(true) {
        return Err(LayoutErr::TeamEmptyDir {
            definition: binding.profile.clone(),
        });
    }
    resolved.apply_role(binding);
    Ok(resolved)
}

/// Resolve `name` through profile inheritance to a canonical kind and launch variant.
pub fn resolve_profile(name: &str, profiles: &ProfilesConfig) -> Result<ResolvedProfile> {
    let chain = launch_layers(ChainRequest {
        name: Some(name),
        own: profiles,
        fallback: None,
        base_override: None,
        selection: no_flags(),
        role: None,
        target_kind: None,
        subagent_scope: false,
        detect_dropped_model: false,
    })?;
    resolve_chain(name, chain, None, None)
}

/// A profile whose tools carry a `capabilities:` grant, a TOML profile over a Markdown kind
/// base that writes one included, reaches RimZ's MCP capabilities only through the grant, so a
/// capability's name in its `skills`, or `rimz` or a capability in its `mcps`, is refused.
/// `skills` keeps every other skill, a `rimz-*` skill alone included. `rimz-blackboard`, which
/// no list names, is refused with `BOARD_REFUSAL` in place of the fix to move it.
fn refuse_rimz_lists_beside_a_grant(resolved: &ResolvedProfile, profile: &str) -> Result<()> {
    if resolved
        .tools
        .as_ref()
        .and_then(crate::agents::ToolSet::rimz)
        .is_none()
    {
        return Ok(());
    }
    // `mcps` reserves `rimz` too, RimZ's own server.
    let capability = |name: &&str| crate::mcp::catalog::Capability::from_name(name).is_some();
    let skills = resolved.skills.iter().flatten().map(|skill| skill.as_str());
    let mcps = resolved.mcps.iter().flatten().map(|entry| entry.name());
    let rimz: Vec<&str> = skills
        .filter(capability)
        .chain(mcps.filter(|name| *name == "rimz" || capability(name)))
        .collect();
    // No list may name the board, so its entry goes rather than into the grant.
    let board: Vec<&str> = rimz
        .iter()
        .copied()
        .filter(|name| {
            crate::mcp::catalog::Capability::from_name(name)
                == Some(crate::mcp::catalog::Capability::Blackboard)
        })
        .collect();
    let (named, why) = if board.is_empty() {
        (
            rimz,
            "; beside `capabilities:` RimZ's MCP capabilities go only in the kind base's \
             `capabilities:`, so list them there instead"
                .to_owned(),
        )
    } else {
        (board, format!(", but {}", crate::config::BOARD_REFUSAL))
    };
    if named.is_empty() {
        return Ok(());
    }
    let named: Vec<String> = named.iter().map(|name| format!("`{name}`")).collect();
    Err(LayoutErr::InvalidProfile {
        profile: profile.to_owned(),
        reason: format!(
            "its kind base sets `capabilities:`, and its `skills` or `mcps` list {}{why}",
            named.join(", ")
        ),
    })
}

/// The effort and fast mode a listing shows for profile `name`: what its launch takes through
/// the kind base and the built-in default, or what the profile writes when it does not resolve.
pub fn listed_tuning(name: &str, profiles: &ProfilesConfig) -> (Option<String>, bool) {
    let on = |fast| fast == Some(crate::agents::Toggle::On);
    match resolve_profile(name, profiles) {
        Ok(resolved) => (resolved.launch.effort, on(resolved.fast)),
        Err(_) => profiles.0.get(name).map_or((None, false), |profile| {
            (profile.effort.clone(), on(profile.fast))
        }),
    }
}

/// Refuse an explicit `--agent` and `--model` pair whose providers disagree, unless the
/// `--agent` provider keeps that model.
pub(crate) fn check_agent_model_family(
    family: &str,
    kind: &str,
    model: &str,
) -> std::result::Result<(), String> {
    let Some(implied) = crate::agents::definition_model_kind(model) else {
        return Ok(());
    };
    if kind == implied || crate::agents::keeps_model(kind, model) {
        return Ok(());
    }
    Err(format!(
        "--agent {family} runs on {kind} but --model {model} runs on {implied}; choose the same family"
    ))
}

/// The resolved target of an `--agent` flag or recorded `LaunchRecord.agent`, looked up in the
/// active table, then the fallback table (`[agents.profiles]` on a subagent-scope launch), then
/// registered runtimes.
pub(crate) struct AgentOverride<'a> {
    pub name: String,
    pub table: Option<&'a ProfilesConfig>,
    pub profile: Option<&'a Profile>,
    pub resolved: ResolvedProfile,
    /// Whether the override's own written chain (excluding kind bases) sets a `model` or `tier`.
    pub writes_engine: bool,
    /// The resolved provider runtime (kind + optional variant).
    runtime: crate::agents::Runtime,
}

impl std::fmt::Debug for AgentOverride<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOverride")
            .field("name", &self.name)
            .field("kind", &self.resolved.kind)
            .field("writes_engine", &self.writes_engine)
            .finish()
    }
}

impl AgentOverride<'_> {
    pub(crate) fn build_base(&self) -> crate::config::definitions::BuildBase<'_> {
        use crate::config::definitions::BuildBase;
        match self.profile {
            Some(p) if p.definition.is_some() => BuildBase::Profile(p),
            _ => BuildBase::Runtime(self.runtime),
        }
    }

    fn strip_in(&self, own: &ProfilesConfig, subagent_scope: bool) -> bool {
        subagent_scope
            && (self.profile.is_none_or(|p| p.kind_base)
                || self.table.is_some_and(|table| !std::ptr::eq(table, own)))
    }
}

/// X and the launch's selection as each agent cell of one launch reads them.
#[derive(Clone, Copy)]
pub(super) struct CellBase<'a> {
    x: Option<&'a AgentOverride<'a>>,
    selection: &'a Selection,
}

#[cfg(test)]
impl<'a> CellBase<'a> {
    /// A cell resolved under `x` with no flags.
    pub(super) fn under(x: Option<&'a AgentOverride<'a>>) -> Self {
        Self {
            x,
            selection: no_flags(),
        }
    }
}

impl CellBase<'static> {
    /// A cell resolved with no `--agent` and no flags.
    pub(super) fn none() -> Self {
        Self {
            x: None,
            selection: no_flags(),
        }
    }
}

/// A launch with no flags, for resolutions outside a launch (replay, explain, seat settings).
fn no_flags() -> &'static Selection {
    static NO_FLAGS: std::sync::LazyLock<Selection> = std::sync::LazyLock::new(Selection::default);
    &NO_FLAGS
}

/// Resolve a profile under an optional `--agent` override using the launch chain rule.
#[cfg(test)]
pub(crate) fn resolve_profile_under(
    name: &str,
    agent_override: Option<&str>,
    profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
) -> Result<ResolvedProfile> {
    let base = resolve_agent_override(agent_override, profiles, fallback)?;
    resolve_profile_with_fallback(
        name,
        profiles,
        fallback,
        CellBase {
            x: base.as_ref(),
            selection: no_flags(),
        },
        None,
    )
}

/// The inheritance chain `rimz agents explain` reports for `name`: L's file chain, then X's,
/// then the final kind base, without any other provider base. Without `--agent`, `--model`,
/// or a kind change from the row's own kind, preserves the stored profile walk.
pub fn explain_profile_chain(
    name: &str,
    agent_override: Option<&str>,
    target_kind: &str,
    model_override: bool,
    profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
) -> Vec<String> {
    let base = resolve_agent_override(agent_override, profiles, fallback)
        .ok()
        .flatten();
    let key = profiles.key(name);
    let target_kind_arg = (base.is_some()
        || model_override
        || profiles.0.get(key).is_some_and(|profile| {
            profile
                .definition
                .as_ref()
                .map_or(profile.agent.as_str(), |def| def.original_kind())
                != target_kind
        }))
    .then_some(target_kind);
    let Ok(mut chain) = launch_layers(ChainRequest {
        name: Some(name),
        own: profiles,
        fallback,
        base_override: base.as_ref(),
        selection: no_flags(),
        role: None,
        target_kind: target_kind_arg,
        subagent_scope: fallback.is_some(),
        detect_dropped_model: false,
    }) else {
        return Vec::new();
    };
    chain.names.push(chain.kind.clone());
    dedup_first(&mut chain.names);
    if chain.built_in() {
        for name in &mut chain.names {
            if *name == chain.kind {
                *name = format!("built-in kind base {}", chain.kind);
            }
        }
    }
    chain.names
}

pub(crate) fn resolve_agent_override<'a>(
    agent_override: Option<&str>,
    own: &'a ProfilesConfig,
    fallback: Option<&'a ProfilesConfig>,
) -> Result<Option<AgentOverride<'a>>> {
    let Some(value) = agent_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let own_key = own.key(value);
    let (key, table) = if let Some(detail) = own.failed(value) {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    } else if own.0.contains_key(own_key) {
        (own_key, Some(own))
    } else if let Some(fb) = fallback
        && let Some(detail) = fb.failed(value)
    {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    } else if let Some(fb) = fallback
        && fb.0.contains_key(fb.key(value))
    {
        (fb.key(value), Some(fb))
    } else if crate::agents::find_runtime(value).is_some() {
        (value, None)
    } else {
        return Err(LayoutErr::UnknownAgentOverride {
            value: value.to_owned(),
            valid: valid_agent_overrides_in(own, fallback),
        });
    };
    let lookup_table = table.unwrap_or(own);
    let resolved = resolve_profile(key, lookup_table)?;
    let profile = table.and_then(|table| table.0.get(key));
    let writes_engine = match profile {
        Some(profile) if profile.kind_base => false,
        Some(profile) => profile
            .definition
            .as_ref()
            .is_some_and(|def| def.writes_engine()),
        None => false,
    };
    let runtime = crate::agents::Runtime {
        definition: crate::agents::find_definition(resolved.kind.as_str())
            .expect("resolved override terminal kind is known"),
        variant: resolved.variant,
    };
    Ok(Some(AgentOverride {
        name: key.to_owned(),
        table,
        profile,
        resolved,
        writes_engine,
        runtime,
    }))
}

fn finalize_resolved_profile(resolved: &mut ResolvedProfile, name: &str) -> Result<()> {
    normalize_auto_compact(&mut resolved.auto_compact, name)?;
    normalize_auto_compact(&mut resolved.kind_base.auto_compact, name)?;
    let adapter = crate::agents::find_definition(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    adapter
        .spec()
        .render_preset(&profile_preset(resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    normalize_budget(&mut resolved.launch.budget, name)?;
    refuse_rimz_lists_beside_a_grant(resolved, name)?;
    Ok(())
}

fn fill_layer(resolved: &mut ResolvedProfile, layer: &Profile, name: &str, skip_model: bool) {
    if !skip_model {
        resolved.fill_missing(layer, name);
        return;
    }
    let mut adjusted = layer.clone();
    adjusted.model = None;
    adjusted.tier_stamp = None;
    resolved.fill_missing(&adjusted, name);
}

pub(super) fn kind_base_for<'a>(
    kind: &str,
    own: &'a ProfilesConfig,
    other: Option<&'a ProfilesConfig>,
    fallback: Option<&'a ProfilesConfig>,
) -> Option<&'a Profile> {
    let pick = |table: &'a ProfilesConfig| table.0.get(kind).filter(|base| base.kind_base);
    pick(own)
        .or_else(|| other.and_then(pick))
        .or_else(|| fallback.and_then(pick))
}

fn apply_role_under(
    resolved: &mut ResolvedProfile,
    binding: &RoleBinding,
    original_kind: &str,
    base_override: Option<&AgentOverride<'_>>,
) {
    let same_kind = original_kind == resolved.kind.as_str();
    let mut effective = binding.clone();
    if !same_kind {
        effective.args = None;
    }
    // The engine module ranked the role's model with the flags, X and L's lines.
    effective.model = None;
    if let Some(x) = base_override
        && !x.writes_engine
        && let Some(role_model) = binding.model.as_deref()
        && !crate::config::engine::keeps_inherited_model(
            crate::config::engine::seat_kind(original_kind, Some(role_model), true),
            &resolved.kind,
            role_model,
        )
    {
        resolved.notices.push(CellNotice::DroppedModel {
            model: role_model.to_owned(),
        });
    }
    resolved.apply_role(&effective);
}

fn strip_direct_only(profile: Cow<'_, Profile>, should_strip: bool) -> Cow<'_, Profile> {
    if !should_strip
        || (profile.subagents.is_none()
            && profile.idle_compact.is_none()
            && profile.keep_warm.is_none())
    {
        return profile;
    }
    let mut owned = profile.into_owned();
    owned.subagents = None;
    owned.idle_compact = None;
    owned.keep_warm = None;
    Cow::Owned(owned)
}

/// Inputs to [`launch_layers`], the single launch chain walk shared by launch resolution,
/// replay, explain, and post-launch seat settings.
struct ChainRequest<'a> {
    name: Option<&'a str>,
    own: &'a ProfilesConfig,
    fallback: Option<&'a ProfilesConfig>,
    base_override: Option<&'a AgentOverride<'a>>,
    selection: &'a Selection,
    /// The team role the cell launches as, whose `model` is an engine input.
    role: Option<&'a RoleBinding>,
    target_kind: Option<&'a str>,
    subagent_scope: bool,
    detect_dropped_model: bool,
}

struct ChainLayer<'a> {
    name: String,
    profile: Cow<'a, Profile>,
}

fn layer<'a>(name: String, profile: Cow<'a, Profile>) -> ChainLayer<'a> {
    ChainLayer { name, profile }
}

#[derive(Default)]
struct LaunchChain<'a> {
    layers: Vec<ChainLayer<'a>>,
    names: Vec<String>,
    kind: String,
    variant: Option<&'static crate::agents::Variant>,
    original_kind: String,
    dropped_model: Option<String>,
    /// The model and tier stamp the engine module chose, over whatever a layer writes; no model
    /// when a resumed launch recorded none.
    engine: Option<(Option<String>, Option<crate::agents::TierStamp>)>,
    /// Whether the engine replays a resumed session's recorded one.
    resumed: bool,
}

impl LaunchChain<'_> {
    fn built_in(&self) -> bool {
        self.layers.iter().any(|entry| entry.profile.built_in_base)
    }
}

/// Pick the first layer value in launch chain order (L -> X -> final kind base) for a
/// post-launch seat setting, falling back to `L`'s row when the chain cannot build.
#[expect(
    clippy::too_many_arguments,
    reason = "bridges effective seat resolution into the shared launch_layers walk"
)]
pub(crate) fn pick_in_launch_chain<T>(
    name: Option<&str>,
    own: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
    base_override: Option<&AgentOverride<'_>>,
    target_kind: Option<&str>,
    subagent_scope: bool,
    direct_when_unchanged: bool,
    pick: impl Fn(&Profile) -> Option<T>,
) -> Option<T> {
    if name.is_some_and(|name| own.failed(name).is_some()) {
        return None;
    }
    if base_override.is_none() && direct_when_unchanged {
        let row = name.and_then(|name| own.0.get(own.key(name)))?;
        if target_kind.is_none_or(|kind| kind == row.agent) {
            return pick(&strip_direct_only(
                Cow::Borrowed(row),
                subagent_scope && row.kind_base,
            ));
        }
    }
    let present = name.filter(|name| own.0.contains_key(own.key(name)));
    if present.is_none() && base_override.is_none() {
        return None;
    }
    match launch_layers(ChainRequest {
        name: present,
        own,
        fallback,
        base_override,
        selection: no_flags(),
        role: None,
        target_kind,
        subagent_scope,
        detect_dropped_model: false,
    }) {
        Ok(chain) => chain.layers.iter().find_map(|layer| pick(&layer.profile)),
        Err(_) => present
            .and_then(|name| own.0.get(own.key(name)))
            .and_then(|row| {
                pick(&strip_direct_only(
                    Cow::Borrowed(row),
                    subagent_scope && row.kind_base,
                ))
            }),
    }
}

/// What one build of L yields under an engine: its layers and chain names in launch order,
/// before the final kind's base.
struct Built<'a> {
    layers: Vec<ChainLayer<'a>>,
    names: Vec<String>,
}

/// L as the engine reads it: the loaded row it names, if any, and its definition.
struct LaunchedName<'a> {
    key: Option<&'a str>,
    root: Option<&'a Profile>,
    def: Option<&'a crate::config::definitions::DefinitionSource>,
    kind: String,
    variant: Option<&'static crate::agents::Variant>,
}

impl LaunchedName<'_> {
    fn side(&self) -> crate::config::engine::Side<'_> {
        let mut side = crate::config::engine::Side::bare(
            self.key.unwrap_or(&self.kind),
            &self.kind,
            self.variant,
        );
        side.file = self.root.is_some();
        if let Some(def) = self.def {
            side.model = def.written_model();
            side.tier = def.written_tier();
            side.preference = def.preference().clone();
        }
        side
    }
}

fn x_side<'a>(x: &'a AgentOverride<'a>) -> crate::config::engine::Side<'a> {
    let mut side =
        crate::config::engine::Side::bare(&x.name, x.resolved.kind.as_str(), x.resolved.variant);
    side.file = x.profile.is_some();
    if let Some(def) = x.profile.and_then(|profile| profile.definition.as_ref()) {
        if x.writes_engine {
            side.model = def.written_model();
            side.tier = def.written_tier();
        }
        side.preference = def.preference().clone();
    }
    side
}

/// Builds L under `kind` with `model` (`None`: what L's lines and the kind base give): a
/// definition rebuilt under X, or a bare name or kind base with X's layers and, on its own kind
/// with no X, its own row. A kind-base file moved to another kind needs that kind's base.
fn build_launched<'a>(
    l: &LaunchedName<'a>,
    req: &ChainRequest<'a>,
    kind: &str,
    model: Option<&str>,
) -> std::result::Result<Built<'a>, crate::config::definitions::DefinitionErr> {
    let x = req.base_override;
    if let (Some(key), Some(root), Some(def)) = (l.key, l.root, l.def) {
        let built = if x.is_none() && model.is_none() && kind == root.agent {
            Cow::Borrowed(root)
        } else {
            Cow::Owned(def.build(x.map(AgentOverride::build_base), kind, model)?)
        };
        let names = if x.is_some() || req.target_kind.is_some() {
            built
                .definition
                .as_ref()
                .map_or_else(|| vec![key.to_owned()], |def| def.chain().to_vec())
        } else {
            vec![key.to_owned()]
        };
        return Ok(Built {
            layers: vec![layer(key.to_owned(), built)],
            names,
        });
    }
    let mut built = Built {
        layers: Vec::new(),
        names: Vec::new(),
    };
    if let Some(x) = x {
        built.names.extend(l.key.map(str::to_owned));
        if let Some(x_profile) = x.profile
            && let Some(x_def) = &x_profile.definition
        {
            let x_built = if kind == x_profile.agent {
                Cow::Borrowed(x_profile)
            } else {
                Cow::Owned(x_def.build(None, kind, None)?)
            };
            match &x_built.definition {
                Some(def) => built.names.extend(def.chain().iter().cloned()),
                None => built.names.push(x.name.clone()),
            }
            let strip_x = x.strip_in(req.own, req.subagent_scope);
            built
                .layers
                .push(layer(x.name.clone(), strip_direct_only(x_built, strip_x)));
        }
    } else if let (Some(key), Some(root)) = (l.key, l.root) {
        // A row moved to another kind runs on that kind's base, under its own name.
        if kind == l.kind {
            built.layers.push(layer(
                key.to_owned(),
                strip_direct_only(Cow::Borrowed(root), req.subagent_scope),
            ));
        }
        built.names.push(key.to_owned());
    }
    // A bare name or a kind base moved onto `kind`, under no X definition, runs on the base of
    // `kind` as the tables hold it, which no definition build checked: a failed one refuses, as
    // it does a definition built for `kind`, and so does none under a kind base's prompt.
    let x_table = x.and_then(|x| x.table);
    if kind != l.kind
        && l.root.is_none_or(|root| root.kind_base)
        && x.is_none_or(|x| x.profile.is_none_or(|profile| profile.definition.is_none()))
    {
        let failed = req
            .own
            .failed_base(kind)
            .or_else(|| x_table.and_then(|table| table.failed_base(kind)))
            .or_else(|| req.fallback.and_then(|table| table.failed_base(kind)));
        if failed.is_some()
            || (l.root.is_some()
                && kind_base_for(kind, req.own, x_table, req.fallback).is_none()
                && built_in_base_for(kind, req).is_none())
        {
            return Err(crate::config::definitions::missing_base(kind, failed));
        }
    }
    Ok(built)
}

/// The built-in kind base of `kind` in the launch's own table, else in its fallback.
fn built_in_base_for<'a>(kind: &str, req: &ChainRequest<'a>) -> Option<&'a Profile> {
    req.own
        .built_in_base(kind)
        .or_else(|| req.fallback.and_then(|table| table.built_in_base(kind)))
}

/// The refusal of a launch whose engine could not be chosen or built, worded as the flag that
/// asked for it.
fn engine_refusal(
    error: crate::config::engine::EngineErr,
    label: &str,
    kind: &str,
    model_flag: Option<&str>,
) -> LayoutErr {
    use crate::config::engine::EngineErr;
    match error {
        EngineErr::DependsOnFailed(error) | EngineErr::Unbuildable(error) => match model_flag {
            Some(model) => LayoutErr::Engine(format!(
                "{label}: model '{model}' ({kind}) cannot run this definition: {}",
                error.message
            )),
            None => LayoutErr::CannotRun {
                profile: label.to_owned(),
                kind: kind.to_owned(),
                reason: error.message,
            },
        },
        EngineErr::NoEligible { reason, .. } => LayoutErr::Engine(reason),
        EngineErr::KindChanged { was, now } => LayoutErr::KindChanged { was, now },
        other => LayoutErr::Engine(other.to_string()),
    }
}

/// Produce the ordered launch layers (L -> X -> final kind base) for one profile under an
/// optional `--agent` override and target kind: the engine module chooses the kind, model and
/// tier from the flags, X, the role's model and L's lines, then L is built under that engine.
fn launch_layers<'a>(req: ChainRequest<'a>) -> Result<LaunchChain<'a>> {
    if let Some(raw_name) = req.name
        && let Some(detail) = req.own.failed(raw_name)
    {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    }
    let key = req.name.map(|name| req.own.key(name));
    let root = key.and_then(|key| req.own.0.get(key));
    let bare_runtime = key
        .filter(|_| root.is_none())
        .and_then(crate::agents::find_runtime);
    if let Some(raw_name) = req.name
        && root.is_none()
        && bare_runtime.is_none()
    {
        return Err(LayoutErr::UnknownProfileBase {
            profile: raw_name.to_owned(),
            base: key.unwrap_or(raw_name).to_owned(),
        });
    }
    if req.base_override.is_none() && req.name.is_none() {
        return Err(LayoutErr::UnknownProfileBase {
            profile: String::new(),
            base: String::new(),
        });
    }
    let def = root
        .filter(|profile| !profile.kind_base)
        .and_then(|profile| profile.definition.as_ref());
    let l = LaunchedName {
        key,
        root,
        def,
        kind: def
            .map(|def| def.original_kind())
            .or_else(|| root.map(|profile| profile.agent.as_str()))
            .or_else(|| bare_runtime.map(crate::agents::Runtime::kind))
            .or(key)
            .unwrap_or_default()
            .to_owned(),
        variant: root.map_or_else(
            || bare_runtime.and_then(|rt| rt.variant),
            |profile| profile.variant,
        ),
    };
    let mut chain = LaunchChain {
        original_kind: l.kind.clone(),
        ..LaunchChain::default()
    };
    let label = req
        .role
        .map(|binding| binding.role.as_str())
        .or(req.name)
        .unwrap_or_default();
    let (kind, variant, built) = if let Some(target) = req.target_kind {
        let target_rt = crate::agents::find_runtime(target);
        let kind: &str = target_rt.map_or(target, |rt| rt.kind());
        let variant = match req.base_override {
            Some(x) => {
                crate::config::engine::variant(kind, x.resolved.kind.as_str(), x.resolved.variant)
            }
            None => target_rt
                .and_then(|rt| rt.variant)
                .or_else(|| crate::config::engine::variant(kind, &l.kind, l.variant)),
        };
        let built = build_launched(&l, &req, kind, None).map_err(|error| {
            engine_refusal(
                crate::config::engine::EngineErr::Unbuildable(error),
                label,
                kind,
                None,
            )
        })?;
        (kind.to_owned(), variant, built)
    } else if let Some(resume) = req.selection.resume_of(
        l.side(),
        req.base_override.map(x_side),
        req.role.and_then(|binding| binding.model.as_deref()),
    ) {
        let mut build = |kind: &str, _variant, model: Option<&str>| {
            // A row with no definition runs on its own kind alone: it no longer builds where
            // the session ran, which reads as the profile having moved.
            if let Some(root) = l.root.filter(|root| l.def.is_none() && !root.kind_base)
                && req.base_override.is_none()
                && kind != root.agent
            {
                return Err(crate::config::definitions::DefinitionErr {
                    path: std::path::PathBuf::new(),
                    message: format!("`{label}` runs on {}", root.agent),
                    cause: crate::config::definitions::DefinitionCause::Invalid,
                });
            }
            build_launched(&l, &req, kind, model)
        };
        let session = resume.session;
        let (engine, built) = crate::config::engine::resume(
            &resume,
            req.selection.tiers(),
            &mut crate::config::engine::Probe {
                build: &mut build,
                unavailable: &mut |kind, model| req.selection.unavailable(kind, model),
            },
        )
        .map_err(|error| engine_refusal(error, label, session, None))?;
        chain.resumed = true;
        if !matches!(engine.model, crate::config::engine::Model::FromBase) {
            chain.engine = Some((engine.model.named().map(str::to_owned), engine.tier));
        }
        (engine.kind, engine.variant, built)
    } else if !rechooses(&req, &l) {
        let natural = root.map_or(l.kind.as_str(), |profile| profile.agent.as_str());
        let built = build_launched(&l, &req, natural, None).map_err(|error| {
            engine_refusal(
                crate::config::engine::EngineErr::Unbuildable(error),
                label,
                natural,
                None,
            )
        })?;
        (natural.to_owned(), l.variant, built)
    } else {
        let selection = req.selection;
        let tiers = def
            .or_else(|| {
                req.base_override
                    .and_then(|x| x.profile?.definition.as_ref())
            })
            .map_or_else(|| selection.tiers(), |def| def.tiers());
        let launch = crate::config::engine::Launch {
            l: l.side(),
            x: req.base_override.map(x_side),
            role: req.role.and_then(|binding| binding.model.as_deref()),
            model: selection.model_flag(),
            tier: selection.tier_flag(),
        };
        let mut tried = String::new();
        let mut build = |kind: &str, _variant, model: Option<&str>| {
            kind.clone_into(&mut tried);
            build_launched(&l, &req, kind, model)
        };
        let picked = crate::config::engine::launch(
            &launch,
            tiers,
            &mut crate::config::engine::Probe {
                build: &mut build,
                unavailable: &mut |kind, model| selection.unavailable(kind, model),
            },
        );
        let (engine, built) =
            picked.map_err(|error| engine_refusal(error, label, &tried, selection.model_flag()))?;
        if let Some(model) = engine.model.named() {
            chain.engine = Some((Some(model.to_owned()), engine.tier.clone()));
        }
        (engine.kind, engine.variant, built)
    };
    chain.dropped_model = req
        .base_override
        .filter(|x| req.detect_dropped_model && !x.writes_engine)
        .and(def)
        .filter(|def| def.written_tier().is_none())
        .and_then(|def| def.written_model())
        .filter(|model| !crate::config::engine::keeps_inherited_model(&l.kind, &kind, model))
        .map(str::to_owned);
    chain.layers = built.layers;
    chain.names = built.names;
    chain.variant = match chain.layers.first() {
        Some(first) if def.is_some() => first.profile.variant,
        _ => variant,
    };
    chain.kind = match chain.layers.first() {
        Some(first) if def.is_some() => first.profile.agent.clone(),
        _ => kind,
    };
    if !chain.layers.iter().any(|entry| entry.name == chain.kind) {
        let base = kind_base_for(
            &chain.kind,
            req.own,
            req.base_override.and_then(|x| x.table),
            req.fallback,
        )
        .map(Cow::Borrowed)
        .or_else(|| {
            let def = chain
                .layers
                .iter()
                .find_map(|entry| entry.profile.definition.as_ref())?;
            Some(Cow::Owned(def.built_in_base(&chain.kind)?.clone()))
        })
        .or_else(|| {
            // A kind base moved onto another kind adds no layer of its own, and runs on the
            // built-in base of the kind it moves onto.
            root.filter(|profile| profile.kind_base)?;
            built_in_base_for(&chain.kind, &req).map(Cow::Borrowed)
        });
        if let Some(base) = base {
            chain.layers.push(layer(
                chain.kind.clone(),
                strip_direct_only(base, req.subagent_scope),
            ));
            chain.names.push(chain.kind.clone());
        }
    }
    if chain.original_kind.is_empty() {
        chain.original_kind = chain.kind.clone();
    }
    dedup_first(&mut chain.names);
    Ok(chain)
}

/// Whether a launch must choose L's engine again rather than run the row as loaded: under X,
/// a role's model, a flag, or a tier L writes, whose walk stamps the launch.
fn rechooses(req: &ChainRequest<'_>, l: &LaunchedName<'_>) -> bool {
    req.base_override.is_some()
        || req.role.is_some_and(|binding| binding.model.is_some())
        || req.selection.tier_flag().is_some()
        || req.selection.model_flag().is_some()
        || l.def.is_some_and(|def| def.written_tier().is_some())
}

fn resolve_chain(
    name: &str,
    chain: LaunchChain<'_>,
    base_override: Option<&AgentOverride<'_>>,
    role: Option<&RoleBinding>,
) -> Result<ResolvedProfile> {
    let mut resolved = ResolvedProfile::bare(&chain.kind);
    resolved.variant = chain.variant;
    for layer in &chain.layers {
        fill_layer(
            &mut resolved,
            &layer.profile,
            &layer.name,
            chain.engine.is_some(),
        );
    }
    if let Some((model, stamp)) = chain.engine {
        resolved.launch.model = model;
        resolved.launch.tier = stamp.map(Box::new);
    }
    resolved.layers = chain.names;
    if let Some(model) = chain.dropped_model {
        resolved.notices.push(CellNotice::DroppedModel { model });
    }
    resolved.fill_kind_defaults();
    finalize_resolved_profile(&mut resolved, name)?;
    if let Some(binding) = role {
        apply_role_under(
            &mut resolved,
            binding,
            &chain.original_kind,
            base_override.filter(|_| !chain.resumed),
        );
    }
    Ok(resolved)
}

fn resolve_profile_with_override(
    name: &str,
    profiles: &ProfilesConfig,
    base: CellBase<'_>,
    role: Option<&RoleBinding>,
) -> Result<ResolvedProfile> {
    resolve_profile_with_fallback(name, profiles, None, base, role)
}

fn resolve_profile_with_fallback(
    name: &str,
    profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
    base: CellBase<'_>,
    role: Option<&RoleBinding>,
) -> Result<ResolvedProfile> {
    let chain = launch_layers(ChainRequest {
        name: Some(name),
        own: profiles,
        fallback,
        base_override: base.x,
        selection: base.selection,
        role,
        target_kind: None,
        subagent_scope: fallback.is_some(),
        detect_dropped_model: !base.selection.resumes()
            && role.is_none_or(|binding| binding.model.is_none()),
    })?;
    resolve_chain(name, chain, base.x, role)
}

fn normalize_budget(budget: &mut Option<String>, profile: &str) -> Result<()> {
    let Some(raw) = budget.as_deref() else {
        return Ok(());
    };
    let spec = raw
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: profile.to_owned(),
            reason: err.to_string(),
        })?;
    *budget = Some(spec.to_string());
    Ok(())
}

fn normalize_auto_compact(value: &mut Option<String>, profile: &str) -> Result<()> {
    use crate::store::message::AutoCompact;

    let Some(raw) = value.as_deref() else {
        return Ok(());
    };
    let tokens = AutoCompact::parse_native_window(raw).map_err(|_| LayoutErr::InvalidProfile {
        profile: profile.to_owned(),
        reason: format!(
            "auto-compact must be between 100k and 1M tokens; use a token count like `200k`, got `{raw}`"
        ),
    })?;
    *value = Some(tokens.to_string());
    Ok(())
}

/// The default tab title for a launch. Worktree launches use the `#channel`
/// spelling shared with agent addresses; a named-team launch uses
/// `team:<name>`; otherwise the title lists up to three short cell labels in layout order.
pub fn default_tab_title(
    spec: &LayoutSpec,
    worktree_name: Option<&str>,
    team: Option<&str>,
) -> String {
    if let Some(name) = worktree_name.filter(|name| !name.is_empty()) {
        return format!("#{name}");
    }
    if let Some(team) = team.filter(|team| !team.is_empty()) {
        return format!("team:{team}");
    }
    let names = spec
        .columns
        .iter()
        .flat_map(|column| &column.rows)
        .map(Cell::pane_name)
        .collect::<Vec<_>>();
    let mut title = crate::mux::tab_name::label_from_pane_names(names.iter().map(String::as_str));
    if title.is_empty() {
        title.push_str("term");
    }
    title
}

pub(super) fn is_known_spec_token(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> bool {
    let raw = raw.trim();
    !raw.is_empty()
        && (spec_team(raw, teams).is_some()
            || is_cell_word(raw, profiles, commands)
            || (raw.contains(':')
                && parse_layout_spec_validated(raw, profiles, commands, CellBase::none()).is_ok())
            || (raw.contains([',', '+', '/'])
                && parse_layout_spec_validated(raw, profiles, commands, CellBase::none()).is_ok()))
}

fn parse_layout_spec_validated(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<LayoutSpec> {
    let structure = parse_layout_structure(raw)?;
    let mut seen_roles = BTreeSet::new();
    fold_layout(structure, |raw_cell| {
        let (cell_name, role) = split_inline_role(raw_cell, profiles, commands);
        if let Some(role) = role {
            validate_inline_role(role)?;
            if !seen_roles.insert(role.to_owned()) {
                return Err(LayoutErr::DuplicateInlineRole {
                    role: role.to_owned(),
                });
            }
        }
        let mut cell = parse_cell(cell_name, profiles, commands, base)?;
        if let Some(role) = role {
            let Cell::Agent(agent) = &mut cell else {
                return Err(LayoutErr::RoleOnCommandCell {
                    cell: cell_name.to_owned(),
                    role: role.to_owned(),
                });
            };
            agent.launch.role = Some(role.to_owned());
        }
        Ok(cell)
    })
}

fn fold_layout<'a>(
    structure: RawLayout<'a>,
    mut resolve: impl FnMut(&'a str) -> Result<Cell>,
) -> Result<LayoutSpec> {
    let columns = structure
        .columns
        .into_iter()
        .map(|column| {
            Ok(Column {
                rows: column
                    .cells
                    .into_iter()
                    .map(&mut resolve)
                    .collect::<Result<Vec<_>>>()?,
                stacked: column.stacked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(LayoutSpec { columns })
}

pub(crate) fn parse_layout_structure(raw: &str) -> Result<RawLayout<'_>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LayoutErr::Empty);
    }

    let mut columns = Vec::new();
    for column_raw in raw.split(',') {
        let column_raw = column_raw.trim();
        if column_raw.is_empty() {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        let tiled = column_raw.contains('+');
        let stacked = column_raw.contains('/');
        if tiled && stacked {
            return Err(LayoutErr::MixedRowOperators {
                column: column_raw.to_owned(),
            });
        }
        let separator = if stacked { '/' } else { '+' };
        let cells = column_raw
            .split(separator)
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.iter().any(|cell| cell.is_empty()) {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        columns.push(RawColumn { cells, stacked });
    }
    Ok(RawLayout { columns })
}

fn is_inline_spec(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    if raw.contains([',', '+', '/']) || is_cell_word(raw, profiles, commands) {
        return true;
    }
    let (cell, role) = split_inline_role(raw, profiles, commands);
    role.is_some() && is_cell_word(cell, profiles, commands)
}

pub(super) fn is_cell_word(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> bool {
    commands.0.contains_key(raw)
        || profiles.0.contains_key(raw)
        || profiles.failed(raw).is_some()
        || raw == "term"
        || crate::agents::find_runtime(raw).is_some()
        || virtual_agent_shape(raw)
        || fast_suffix_base(raw).is_some_and(|base| {
            profiles.0.contains_key(base)
                || profiles.failed(base).is_some()
                || crate::agents::find_definition(base).is_some()
                || virtual_agent_shape(base)
        })
}

pub(crate) fn split_inline_role<'a>(
    raw: &'a str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> (&'a str, Option<&'a str>) {
    if is_cell_word(raw, profiles, commands) {
        return (raw, None);
    }
    raw.split_once(':')
        .map_or((raw, None), |(cell, role)| (cell, Some(role)))
}

fn validate_inline_role(name: &str) -> Result<()> {
    if invalid_role_name(name) {
        return Err(LayoutErr::InvalidInlineRole {
            name: name.to_owned(),
        });
    }
    if let Some(reason) = address_grammar_clash(name) {
        return Err(LayoutErr::InlineRoleShadowsAddress {
            name: name.to_owned(),
            reason,
        });
    }
    if crate::agents::find_definition(name).is_some() {
        return Err(LayoutErr::InlineRoleShadowsAddress {
            name: name.to_owned(),
            reason: "it is a built-in kind handle like `@claude`",
        });
    }
    Ok(())
}

fn parse_cell(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
) -> Result<Cell> {
    if let Some(command) = commands.0.get(raw) {
        return command_cell(raw, command);
    }
    if let Some(detail) = profiles.failed(raw) {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    }
    if profiles.0.contains_key(profiles.key(raw)) {
        return Ok(Cell::Agent(profile_cell_with_base_override(
            profiles.key(raw),
            profiles,
            base,
            None,
        )?));
    }
    if raw == "term" {
        return Ok(Cell::shell());
    }
    if let Some(runtime) = crate::agents::find_runtime(raw) {
        let resolved = resolve_profile_with_override(raw, profiles, base, None)?;
        return Ok(Cell::Agent(agent_cell_from(
            &resolved,
            render_profile_args(raw, &resolved)?,
            runtime.variant.map(|_| runtime.name().to_owned()),
            None,
            resolved.launch.mode,
        )));
    }
    if let Some(cell) = virtual_agent_cell(raw, profiles, base)? {
        return Ok(cell);
    }
    if let Some(cell) = fast_suffix_cell(raw, profiles, base)? {
        return Ok(cell);
    }
    if let Some(cell) = path_command_cell(raw) {
        return Ok(cell);
    }
    Err(LayoutErr::UnknownCell(Box::new(UnknownSpec {
        word: raw.to_owned(),
        candidates: cell_candidates(profiles, commands),
    })))
}

/// Last-resort cell resolution: a bare word that is executable on PATH
/// launches as a raw command pane.
fn path_command_cell(raw: &str) -> Option<Cell> {
    which::which(raw).ok()?;
    Some(Cell::Command {
        argv: vec![raw.to_owned()],
    })
}

/// The agent cell a named profile launches as: its rendered provider argv plus
/// the typed launch params it declares.
#[cfg(test)]
pub(super) fn profile_cell(name: &str, profiles: &ProfilesConfig) -> Result<AgentCell> {
    profile_cell_with_base_override(name, profiles, CellBase::none(), None)
}

pub(super) fn profile_cell_with_base_override(
    name: &str,
    profiles: &ProfilesConfig,
    base: CellBase<'_>,
    role: Option<&RoleBinding>,
) -> Result<AgentCell> {
    let resolved = resolve_profile_with_override(name, profiles, base, role)?;
    cell_from_profile(name, &resolved)
}

/// The cell a relaunch of `name` starts under X and `selection`, which holds it to a resumed
/// session's recorded engine when it carries one.
pub(super) fn profile_cell_for_replay(
    name: &str,
    profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
    base_override: Option<&AgentOverride<'_>>,
    role: Option<&RoleBinding>,
    selection: &Selection,
) -> Result<AgentCell> {
    let resolved = resolve_profile_with_fallback(
        name,
        profiles,
        fallback,
        CellBase {
            x: base_override,
            selection,
        },
        role,
    )?;
    cell_from_profile(name, &resolved)
}

fn cell_from_profile(name: &str, resolved: &ResolvedProfile) -> Result<AgentCell> {
    Ok(agent_cell_from(
        resolved,
        render_profile_args(name, resolved)?,
        Some(name.to_owned()),
        None,
        resolved.launch.mode,
    ))
}

fn agent_cell_from(
    resolved: &ResolvedProfile,
    rendered: (Vec<String>, Vec<CellNotice>),
    profile: Option<String>,
    role: Option<String>,
    mode: Option<PermissionMode>,
) -> AgentCell {
    let (args, tool_notices) = rendered;
    let mut notices = resolved.notices.clone();
    notices.extend(tool_notices);
    AgentCell {
        notices,
        resume_model_override: false,
        empty_dir: resolved.empty_dir,
        timeout: resolved.timeout,
        kind: resolved.kind.clone(),
        isolation_default: resolved.isolation_default,
        args,
        auto_compact: resolved.effective_auto_compact(),
        tuning: TuningSources {
            auto_compact: resolved.sources.auto_compact.clone().or_else(|| {
                resolved
                    .effective_auto_compact()
                    .map(|_| TuningSource::MachineDefault)
            }),
            ..resolved.sources.clone()
        },
        system_prompt_file: resolved.system_prompt_file.clone(),
        append_system_prompt_files: resolved.append_system_prompt_files.clone(),
        team_prompt: None,
        leads_team: false,
        skills: resolved.skills.clone(),
        seat_consensus_skill: resolved.seat_consensus_skill,
        mcps: resolved.mcps.clone(),
        no_skill_tool: crate::agents::skills_lack_skill_tool(
            resolved.kind.as_str(),
            resolved.tools.as_ref(),
        ),
        rimz_capabilities: resolved
            .tools
            .as_ref()
            .and_then(crate::agents::ToolSet::rimz)
            .map(<[_]>::to_vec),
        allowed_tools: resolved.allowed_tools.clone(),
        tools: resolved.tools.as_ref().map(|tools| tools.bases().to_vec()),
        launch: crate::agents::LaunchParams {
            profile,
            mode,
            role,
            fast: resolved.fast.and_then(crate::agents::Toggle::enabled),
            code_mode: resolved.code_mode.and_then(crate::agents::Toggle::enabled),
            ..resolved.launch.clone()
        },
    }
}

fn render_profile_args(
    name: &str,
    resolved: &ResolvedProfile,
) -> Result<(Vec<String>, Vec<CellNotice>)> {
    let adapter = crate::agents::find_definition(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let mut argv = adapter
        .spec()
        .render_preset(&profile_preset(resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    if let Some(variant) = resolved.variant {
        argv.splice(0..0, variant.args.iter().map(|arg| (*arg).to_owned()));
    }
    if let Some(mode) = resolved.launch.mode {
        argv.extend(adapter.spec().launch.permission_args(mode));
    }
    let (tools, notices) = crate::agents::render_launch_tool_args(
        &resolved.kind,
        resolved.tools.as_ref(),
        resolved.code_mode.and_then(crate::agents::Toggle::enabled),
    )
    .map_err(|error| LayoutErr::InvalidProfile {
        profile: name.to_owned(),
        reason: error.to_string(),
    })?;
    argv.extend(tools);
    if let Some(raw) = resolved
        .args
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    {
        let mut extra = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: "check shell quoting in `args`".to_owned(),
        })?;
        argv.append(&mut extra);
    }
    let notices = notices
        .into_iter()
        .map(|notice| match notice {
            crate::agents::ToolNotice::Dropped(tools) => CellNotice::DroppedTools { tools },
            crate::agents::ToolNotice::Restrictions(tools) => {
                CellNotice::ToolRestrictions { tools }
            }
            crate::agents::ToolNotice::NotApplied => CellNotice::ToolsNotApplied,
        })
        .collect();
    Ok((argv, notices))
}

fn profile_preset(resolved: &ResolvedProfile) -> crate::agents::LaunchPreset {
    let preset = crate::agents::LaunchPreset {
        model: resolved.launch.model.clone(),
        effort: resolved.launch.effort.clone(),
        auto_compact: resolved.effective_auto_compact(),
        fast: resolved.fast.and_then(crate::agents::Toggle::enabled),
        ..Default::default()
    };
    crate::agents::find_definition(&resolved.kind)
        .expect("resolved profile terminal kind is known")
        .spec()
        .launch
        .normalize_preset(preset)
        .0
}

fn command_cell(name: &str, raw: &str) -> Result<Cell> {
    let argv = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidCommand {
        command: name.to_owned(),
        reason: "check shell quoting in command".to_owned(),
    })?;
    if argv.is_empty() {
        return Err(LayoutErr::InvalidCommand {
            command: name.to_owned(),
            reason: "command expands to no argv".to_owned(),
        });
    }
    Ok(Cell::Command { argv })
}

fn virtual_agent_cell(
    raw: &str,
    profiles: &ProfilesConfig,
    base: CellBase<'_>,
) -> Result<Option<Cell>> {
    let Some((kind_name, mode)) = virtual_agent_parts(raw) else {
        return Ok(None);
    };
    let (resolved, profile_name) = virtual_base(kind_name, profiles, base)?;
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    let Some(posture) = supported_virtual_agent_args(&resolved.kind, mode) else {
        return Ok(None);
    };
    Ok(Some(virtual_cell_from(
        resolved,
        profile_name,
        posture,
        Some(mode),
    )?))
}

fn virtual_cell_from(
    resolved: ResolvedProfile,
    profile_name: Option<String>,
    extra_args: Vec<String>,
    mode: Option<PermissionMode>,
) -> Result<Cell> {
    let mut base = resolved.clone();
    base.launch.mode = None;
    let error_origin = profile_name
        .as_deref()
        .unwrap_or_else(|| resolved.kind.as_str());
    let (mut args, notices) = render_profile_args(error_origin, &base)?;
    args.extend(extra_args);
    Ok(Cell::Agent(agent_cell_from(
        &resolved,
        (args, notices),
        profile_name,
        None,
        mode,
    )))
}

fn virtual_base(
    kind_name: &str,
    profiles: &ProfilesConfig,
    base: CellBase<'_>,
) -> Result<(Option<ResolvedProfile>, Option<String>)> {
    if crate::agents::find_definition(kind_name).is_none() {
        return Ok((None, None));
    }
    if let Some(detail) = profiles.failed(kind_name) {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    }
    let kind_name = profiles.key(kind_name);
    let profile_name = profiles
        .0
        .contains_key(kind_name)
        .then(|| kind_name.to_owned());
    let resolved = resolve_profile_with_override(kind_name, profiles, base, None)?;
    Ok((Some(resolved), profile_name))
}

/// The base of a `<base>-fast` cell word, before any check that it names a
/// profile, kind, or permission-mode cell.
pub(super) fn fast_suffix_base(raw: &str) -> Option<&str> {
    raw.strip_suffix("-fast").filter(|base| !base.is_empty())
}

/// `<base>[-<mode>]-fast`: the base profile or kind, launched as `--fast`
/// would launch it. The profile name stays the base and fast is stamped on
/// the cell; a kind without a fast switch still builds its base cell rather
/// than falling through to a PATH executable. Exact command, profile, `term`, and kind
/// names were tried first, so a profile literally named `x-fast` wins.
fn fast_suffix_cell(
    raw: &str,
    profiles: &ProfilesConfig,
    base: CellBase<'_>,
) -> Result<Option<Cell>> {
    let Some(raw_base) = fast_suffix_base(raw) else {
        return Ok(None);
    };
    if let Some(detail) = profiles.failed(raw_base) {
        return Err(LayoutErr::FailedProfile(detail.to_owned()));
    }
    let name = profiles.key(raw_base);
    let (mut resolved, profile_name, mode) = if profiles.0.contains_key(name) {
        let resolved = resolve_profile_with_override(name, profiles, base, None)?;
        (resolved, Some(name.to_owned()), None)
    } else if crate::agents::find_definition(name).is_some() {
        let resolved = resolve_profile_with_override(name, profiles, base, None)?;
        (resolved, None, None)
    } else if let Some((kind_name, mode)) = virtual_agent_parts(name) {
        let (resolved, profile_name) = virtual_base(kind_name, profiles, base)?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        (resolved, profile_name, Some(mode))
    } else {
        return Ok(None);
    };
    resolved.fast = Some(crate::agents::Toggle::On);
    resolved.sources.fast = Some(TuningSource::Target(raw.to_owned()));
    let Some(mode) = mode else {
        return Ok(Some(match profile_name {
            Some(name) => Cell::Agent(cell_from_profile(&name, &resolved)?),
            None => Cell::Agent(agent_cell_from(
                &resolved,
                render_profile_args(name, &resolved)?,
                None,
                None,
                resolved.launch.mode,
            )),
        }));
    };
    let Some(posture) = supported_virtual_agent_args(&resolved.kind, mode) else {
        return Ok(None);
    };
    Ok(Some(virtual_cell_from(
        resolved,
        profile_name,
        posture,
        Some(mode),
    )?))
}

pub(super) fn virtual_agent_parts(raw: &str) -> Option<(&str, PermissionMode)> {
    let (kind, mode) = raw.rsplit_once('-')?;
    let mode = PermissionMode::from_str(mode).ok()?;
    (crate::agents::find_definition(kind).is_some()).then_some((kind, mode))
}

fn virtual_agent_shape(raw: &str) -> bool {
    let Some((kind, mode)) = virtual_agent_parts(raw) else {
        return false;
    };
    supported_virtual_agent_args(kind, mode).is_some()
}

fn supported_virtual_agent_args(kind: &str, mode: PermissionMode) -> Option<Vec<String>> {
    let adapter = crate::agents::find_definition(kind)?;
    let args = adapter.spec().launch.permission_args(mode);
    if mode == PermissionMode::Ask
        || mode == PermissionMode::Plan
        || !args.is_empty()
        || adapter
            .spec()
            .launch
            .permission
            .resolve_mode(None)
            .is_some()
    {
        Some(args)
    } else {
        None
    }
}

fn validate_profile_names(profiles: &ProfilesConfig) -> Result<()> {
    for (name, profile) in &profiles.0 {
        if let Some(crate::config::KeepWarmPolicy::Block(fields)) = &profile.keep_warm
            && let Some(clause) = &fields.while_clause
        {
            return Err(LayoutErr::KeepWarm {
                definition: format!("profile '{name}'"),
                clause: clause.clone(),
                reason: "a solo profile has no board; remove `while` and use `max` only".into(),
            });
        }
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, ',' | '+' | '/' | '(' | ')'))
        {
            return Err(LayoutErr::InvalidProfileName { name: name.clone() });
        }
        if petname::RESERVED_AGENT_WORDS.contains(&name.as_str())
            || petname::HEADER_PSEUDO_HANDLES.contains(&name.as_str())
        {
            return Err(LayoutErr::ReservedProfileName { name: name.clone() });
        }
        if let Some(reason) = address_grammar_clash(name) {
            return Err(LayoutErr::ProfileShadowsAddress {
                name: name.clone(),
                reason,
            });
        }
    }
    Ok(())
}

fn validate_command_names(commands: &CommandsConfig) -> Result<()> {
    for name in commands.0.keys() {
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+' || ch == '/')
        {
            return Err(LayoutErr::InvalidCommandName { name: name.clone() });
        }
        if petname::RESERVED_AGENT_WORDS.contains(&name.as_str())
            || petname::HEADER_PSEUDO_HANDLES.contains(&name.as_str())
            || name == "me"
        {
            return Err(LayoutErr::ReservedCommandName { name: name.clone() });
        }
    }
    Ok(())
}

/// Why an agent profile name would collide with the agent-address grammar, so
/// the rendered `@<profile>` handle could never name the profile
/// unambiguously: it shadows the broadcast handle (`@all`), a kind ordinal
/// (`@claude-2`), or a pane/channel address (`zellij:%1`, `@x#chan`).
fn address_grammar_clash(name: &str) -> Option<&'static str> {
    if petname::HEADER_PSEUDO_HANDLES.contains(&name) {
        return Some("it is reserved for a non-agent message sender");
    }
    if name == "all" {
        return Some("`@all` is the broadcast handle");
    }
    if name == "me" {
        return Some("`@me` is the calling agent");
    }
    if is_kind_ordinal_shape(name) {
        return Some("it reads as a kind ordinal like `@claude-2`");
    }
    if name.contains(':') || name.contains('#') {
        return Some("`:` and `#` are reserved for pane and channel addresses");
    }
    None
}

/// Whether `name` reads as a `<kind>-<n>` ordinal handle (a known kind, then a
/// positive integer) — the shape `@claude-2` parses as.
fn is_kind_ordinal_shape(name: &str) -> bool {
    let Some((kind, ordinal)) = name.rsplit_once('-') else {
        return false;
    };
    crate::agents::find_definition(kind).is_some() && ordinal.parse::<u32>().is_ok_and(|n| n > 0)
}

fn validate_team_names<'a>(mut names: impl Iterator<Item = &'a str> + Clone) -> Result<()> {
    if let Some(name) = names.clone().find(|name| name.contains(['.', '/'])) {
        return Err(LayoutErr::InvalidTeamName {
            name: name.to_owned(),
        });
    }
    if let Some(name) = names.find(|name| {
        petname::RESERVED_AGENT_WORDS.contains(name)
            || petname::HEADER_PSEUDO_HANDLES.contains(name)
            || *name == "me"
    }) {
        return Err(LayoutErr::ReservedTeamName(name.to_owned()));
    }
    Ok(())
}

pub(super) fn validate_team_stages(name: &str, team: &Team) -> Result<()> {
    let mut stages = BTreeSet::new();
    for stage in &team.stages {
        if stage == crate::config::DONE_STAGE {
            return Err(LayoutErr::ReservedStage {
                team: name.to_owned(),
                name: stage.clone(),
            });
        }
        if stage.is_empty() || stage.trim() != stage || stage.chars().any(char::is_control) {
            return Err(LayoutErr::InvalidStageName {
                team: name.to_owned(),
                name: stage.clone(),
            });
        }
        if !stages.insert(stage) {
            return Err(LayoutErr::DuplicateStage {
                team: name.to_owned(),
                name: stage.clone(),
            });
        }
    }
    let mut owners = BTreeMap::new();
    for role in &team.roles {
        if !role.owns.is_empty()
            && (invalid_role_name(&role.role)
                || role.role.contains(['(', ')'])
                || role.role.chars().any(char::is_control))
        {
            return Err(LayoutErr::InvalidRoleName {
                team: name.to_owned(),
                name: role.role.clone(),
            });
        }
        for stage in &role.owns {
            if stage.is_empty() || stage.trim() != stage || stage.chars().any(char::is_control) {
                return Err(LayoutErr::InvalidStageName {
                    team: name.to_owned(),
                    name: stage.clone(),
                });
            }
            if stage == crate::config::DONE_STAGE {
                return Err(LayoutErr::ReservedStage {
                    team: name.to_owned(),
                    name: stage.clone(),
                });
            }
            if let Some(previous) = owners.insert(stage, &role.role) {
                return Err(LayoutErr::DuplicateStageOwner {
                    team: name.to_owned(),
                    stage: stage.clone(),
                    roles: [previous.clone(), role.role.clone()],
                });
            }
            if !stages.is_empty() && !stages.contains(stage) {
                return Err(LayoutErr::UnknownOwnedStage {
                    team: name.to_owned(),
                    role: role.role.clone(),
                    stage: stage.clone(),
                });
            }
        }
    }
    Ok(())
}

/// The team prompt layer one role's cell receives, or the refusal for a declared
/// layer it cannot take: the one rule team compile and a member's relaunch share.
pub(super) fn role_team_prompt(
    name: &str,
    team: &Team,
    role: &str,
    kind: &AgentKind,
    role_has_base: bool,
) -> Result<Option<TeamPrompt>> {
    if crate::harness::team_prompt::declared(team) {
        if !team.staged() {
            return Err(LayoutErr::TeamPromptWithoutStages {
                team: name.to_owned(),
            });
        }
        if !role_has_base {
            return Err(LayoutErr::TeamPromptWithoutBase {
                team: name.to_owned(),
                role: role.to_owned(),
                kind: kind.clone(),
            });
        }
    }
    Ok(TeamPrompt::for_role(team, role_has_base))
}

fn team_kind_base_fix(kind: &AgentKind) -> String {
    if !crate::agents::find_definition(kind.as_str())
        .is_some_and(|definition| definition.spec().launch.takes_system_prompt())
    {
        return String::new();
    }
    let path = crate::disk::paths::agents_home()
        .join("agents")
        .join(format!("{kind}.md"));
    format!(
        ", or write {} with a nonempty body as the {kind} kind base \
         (run `rimz agents validate` if it did not load)",
        path.display()
    )
}

fn validate_team_header(name: &str, team: &Team) -> Result<()> {
    validate_team_stages(name, team)?;
    let stages = team
        .pipeline_stages()
        .into_iter()
        .chain([crate::config::DONE_STAGE.to_owned()])
        .collect();
    let declarations = std::iter::once((format!("team '{name}'"), team.keep_warm.as_ref())).chain(
        team.roles.iter().map(|binding| {
            (
                format!("team '{name}' role '{}'", binding.role),
                binding.keep_warm.as_ref(),
            )
        }),
    );
    for (definition, policy) in declarations {
        let Some(crate::config::KeepWarmPolicy::Block(fields)) = policy else {
            continue;
        };
        let Some(clause) = &fields.while_clause else {
            continue;
        };
        let refuse = |reason| LayoutErr::KeepWarm {
            definition: definition.clone(),
            clause: clause.clone(),
            reason,
        };
        if !team.staged() {
            return Err(refuse(
                "an unstaged team has no board; declare stages and owners or remove `while`".into(),
            ));
        }
        if clause == "off" {
            continue;
        }
        let expr = crate::harness::when::WhenExpr::parse_with_stages(
            std::slice::from_ref(clause),
            Some(&stages),
        )
        .map_err(|error| refuse(error.to_string()))?;
        if expr
            .terms()
            .any(|term| term.team_stage().is_some_and(|key| key.branch().is_some()))
        {
            return Err(refuse("a team's condition reads its own board; use `team.stage` without a branch selector".into()));
        }
        if !expr.terms().all(|term| term.team_stage().is_some()) {
            return Err(refuse("only `team.stage` is admitted; replace forge/window conditions with a team stage condition".into()));
        }
    }
    let team_prompt_declared = crate::harness::team_prompt::declared(team);
    if team_prompt_declared && !team.staged() {
        return Err(LayoutErr::TeamPromptWithoutStages {
            team: name.to_owned(),
        });
    }
    for pattern in &team.scratch_patterns() {
        if let Some(reason) = invalid_scratch_pattern(pattern) {
            return Err(LayoutErr::InvalidScratchPattern {
                team: name.to_owned(),
                pattern: pattern.clone(),
                reason,
            });
        }
    }
    if team.roles.is_empty() && team.layout.is_none() {
        return Err(LayoutErr::EmptyTeam {
            team: name.to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    for binding in &team.roles {
        if invalid_role_name(&binding.role) {
            return Err(LayoutErr::InvalidRoleName {
                team: name.to_owned(),
                name: binding.role.clone(),
            });
        }
        if let Some(reason) = address_grammar_clash(&binding.role) {
            return Err(LayoutErr::RoleShadowsAddress {
                team: name.to_owned(),
                name: binding.role.clone(),
                reason,
            });
        }
        if crate::agents::find_definition(&binding.role).is_some() {
            return Err(LayoutErr::RoleShadowsAddress {
                team: name.to_owned(),
                name: binding.role.clone(),
                reason: "it is a built-in kind handle like `@claude`",
            });
        }
        if !seen.insert(binding.role.clone()) {
            return Err(LayoutErr::DuplicateRole {
                team: name.to_owned(),
                role: binding.role.clone(),
            });
        }
    }
    if let Some(leader) = team.leader.as_deref()
        && !team.roles.is_empty()
        && !team.roles.iter().any(|binding| binding.role == leader)
    {
        return Err(LayoutErr::UnknownLeaderRole {
            team: name.to_owned(),
            leader: leader.to_owned(),
            valid_roles: valid_team_roles(team),
        });
    }
    validate_team_signals(name, team)
}

fn prepare_team<'a>(
    name: &str,
    team: &'a Team,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    base: CellBase<'_>,
    launched_role: Option<&str>,
) -> Result<PreparedTeam<'a>> {
    validate_team_header(name, team)?;
    if team.leader.is_none() {
        return Err(missing_leader(name, team, profiles, commands));
    }
    let unlaunched = base.selection.without_flags();
    let mut roles = Vec::with_capacity(team.roles.len());
    for binding in &team.roles {
        let base = if launched_role.is_none_or(|role| role == binding.role) {
            base
        } else {
            CellBase {
                x: None,
                selection: &unlaunched,
            }
        };
        if let Some(detail) = profiles.failed(&binding.profile) {
            return Err(LayoutErr::FailedProfile(detail.to_owned()));
        }
        if !profiles.0.contains_key(&binding.profile)
            && crate::agents::find_runtime(&binding.profile).is_none()
        {
            return Err(LayoutErr::UnknownRoleProfile {
                team: name.to_owned(),
                role: binding.role.clone(),
                profile: binding.profile.clone(),
            });
        }
        let mut resolved =
            resolve_profile_with_override(&binding.profile, profiles, base, Some(binding))?;
        normalize_auto_compact(&mut resolved.auto_compact, &binding.profile)?;
        if resolved.empty_dir == Some(true) {
            return Err(LayoutErr::TeamEmptyDir {
                definition: binding.profile.clone(),
            });
        }
        let rendered = render_profile_args(&binding.profile, &resolved)?;
        roles.push(PreparedRole {
            role: binding.role.clone(),
            profile: binding.profile.clone(),
            resolved,
            rendered,
            team_prompt: None,
        });
    }
    Ok(PreparedTeam { team, roles })
}

fn resolve_team_prompts(name: &str, prepared: &mut PreparedTeam<'_>) -> Result<()> {
    for role in &mut prepared.roles {
        role.team_prompt = role_team_prompt(
            name,
            prepared.team,
            &role.role,
            &role.resolved.kind,
            role.resolved.system_prompt_file.is_some(),
        )?;
    }
    Ok(())
}

fn validate_flip_compact(
    name: &str,
    prepared: &PreparedTeam<'_>,
    default: Option<crate::config::FlipCompact>,
) -> Result<()> {
    for role in &prepared.roles {
        if prepared.team.flip_compact(&role.role, default).is_some()
            && crate::agents::spec_by_kind(role.resolved.kind.as_str())
                .and_then(|spec| spec.launch.compact_command(""))
                .is_none()
        {
            return Err(LayoutErr::FlipCompactUnsupported {
                team: name.to_owned(),
                role: role.role.clone(),
                kind: role.resolved.kind.clone(),
            });
        }
    }
    Ok(())
}

fn validate_team_signals(name: &str, team: &Team) -> Result<()> {
    for (role, index, binding) in team.roles.iter().flat_map(|role| {
        role.signals
            .iter()
            .enumerate()
            .map(move |(index, binding)| (&role.role, index, binding))
    }) {
        let index = index + 1;
        let selector = crate::harness::signal::parse_signal_selector(
            name,
            &binding.signal,
            Some(&binding.matches),
        )
        .map_err(|source| LayoutErr::InvalidTeamSignal {
            team: name.to_owned(),
            role: role.clone(),
            index,
            source: Box::new(source),
        })?;
        if selector.family() == "agent"
            && !["handle", "session"].iter().any(|key| {
                binding.matches.get(*key).is_some_and(|value| {
                    !value.trim().is_empty() && !crate::harness::signal::is_match_wildcard(value)
                })
            })
        {
            return Err(LayoutErr::UnscopedTeamAgentSignal {
                team: name.to_owned(),
                role: role.clone(),
                index,
                signal: binding.signal.clone(),
            });
        }
    }
    Ok(())
}

fn dedup_first(layers: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    layers.retain(|layer| seen.insert(layer.clone()));
}

fn invalid_scratch_pattern(pattern: &str) -> Option<&'static str> {
    if pattern.trim().is_empty() {
        Some("patterns cannot be empty")
    } else if pattern != pattern.trim() {
        Some("leading or trailing whitespace is not allowed")
    } else if pattern.starts_with('!') {
        Some("negation patterns are not allowed")
    } else if pattern.starts_with('#') {
        Some("comment lines are not patterns")
    } else if pattern.chars().any(|ch| ch.is_ascii_control()) {
        Some("ASCII control characters are not allowed")
    } else if pattern.split('/').any(|segment| segment == "..") {
        Some("`..` path segments are not allowed")
    } else {
        None
    }
}

fn invalid_role_name(name: &str) -> bool {
    name.is_empty()
        || name.chars().any(|ch| {
            ch.is_whitespace() || ch == ',' || ch == '+' || ch == '/' || ch == ':' || ch == '#'
        })
}

fn valid_cells(profiles: &ProfilesConfig, commands: &CommandsConfig) -> String {
    cell_names(profiles, commands)
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ")
}

fn cell_names(profiles: &ProfilesConfig, commands: &CommandsConfig) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    values.insert("term".to_owned());
    values.extend(crate::agents::runtime_names().map(str::to_owned));
    for kind in crate::agents::known_kinds() {
        for mode in PERMISSION_MODE_NAMES {
            let parsed = PermissionMode::from_str(mode).expect("permission mode constant is valid");
            if supported_virtual_agent_args(kind, parsed).is_some() {
                values.insert(format!("{kind}-{mode}"));
            }
        }
    }
    values.extend(commands.0.keys().cloned());
    values.extend(profiles.0.keys().cloned());
    values
}

fn valid_agent_overrides_in(
    profiles: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
) -> String {
    let mut values = crate::agents::runtime_names()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    values.extend(profiles.0.keys().cloned());
    if let Some(fallback) = fallback {
        values.extend(fallback.0.keys().cloned());
    }
    values.into_iter().collect::<Vec<_>>().join(", ")
}

fn cell_candidates(profiles: &ProfilesConfig, commands: &CommandsConfig) -> Vec<SpecCandidate> {
    let mut candidates = BTreeMap::new();
    for name in commands.0.keys() {
        candidates.insert(name.clone(), SpecKind::Command);
    }
    for (name, profile) in &profiles.0 {
        if crate::agents::find_runtime(name).is_none() {
            candidates.entry(name.clone()).or_insert_with(|| {
                let mut listed = profile.clone();
                let fast;
                (listed.effort, fast) = listed_tuning(name, profiles);
                listed.fast = fast.then_some(crate::agents::Toggle::On);
                SpecKind::Profile(Box::new(listed))
            });
        }
    }
    let mut cells = cell_names(profiles, commands);
    for base in cells.clone() {
        let fast = format!("{base}-fast");
        if fast_suffix_cell(&fast, profiles, CellBase::none())
            .ok()
            .flatten()
            .is_some()
        {
            cells.insert(fast);
        }
    }
    for name in cells {
        let kind = if name == "term" || crate::agents::find_runtime(&name).is_some() {
            SpecKind::Kind
        } else {
            SpecKind::Cell
        };
        candidates.entry(name).or_insert(kind);
    }
    ordered_candidates(candidates)
}

fn spec_candidates(
    profiles: &ProfilesConfig,
    team_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Vec<SpecCandidate> {
    let cells = cell_candidates(profiles, commands);
    let mut candidates = cells
        .iter()
        .filter(|candidate| {
            matches!(candidate.kind, SpecKind::Command)
                || (matches!(candidate.kind, SpecKind::Profile(_))
                    && teams.role_spec(&candidate.name).is_none())
        })
        .map(|candidate| (candidate.name.clone(), candidate.kind.clone()))
        .collect::<BTreeMap<_, _>>();
    for (name, team) in &teams.0 {
        candidates
            .entry(name.clone())
            .or_insert_with(|| SpecKind::Team(team_summary(team, team_profiles)));
        for binding in &team.roles {
            candidates.insert(format!("{name}.{}", binding.role), SpecKind::Role);
        }
    }
    for SpecCandidate { name, kind } in cells {
        if teams.role_spec(&name).is_some() {
            continue;
        }
        candidates.entry(name).or_insert(kind);
    }
    ordered_candidates(candidates)
}

fn ordered_candidates(candidates: BTreeMap<String, SpecKind>) -> Vec<SpecCandidate> {
    let mut candidates = candidates
        .into_iter()
        .map(|(name, kind)| SpecCandidate { name, kind })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| {
        (
            match candidate.kind {
                SpecKind::Profile(_) | SpecKind::Command => 0,
                SpecKind::Team(_) => 1,
                SpecKind::Kind | SpecKind::Cell => 2,
                SpecKind::Role => 3,
            },
            candidate.name.clone(),
        )
    });
    candidates
}

fn wrap_choices<'a>(label: &str, names: impl IntoIterator<Item = &'a str>) -> String {
    use unicode_width::UnicodeWidthStr;

    let mut wrapped = format!("{label:<14}");
    let mut width = 14;
    for name in names {
        if width > 14 {
            if width + 2 + name.width() > 85 {
                wrapped.push_str(",\n              ");
                width = 14;
            } else {
                wrapped.push_str(", ");
                width += 2;
            }
        }
        wrapped.push_str(name);
        width += name.width();
    }
    wrapped
}

/// The refusal of a team that declares no `leader`, naming what its `leader` may name: one of its
/// roles, or for a roleless team a [`written_token`] that only one of its cells carries.
fn missing_leader(
    name: &str,
    team: &Team,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> LayoutErr {
    let fix = match team.layout.as_deref() {
        Some(layout) if team.roles.is_empty() => {
            let tokens = parse_layout_structure(layout)
                .map(|structure| {
                    structure
                        .cells()
                        .filter_map(|word| written_token(word, profiles, commands).ok().flatten())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let unique = tokens
                .iter()
                .filter(|token| tokens.iter().filter(|other| other == token).count() == 1)
                .map(String::as_str)
                .collect::<Vec<_>>();
            if unique.is_empty() {
                format!(
                    "no cell of its layout {layout} has a profile or kind of its own, so give one \
                     cell a profile no other cell carries and add `leader = \"<profile>\"` naming it"
                )
            } else {
                format!(
                    "add `leader = \"<profile>\"` naming the profile or kind that only one of its \
                     layout cells carries: {}",
                    unique.join(", ")
                )
            }
        }
        _ => format!(
            "add `leader = \"<role>\"` naming one of its roles: {}",
            valid_team_roles(team)
        ),
    };
    LayoutErr::MissingLeader {
        team: name.to_owned(),
        fix,
    }
}

pub(super) fn valid_team_roles(team: &Team) -> String {
    let values = team
        .roles
        .iter()
        .map(|binding| binding.role.clone())
        .collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests;
