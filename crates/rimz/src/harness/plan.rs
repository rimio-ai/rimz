//! Resolve launch layouts from effective config, then compile them into launch
//! identities and backend-neutral pane commands.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::agents::account::{ProviderStatus, read_accounts_cache};
use crate::agents::definition::LaunchPermissionArgs;
use crate::agents::{PermissionMode, TierSkipReason, pane_title};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::ancestry::LaunchAncestry;
use crate::harness::budget::{BudgetSpec, DailyBudgetScope};
use crate::harness::prompt_compose::SystemPromptSources;
use crate::harness::spec::{AgentCell, Cell, LayoutSpec, TuningSource};
use crate::harness::spend_ladder::{SpendFacts, SpendStep};
use crate::ids::{AgentKind, AgentSessionId, EventId, LaneName};
use crate::mux::{LayoutColumn, LayoutPanes, PaneCmd};
use crate::store::message::AutoCompact;
use crate::store::{
    writer::AgentLaunchIdentity, writer::AgentLaunchName, writer::AgentLaunchRequest,
};

pub struct LaunchAvailability {
    spend: BTreeMap<AgentKind, SpendFacts>,
    auto_redeem: crate::config::AutoRedeemConfig,
    logged_out: BTreeSet<AgentKind>,
    daily_caps: BTreeMap<AgentKind, TierSkipReason>,
    model_pins: BTreeMap<AgentKind, BTreeMap<String, String>>,
    now: jiff::Timestamp,
}

impl LaunchAvailability {
    pub fn read(
        runtime: &RuntimePaths,
        state: &StatePaths,
        config: &crate::config::MachineConfig,
        now: jiff::Timestamp,
    ) -> Self {
        Self::read_as(runtime, state, config, now, |_| {
            crate::store::writer::LaunchLogin::RoomDefault
        })
    }

    /// Judge each kind on the account `login` names for it; a kind where that
    /// account does not resolve contributes no facts.
    pub fn read_as(
        runtime: &RuntimePaths,
        state: &StatePaths,
        config: &crate::config::MachineConfig,
        now: jiff::Timestamp,
        login: impl Fn(&AgentKind) -> crate::store::writer::LaunchLogin,
    ) -> Self {
        let defaults = crate::agents::room_accounts(&state.workspace_record, None, config).ok();
        let logins: BTreeMap<_, _> = crate::agents::known_kinds()
            .filter_map(|kind| {
                let kind = AgentKind::new_unchecked(kind);
                let login = login(&kind)
                    .resolve(&kind, defaults.as_ref()?, &config.accounts)
                    .ok()?;
                Some((kind, login.key()))
            })
            .collect();
        let accounts = read_accounts_cache(&runtime.shared_accounts_path());
        let now_ms = u64::try_from(now.as_millisecond()).unwrap_or_default();
        let provider = crate::agents::spending::read_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
        );
        let selected = crate::agents::RoomLoginSet::new(
            defaults.map(|defaults| {
                defaults.with_names(
                    logins
                        .values()
                        .map(|key| (key.kind.clone(), key.name.clone()))
                        .collect(),
                )
            }),
            Some(crate::agents::LoginCatalog::room_view(&config.accounts).0),
            crate::agents::ambient_env(),
        );
        let in_use = selected.keys_in_use();
        let mut result = Self {
            spend: logins
                .iter()
                .filter(|(_, key)| in_use.contains(*key))
                .map(|(kind, key)| (kind.clone(), SpendFacts::read(runtime, key, now)))
                .collect(),
            auto_redeem: config.harness.auto_redeem.clone(),
            logged_out: Default::default(),
            daily_caps: Default::default(),
            model_pins: config.models.clone(),
            now,
        };
        let catalog = crate::agents::LoginCatalog::room_view(&config.accounts).0;
        let families: BTreeSet<_> = config
            .tiers
            .entries(crate::config::tiers::ModelTier::Intern)
            .filter_map(|(_, kind, _)| kind)
            .chain(
                crate::agents::known_kinds()
                    .filter(|kind| crate::agents::runs_other_providers_models(kind)),
            )
            .collect();
        for kind in families {
            let Some(login) = logins.get(kind) else {
                continue;
            };
            if ProviderStatus::at_launch(accounts.logins.get(login), now_ms)
                == ProviderStatus::LoggedOut
            {
                result.logged_out.insert(login.kind.clone());
            }
            if let Some((spend, cap)) = DailyBudgetScope::account(&catalog, login)
                .exhausted(runtime, state, config, now, &provider)
                && let (Some(spend_usd), Some(cap_usd)) = (
                    serde_json::Number::from_f64(spend),
                    serde_json::Number::from_f64(cap.cap_usd),
                )
            {
                result.daily_caps.insert(
                    login.kind.clone(),
                    TierSkipReason::DailyCap {
                        spend_usd,
                        cap_usd,
                        window: (cap.window == crate::agents::BudgetWindow::Month)
                            .then_some(cap.window),
                    },
                );
            }
        }
        result
    }

    pub fn unavailable(&self, kind: &str, model: &str) -> Option<TierSkipReason> {
        if self.logged_out.contains(kind) {
            return Some(TierSkipReason::LoggedOut);
        }
        let model = self
            .model_pins
            .get(kind)
            .and_then(|pins| pins.get(model))
            .map_or(model, String::as_str);
        if let Some(facts) = self.spend.get(kind)
            && facts.step(&self.auto_redeem, Some(model), self.now) == SpendStep::Spent
            && let Some(capacity) = &facts.capacity
        {
            return Some(TierSkipReason::Exhausted {
                until: capacity.latest_spent_window_reset_for_model(self.now, Some(model)),
            });
        }
        self.daily_caps.get(kind).cloned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionModeChoice {
    Explicit(PermissionMode),
    Default(PermissionMode),
}

#[derive(Clone, Copy, Debug)]
pub struct LaunchCompactPolicy<'a> {
    teams: &'a crate::config::TeamsConfig,
    profiles: &'a crate::config::ProfilesConfig,
    subagent_profiles: &'a crate::config::ProfilesConfig,
    harness: &'a crate::config::HarnessConfig,
    scope: crate::config::effective::ProfileScope,
    team: Option<&'a str>,
}

impl<'a> LaunchCompactPolicy<'a> {
    pub fn new(
        effective: &'a crate::config::effective::LaunchAgents,
        harness: &'a crate::config::HarnessConfig,
        scope: crate::config::effective::ProfileScope,
        team: Option<&'a str>,
    ) -> Self {
        Self {
            teams: &effective.teams,
            profiles: &effective.profiles,
            subagent_profiles: &effective.subagent_profiles,
            harness,
            scope,
            team,
        }
    }

    fn append_warnings(self, cell: &AgentCell, warnings: &mut Vec<LaunchFinalizeWarning>) {
        let seat = crate::config::effective::SmartCompactSeat {
            team: cell.launch.team.as_deref().or(self.team),
            role: cell.launch.role.as_deref(),
            profile: cell.launch.profile.as_deref(),
            agent: cell
                .launch
                .record
                .as_ref()
                .and_then(|record| record.agent.as_deref()),
            kind: Some(cell.kind.as_str()),
            scope: cell.launch.profile_scope.unwrap_or(self.scope),
        };
        let Some(line) = seat.resolve(
            self.teams,
            self.profiles,
            self.subagent_profiles,
            self.harness,
        ) else {
            return;
        };
        let profile = cell.launch.profile.as_deref().unwrap_or(cell.kind.as_str());
        let explicit = seat
            .explicit(self.teams, self.profiles, self.subagent_profiles)
            .is_some();
        if explicit
            && let Some(adapter) = crate::agents::find_definition(&cell.kind)
            && let Some(reason) = crate::agents::reminder_skip(adapter)
        {
            warnings.push(LaunchFinalizeWarning::SmartCompactSkipped {
                profile: profile.to_owned(),
                agent: adapter.spec().kind,
                line,
                reason,
            });
        }
        let AutoCompact::Tokens(tokens) = line else {
            return;
        };
        let Some(AutoCompact::Tokens(window)) = cell
            .auto_compact
            .as_deref()
            .and_then(|value| AutoCompact::parse(value).ok())
        else {
            return;
        };
        if tokens >= window {
            let enforcer = crate::agents::find_definition(&cell.kind)
                .and_then(crate::agents::auto_compact_enforcer)
                .unwrap_or(crate::agents::AutoCompactEnforcer::Native);
            warnings.push(LaunchFinalizeWarning::SmartCompactNativeWindow {
                profile: profile.to_owned(),
                line,
                window: AutoCompact::Tokens(window),
                enforcer,
            });
        } else if explicit
            && crate::agents::find_definition(&cell.kind)
                .and_then(crate::agents::reminder_skip)
                .is_none()
            && let Some(second @ AutoCompact::Tokens(second_tokens)) =
                crate::harness::soft_compact::second_line(line)
            && second_tokens >= window
        {
            warnings.push(LaunchFinalizeWarning::SmartCompactSecondLineWindow {
                profile: profile.to_owned(),
                second,
                window: AutoCompact::Tokens(window),
            });
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LaunchFinalizeOptions<'a> {
    pub smart_compact: Option<LaunchCompactPolicy<'a>>,
    pub agent_base: Option<&'a str>,
    pub permission_mode: Option<PermissionModeChoice>,
    pub isolation: Option<crate::config::Isolation>,
    pub preset: &'a crate::agents::LaunchPreset,
    pub passthrough: &'a [String],
    pub budget: Option<BudgetSpec>,
    pub max_turns: Option<u32>,
    /// The machine config whose model pins pick the model effort settles for.
    pub machine_config: Option<&'a crate::config::MachineConfig>,
}

#[derive(Clone, Debug)]
pub struct ResolvedLaunch {
    pub teams: crate::config::TeamsConfig,
    pub layout: LayoutSpec,
    pub team_name: Option<String>,
}

/// One cell in an explicit cohort resume spec, reduced to matching fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortCell {
    pub kind: crate::ids::AgentKind,
    pub role: Option<String>,
    /// The profile the spec resolved; absent for a bare kind.
    pub profile: Option<String>,
}

/// One agent cell's explicit-resume seed.
#[derive(Clone, Debug, PartialEq)]
pub enum CohortSeed {
    Resume(Box<crate::agents::AgentState>),
    Fresh,
}

/// Explicit cohort resume state parallel to layout agent cells.
#[derive(Clone, Debug, PartialEq)]
pub struct CohortResumePlan {
    pub spec: String,
    pub lane_label: String,
    pub matching_lanes: usize,
    pub seeds: Vec<CohortSeed>,
    pub cwd: Option<PathBuf>,
    pub channel: Option<String>,
    pub fresh: Vec<String>,
    pub refilled: BTreeSet<(AgentKind, AgentSessionId)>,
    pub launch_group: Option<String>,
}

/// Durable identity projected by resume planning for argv compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResumeLaunchIdentity {
    pub checkout: Option<String>,
    pub add_dirs: Vec<String>,
    pub untrusted_checkout: bool,
    pub record: Option<Box<crate::agents::LaunchRecord>>,
    pub kind: crate::ids::AgentKind,
    pub login: Option<crate::ids::LoginName>,
    pub session_id: AgentSessionId,
    pub launch_id: Option<AgentSessionId>,
    /// The row's swap epoch, carried unchanged so the resumed process's
    /// events are not stale on its own row.
    pub epoch: u32,
    pub name: Option<String>,
    pub name_explicit: bool,
    pub profile: Option<String>,
    pub profile_scope: Option<crate::config::effective::ProfileScope>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub automation: Option<crate::agents::Automation>,
    pub launch_group: Option<String>,
    pub launch_ordinal: Option<u32>,
    pub channel: Option<String>,
    pub lane: Option<crate::ids::Lane>,
    pub launch_depth: Option<u8>,
    pub launched_by: Option<crate::agents::LaunchedBy>,
    pub isolation: Option<crate::config::Isolation>,
    pub tier: Option<Box<crate::agents::TierStamp>>,
    /// The loop fire the resumed launch is, with its booking chain: the row's own, or a new
    /// fire's when that fire resumes it.
    pub loop_fire: Option<crate::ids::FireId>,
    pub loop_booked_by: Vec<crate::agents::LaunchedBy>,
}

impl From<&crate::agents::AgentState> for ResumeLaunchIdentity {
    fn from(agent: &crate::agents::AgentState) -> Self {
        Self {
            checkout: agent.checkout.clone(),
            add_dirs: agent.add_dirs.clone(),
            untrusted_checkout: agent.untrusted_checkout,
            record: agent.record.clone(),
            kind: agent.kind.clone(),
            login: agent.login.clone(),
            session_id: agent.agent_id.clone(),
            launch_id: agent.launch_id.clone(),
            epoch: agent.epoch,
            name: agent.name.clone(),
            name_explicit: agent.name_explicit,
            profile: agent.profile.clone(),
            profile_scope: agent.profile_scope,
            role: agent.role.clone(),
            team: agent.team.clone(),
            automation: agent.automation.clone(),
            launch_group: agent.launch_group.clone(),
            launch_ordinal: agent.launch_ordinal,
            channel: agent
                .lane
                .as_ref()
                .and_then(crate::ids::Lane::pin_name)
                .map(Into::into),
            lane: agent.lane.clone(),
            launch_depth: agent.launch_depth,
            launched_by: agent.launched_by.clone(),
            isolation: agent.isolation,
            tier: agent.tier.clone(),
            loop_fire: agent.loop_fire.clone(),
            loop_booked_by: agent.loop_booked_by.clone(),
        }
    }
}

/// Finalized cell values applied by a resume launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeLaunchPosture {
    pub notices: Vec<crate::harness::spec::CellNotice>,
    pub resume_model_override: bool,
    pub tier: Option<Box<crate::agents::TierStamp>>,
    pub record: Option<Box<crate::agents::LaunchRecord>>,
    pub isolation_default: Option<crate::config::Isolation>,
    pub isolation: Option<crate::config::Isolation>,
    pub args: Vec<String>,
    pub auto_compact: Option<String>,
    pub tuning: crate::harness::spec::TuningSources,
    pub system_prompt_file: Option<crate::config::PromptSource>,
    pub append_system_prompt_files: Vec<crate::config::PromptSource>,
    pub team_prompt: Option<crate::harness::team_prompt::TeamPrompt>,
    pub skills: Option<Vec<crate::config::SkillName>>,
    pub seat_consensus_skill: bool,
    pub mcps: Option<Vec<crate::config::McpName>>,
    pub no_skill_tool: bool,
    pub rimz_capabilities: Option<Vec<crate::config::RimzEntry>>,
    pub allowed_tools: Option<Vec<crate::config::ToolRule>>,
    pub tools: Option<Vec<String>>,
    pub mode: Option<PermissionMode>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: Option<bool>,
    pub code_mode: Option<bool>,
    pub budget: Option<String>,
}

impl ResumeLaunchPosture {
    pub(super) fn into_cell(self, kind: AgentKind, profile: Option<String>) -> AgentCell {
        AgentCell {
            notices: self.notices,
            kind,
            empty_dir: None,
            timeout: None,
            auto_compact: self.auto_compact,
            tuning: self.tuning,
            resume_model_override: self.resume_model_override,
            isolation_default: self.isolation_default,
            args: self.args,
            system_prompt_file: self.system_prompt_file,
            append_system_prompt_files: self.append_system_prompt_files,
            team_prompt: self.team_prompt,
            leads_team: false,
            skills: self.skills,
            seat_consensus_skill: self.seat_consensus_skill,
            mcps: self.mcps,
            no_skill_tool: self.no_skill_tool,
            rimz_capabilities: self.rimz_capabilities,
            allowed_tools: self.allowed_tools,
            tools: self.tools,
            launch: crate::agents::LaunchParams {
                profile,
                tier: self.tier,
                record: self.record,
                isolation: self.isolation,
                mode: self.mode,
                model: self.model,
                effort: self.effort,
                fast: self.fast,
                code_mode: self.code_mode,
                budget: self.budget,
                ..Default::default()
            },
        }
    }

    /// The layout's check of the lists only an agent file carries, for a fresh launch of this
    /// posture that no layout carries: a molt, or a restart whose conversation cannot resume.
    pub fn validate_fresh_agent_file_lists(
        &self,
        kind: &AgentKind,
    ) -> std::result::Result<(), LaunchFinalizeError> {
        validate_fresh_agent_file_lists(&self.clone().into_cell(kind.clone(), None))
    }

    /// Apply the posture's arguments and prompt/skill defaults to a launch action.
    pub fn exec_request(
        &self,
        kind: crate::ids::AgentKind,
        mut action: crate::harness::launch::ExecAction,
    ) -> crate::harness::launch::ExecRequest {
        *action.extra_args_mut() = self.args.clone();
        let reminder_section = self
            .record
            .as_ref()
            .filter(|_| !matches!(&action, crate::harness::launch::ExecAction::Fork { .. }))
            .and_then(|record| record.loop_section.clone())
            .map(crate::harness::launch::ReminderSection::Loop);
        crate::harness::launch::ExecRequest {
            action,
            system_prompt_file: self.system_prompt_file.clone(),
            append_system_prompt_files: self.append_system_prompt_files.clone(),
            team_prompt: self.team_prompt.clone(),
            skills: self.skills.clone(),
            seat_consensus_skill: self.seat_consensus_skill,
            mcps: self.mcps.clone(),
            no_skill_tool: self.no_skill_tool,
            rimz_capabilities: self.rimz_capabilities.clone(),
            allowed_tools: self.allowed_tools.clone(),
            tools: self.tools.clone(),
            isolation_default: self.isolation_default,
            reminder_section,
            ..crate::harness::launch::ExecRequest::bare_launch(kind, Vec::new())
        }
    }
}

impl From<&AgentCell> for ResumeLaunchPosture {
    fn from(cell: &AgentCell) -> Self {
        Self {
            notices: cell.notices.clone(),
            resume_model_override: cell.resume_model_override,
            tier: cell.launch.tier.clone(),
            record: cell.launch.record.clone(),
            isolation: cell.launch.isolation,
            args: cell.args.clone(),
            auto_compact: cell.auto_compact.clone(),
            tuning: cell.tuning.clone(),
            system_prompt_file: cell.system_prompt_file.clone(),
            append_system_prompt_files: cell.append_system_prompt_files.clone(),
            team_prompt: cell.team_prompt.clone(),
            skills: cell.skills.clone(),
            seat_consensus_skill: cell.seat_consensus_skill,
            mcps: cell.mcps.clone(),
            no_skill_tool: cell.no_skill_tool,
            rimz_capabilities: cell.rimz_capabilities.clone(),
            allowed_tools: cell.allowed_tools.clone(),
            tools: cell.tools.clone(),
            isolation_default: cell.isolation_default,
            mode: cell.launch.mode,
            model: cell.launch.model.clone(),
            effort: cell.launch.effort.clone(),
            fast: cell.launch.fast,
            code_mode: cell.launch.code_mode,
            budget: cell.launch.budget.clone(),
        }
    }
}

/// A resumed session's isolation: the launch's `--isolation` override, else
/// the stored value. `None` follows the profile default, then machine policy.
pub fn effective_resume_isolation(
    cell: Option<crate::config::Isolation>,
    stored: Option<crate::config::Isolation>,
) -> Option<crate::config::Isolation> {
    cell.or(stored)
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChildIsolation {
    pub isolation: Option<crate::config::Isolation>,
    pub clamped: Option<ClampSource>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClampSource {
    ProfileDefault {
        requested: crate::config::Isolation,
        parent: crate::config::Isolation,
    },
    MachinePolicy {
        requested: crate::config::Isolation,
        parent: crate::config::Isolation,
    },
}

impl std::fmt::Display for ClampSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (source, requested, parent) = match self {
            Self::ProfileDefault { requested, parent } => ("its definition", requested, parent),
            Self::MachinePolicy { requested, parent } => ("machine policy", requested, parent),
        };
        write!(f, "runs {parent}: {source} asks for {requested} isolation")
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "a subagent never runs looser than its parent: --isolation {requested} is unavailable under a {parent} parent; launch it from a {requested} agent or a host shell"
)]
pub struct ChildIsolationErr {
    requested: crate::config::Isolation,
    parent: crate::config::Isolation,
}

pub fn cap_child_isolation(
    parent: Option<crate::config::Isolation>,
    child_override: Option<crate::config::Isolation>,
    child_default: Option<crate::config::Isolation>,
    machine: crate::config::Isolation,
) -> Result<ChildIsolation, ChildIsolationErr> {
    use crate::config::Isolation;
    let parent = parent.unwrap_or_default();
    let requested = Isolation::resolve(child_override, child_default, machine);
    if requested >= parent {
        return Ok(ChildIsolation {
            isolation: child_override,
            clamped: None,
        });
    }
    if child_override.is_some() {
        return Err(ChildIsolationErr { requested, parent });
    }
    Ok(ChildIsolation {
        isolation: Some(parent),
        clamped: Some(if child_default.is_some() {
            ClampSource::ProfileDefault { requested, parent }
        } else {
            ClampSource::MachinePolicy { requested, parent }
        }),
    })
}

/// A default grant lowered to the parent's recorded ceiling.
#[derive(Debug, PartialEq, Eq)]
pub struct PermissionClamp {
    pub mode: PermissionMode,
    pub requested: PermissionMode,
    pub source: PermissionClampSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionClampSource {
    AutoDefault,
    ProfileDefault,
    ModeCell,
}

impl fmt::Display for PermissionClampSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AutoDefault => "the auto default",
            Self::ProfileDefault => "its definition",
            Self::ModeCell => "its <kind>-<mode> cell",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChildPermissionErr {
    #[error(
        "requested {requested} mode exceeds parent mode {parent}: a subagent never runs looser than its parent; launch the parent with --yolo or a mode: yolo definition",
        parent = parent_permission_label(*.parent)
    )]
    Explicit {
        requested: PermissionMode,
        parent: Option<PermissionMode>,
    },
    #[error(
        "child mode {child} exceeds parent mode {parent}: a subagent never runs looser than its parent; launch a new child",
        parent = parent_permission_label(*.parent)
    )]
    Resume {
        child: PermissionMode,
        parent: Option<PermissionMode>,
    },
}

fn parent_permission_label(mode: Option<PermissionMode>) -> String {
    mode.map_or_else(
        || "unrecorded (treated as auto)".to_owned(),
        |mode| mode.to_string(),
    )
}

fn cap_child_permission(
    child_kind: &AgentKind,
    caller: Option<&crate::agents::AgentState>,
    explicit: Option<PermissionMode>,
    default: Option<PermissionMode>,
    source: PermissionClampSource,
) -> Result<(PermissionModeChoice, Option<PermissionClamp>), ChildPermissionErr> {
    let choice = explicit.map_or(
        PermissionModeChoice::Default(PermissionMode::Auto),
        PermissionModeChoice::Explicit,
    );
    let Some(caller) = caller else {
        return Ok((choice, None));
    };
    let permission = LaunchPermissionArgs::of_kind(child_kind.as_str());
    let requested = permission
        .resolve_mode(explicit.or(default))
        .unwrap_or(PermissionMode::Auto);
    let ceiling = permission
        .resolve_mode(Some(caller.mode.unwrap_or(PermissionMode::Auto)))
        .unwrap_or(PermissionMode::Auto);
    if requested.rank() <= ceiling.rank() {
        return Ok((choice, None));
    }
    if explicit.is_some() {
        return Err(ChildPermissionErr::Explicit {
            requested,
            parent: caller.mode,
        });
    }
    Ok((
        PermissionModeChoice::Explicit(ceiling),
        Some(PermissionClamp {
            mode: ceiling,
            requested,
            source,
        }),
    ))
}

/// Refuse a parent-message resume above the parent's recorded grant, mapping both through the child
/// kind. Unrecorded parents count as auto before mapping; a resumed grant is never lowered.
pub fn check_child_permission_mode(
    child_kind: &AgentKind,
    parent: Option<PermissionMode>,
    child: Option<PermissionMode>,
) -> Result<(), ChildPermissionErr> {
    let permission = LaunchPermissionArgs::of_kind(child_kind.as_str());
    let child = permission
        .resolve_mode(child)
        .unwrap_or(PermissionMode::Auto);
    let ceiling = permission
        .resolve_mode(Some(parent.unwrap_or(PermissionMode::Auto)))
        .unwrap_or(PermissionMode::Auto);
    if child.rank() <= ceiling.rank() {
        return Ok(());
    }
    Err(ChildPermissionErr::Resume { child, parent })
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveLaunchError {
    #[error(transparent)]
    Layout(#[from] crate::harness::spec::LayoutErr),
    #[error(transparent)]
    Effective(#[from] crate::config::effective::EffectiveConfigErr),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{origin} {field} `{}` not found; create it or fix the launch config",
    path.display()
)]
pub struct ProfilePromptFileError {
    origin: String,
    field: &'static str,
    path: PathBuf,
}

/// The one cell of a multi-cell spec that failed to resolve, so a failed definition refuses by name.
fn unknown_cell(err: &crate::harness::spec::LayoutErr) -> Option<&str> {
    match err {
        crate::harness::spec::LayoutErr::UnknownCell(unknown) => Some(&unknown.word),
        _ => None,
    }
}

fn block_failed_team_prompt(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
    err: &crate::harness::spec::LayoutErr,
    agent_override: Option<&str>,
) -> std::result::Result<(), crate::config::effective::EffectiveConfigErr> {
    let crate::harness::spec::LayoutErr::TeamPromptWithoutBase { team, role, .. } = err else {
        return Ok(());
    };
    // Base refusals name a role prepared from this same team table.
    let binding = launch.teams.0[team]
        .roles
        .iter()
        .find(|binding| binding.role == *role)
        .expect("team prompt refusal names a prepared role");
    launch.block_failed_profile(
        &binding.profile,
        crate::config::effective::ProfileScope::Agents,
        &launch.profiles,
    )?;
    if let Some(name) = agent_override {
        block_failed_override(launch, scope, name)?;
    }
    Ok(())
}

/// Refuse a failed `--agent` name of a launch in `scope`, read from its table then, on a
/// subagent launch, the agents table.
fn block_failed_override(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
    name: &str,
) -> std::result::Result<(), crate::config::effective::EffectiveConfigErr> {
    let fallback =
        (scope == crate::config::effective::ProfileScope::Subagents).then_some(&launch.profiles);
    launch.block_failed_override(name, scope, launch.profiles_for(scope), fallback)
}

/// Refuse a failed name the spec reads, the spec itself and then each of its cells, ahead of the
/// refusal its resolution gave: cells resolve in order, so a tier walk on one would otherwise
/// answer over the failed file of a cell after it.
fn block_failed_spec_words(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
    spec: Option<&str>,
) -> std::result::Result<(), crate::config::effective::EffectiveConfigErr> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return Ok(());
    };
    for word in spec_words(spec) {
        launch.block_failed_reference(scope, Some(word), None)?;
    }
    Ok(())
}

/// The words a launch names: the spec whole, then each of its cells.
fn spec_words(spec: &str) -> impl Iterator<Item = &str> {
    let cells = crate::harness::spec::parse_layout_structure(spec)
        .map(|layout| {
            layout
                .columns
                .into_iter()
                .flat_map(|column| column.cells)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    std::iter::once(spec).chain(cells)
}

fn block_failed_kind_bases(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
    layout: &LayoutSpec,
    spec: Option<&str>,
    agent_override: Option<&str>,
) -> std::result::Result<(), crate::config::effective::EffectiveConfigErr> {
    let profiles = launch.profiles_for(scope);
    // A failed row refuses the launches whose words name it: the spec and each of its cells.
    let spec = spec.map(str::trim).filter(|spec| !spec.is_empty());
    for name in spec.into_iter().flat_map(spec_words).filter(|name| {
        crate::agents::find_runtime(name).is_some() || profiles.0.contains_key(*name)
    }) {
        launch.block_failed_profile(name, scope, profiles)?;
    }
    if let Some(name) = agent_override {
        block_failed_override(launch, scope, name)?;
    }
    let cell_scope = if spec
        .and_then(|spec| crate::harness::spec::spec_team(spec, &launch.teams))
        .is_some()
    {
        crate::config::effective::ProfileScope::Agents
    } else {
        scope
    };
    let table = launch.profiles_for(cell_scope);
    for cell in layout.agent_cells() {
        let kind = cell.kind.as_str();
        let name = match cell.launch.profile.as_deref() {
            Some(profile) => profile,
            // A cell with no profile runs on its kind's base, whose failure is the only one it
            // reads: a written, built-in or absent base alike, never a failed row named after
            // the kind, which the words above refuse where the launch names it.
            None if table.failed_base(kind).is_some() => kind,
            None => continue,
        };
        launch.block_failed_profile(name, cell_scope, table)?;
    }
    Ok(())
}

/// Refuse a `--subagent-profile` spec that is not one `[subagents.profiles]` name, before
/// anything resolves: under the subagent scope `resolve_launch` still resolves teams, roles,
/// kinds, commands, and built-in cells. A name with no healthy subagent row refuses with the
/// failure a subagent launch of it reads, and a name nothing defines passes, for resolution to
/// report.
pub fn check_subagent_profile_spec(
    launch: &crate::config::effective::LaunchAgents,
    commands: &crate::config::CommandsConfig,
    spec: Option<&str>,
) -> Result<()> {
    const TAKES: &str =
        "--subagent-profile takes one profile name defined under [subagents.profiles]";
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        bail!("{TAKES}; name one (`rimz subagents profiles` lists them)");
    };
    if launch
        .subagent_profiles
        .0
        .get(spec)
        .is_some_and(|profile| !profile.kind_base)
    {
        return Ok(());
    }
    launch.block_failed_reference(
        crate::config::effective::ProfileScope::Subagents,
        Some(spec),
        None,
    )?;
    let what = if launch.teams.0.contains_key(spec) {
        "is a team"
    } else if launch.teams.role_spec(spec).is_some() {
        "is a team role"
    } else if crate::agents::find_definition(spec).is_some() {
        "is an agent kind"
    } else if launch.profiles.0.contains_key(spec) {
        "is defined only under [agents.profiles]; drop --subagent-profile to launch it"
    } else if commands.0.contains_key(spec) {
        "is an [agents.commands] entry"
    } else if crate::harness::spec::is_cell_word(spec, &launch.subagent_profiles, commands) {
        "is a built-in cell or a kind or profile variant, not a profile name"
    } else if !spec
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || matches!(char, '-' | '_'))
    {
        "is not a single profile name"
    } else {
        return Ok(());
    };
    bail!("{TAKES}, and `{spec}` {what}");
}

/// Resolve effective profiles/teams without applying runtime launch options.
pub fn resolve_launch(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
    commands: &crate::config::CommandsConfig,
    spec: Option<&str>,
    selection: &crate::config::Selection,
) -> std::result::Result<ResolvedLaunch, ResolveLaunchError> {
    let agent_override = selection.agent_name();
    launch.block_set_failure()?;
    if let Some(raw) = spec.map(str::trim).filter(|raw| !raw.is_empty())
        && !commands.0.contains_key(raw)
    {
        launch.block_failed_team(raw)?;
    }
    let profiles = launch.profiles_for(scope);
    let fallback =
        (scope == crate::config::effective::ProfileScope::Subagents).then_some(&launch.profiles);
    let layout = match crate::harness::spec::resolve_spec_in_scope(
        spec,
        profiles,
        &launch.profiles,
        fallback,
        commands,
        &launch.teams,
        selection,
    ) {
        Ok(layout) => layout,
        Err(err @ crate::harness::spec::LayoutErr::UnknownSpec(_))
        | Err(err @ crate::harness::spec::LayoutErr::UnknownCell(_)) => {
            launch.block_untrusted_reference(scope, spec, commands)?;
            launch.block_failed_reference(scope, spec, agent_override)?;
            launch.block_failed_reference(scope, unknown_cell(&err), None)?;
            if let Some(name) = spec
                .map(str::trim)
                .filter(|name| other_profiles(launch, scope).0.contains_key(*name))
            {
                return Err(wrong_doorway(name, scope).into());
            }
            return Err(err.into());
        }
        Err(err @ crate::harness::spec::LayoutErr::UnknownAgentOverride { .. }) => {
            launch.block_failed_reference(scope, None, agent_override)?;
            if let Some(name) = agent_override
                .map(str::trim)
                .filter(|name| other_profiles(launch, scope).0.contains_key(*name))
            {
                return Err(wrong_doorway(name, scope).into());
            }
            return Err(err.into());
        }
        // A `--tier` walk under an `--agent` that resolved names why each kind it tried
        // failed, that agent's own failure included, so no failure record answers over it.
        Err(err @ crate::harness::spec::LayoutErr::Engine(_))
            if selection.tier_flag().is_some() =>
        {
            block_failed_spec_words(launch, scope, spec)?;
            return Err(err.into());
        }
        Err(err) => {
            block_failed_spec_words(launch, scope, spec)?;
            block_failed_team_prompt(launch, scope, &err, agent_override)?;
            if let Some(name) = agent_override {
                block_failed_override(launch, scope, name)?;
            }
            return Err(err.into());
        }
    };
    block_failed_kind_bases(launch, scope, &layout, spec, agent_override)?;
    if selection.tier_flag().is_some() && layout.agent_cells().next().is_none() {
        return Err(crate::harness::spec::LayoutErr::Engine(format!(
            "--tier needs an agent cell, and `{}` launches none",
            spec.unwrap_or_default().trim()
        ))
        .into());
    }
    let team_name = spec
        .and_then(|spec| crate::harness::spec::spec_team(spec, &launch.teams))
        .map(str::to_owned);
    Ok(ResolvedLaunch {
        teams: launch.teams.clone(),
        layout,
        team_name,
    })
}

fn other_profiles(
    launch: &crate::config::effective::LaunchAgents,
    scope: crate::config::effective::ProfileScope,
) -> &crate::config::ProfilesConfig {
    use crate::config::effective::ProfileScope;
    match scope {
        ProfileScope::Agents => &launch.subagent_profiles,
        ProfileScope::Subagents => &launch.profiles,
    }
}

fn wrong_doorway(
    name: &str,
    scope: crate::config::effective::ProfileScope,
) -> crate::harness::spec::LayoutErr {
    use crate::config::effective::ProfileScope;
    let (defined_in, command, expected_in, names_flag) = match scope {
        ProfileScope::Agents => ("subagents", "`rimz agents`", "agents", true),
        ProfileScope::Subagents => ("agents", "`rimz subagents`", "subagents", false),
    };
    crate::harness::spec::LayoutErr::ProfileWrongDoorway {
        name: name.to_owned(),
        defined_in,
        command,
        expected_in,
        names_flag,
    }
}

fn validate_agent_prompt_files(
    cell: &AgentCell,
) -> std::result::Result<(), ProfilePromptFileError> {
    let origin = || match (cell.launch.role.as_deref(), cell.launch.profile.as_deref()) {
        (Some(role), Some(profile)) => format!("role `{role}` profile `{profile}`"),
        (Some(role), None) => format!("role `{role}`"),
        (None, Some(profile)) => format!("profile `{profile}`"),
        (None, None) => "agent cell".to_owned(),
    };
    if let Some(path) = cell
        .system_prompt_file
        .as_ref()
        .and_then(crate::config::PromptSource::file)
        && !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    {
        return Err(ProfilePromptFileError {
            origin: origin(),
            field: "system-prompt-file",
            path: path.to_path_buf(),
        });
    }
    for path in cell
        .append_system_prompt_files
        .iter()
        .filter_map(crate::config::PromptSource::file)
    {
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(ProfilePromptFileError {
                origin: origin(),
                field: "append-system-prompt-files entry",
                path: path.to_path_buf(),
            });
        }
    }
    let Some(team_prompt) = &cell.team_prompt else {
        return Ok(());
    };
    let consensus_file = match &team_prompt.consensus {
        crate::harness::team_prompt::Consensus::BuiltIn => None,
        crate::harness::team_prompt::Consensus::File(path) => {
            Some(("team consensus-file", path.as_path()))
        }
    };
    for (field, path) in consensus_file.into_iter().chain(
        team_prompt
            .files
            .iter()
            .filter_map(crate::config::PromptSource::file)
            .map(|path| ("team append-system-prompt-files entry", path)),
    ) {
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(ProfilePromptFileError {
                origin: origin(),
                field,
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Normalize an optional launch preset override, dropping blank values.
pub fn normalized_preset_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Reject a likely comma-separated spec typo parsed as a supervised prompt.
pub fn reject_prompt_that_looks_like_spec(
    spec: Option<&str>,
    prompt: Option<&str>,
    profiles: &crate::config::ProfilesConfig,
    commands: &crate::config::CommandsConfig,
    layouts: &crate::config::TeamsConfig,
) -> Result<()> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return Ok(());
    };
    let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) else {
        return Ok(());
    };
    if crate::harness::spec::is_known_spec_token(prompt, profiles, commands, layouts) {
        bail!(
            "prompt `{prompt}` looks like another spec cell; did you mean `rimz agents {spec},{prompt}`?"
        );
    }
    Ok(())
}

/// One supervised run's launch: a layout holding exactly one cell, an agent cell.
#[derive(Debug)]
pub struct SupervisedLaunch {
    pub layout: LayoutSpec,
    pub team_name: Option<String>,
    /// The lane a bare team role was qualified in; set only when qualification
    /// rewrote the spec.
    pub inferred_lane: Option<LaneName>,
    /// Why a subagent's isolation was capped at its scoped parent's.
    pub clamped: Option<ClampSource>,
    /// A higher default permission grant lowered to the caller's recorded mode.
    pub permission_clamp: Option<PermissionClamp>,
    /// A yolo child needs the scoped view's sudo/setuid note, with a usable host override.
    pub yolo_sandbox: bool,
}

/// Whether launch `spec` names its resolved team `team` whole, with or without a `#lane`, rather
/// than one of the team's seats.
pub(crate) fn names_team(spec: &str, team: Option<&str>) -> bool {
    let spec = spec.split_once('#').map_or(spec, |(team, _)| team);
    team.is_some_and(|team| team == spec)
}

/// Plan a supervised run's one-cell launch over the caller's one effective
/// config load. `lane` is the launch channel and the team it belongs to;
/// `parent_isolation` is the isolation the launching process runs under;
/// `login` names the account each kind's tier availability is judged on.
/// Finalize warnings land in `warnings`, also when a later refusal returns.
#[expect(
    clippy::too_many_arguments,
    reason = "each input is a distinct caller-resolved fact; the planner has one caller"
)]
pub fn plan_supervised_launch(
    request: &crate::harness::run::SupervisedRunRequest,
    caller: Option<&crate::agents::AgentState>,
    lane: Option<(&LaneName, &str)>,
    parent_isolation: Option<crate::config::Isolation>,
    workspace: &crate::workspace::ResolvedWorkspace,
    machine_config: &crate::config::MachineConfig,
    effective: crate::config::effective::LaunchAgents,
    login: impl Fn(&AgentKind) -> crate::store::writer::LaunchLogin,
    warnings: &mut Vec<LaunchFinalizeWarning>,
) -> Result<SupervisedLaunch> {
    let scope = if request.subagent || request.subagent_profile {
        crate::config::effective::ProfileScope::Subagents
    } else {
        crate::config::effective::ProfileScope::Agents
    };
    let commands = &machine_config.agents.commands;
    if request.subagent_profile {
        check_subagent_profile_spec(&effective, commands, Some(&request.spec))?;
    }
    let mut spec = Cow::Borrowed(request.spec.as_str());
    let mut inferred_lane = None;
    // --subagent-profile already names one profile, so a lane's team role never claims it.
    if let Some((channel, team)) = lane.filter(|_| !request.subagent_profile) {
        let qualified = crate::harness::spec::qualify_spec_in_channel(
            &request.spec,
            channel.as_str(),
            team,
            &effective.teams,
            effective.profiles_for(scope),
            commands,
        )?;
        if let Cow::Owned(qualified) = qualified {
            spec = Cow::Owned(qualified);
            inferred_lane = Some(channel.clone());
        }
    }
    let isolation = request.isolation.or_else(|| {
        caller
            .filter(|_| request.subagent)
            .and_then(|caller| caller.isolation)
    });
    let state = StatePaths::for_project_root(&workspace.project_root)?;
    let runtime = RuntimePaths::for_state(&state)?;
    let availability = LaunchAvailability::read_as(
        &runtime,
        &state,
        machine_config,
        jiff::Timestamp::now(),
        login,
    );
    let mut selection = crate::config::Selection::new(&machine_config.tiers)
        .agent(request.agent.as_deref())
        .model(request.model.as_deref())
        .tier(request.tier);
    // An explicit account pin binds the provider too, without availability fallback.
    if !matches!(request.login, crate::store::writer::LaunchLogin::Pinned(_)) {
        selection =
            selection.availability(move |kind, model| availability.unavailable(kind, model));
    }
    let mut resolved = resolve_launch(&effective, scope, commands, Some(&spec), &selection)?;
    reject_prompt_that_looks_like_spec(
        Some(&spec),
        Some(&request.prompt),
        effective.profiles_for(scope),
        commands,
        &effective.teams,
    )?;
    // An `agent` call's run refuses as the call's peer launch does, in the tool's terms; the CLI's
    // `--stay false` runs a team seat alone, as `-p --bg` did.
    let cells: usize = resolved
        .layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum();
    let agents = resolved.layout.agent_cells().count();
    let one_agent = request
        .call
        .as_ref()
        .is_some_and(|call| request.background || call.schema.is_some());
    if one_agent && names_team(&spec, resolved.team_name.as_deref()) {
        let spec = spec.clone().into_owned();
        return Err(crate::harness::call::SeatRefusal::Team { spec }.into());
    }
    if request.call.is_some() && request.background {
        use crate::harness::call::SeatRefusal;
        let spec = spec.clone().into_owned();
        if cells > 1 {
            return Err(SeatRefusal::Layout { spec }.into());
        }
        if agents == 0 {
            return Err(SeatRefusal::NoAgent { spec }.into());
        }
        // A seat run alone would take the team's lane with no team run behind it.
        if !request.team_seat_alone
            && let Some(team) = resolved.team_name.clone()
        {
            return Err(SeatRefusal::TeamSeat { spec, team }.into());
        }
    }
    if agents != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    let mut cells = resolved
        .layout
        .columns
        .iter()
        .flat_map(|column| &column.rows);
    let (Some(Cell::Agent(cell)), None) = (cells.next(), cells.next()) else {
        bail!("--print requires a single-cell agent layout");
    };
    let profiles = effective.profiles_for(scope);
    let (cell_spec, _) = crate::harness::spec::split_inline_role(spec.trim(), profiles, commands);
    let mode_cell = crate::harness::spec::fast_suffix_base(cell_spec).unwrap_or(cell_spec);
    let source = if cell.launch.mode.is_none() {
        PermissionClampSource::AutoDefault
    } else if !profiles.0.contains_key(cell_spec)
        && !profiles.0.contains_key(mode_cell)
        && crate::harness::spec::virtual_agent_parts(mode_cell).is_some()
    {
        PermissionClampSource::ModeCell
    } else {
        PermissionClampSource::ProfileDefault
    };
    let (permission_mode, permission_clamp) = cap_child_permission(
        &cell.kind,
        caller.filter(|_| request.subagent),
        request.permission_mode,
        cell.launch.mode,
        source,
    )?;
    let preset = crate::agents::LaunchPreset {
        model: normalized_preset_value(request.model.as_deref()),
        effort: normalized_preset_value(request.effort.as_deref()),
        auto_compact: None,
        system_prompt_file: request.system_prompt_file.clone(),
        append_system_prompt_files: request.append_system_prompt_files.clone(),
        fast: request.fast,
    };
    let finalized = finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            smart_compact: Some(LaunchCompactPolicy::new(
                &effective,
                &machine_config.harness,
                scope,
                resolved.team_name.as_deref(),
            )),
            agent_base: request.agent.as_deref(),
            permission_mode: Some(permission_mode),
            isolation,
            preset: &preset,
            passthrough: &request.passthrough,
            budget: request.budget,
            max_turns: request.max_turns,
            machine_config: Some(machine_config),
        },
    );
    match finalized {
        Ok(finalize_warnings) => warnings.extend(finalize_warnings),
        Err(err) => {
            warnings.extend_from_slice(err.warnings());
            return Err(err.into());
        }
    }
    let mut layout = resolved.layout;
    let cell = layout
        .agent_cells_mut()
        .next()
        .expect("the supervised layout was checked to hold exactly one agent cell");
    if request.subagent_profile {
        cell.launch.profile_scope = Some(scope);
    }
    let mut clamped = None;
    if request.subagent {
        let capped = cap_child_isolation(
            parent_isolation,
            cell.launch.isolation,
            cell.isolation_default,
            machine_config.agents.isolation,
        )?;
        cell.launch.isolation = capped.isolation;
        clamped = capped.clamped;
    }
    let yolo_sandbox = request.subagent
        && cell.launch.mode == Some(PermissionMode::Yolo)
        && !parent_isolation.is_some_and(crate::config::Isolation::is_scoped)
        && crate::config::Isolation::resolve(
            cell.launch.isolation,
            cell.isolation_default,
            machine_config.agents.isolation,
        )
        .is_scoped();
    Ok(SupervisedLaunch {
        layout,
        team_name: resolved.team_name,
        inferred_lane,
        clamped,
        permission_clamp,
        yolo_sandbox,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchFinalizeWarning {
    CellNotice {
        profile: String,
        kind: AgentKind,
        notice: crate::harness::spec::CellNotice,
    },
    SkippedField {
        profile: String,
        agent: &'static str,
        field: crate::agents::definition::SkippedPresetField,
    },
    SmartCompactSkipped {
        profile: String,
        agent: &'static str,
        line: AutoCompact,
        reason: crate::agents::SoftCompactSkip,
    },
    SmartCompactNativeWindow {
        profile: String,
        line: AutoCompact,
        window: AutoCompact,
        enforcer: crate::agents::AutoCompactEnforcer,
    },
    SmartCompactSecondLineWindow {
        profile: String,
        second: AutoCompact,
        window: AutoCompact,
    },
    TierMoved {
        profile: String,
        stamp: crate::agents::TierStamp,
    },
    LaterModelWins {
        profile: String,
        setting: String,
        model: String,
    },
    DeclaredFieldWins {
        profile: String,
        setting: String,
        field: &'static str,
        value: String,
    },
    DeclaredPromptWins {
        profile: String,
        setting: String,
    },
}

impl LaunchFinalizeWarning {
    /// Drain cell notices after model overrides have been applied.
    pub fn take_cell_notices(cell: &mut AgentCell, profile: &str) -> Vec<Self> {
        let mut warnings = Vec::new();
        for notice in std::mem::take(&mut cell.notices) {
            if matches!(
                &notice,
                crate::harness::spec::CellNotice::DroppedModel { .. }
            ) && cell.launch.model.is_some()
            {
                continue;
            }
            warnings.push(Self::CellNotice {
                profile: profile.to_owned(),
                kind: cell.kind.clone(),
                notice,
            });
        }
        warnings
    }
}

impl fmt::Display for LaunchFinalizeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CellNotice {
                profile,
                kind,
                notice,
            } => {
                use crate::harness::spec::CellNotice;
                match notice {
                    CellNotice::DroppedModel { model } => write!(
                        formatter,
                        "warning: profile `{profile}` drops model `{model}` when switching to {kind}"
                    ),
                    CellNotice::DroppedTools { tools } => write!(
                        formatter,
                        "warning: profile `{profile}` drops tools on {kind}: {}",
                        tools.join(", ")
                    ),
                    CellNotice::ToolRestrictions { tools } => write!(
                        formatter,
                        "warning: profile `{profile}` tool restrictions are not enforced on {kind}: {}",
                        tools.join(", ")
                    ),
                    CellNotice::ToolsNotApplied => write!(
                        formatter,
                        "warning: profile `{profile}` tools list is not applied on {kind}"
                    ),
                }
            }
            Self::SkippedField {
                profile,
                agent,
                field,
            } => {
                use crate::agents::definition::SkippedPresetField;
                match field {
                    SkippedPresetField::AutoCompact(value) => write!(
                        formatter,
                        "warning: `{profile}` auto-compact {value} is unsupported on {agent}; launching without it"
                    ),
                    SkippedPresetField::Effort(value) => write!(
                        formatter,
                        "warning: `{profile}` effort {value} is unsupported on {agent}; launching without it"
                    ),
                    SkippedPresetField::EffortNeedsModel(value) => write!(
                        formatter,
                        "warning: `{profile}` effort {value} needs a model on {agent}; launching without it; set `model:` or pass --model"
                    ),
                    SkippedPresetField::EffortLowered { value, level } => write!(
                        formatter,
                        "warning: `{profile}` effort {value} is unsupported on {agent}; launching at {level}"
                    ),
                    SkippedPresetField::EffortNeedsModelSwitch(value) => write!(
                        formatter,
                        "warning: `{profile}` effort {value} reaches {agent} only through a model switch; resuming at the session's saved level; pass --model with --effort"
                    ),
                    SkippedPresetField::Fast { alternative } => {
                        write!(
                            formatter,
                            "warning: `{profile}` fast is unsupported on {agent}; launching without it"
                        )?;
                        match alternative {
                            Some(alternative) => write!(formatter, "; {alternative}"),
                            None => Ok(()),
                        }
                    }
                    SkippedPresetField::FastOff { fix } => write!(
                        formatter,
                        "warning: `{profile}` fast off cannot be forced on {agent}; {fix}"
                    ),
                    SkippedPresetField::CodeMode => write!(
                        formatter,
                        "warning: `{profile}` code-mode is unsupported on {agent}; launching without it"
                    ),
                }
            }
            Self::SmartCompactSkipped {
                profile,
                agent,
                line,
                reason,
            } => write!(
                formatter,
                "warning: `{profile}` smart-compact {line}: {agent} skips the mid-turn smart-compaction reminder ({reason})"
            ),
            Self::SmartCompactNativeWindow {
                profile,
                line,
                window,
                enforcer,
            } => {
                let actor = match enforcer {
                    crate::agents::AutoCompactEnforcer::Native => "the provider",
                    crate::agents::AutoCompactEnforcer::Rimz => "RimZ",
                };
                write!(
                    formatter,
                    "warning: `{profile}` smart-compact {line} is at or above its auto-compact {window}; {actor} compacts first"
                )
            }
            Self::SmartCompactSecondLineWindow {
                profile,
                second,
                window,
            } => write!(
                formatter,
                "warning: `{profile}` smart-compact's second reminder at {second} is at or above its auto-compact {window}; only the first fires"
            ),
            Self::TierMoved { profile, stamp } => {
                let passes = stamp
                    .passes()
                    .map(|pass| format!("{pass:#}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                write!(
                    formatter,
                    "warning: `{profile}` tier {} chose {}; passed {}",
                    stamp.tier,
                    stamp.model,
                    passes.replace(['\r', '\n'], " ")
                )
            }
            Self::LaterModelWins {
                profile,
                setting,
                model,
            } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; later model {model} wins"
            ),
            Self::DeclaredFieldWins {
                profile,
                setting,
                field,
                value,
            } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; declared {field} {value} wins"
            ),
            Self::DeclaredPromptWins { profile, setting } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; declared system prompt wins"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchFinalizeError {
    #[error(transparent)]
    PresetValue(crate::agents::PresetErr),
    #[error("unknown agent kind `{kind}`")]
    UnknownAdapter { kind: String },
    #[error(transparent)]
    PromptFile(#[from] ProfilePromptFileError),
    #[error(
        "{agent} does not support --{field}; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedPresetField {
        agent: &'static str,
        field: &'static str,
    },
    #[error(
        "{agent} does not support config key `system-prompt-file` / flag `--system-prompt-file`; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedSystemPrompt { agent: &'static str },
    #[error(
        "{agent} takes no system prompt, so the prompt from {} cannot be delivered; \
         launch on a kind that takes a system prompt",
        origin.display()
    )]
    UnsupportedDefinitionPrompt {
        agent: &'static str,
        origin: PathBuf,
    },
    #[error(
        "{agent} append-system-prompt-files requires a base system-prompt-file; add one to the profile, role, or launch command"
    )]
    MissingSystemPromptBase { agent: &'static str },
    #[error(
        "{agent} applies a `{field}` list only through the agent file its system prompt rides, and this launch has no system prompt; write the home kind base `agents/{agent}.md` or another prompt source, or remove `{field}`"
    )]
    ListNeedsAgentFile {
        agent: &'static str,
        field: &'static str,
    },
    #[error("{reason}")]
    PromptValidation { reason: String },
    #[error("{agent} does not support --max-turns")]
    UnsupportedMaxTurns {
        agent: &'static str,
        warnings: Vec<LaunchFinalizeWarning>,
    },
}

impl LaunchFinalizeError {
    pub fn warnings(&self) -> &[LaunchFinalizeWarning] {
        match self {
            Self::UnsupportedMaxTurns { warnings, .. } => warnings,
            Self::UnknownAdapter { .. }
            | Self::PresetValue(_)
            | Self::PromptFile(_)
            | Self::UnsupportedPresetField { .. }
            | Self::UnsupportedSystemPrompt { .. }
            | Self::UnsupportedDefinitionPrompt { .. }
            | Self::MissingSystemPromptBase { .. }
            | Self::ListNeedsAgentFile { .. }
            | Self::PromptValidation { .. } => &[],
        }
    }
}

/// Validate a launch preset without changing argv, returning skipped-field warnings.
pub fn validate_launch_preset(
    kind: &str,
    preset: &crate::agents::LaunchPreset,
) -> std::result::Result<Vec<LaunchFinalizeWarning>, LaunchFinalizeError> {
    let adapter = crate::agents::find_definition(kind).ok_or_else(|| {
        LaunchFinalizeError::UnknownAdapter {
            kind: kind.to_owned(),
        }
    })?;
    let rimz_enforced = crate::agents::auto_compact_enforcer(adapter)
        == Some(crate::agents::AutoCompactEnforcer::Rimz);
    let (preset, skipped) = adapter.spec().launch.normalize_preset(preset.clone());
    adapter
        .spec()
        .render_preset(&preset)
        .map_err(unsupported_preset_error)?;
    Ok(skipped
        .into_iter()
        .filter(|field| {
            !(rimz_enforced
                && matches!(
                    field,
                    crate::agents::definition::SkippedPresetField::AutoCompact(_)
                ))
        })
        .map(|field| LaunchFinalizeWarning::SkippedField {
            profile: kind.to_owned(),
            agent: adapter.spec().kind,
            field,
        })
        .collect())
}

/// Apply launch-wide CLI options and provider preset reconciliation to a
/// resolved layout. Command cells remain unchanged.
pub fn finalize_launch_layout(
    layout: &mut LayoutSpec,
    options: LaunchFinalizeOptions<'_>,
) -> std::result::Result<Vec<LaunchFinalizeWarning>, LaunchFinalizeError> {
    let mut warnings = Vec::new();
    for cell in layout.agent_cells_mut() {
        finalize_agent_cell(cell, options, &mut warnings)?;
        if let Some(stamp) = cell.launch.tier.as_ref().filter(|stamp| stamp.moved()) {
            warnings.push(LaunchFinalizeWarning::TierMoved {
                profile: cell
                    .launch
                    .role
                    .as_ref()
                    .or(cell.launch.profile.as_ref())
                    .map_or_else(|| cell.kind.to_string(), Clone::clone),
                stamp: stamp.as_ref().clone(),
            });
        }
    }

    if let Some(limit) = options.max_turns {
        for cell in layout.agent_cells_mut() {
            let adapter = crate::agents::find_definition(&cell.kind).ok_or_else(|| {
                LaunchFinalizeError::UnknownAdapter {
                    kind: cell.kind.to_string(),
                }
            })?;
            let Some(turn_args) = adapter.spec().launch.max_turns_args(limit) else {
                return Err(LaunchFinalizeError::UnsupportedMaxTurns {
                    agent: adapter.spec().kind,
                    warnings,
                });
            };
            cell.args.extend(turn_args);
        }
    }

    Ok(warnings)
}

pub(super) fn finalize_agent_cell(
    cell: &mut AgentCell,
    options: LaunchFinalizeOptions<'_>,
    warnings: &mut Vec<LaunchFinalizeWarning>,
) -> std::result::Result<(), LaunchFinalizeError> {
    let mut preset = options.preset.clone();
    // Tier-routed cells already carry their resolved model; other cells still take the CLI model.
    if cell.launch.tier.is_some() {
        preset.model = None;
    }
    let options = LaunchFinalizeOptions {
        preset: &preset,
        ..options
    };
    let adapter = crate::agents::find_definition(&cell.kind);
    let rendered_effort = cell.launch.effort.is_some();
    if options.isolation.is_some() {
        cell.launch.isolation = options.isolation;
    }
    if let Some(PermissionModeChoice::Explicit(mode)) = options.permission_mode
        && let Some(adapter) = adapter
    {
        adapter.spec().launch.strip_permission_args(&mut cell.args);
        cell.args
            .extend(adapter.spec().launch.permission_args(mode));
        cell.launch.mode = Some(mode);
    }
    let mut overridden = Vec::new();
    if !options.preset.is_empty() {
        let adapter = adapter.ok_or_else(|| LaunchFinalizeError::UnknownAdapter {
            kind: cell.kind.to_string(),
        })?;
        if let Some(path) = options.preset.system_prompt_file.as_ref() {
            cell.system_prompt_file = Some(path.clone().into());
        }
        if !options.preset.append_system_prompt_files.is_empty() {
            cell.append_system_prompt_files = options
                .preset
                .append_system_prompt_files
                .iter()
                .cloned()
                .map(Into::into)
                .collect();
        }
        if options.preset.system_prompt_file.is_some()
            || !options.preset.append_system_prompt_files.is_empty()
        {
            overridden.push(crate::agents::PresetField::SystemPromptFile);
        }
        if let Some(model) = options
            .preset
            .model
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            cell.launch.model = Some(model.clone());
            cell.launch.tier = None;
            overridden.push(crate::agents::PresetField::Model);
        }
        if let Some(effort) = options
            .preset
            .effort
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            cell.launch.effort = Some(effort.clone());
            cell.tuning.effort = Some(TuningSource::Flag("--effort"));
            overridden.push(crate::agents::PresetField::Effort);
        }
        if let Some(fast) = options.preset.fast {
            cell.launch.fast = Some(fast);
            cell.tuning.fast = Some(TuningSource::Flag(if fast {
                "--fast"
            } else {
                "--no-fast"
            }));
        }
        apply_preset_args(
            &mut cell.args,
            adapter,
            options.preset,
            &overridden,
            cell.launch.model.as_deref(),
        )?;
    }
    drop_lone_built_in_base(cell);
    validate_finalized_cell(cell, adapter)?;
    cell.args.extend(options.passthrough.iter().cloned());
    if let Some(budget) = options.budget {
        cell.launch.budget = Some(budget.to_string());
    }
    let team = cell
        .launch
        .team
        .as_deref()
        .or_else(|| options.smart_compact.and_then(|policy| policy.team));
    let profile = match (team, cell.launch.role.as_deref()) {
        (Some(team), Some(role)) => format!("{team}.{role}"),
        _ => cell
            .launch
            .profile
            .clone()
            .unwrap_or_else(|| cell.kind.to_string()),
    };
    if let Some(adapter) = adapter {
        let model = cell.launch.model.as_deref().map(|model| {
            options
                .machine_config
                .and_then(|config| config.model_alias(&cell.kind, model))
                .unwrap_or(model)
                .to_owned()
        });
        if matches!(cell.tuning.effort, Some(TuningSource::BuiltIn)) {
            let defaults = match (cell.launch.model.as_deref(), model.as_deref()) {
                (Some(name), Some(pinned)) => {
                    crate::agents::pinned_definition_defaults(&cell.kind, name, pinned)
                }
                _ => crate::agents::definition_defaults(&cell.kind, None),
            };
            cell.launch.effort = defaults.effort.map(str::to_owned);
        }
        for field in normalize_cell_preset(cell, adapter, model.as_deref()) {
            warnings.push(LaunchFinalizeWarning::SkippedField {
                profile: profile.clone(),
                agent: adapter.spec().kind,
                field,
            });
        }
        if rendered_effort || overridden.contains(&crate::agents::PresetField::Effort) {
            settle_effort_arg(cell, adapter)?;
        }
        reconcile_preset_args(cell, adapter, &overridden, warnings)?;
    }
    warnings.extend(LaunchFinalizeWarning::take_cell_notices(cell, &profile));
    if let Some(adapter) = adapter
        && cell.launch.model.is_none()
        && let Some(default) = adapter.default_launch_model()
    {
        cell.args.extend(
            adapter
                .spec()
                .render_preset(&crate::agents::LaunchPreset {
                    model: Some(default.clone()),
                    ..Default::default()
                })
                .map_err(unsupported_preset_error)?,
        );
        cell.launch.model = Some(default);
    }
    if let Some(adapter) = adapter {
        let default = match options.permission_mode {
            Some(PermissionModeChoice::Default(mode)) => Some(mode),
            _ => None,
        };
        cell.launch.mode =
            adapter
                .spec()
                .launch
                .settle_permission_mode(&mut cell.args, cell.launch.mode, default);
    }
    cell.launch.record = Some(Box::new(crate::agents::LaunchRecord {
        model: cell.launch.model.clone(),
        effort: cell.launch.effort.clone(),
        agent: options.agent_base.map(str::to_owned).or_else(|| {
            cell.launch
                .record
                .as_ref()
                .and_then(|record| record.agent.clone())
        }),
        fast: cell.launch.fast,
        auto_compact: cell
            .auto_compact
            .as_deref()
            .and_then(|value| AutoCompact::parse_native_window(value).ok()),
        notes: cell
            .launch
            .record
            .as_ref()
            .and_then(|record| record.notes.clone()),
        loop_section: cell
            .launch
            .record
            .as_ref()
            .and_then(|record| record.loop_section.clone()),
    }));
    if let Some(policy) = options.smart_compact {
        policy.append_warnings(cell, warnings);
    }
    Ok(())
}

pub(super) fn normalize_cell_preset(
    cell: &mut AgentCell,
    adapter: &crate::agents::AgentDefinition,
    model: Option<&str>,
) -> Vec<crate::agents::definition::SkippedPresetField> {
    let launch = adapter.spec().launch;
    let rimz_enforced = crate::agents::auto_compact_enforcer(adapter)
        == Some(crate::agents::AutoCompactEnforcer::Rimz);
    let enforced_auto_compact = rimz_enforced.then(|| cell.auto_compact.take()).flatten();
    let mut preset = crate::agents::LaunchPreset {
        model: model.map(str::to_owned),
        auto_compact: cell.auto_compact.take(),
        effort: cell.launch.effort.take(),
        fast: cell.launch.fast,
        ..Default::default()
    };
    let refused = launch.drop_refused_effort(&mut preset, model);
    let (mut preset, normalized) = launch.normalize_preset(preset);
    let mut skipped: Vec<_> = refused.into_iter().chain(normalized).collect();
    if adapter.spec().launch.model_session_step
        && cell.launch.model.as_deref().is_none_or(str::is_empty)
        && let Some(effort) = preset.effort.take()
    {
        skipped.push(crate::agents::definition::SkippedPresetField::EffortNeedsModel(effort));
    }
    if adapter.spec().launch.presets.code_mode
        == crate::agents::definition::CodeModeSwitch::Unsupported
        && let Some(on) = cell.launch.code_mode.take()
        && on
    {
        skipped.push(crate::agents::definition::SkippedPresetField::CodeMode);
    }
    cell.auto_compact = enforced_auto_compact.or(preset.auto_compact);
    cell.launch.effort = preset.effort;
    cell.launch.fast = preset.fast;
    if let Some(record) = &mut cell.launch.record {
        record.effort.clone_from(&cell.launch.effort);
        record.fast = cell.launch.fast;
    }
    skipped
}

/// Re-render the one effort arg RimZ rendered when the effort settled since, for the model the
/// launch finally runs: `max` lowers to its level and a built-in default follows the model.
/// Several effort args are a conflict `reconcile_preset_args` warns about instead.
pub(super) fn settle_effort_arg(
    cell: &mut AgentCell,
    adapter: &crate::agents::AgentDefinition,
) -> std::result::Result<(), LaunchFinalizeError> {
    let field = crate::agents::PresetField::Effort;
    let Some(matcher) = adapter.spec().launch.preset_arg_matcher(field) else {
        return Ok(());
    };
    let occurrences = matcher.occurrences(&cell.args);
    let [rendered] = occurrences.as_slice() else {
        return Ok(());
    };
    if cell.launch.effort.as_ref() == Some(&rendered.value) {
        return Ok(());
    }
    remove_occurrences(&mut cell.args, &occurrences);
    if let Some(effort) = cell.launch.effort.clone() {
        cell.args.extend(
            adapter
                .spec()
                .render_preset(&field.launch_preset(effort))
                .map_err(unsupported_preset_error)?,
        );
    }
    Ok(())
}

/// `model` is the cell's model, which picks the level `max` effort launches at when the
/// preset names none.
pub(super) fn apply_preset_args(
    args: &mut Vec<String>,
    adapter: &crate::agents::AgentDefinition,
    preset: &crate::agents::LaunchPreset,
    replaced: &[crate::agents::PresetField],
    model: Option<&str>,
) -> std::result::Result<(), LaunchFinalizeError> {
    let requested = preset.model.clone();
    let model = requested.clone().or(model.map(str::to_owned));
    let mut preset = crate::agents::LaunchPreset {
        model: model.clone(),
        ..preset.clone()
    };
    let launch = adapter.spec().launch;
    launch.drop_refused_effort(&mut preset, model.as_deref());
    let (mut preset, _) = launch.normalize_preset(preset);
    preset.model = requested;
    let rendered = adapter
        .spec()
        .render_preset(&preset)
        .map_err(unsupported_preset_error)?;
    for field in replaced {
        if matches!(
            field,
            crate::agents::PresetField::Model | crate::agents::PresetField::Effort
        ) && let Some(matcher) = adapter.spec().launch.preset_arg_matcher(*field)
        {
            matcher.remove_occurrences(args);
        }
    }
    if preset.fast.is_some() {
        adapter.spec().launch.strip_fast_args(args);
    }
    args.extend(rendered);
    Ok(())
}

/// Reconcile declared launch fields against the args a profile carries. Fields
/// in `overridden` were named on this command line: the CLI value replaces the
/// profile's arg silently, since the override is the user's stated intent.
fn reconcile_preset_args(
    cell: &mut AgentCell,
    adapter: &crate::agents::AgentDefinition,
    overridden: &[crate::agents::PresetField],
    warnings: &mut Vec<LaunchFinalizeWarning>,
) -> std::result::Result<(), LaunchFinalizeError> {
    use crate::agents::PresetField;

    let label = cell
        .launch
        .profile
        .as_deref()
        .unwrap_or(cell.kind.as_str())
        .to_owned();
    let declared = [
        (PresetField::Model, cell.launch.model.clone()),
        (PresetField::Effort, cell.launch.effort.clone()),
        (PresetField::AutoCompact, cell.auto_compact.clone()),
    ];

    for (field, value) in declared {
        let Some(matcher) = adapter.spec().launch.preset_arg_matcher(field) else {
            continue;
        };
        let occurrences = matcher.occurrences(&cell.args);
        let Some(declared_value) = value else {
            if field == PresetField::Model
                && let Some(winner) = occurrences.last()
            {
                for occurrence in &occurrences[..occurrences.len() - 1] {
                    if occurrence.value != winner.value {
                        warnings.push(LaunchFinalizeWarning::LaterModelWins {
                            profile: label.clone(),
                            setting: occurrence.written(&cell.args),
                            model: winner.value.clone(),
                        });
                    }
                }
                cell.launch.model = Some(winner.value.clone());
                remove_occurrences(&mut cell.args, &occurrences[..occurrences.len() - 1]);
            }
            continue;
        };
        if occurrences.len() <= 1 {
            continue;
        }
        for occurrence in &occurrences {
            if occurrence.value != declared_value && !overridden.contains(&field) {
                warnings.push(LaunchFinalizeWarning::DeclaredFieldWins {
                    profile: label.clone(),
                    setting: occurrence.written(&cell.args),
                    field: field.flag_name(),
                    value: declared_value.clone(),
                });
            }
        }
        let canonical = adapter
            .spec()
            .render_preset(&field.launch_preset(declared_value))
            .map_err(unsupported_preset_error)?;
        remove_occurrences(&mut cell.args, &occurrences);
        cell.args.extend(canonical);
    }

    if !SystemPromptSources::from_cell(cell).is_empty() {
        let matcher = adapter
            .spec()
            .launch
            .preset_arg_matcher(PresetField::SystemPromptFile)
            .expect("prompt support was validated before preset reconciliation");
        let occurrences = matcher.occurrences(&cell.args);
        if !overridden.contains(&PresetField::SystemPromptFile) {
            for occurrence in &occurrences {
                let matches_declared_path = !SystemPromptSources::from_cell(cell).composes()
                    && cell
                        .system_prompt_file
                        .as_ref()
                        .and_then(crate::config::PromptSource::file)
                        .is_some_and(|path| path.to_string_lossy() == occurrence.value);
                if !matches_declared_path {
                    warnings.push(LaunchFinalizeWarning::DeclaredPromptWins {
                        profile: label.clone(),
                        setting: occurrence.written(&cell.args),
                    });
                }
            }
        }
        remove_occurrences(&mut cell.args, &occurrences);
    }
    Ok(())
}

/// The one rule for when a built-in kind base's body is sent, decided once the launch's prompt
/// flags and team layer are known: only beneath a prompt that composes onto it, or on a kind
/// that applies its tools only inside the file that prompt rides. Alone, it would replace the
/// provider's own prompt with RimZ's, so the launch sends none.
pub(super) fn drop_lone_built_in_base(cell: &mut AgentCell) {
    if cell
        .system_prompt_file
        .as_ref()
        .is_some_and(|base| base.built_in_kind().is_some())
        && !SystemPromptSources::from_cell(cell).composes()
        && !crate::agents::tools_ride_prompt(cell.kind.as_str())
    {
        cell.system_prompt_file = None;
    }
}

fn validate_system_prompt_support(
    cell: &AgentCell,
    adapter: Option<&crate::agents::AgentDefinition>,
) -> std::result::Result<(), LaunchFinalizeError> {
    let sources = SystemPromptSources::from_cell(cell);
    if sources.is_empty() {
        return Ok(());
    }
    let adapter = adapter.ok_or_else(|| LaunchFinalizeError::UnknownAdapter {
        kind: cell.kind.to_string(),
    })?;
    if !adapter.spec().launch.takes_system_prompt() {
        if let Some(source) = sources
            .system_prompt_file
            .iter()
            .chain(&sources.append_system_prompt_files)
            .chain(sources.team_prompt.iter().flat_map(|prompt| &prompt.files))
            .find(|source| source.file().is_none())
        {
            return Err(LaunchFinalizeError::UnsupportedDefinitionPrompt {
                agent: adapter.spec().kind,
                origin: source.origin().to_owned(),
            });
        }
        return Err(LaunchFinalizeError::UnsupportedSystemPrompt {
            agent: adapter.spec().kind,
        });
    }
    if cell.system_prompt_file.is_none() {
        return Err(LaunchFinalizeError::MissingSystemPromptBase {
            agent: adapter.spec().kind,
        });
    }
    Ok(())
}

/// A kind whose prompt rides an agent file applies a `tools` list and a limiting `mcps` list
/// only inside that file, which a fresh launch writes only from prompt sources. A resume keeps
/// the tools and servers its conversation started with, so only a fresh cell is checked.
fn validate_fresh_agent_file_lists(
    cell: &AgentCell,
) -> std::result::Result<(), LaunchFinalizeError> {
    let Some(adapter) = crate::agents::find_definition(cell.kind.as_str()) else {
        return Ok(());
    };
    if !adapter.rides_agent_file() || !SystemPromptSources::from_cell(cell).is_empty() {
        return Ok(());
    }
    let field = if cell.tools.is_some() {
        // A `capabilities:` list lowers to the tools value; name the key the user wrote.
        if cell.rimz_capabilities.is_some() {
            "capabilities"
        } else {
            "tools"
        }
    } else if crate::config::mcps_limit(cell.mcps.as_deref()) {
        "mcps"
    } else {
        return Ok(());
    };
    Err(LaunchFinalizeError::ListNeedsAgentFile {
        agent: adapter.spec().kind,
        field,
    })
}

fn validate_system_prompt_text(cell: &AgentCell) -> std::result::Result<(), LaunchFinalizeError> {
    let sources = SystemPromptSources::from_cell(cell);
    if sources.is_empty() {
        return Ok(());
    }
    crate::harness::prompt_compose::validate_text_prompt_size(&cell.kind, &sources).map_err(
        |err| LaunchFinalizeError::PromptValidation {
            reason: err.to_string(),
        },
    )?;
    Ok(())
}

pub(super) fn validate_finalized_cell(
    cell: &AgentCell,
    adapter: Option<&crate::agents::AgentDefinition>,
) -> std::result::Result<(), LaunchFinalizeError> {
    validate_system_prompt_support(cell, adapter)?;
    validate_agent_prompt_files(cell)?;
    validate_system_prompt_text(cell)
}

fn remove_occurrences(args: &mut Vec<String>, occurrences: &[crate::agents::PresetArgOccurrence]) {
    for occurrence in occurrences.iter().rev() {
        args.drain(occurrence.argv_range.clone());
    }
}

fn unsupported_preset_error(err: crate::agents::PresetErr) -> LaunchFinalizeError {
    match err {
        err @ (crate::agents::PresetErr::UnsupportedValue { .. }
        | crate::agents::PresetErr::UnsupportedFast { .. }) => {
            LaunchFinalizeError::PresetValue(err)
        }
        crate::agents::PresetErr::UnsupportedField { agent, field } => {
            LaunchFinalizeError::UnsupportedPresetField { agent, field }
        }
    }
}

/// Reduce a resolved layout to the agent cells used by cohort resume matching.
pub fn cohort_cells(layout: &LayoutSpec) -> Vec<CohortCell> {
    layout
        .agent_cells()
        .map(|cell| CohortCell {
            kind: cell.kind.clone(),
            role: cell.launch.role.clone(),
            profile: cell.launch.profile.clone(),
        })
        .collect()
}

/// Compile one launch request per agent cell in layout order.
#[expect(
    clippy::too_many_arguments,
    reason = "one explicit identity-allocation boundary without a duplicate launch DTO"
)]
pub fn launch_identity_requests(
    layout: &LayoutSpec,
    explicit_name: Option<&str>,
    generated_worktree_name: Option<&str>,
    team: Option<&str>,
    lane: Option<&crate::ids::Lane>,
    prompt: Option<(&str, usize)>,
    resume: Option<&CohortResumePlan>,
    ancestry: Option<&LaunchAncestry>,
) -> Result<Vec<AgentLaunchRequest>> {
    let agent_cells: Vec<&AgentCell> = layout.agent_cells().collect();
    let agent_count = agent_cells.len();
    if let Some(resume) = resume
        && resume.seeds.len() != agent_count
    {
        bail!(
            "resume plan has {} seeds for {agent_count} agent cells",
            resume.seeds.len()
        );
    }
    let inline_launch_group = (team.is_none() && agent_count >= 2).then(|| {
        resume
            .and_then(|plan| plan.launch_group.clone())
            .unwrap_or_else(mint_launch_group)
    });
    let mut requests = Vec::with_capacity(agent_cells.len());
    for (index, cell) in agent_cells.into_iter().enumerate() {
        if resume.is_some_and(|plan| matches!(plan.seeds[index], CohortSeed::Resume(_))) {
            continue;
        }
        let launch_ordinal = (team.is_some() || inline_launch_group.is_some())
            .then(|| index_to_launch_ordinal(index));
        let name = if agent_count == 1 && index == 0 {
            match explicit_name {
                Some(name) => {
                    validate_agent_name(name)?;
                    AgentLaunchName::Explicit(name.to_owned())
                }
                None => generated_worktree_name
                    .map(|name| AgentLaunchName::Soft(name.to_owned()))
                    .unwrap_or(AgentLaunchName::Mint),
            }
        } else {
            AgentLaunchName::Mint
        };
        let mut launch = cell.launch.clone();
        launch.team = team.map(ToOwned::to_owned);
        launch.launch_group.clone_from(&inline_launch_group);
        launch.launch_ordinal = launch_ordinal;
        launch.lane = lane.cloned();
        launch.kind_ordinal = None;
        if let Some(ancestry) = ancestry {
            match ancestry {
                LaunchAncestry::Peer {
                    launch_generation,
                    launched_by,
                } => {
                    launch.launch_depth = Some(*launch_generation);
                    launch.launched_by = launched_by.clone().map(Box::new);
                }
                LaunchAncestry::Subagent {
                    parent_agent_id,
                    parent_agent_kind,
                    launch_generation,
                } => {
                    launch.parent_agent_id = Some(parent_agent_id.clone());
                    launch.parent_agent_kind = Some(parent_agent_kind.clone());
                    launch.launch_depth = Some(*launch_generation);
                }
            }
        }
        requests.push(AgentLaunchRequest {
            login: crate::store::writer::LaunchLogin::RoomDefault,
            kind: cell.kind.clone(),
            agent_id: mint_launch_id(),
            name,
            launch,
            run_id: None,
            prompt: prompt
                .filter(|(_, leader_index)| *leader_index == index)
                .map(|(prompt, _)| prompt.to_owned()),
        });
    }
    Ok(requests)
}

pub fn mint_launch_id() -> AgentSessionId {
    let raw = EventId::new();
    let suffix = raw.as_str().strip_prefix("evt_").unwrap_or(raw.as_str());
    AgentSessionId::from(format!("launch_{suffix}"))
}

fn mint_launch_group() -> String {
    mint_launch_id().to_string()
}

fn index_to_launch_ordinal(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Compile one provider-native resume launch from planner-owned data.
pub(super) fn resume_command(
    rimz_bin: &Path,
    runtime: &RuntimePaths,
    identity: &ResumeLaunchIdentity,
    posture: &ResumeLaunchPosture,
    close_pane_on_exit: bool,
    cwd: Option<&Path>,
) -> Result<Vec<String>, crate::harness::launch::ExecWireErr> {
    let params = crate::agents::LaunchParams {
        checkout: identity.checkout.clone(),
        add_dirs: identity.add_dirs.clone(),
        untrusted_checkout: identity.untrusted_checkout,
        login: identity.login.clone(),
        launch_depth: identity.launch_depth,
        launched_by: identity.launched_by.clone().map(Box::new),
        profile: identity.profile.clone(),
        profile_scope: identity.profile_scope,
        role: identity.role.clone(),
        team: identity.team.clone(),
        launch_group: identity.launch_group.clone(),
        launch_ordinal: identity.launch_ordinal,
        lane: identity.lane.clone(),
        mode: posture.mode,
        isolation: effective_resume_isolation(posture.isolation, identity.isolation),
        model: posture.model.clone(),
        effort: posture.effort.clone(),
        fast: posture.fast,
        code_mode: posture.code_mode,
        budget: posture.budget.clone(),
        tier: posture.tier.clone(),
        record: posture.record.clone(),
        epoch: (identity.epoch > 0).then_some(identity.epoch),
        loop_fire: identity.loop_fire.clone(),
        loop_booked_by: identity.loop_booked_by.clone(),
        ..Default::default()
    };
    crate::harness::launch::exec_argv(
        rimz_bin,
        runtime,
        &crate::harness::launch::ExecRequest {
            close_pane_on_exit,
            worktree_path: cwd.map(Path::to_path_buf),
            identity: crate::harness::launch::ExecIdentity {
                resume_model_override: posture.resume_model_override,
                row_key: None,
                name: identity.name.clone(),
                name_explicit: identity.name_explicit,
                launch_id: Some(
                    identity
                        .launch_id
                        .as_ref()
                        .unwrap_or(&identity.session_id)
                        .to_string(),
                ),
                params,
            },
            ..posture.exec_request(
                identity.kind.clone(),
                crate::harness::launch::ExecAction::Resume {
                    session_id: identity.session_id.to_string(),
                    extra_args: Vec::new(),
                },
            )
        },
    )
}

#[derive(Clone, Copy)]
pub struct LayoutRunParams<'a> {
    pub run_id: &'a crate::ids::RunId,
    pub resume: Option<&'a crate::ids::AgentSessionId>,
    pub leader_index: usize,
    pub managed_launch: Option<&'a crate::agents::ManagedLaunchState>,
    pub cleanup: bool,
    pub subagent: bool,
    /// The run's `RunRecord::result_file`.
    pub result_file: Option<&'a Path>,
}

#[derive(Clone, Copy)]
pub struct LayoutPaneParams<'a> {
    pub runtime: &'a RuntimePaths,
    pub cwd: &'a Path,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub pick_session: bool,
    pub resume_seeds: Option<&'a [CohortSeed]>,
    pub launch_identities: &'a [AgentLaunchIdentity],
    /// Parent rows used to compose agent pane display titles.
    pub agents: &'a [crate::agents::AgentState],
    /// The reminder section for the cell that receives the prompt.
    pub reminder_section: Option<&'a crate::harness::launch::ReminderSection>,
    pub leader_run: Option<LayoutRunParams<'a>>,
}

/// Compile backend-neutral pane commands for a resolved layout.
///
/// This is the single layout-to-pane boundary: it validates launch inputs,
/// keeps resumed and fresh agents aligned with their cells, and preserves
/// command cells in layout order.
pub fn compile_layout_panes(
    layout: &LayoutSpec,
    params: LayoutPaneParams<'_>,
) -> Result<LayoutPanes> {
    // The run-bearing recipe validated its leader during preparation; retries
    // reuse that preparation rather than checking its files again.
    let layout_cells = || {
        layout
            .agent_cells()
            .enumerate()
            .filter_map(|(index, cell)| {
                params
                    .leader_run
                    .is_none_or(|run| run.leader_index != index)
                    .then_some(cell)
            })
    };
    for cell in layout_cells() {
        validate_system_prompt_support(cell, crate::agents::find_definition(cell.kind.as_str()))?;
    }
    for (index, cell) in layout.agent_cells().enumerate() {
        let fresh = params
            .resume_seeds
            .is_none_or(|seeds| matches!(seeds.get(index), Some(CohortSeed::Fresh)))
            && params
                .leader_run
                .is_none_or(|run| run.leader_index != index || run.resume.is_none());
        if fresh {
            validate_fresh_agent_file_lists(cell)?;
        }
    }
    for cell in layout_cells() {
        validate_agent_prompt_files(cell)?;
    }
    for cell in layout_cells() {
        validate_system_prompt_text(cell)?;
    }
    let agent_count = layout.agent_cells().count();
    if let Some(seeds) = params.resume_seeds
        && seeds.len() != agent_count
    {
        bail!(
            "resume plan has {} seeds for {agent_count} agent cells",
            seeds.len()
        );
    }
    let rimz_bin = crate::proc::rimz_exe();
    let mut agent_index = 0usize;
    let mut launches = params.launch_identities.iter();
    let columns = layout
        .columns
        .iter()
        .map(|column| {
            let panes = column
                .rows
                .iter()
                .map(|cell| {
                    let (argv, name) = match cell {
                        Cell::Command { argv } => {
                            let mut argv = if argv.is_empty() {
                                vec![crate::proc::user_shell_program()]
                            } else {
                                argv.clone()
                            };
                            let argv = if let Some(run) = params.leader_run {
                                let mut wrapper = vec![
                                    rimz_bin.to_string_lossy().into_owned(),
                                    "agents".to_owned(),
                                    "run-command".to_owned(),
                                    "--workspace".to_owned(),
                                    params.runtime.workspace_id.to_string(),
                                    "--run".to_owned(),
                                    run.run_id.to_string(),
                                    "--".to_owned(),
                                ];
                                wrapper.append(&mut argv);
                                wrapper
                            } else {
                                argv
                            };
                            (argv, cell.pane_name().into())
                        }
                        Cell::Agent(cell) => {
                            let seed = params.resume_seeds.map(|seeds| &seeds[agent_index]);
                            let (argv, seat) = match seed {
                                Some(CohortSeed::Resume(agent)) => {
                                    // The layout already resolved this cell from its profile or
                                    // role binding, so the posture to replay is right here.
                                    let checkout = agent
                                        .checkout
                                        .as_ref()
                                        .and(agent.worktree_path.as_deref())
                                        .map(Path::new);
                                    let argv = resume_command(
                                        &rimz_bin,
                                        params.runtime,
                                        &ResumeLaunchIdentity::from(agent.as_ref()),
                                        &ResumeLaunchPosture::from(cell),
                                        !params.in_place,
                                        checkout,
                                    )?;
                                    (argv, pane_title::Seat::from(agent.as_ref()))
                                }
                                Some(CohortSeed::Fresh) | None => {
                                    let Some(launch) = launches.next() else {
                                        bail!(
                                            "launch plan missing identity for agent cell {agent_index}"
                                        );
                                    };
                                    let argv = fresh_agent_argv(
                                        cell, launch, &rimz_bin, params, agent_index,
                                    )?;
                                    let seat = pane_title::Seat::from_launch(
                                        &launch.kind,
                                        Some(&launch.name),
                                        &launch.launch,
                                    );
                                    (argv, seat)
                                }
                            };
                            agent_index = agent_index.saturating_add(1);
                            (argv, pane_title::title(seat, params.agents))
                        }
                    };
                    Ok(PaneCmd {
                        argv,
                        name: Some(name),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(LayoutColumn {
                panes,
                stacked: column.stacked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if launches.next().is_some() {
        bail!("launch plan has more identities than fresh agent cells");
    }
    Ok(LayoutPanes {
        columns,
        focused_pane: 0,
    })
}

fn fresh_agent_argv(
    cell: &AgentCell,
    launch: &AgentLaunchIdentity,
    rimz_bin: &Path,
    params: LayoutPaneParams<'_>,
    agent_index: usize,
) -> Result<Vec<String>> {
    validate_agent_name(&launch.name)?;
    let mut request = crate::harness::launch::ExecRequest::fresh(
        cell,
        crate::harness::launch::ExecIdentity {
            resume_model_override: false,
            row_key: None,
            name: Some(launch.name.clone()),
            name_explicit: launch.name_explicit,
            launch_id: Some(launch.agent_id.to_string()),
            params: launch.launch.clone(),
        },
        params.cleanup_worktree.then(|| params.cwd.to_path_buf()),
        !params.cleanup_worktree && !params.in_place,
    );
    if let crate::harness::launch::ExecAction::Launch { prompt, .. } = &mut request.action {
        *prompt = launch.prompt.clone();
    }
    if params.pick_session {
        request.action = crate::harness::launch::ExecAction::Pick {
            extra_args: std::mem::take(request.action.extra_args_mut()),
        };
    }
    if launch.prompt.is_some() {
        request.reminder_section = params.reminder_section.cloned();
    }
    if let Some(run) = params
        .leader_run
        .filter(|run| run.leader_index == agent_index)
    {
        request.run_id = Some(run.run_id.clone());
        if let Some(session) = run.resume {
            request.action = crate::harness::launch::ExecAction::Resume {
                session_id: session.to_string(),
                extra_args: std::mem::take(request.action.extra_args_mut()),
            };
            request.reminder_section = params.reminder_section.cloned();
        }
        request.subagent = run.subagent;
        request.result_file = run.result_file.map(Path::to_path_buf);
        (request.close_pane_on_exit, request.exit_on_run_completion) = run_exit_policy(run.cleanup);
        request.provider_account = run.managed_launch.and_then(|state| state.binding()).map_or(
            crate::harness::launch::ProviderAccountState::Unbound,
            |binding| crate::harness::launch::ProviderAccountState::Pending {
                binding: binding.clone(),
            },
        );
    }
    Ok(crate::harness::launch::exec_argv(
        rimz_bin,
        params.runtime,
        &request,
    )?)
}

pub fn run_exit_policy(self_cleanup_on_completion: bool) -> (bool, bool) {
    (self_cleanup_on_completion, self_cleanup_on_completion)
}

pub fn validate_agent_name(name: &str) -> Result<()> {
    if !crate::agents::petname::valid_agent_name(name)
        || crate::agents::petname::HEADER_PSEUDO_HANDLES.contains(&name)
    {
        bail!("invalid agent name `{name}`; use ASCII letters, numbers, and `-`");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
