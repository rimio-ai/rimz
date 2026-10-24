//! Shared launch preparation, admission, and pane execution.

pub mod checkout;
pub mod context;
pub mod lsp_admission;
pub mod placement;
mod reconcile;
pub mod resolve;
pub mod trust;
pub mod worktree_entry;

use anyhow::{Context, Result, bail};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::PermissionMode;
use crate::harness::budget::BudgetSpec;
use crate::store::run::ReportTo;
use crate::store::writer::AgentLaunchIdentity;

#[derive(Clone)]
pub struct LaunchContext {
    pub workspace: crate::ResolvedWorkspace,
    pub store: crate::Store,
    pub machine: Arc<crate::config::MachineConfig>,
    pub mux: Option<crate::ids::MuxName>,
    pub root: Option<PathBuf>,
    pub channel: Option<String>,
}

pub struct LayoutRequest {
    pub context: LaunchContext,
    pub launch: LayoutLaunch,
    pub cwd: Option<PathBuf>,
    pub allow_in_place: bool,
    pub entry: RoomEntry,
    pub starter: Starter,
    pub resident_loop: Option<ResidentLoop>,
}

#[derive(Default)]
pub struct LayoutLaunch {
    pub spec: Option<String>,
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub cohort: CohortRequest,
    pub selection: Selection,
    pub new_pane: bool,
    pub overrides: LaunchOverrides,
    pub max_turns: Option<u32>,
    pub teams_scene: bool,
    pub report_to: ReportTo,
}

#[derive(Default)]
pub struct CohortRequest {
    pub worktree: Option<String>,
    pub from_pr: Option<crate::forge::PrTarget>,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub budget: Option<BudgetSpec>,
    pub fresh: bool,
    pub bg: bool,
    pub new_tab: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LaunchOverrides {
    pub mode: Option<PermissionMode>,
    pub tier: Option<crate::config::tiers::ModelTier>,
    pub agent: Option<String>,
    pub preset: crate::agents::LaunchPreset,
    pub isolation: Option<crate::config::Isolation>,
    pub passthrough: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    Fresh,
    Picker,
    Cohort,
}

#[derive(Clone, Copy, Debug)]
pub enum RoomEntry {
    Live,
}

#[derive(Clone, Copy, Debug)]
pub enum Starter {
    Human,
    Agent,
    Team,
    Loop,
}

pub struct ResidentLoop {
    pub task: String,
    pub reminder: String,
}

pub enum Preparation {
    Prepared(Box<PreparedLaunch>),
    Handled(LaunchOutcome),
}

pub enum LaunchOutcome {
    Declined,
    Existing,
    BudgetExceeded { reason: String },
    Opened(Box<OpenedLayout>),
}

pub struct OpenedLayout {
    pub identities: Vec<AgentLaunchIdentity>,
    pub leader_index: Option<usize>,
    pub team: Option<(String, crate::config::Team)>,
    pub channel: Option<String>,
    pub cwd: PathBuf,
    pub in_place: bool,
    pub pick_session: bool,
    pub peer: Option<crate::store::run::RunRecord>,
}

#[derive(Clone, Copy)]
pub enum Notice<'a> {
    Config(&'a crate::config::MachineConfig),
    LaunchWarning(&'a crate::harness::plan::LaunchFinalizeWarning),
    Warning(&'a str),
    Trust(&'a trust::TrustNotice),
    LspIgnored(&'a str),
    LspRefused(&'a str),
    LspWait(&'a crate::lsp::admission::Wait),
    Reconcile(&'a str),
    Dispatch {
        target: &'a str,
        result: &'a crate::message::dispatch::DispatchResult,
    },
    PickerReceipt {
        kind: &'a str,
        channel: Option<&'a str>,
        cwd: &'a std::path::Path,
        project: &'a std::path::Path,
    },
    ResumeAnnouncement(&'a crate::harness::plan::CohortResumePlan),
    ResumeReceipt {
        plan: &'a crate::harness::plan::CohortResumePlan,
        team: Option<&'a str>,
        channel: Option<&'a str>,
        cwd: &'a Path,
        project: &'a Path,
        identities: &'a [AgentLaunchIdentity],
        leader: Option<&'a str>,
    },
}

pub struct ClosedCohort<'a> {
    pub name: &'a str,
    pub subject: &'a str,
    pub facts: &'a crate::harness::worktree_entry::WorktreeEntry,
    pub resume_command: Option<&'a str>,
    pub fresh_command: &'a str,
}

impl ClosedCohort<'_> {
    pub fn refuse<T>(&self) -> Result<T> {
        worktree_entry::refuse(self)
    }
}

pub enum CohortChoice {
    Resume,
    Fresh,
    Recreate,
    Cancel,
}

pub trait Interaction {
    fn cross_repo(&mut self, workspace: &crate::ResolvedWorkspace) -> anyhow::Result<bool>;
    fn pr_branch(
        &mut self,
        facts: &crate::worktree::WorktreeErr,
    ) -> anyhow::Result<Option<crate::worktree::PrBranchChoice>>;
    fn unmanaged_checkout(&mut self, name: &str, cwd: &std::path::Path) -> anyhow::Result<bool>;
    fn closed_cohort(&mut self, cohort: ClosedCohort<'_>) -> anyhow::Result<CohortChoice>;
    fn notice(&mut self, notice: Notice<'_>) -> anyhow::Result<()>;
}

pub struct Unattended;

impl Interaction for Unattended {
    fn cross_repo(&mut self, workspace: &crate::ResolvedWorkspace) -> anyhow::Result<bool> {
        let Some(root) = workspace
            .cwd_project_root
            .as_deref()
            .filter(|root| *root != workspace.project_root)
        else {
            return Ok(true);
        };
        anyhow::bail!(
            "room root {} differs from current Git root {}; pass --root {} (or run from the room's checkout) for a non-interactive launch",
            workspace.project_root.display(),
            root.display(),
            root.display()
        );
    }
    fn pr_branch(
        &mut self,
        facts: &crate::worktree::WorktreeErr,
    ) -> anyhow::Result<Option<crate::worktree::PrBranchChoice>> {
        if matches!(
            facts,
            crate::worktree::WorktreeErr::PrBranchDiverged {
                divergence: crate::worktree::PrBranchDivergence::Behind { .. },
                ..
            }
        ) {
            return Ok(Some(crate::worktree::PrBranchChoice::Remote));
        }
        anyhow::bail!("{facts}")
    }
    fn unmanaged_checkout(&mut self, name: &str, _cwd: &std::path::Path) -> anyhow::Result<bool> {
        anyhow::bail!(
            "worktree `{name}` is not RimZ-managed; rerun in a terminal to confirm entering it"
        )
    }
    fn closed_cohort(&mut self, cohort: ClosedCohort<'_>) -> anyhow::Result<CohortChoice> {
        cohort.refuse()
    }
    fn notice(&mut self, _notice: Notice<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

use crate::agents::AgentState;
use crate::harness::ancestry::{self, LaunchAncestry, LaunchFocus};
use crate::harness::plan::{
    LayoutPaneParams, ResolvedLaunch, cohort_cells, compile_layout_panes, launch_identity_requests,
};
use crate::harness::spec::{Cell, LayoutSpec};
use crate::ids::{LoginKey, LoginName};
use crate::room::RoomContext;
use crate::store::writer::AgentLaunchScope;
use placement::{
    Placement, PlacementErrors, PlacementRequest, apply_in_place_downgrade, resolve_placement,
    resume_outside_launch_dir,
};
use resolve::{FinalizedLaunch, resolve_finalized_layout};

const LAUNCH_PLACEMENT_ERRORS: PlacementErrors = PlacementErrors {
    new_tab: "opening agent tab",
    new_pane: "splitting the agent into a new pane",
    same_pane: "running the agent in the current pane",
};

pub struct PreparedLaunch {
    request: LayoutRequest,
    effective: crate::config::effective::LaunchAgents,
    snapshot: Option<crate::store::snapshot::SidebarSnapshot>,
    projection: crate::store::runtime::RuntimeProjection,
    layout: LayoutSpec,
    teams: crate::config::TeamsConfig,
    team_name: Option<String>,
    inferred_lane: Option<String>,
    default_lane: Option<String>,
    matching_overrides: LaunchOverrides,
    explicit_worktree_name: Option<String>,
    ancestry: Option<LaunchAncestry>,
    focus: LaunchFocus,
    receipt_leader_index: Option<usize>,
    prompt_agent_index: Option<usize>,
    checked_folder_trust: HashSet<(LoginKey, PathBuf)>,
    noticed_hook_trust: HashSet<LoginKey>,
    preflighted_logins: Vec<LoginName>,
    empty_dir: Option<crate::agents::empty_dir::Prepared>,
    placement: Placement,
    resume_scope: Option<ResumeScope>,
}

pub fn prepare_layout(
    request: LayoutRequest,
    interaction: &mut dyn Interaction,
) -> Result<Preparation> {
    let mut args = request;
    let ctx = &args.context;
    let machine_config = ctx.machine.clone();
    let cwd = args.cwd.clone();
    let allow_in_place = args.allow_in_place;
    let loop_fire = args
        .resident_loop
        .as_ref()
        .map(|fire| (fire.task.as_str(), fire.reminder.as_str()));
    let loop_task = loop_fire.map(|(task, _)| task);
    let explicit_worktree_name = if args.launch.selection == Selection::Cohort {
        None
    } else {
        args.launch
            .cohort
            .worktree
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| crate::worktree::parse_requested_name(name).map(|requested| requested.name))
            .transpose()?
    };
    let worktree_launch =
        args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some();
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    interaction.notice(Notice::Config(&machine_config))?;
    let effective = crate::config::effective::load(&machine_config, &workspace.project_root)?;
    // A bare `--resume` opens the provider's picker on the freshly routed layout;
    // `--continue` and an exact id replay recorded posture instead.
    let fresh_launch = args.launch.selection != Selection::Cohort;
    if fresh_launch {
        machine_config.agents.startup_relaunch_wait()?;
    }
    let default_lane = if loop_task.is_some() {
        context::directory_channel(
            workspace,
            cwd.as_deref().unwrap_or(&workspace.worktree_root),
        )
    } else {
        ctx.channel.clone()
    };
    let lane = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or(default_lane.as_deref());
    let snapshot = if args.launch.spec.is_some() && lane.is_some() {
        Some(ctx.store.snapshot_cached()?)
    } else {
        None
    };
    let matching_overrides = LaunchOverrides {
        agent: None,
        tier: None,
        ..args.launch.overrides.clone()
    };
    let mut print_warning = |warning: &crate::harness::plan::LaunchFinalizeWarning| {
        if !fresh_launch
            && matches!(
                warning,
                crate::harness::plan::LaunchFinalizeWarning::TierMoved { .. }
            )
        {
            return Ok(());
        }
        interaction.notice(Notice::LaunchWarning(warning))
    };
    let FinalizedLaunch {
        profiles: _,
        resolved,
        preset: _preset,
        warnings,
        inferred_lane,
        qualified_spec,
    } = resolve_finalized_layout(
        snapshot.as_ref(),
        &machine_config,
        &effective,
        args.launch.spec.as_deref(),
        args.launch
            .prompt
            .as_deref()
            .filter(|_| args.launch.selection != Selection::Cohort),
        if args.launch.selection == Selection::Cohort {
            &matching_overrides
        } else {
            &args.launch.overrides
        },
        args.launch.cohort.budget,
        args.launch.max_turns,
        lane,
        args.launch.name.is_some(),
        Some((store.runtime_paths(), store.paths())),
    )
    .inspect_err(|err| {
        if let Some(err) = err.downcast_ref::<crate::harness::plan::LaunchFinalizeError>() {
            for warning in err.warnings() {
                let _ = print_warning(warning);
            }
        }
    })?;
    if let Some(spec) = qualified_spec {
        args.launch.spec = Some(spec);
    }
    for warning in &warnings {
        print_warning(warning)?;
    }
    let ResolvedLaunch {
        teams,
        layout,
        team_name,
    } = resolved;
    let pick_session = args.launch.selection == Selection::Picker;
    if pick_session {
        validate_picker(&layout, team_name.as_deref())?;
    }
    let team = team_name.as_deref().and_then(|name| teams.0.get(name));
    let mut preflight_workspace = match cwd.as_deref() {
        Some(cwd) => workspace.at_launch_dir(cwd)?,
        None => workspace.clone(),
    };
    if worktree_launch {
        preflight_workspace.cwd_project_root = Some(workspace.launch_repo_root().to_path_buf());
    }
    if args.launch.selection != Selection::Cohort
        && args.launch.cohort.from_pr.is_none()
        && let Some(name) = team_name.as_deref()
        && let Some(team) = team
    {
        crate::harness::schedule::team::validate_launch(
            name,
            team,
            if loop_task.is_some() {
                &preflight_workspace
            } else {
                workspace
            },
            explicit_worktree_name.as_deref(),
        )?;
    }
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let caller = if matches!(args.starter, Starter::Loop) {
        None
    } else {
        ancestry::resolve_caller(&projection.agents)
    };
    let focus = LaunchFocus::resolve(args.launch.cohort.bg, caller.as_ref());
    let ancestry = ancestry::resolve_launch_ancestry(
        caller
            .as_ref()
            .map(|caller| ancestry::resolve_launch_caller(&projection.agents, caller))
            .transpose()?,
        false,
        machine_config.agents.max_chain_length,
    )?;
    let prompt = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty());
    let leader = crate::harness::spec::prompt_leader(&layout, team);
    let receipt_leader_index = if prompt.is_some() {
        Some(leader?)
    } else {
        leader.ok()
    };
    let prompt_agent_index = prompt.and(receipt_leader_index);
    let mut checked_folder_trust = HashSet::new();
    let mut noticed_hook_trust = HashSet::new();
    let mut preflighted_logins = Vec::new();
    let mut empty_dir = None;
    if args.launch.selection != Selection::Cohort {
        checkout::admit_launch_worktree_name(
            workspace,
            &machine_config.agents.worktree,
            &projection.agents,
            explicit_worktree_name.as_deref(),
        )?;
    }
    // Resolve where the launch lands before any side effect — the live-session
    // probe, worktree creation, the store append, the sidebar build — so an
    // invalid `--new-pane` (a multi-cell layout, or run outside a room) refuses
    // cleanly and leaves no provisional rows or worktree behind. Feasibility
    // reads the ambient pane; the split target is re-derived for the resolved
    // backend below.
    let single_cell = layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum::<usize>()
        == 1;
    let resume_scope = if args.launch.selection == Selection::Cohort {
        let scope = if args.launch.teams_scene
            && let Some(lane) = args
                .launch
                .cohort
                .worktree
                .as_deref()
                .filter(|lane| !lane.trim().is_empty())
        {
            ResumeScope::Lane(lane.to_owned())
        } else {
            resume_worktree_scope(
                args.launch.cohort.worktree.as_deref(),
                workspace,
                &machine_config,
            )?
            .map_or(ResumeScope::Room, ResumeScope::Worktree)
        };
        Some(scope)
    } else {
        None
    };
    let channel_launch = args.launch.cohort.channel.is_some();
    let placement = if resume_scope.is_some() {
        Placement::NewTab
    } else {
        apply_in_place_downgrade(
            resolve_placement(
                args.launch.cohort.new_tab,
                args.launch.new_pane,
                machine_config.agents.placement,
                worktree_launch || channel_launch,
                single_cell,
                crate::mux::ambient_pane_id().is_some(),
            )?,
            focus,
            allow_in_place,
        )
    };
    let empty_dir_eligible = single_cell && !worktree_launch && args.launch.cwd.is_none();
    if resume_scope.is_none() {
        if !single_cell
            && !worktree_launch
            && args.launch.cwd.is_none()
            && layout
                .agent_cells()
                .any(|cell| cell.empty_dir == Some(true))
        {
            interaction.notice(Notice::Warning(
                "empty-dir ignored: applies only to single-agent launches",
            ))?;
        }
        for (index, cell) in layout.agent_cells().enumerate() {
            let (login, prepared) = check_cell(
                &preflight_workspace,
                store.runtime_paths(),
                cell,
                crate::config::Isolation::resolve(
                    cell.launch.isolation,
                    cell.isolation_default,
                    machine_config.agents.isolation,
                ),
                if pick_session {
                    CellStart::Pick
                } else {
                    CellStart::Launch(prompt.filter(|_| Some(index) == prompt_agent_index))
                },
                &mut checked_folder_trust,
                &mut noticed_hook_trust,
                || {
                    crate::config::definitions::hook_trust_fix(
                        &cell.kind,
                        cell.launch.profile.as_deref(),
                        None,
                    )
                },
                match args.starter {
                    Starter::Loop => Some(trust::Unattended::Loop),
                    Starter::Agent => Some(trust::Unattended::Agent),
                    Starter::Team => Some(trust::Unattended::Team),
                    Starter::Human => None,
                }
                .or_else(|| caller.as_ref().map(|_| trust::Unattended::Agent))
                .or_else(|| team_name.as_ref().map(|_| trust::Unattended::Team)),
                empty_dir_eligible.then_some((&projection.agents, args.launch.name.as_deref())),
                interaction,
            )?;
            preflighted_logins.push(login);
            empty_dir = prepared;
        }
    }
    Ok(Preparation::Prepared(Box::new(PreparedLaunch {
        request: args,
        effective,
        snapshot,
        projection,
        layout,
        teams,
        team_name,
        inferred_lane,
        default_lane,
        matching_overrides,
        explicit_worktree_name,
        ancestry,
        focus,
        receipt_leader_index,
        prompt_agent_index,
        checked_folder_trust,
        noticed_hook_trust,
        preflighted_logins,
        empty_dir,
        placement,
        resume_scope,
    })))
}

pub fn execute(
    prepared: PreparedLaunch,
    interaction: &mut dyn Interaction,
) -> Result<LaunchOutcome> {
    let PreparedLaunch {
        request: args,
        effective,
        snapshot,
        projection,
        layout,
        teams,
        team_name,
        inferred_lane,
        default_lane,
        matching_overrides,
        explicit_worktree_name,
        ancestry,
        focus,
        receipt_leader_index,
        prompt_agent_index,
        checked_folder_trust,
        noticed_hook_trust,
        preflighted_logins,
        empty_dir,
        placement,
        resume_scope,
    } = prepared;
    let ctx = &args.context;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let machine_config = ctx.machine.clone();
    let cwd = args.cwd.clone();
    let loop_fire = args
        .resident_loop
        .as_ref()
        .map(|fire| (fire.task.as_str(), fire.reminder.as_str()));
    let loop_task = loop_fire.map(|(task, _)| task);
    let worktree_launch =
        args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some();
    let single_cell = layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum::<usize>()
        == 1;
    let in_place = placement == Placement::SamePane;
    let pick_session = args.launch.selection == Selection::Picker;
    if let Some(scope) = resume_scope {
        launch_resume_layout(
            &args,
            &machine_config,
            &teams,
            &effective.profiles,
            layout,
            team_name,
            single_cell,
            scope,
            ancestry.as_ref(),
            focus,
            checked_folder_trust,
            noticed_hook_trust,
            interaction,
        )?;
        return Ok(LaunchOutcome::Existing);
    }
    let team = team_name.as_deref().and_then(|name| teams.0.get(name));
    let prompt = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty());
    let lane = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or(default_lane.as_deref());
    let room = match args.entry {
        RoomEntry::Live => RoomContext::live_tab(workspace, machine_config.clone(), ctx.mux)?,
    };
    let mux = room.mux_name();
    let backend = room.backend();
    if worktree_launch && !checkout::confirm_cross_repo_worktree(workspace, interaction)? {
        return Ok(LaunchOutcome::Declined);
    }

    let cells = cohort_cells(&layout);
    let base = if team_name.is_some() {
        crate::worktree::LaunchBase::Team
    } else {
        crate::worktree::LaunchBase::Configured
    };
    // A choice-less PR lookup discovers the holder without moving its local tip.
    let mut checkout = args.launch.cohort.from_pr.as_ref().map(|pr| {
        crate::worktree::resolve_launch_checkout(
            workspace,
            &machine_config.agents.worktree,
            args.launch.cohort.worktree.as_deref(),
            Some(pr),
            None,
            cwd.as_deref(),
            base,
        )
    });
    let cohort_target = if team_name.is_some() || cells.len() >= 2 {
        match checkout.as_ref() {
            Some(Ok(launch)) if launch.reused => launch
                .worktree_name
                .as_ref()
                .map(|name| (name.clone(), launch.cwd.clone())),
            // Agents record the marker's lexical path; Git lists the holder realpath'd.
            Some(Err(crate::worktree::WorktreeErr::PrBranchDiverged {
                holder: Some(path),
                ..
            })) => crate::worktree::read_marker_for_worktree(path)?
                .map(|marker| (marker.name, marker.worktree_path)),
            Some(_) => None,
            None => explicit_worktree_name
                .as_deref()
                .map(|name| {
                    reconcile::cohort_worktree_path(
                        workspace,
                        &machine_config.agents.worktree,
                        name,
                    )
                    .map(|path| (name.to_owned(), path))
                })
                .transpose()?,
        }
    } else {
        None
    };
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd — a shell pane that has `cd`'d into a
    // subdirectory would otherwise stamp that subdirectory's basename.
    let explicit_channel = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or(inferred_lane.as_deref().filter(|_| cwd.is_none()));
    if let Some(team) = team_name.as_deref() {
        let target = if let Some(path) = cwd.as_deref() {
            Some((None, path))
        } else if let Some((name, path)) = cohort_target.as_ref() {
            Some((Some(name.as_str()), path.as_path()))
        } else {
            match checkout.as_ref() {
                Some(Ok(launch)) => Some((launch.worktree_name.as_deref(), launch.cwd.as_path())),
                Some(Err(crate::worktree::WorktreeErr::PrBranchDiverged {
                    holder: Some(path),
                    ..
                })) => Some((None, path.as_path())),
                Some(Err(_)) => None,
                // A generated worktree has no prior occupants.
                None if args.launch.cohort.worktree.is_some() => None,
                None => Some((None, workspace.worktree_root.as_path())),
            }
        };
        if let Some((name, path)) = target
            && let Some(hold) = crate::harness::resume::inspect_team_hold(&projection.agents, path)
            && hold.team != team
        {
            let (reason, release, resume) = team_hold_guidance(&hold);
            let place = match name {
                Some(name) => format!("worktree `{name}`"),
                None => format!("checkout `{}`", path.display()),
            };
            bail!(
                "{place} already holds team `{}` ({reason}); one team per checkout: resume it with {resume}, or launch into another worktree{release}",
                hold.team
            );
        }
        if let Some((_, path)) = target
            && let Some(channel) = crate::harness::spec::resolve_room_channel(
                &workspace.project_root,
                path,
                Some(team),
                explicit_channel,
            )
            && let Some(hold) =
                crate::harness::resume::inspect_channel_hold(&projection.agents, &channel)
        {
            let comparable_path = |path: &Path| {
                std::fs::canonicalize(path)
                    .unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(path))
            };
            if comparable_path(&hold.checkout) != comparable_path(path) {
                let (reason, release, resume) = team_hold_guidance(&hold);
                bail!(
                    "channel `#{channel}` already carries team `{}` at checkout `{}` ({reason}); one team per channel: resume it with {resume}, or launch on another channel{release}",
                    hold.team,
                    hold.checkout.display()
                );
            }
        }
    }
    let mut check_entry = cohort_target.is_none();
    if let Some((name, path)) = cohort_target {
        let spec_display = args.launch.spec.as_deref().unwrap_or("<spec>");
        match reconcile::reconcile_cohort_launch(
            workspace,
            &machine_config,
            ctx.root.as_deref(),
            ctx.mux,
            backend,
            store,
            &name,
            &path,
            spec_display,
            team_name.as_deref(),
            &cells,
            receipt_leader_index,
            prompt,
            args.launch.cohort.fresh,
            focus,
            loop_task.is_some(),
            interaction,
        )? {
            reconcile::Reconciled::Done | reconcile::Reconciled::PromptDispatched => {
                return Ok(LaunchOutcome::Existing);
            }
            reconcile::Reconciled::Resume(path) => {
                validate_resume_inputs(&args.launch.overrides, ResumeEntrance::Reconcile)?;
                let layout = resolve_finalized_layout(
                    snapshot.as_ref(),
                    &machine_config,
                    &effective,
                    args.launch.spec.as_deref(),
                    None,
                    &matching_overrides,
                    args.launch.cohort.budget,
                    args.launch.max_turns,
                    lane,
                    false,
                    Some((store.runtime_paths(), store.paths())),
                )?
                .resolved
                .layout;
                if let Some(checkout) = checkout.take()
                    && checkout::settle_launch_checkout(
                        workspace,
                        &machine_config.agents.worktree,
                        args.launch.cohort.worktree.as_deref(),
                        args.launch.cohort.from_pr.as_ref(),
                        cwd.as_deref(),
                        base,
                        checkout,
                        interaction,
                    )?
                    .is_none()
                {
                    return Ok(LaunchOutcome::Declined);
                }
                return launch_resume_layout(
                    &args,
                    &machine_config,
                    &teams,
                    &effective.profiles,
                    layout,
                    team_name,
                    single_cell,
                    ResumeScope::Worktree(path),
                    ancestry.as_ref(),
                    focus,
                    checked_folder_trust,
                    noticed_hook_trust,
                    interaction,
                )
                .map(|()| LaunchOutcome::Existing);
            }
            reconcile::Reconciled::Absent => check_entry = true,
            reconcile::Reconciled::Continue => {}
            reconcile::Reconciled::Removed => checkout = None,
        }
    }

    if check_entry
        && loop_task.is_none()
        && args.launch.cohort.from_pr.is_none()
        && args.launch.selection == Selection::Fresh
        && let Some(name) = explicit_worktree_name.as_deref()
    {
        worktree_entry::check(
            workspace,
            &machine_config,
            ctx.root.as_deref(),
            ctx.mux,
            store,
            name,
            args.launch.spec.as_deref().unwrap_or("<spec>"),
            team_name.as_deref(),
            prompt,
            args.launch.cohort.fresh,
            None,
            interaction,
        )?;
    }

    let launch = match checkout {
        Some(checkout) => checkout::settle_launch_checkout(
            workspace,
            &machine_config.agents.worktree,
            args.launch.cohort.worktree.as_deref(),
            args.launch.cohort.from_pr.as_ref(),
            cwd.as_deref(),
            base,
            checkout,
            interaction,
        ),
        None => checkout::resolve_launch_checkout(
            workspace,
            &machine_config,
            args.launch.cohort.worktree.as_deref(),
            args.launch.cohort.from_pr.as_ref(),
            cwd.as_deref(),
            base,
            interaction,
        ),
    }?;
    let Some(mut launch) = launch else {
        return Ok(LaunchOutcome::Declined);
    };
    let checkout = empty_dir.as_ref().map(|_| launch.cwd.display().to_string());
    if let Some(prepared) = &empty_dir {
        launch.cwd.clone_from(&prepared.cwd);
        launch.repo_root = None;
    }
    if let (Some(name), Some(team)) = (team_name.as_deref(), team) {
        crate::harness::team_notes::register(
            store,
            &launch.cwd,
            name,
            team,
            crate::harness::team_notes::Launch::Fresh,
        )?;
    }
    lsp_admission::admit(&launch.cwd, &machine_config, interaction)?;
    if let Some(channel) = args.launch.cohort.channel.as_deref() {
        crate::channel::admit_launch(workspace, channel)?;
    }
    let room_channel = crate::harness::spec::resolve_room_channel(
        &workspace.project_root,
        checkout.as_deref().map(Path::new).unwrap_or(&launch.cwd),
        team_name.as_deref(),
        explicit_channel,
    );
    let mut launch_requests = launch_identity_requests(
        &layout,
        args.launch.name.as_deref(),
        empty_dir
            .as_ref()
            .map(|prepared| prepared.name.as_str())
            .or_else(|| launch.generated_name()),
        team_name.as_deref(),
        team.map(|team| team.roles.as_slice()),
        room_channel.as_deref(),
        prompt.zip(prompt_agent_index),
        None,
        ancestry.as_ref(),
    )?;
    for (request, login) in launch_requests.iter_mut().zip(preflighted_logins) {
        request.login = crate::store::writer::LaunchLogin::Pinned(login);
        request.launch.checkout.clone_from(&checkout);
    }
    if let Some(index) = prompt_agent_index {
        launch_requests[index].launch.loop_task = loop_task.map(str::to_owned);
    }
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            branch: launch.branch.clone(),
            description: args.launch.cohort.description.clone(),
        },
    )?;
    let peer_prompt = prepare_peer_prompt(
        store,
        launch_batch.identities(),
        &launch.cwd,
        args.launch.report_to,
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&launch_batch);
    })?;
    let fail_peer_prompt = || {
        if let Some((peer, _)) = &peer_prompt {
            match crate::harness::run::fail_peer_run(store, peer, "peer launch failed") {
                Ok(Some(_)) => {
                    if let Some((_, launcher)) = peer.launcher() {
                        crate::harness::orphan_sweep::spawn_digest_helper(
                            store.runtime_paths(),
                            launcher.clone(),
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not fail peer launch run"),
            }
        }
    };
    let cleanup_worktree = launch.owns_checkout_lifecycle();
    let worktree_name = launch.worktree_name.clone();
    let cwd = launch.cwd;
    let title = room_channel.as_deref().map_or_else(
        || {
            crate::harness::spec::default_tab_title(
                &layout,
                worktree_name.as_deref(),
                team_name.as_deref(),
            )
        },
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd);
    let mut panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            runtime: store.runtime_paths(),
            cwd: &cwd,
            cleanup_worktree,
            in_place,
            pick_session,
            resume_seeds: None,
            launch_identities: launch_batch.identities(),
            fallback_channel: None,
            loop_reminder: loop_fire.map(|(_, reminder)| reminder),
        },
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&launch_batch);
        fail_peer_prompt();
    })?;
    panes.focused_pane = team_leader_pane(&layout, team);
    if pick_session && in_place {
        interaction.notice(Notice::PickerReceipt {
            kind: launch_batch.identities()[0].kind.as_str(),
            channel: room_channel.as_deref(),
            cwd: launch_batch.identities()[0]
                .launch
                .checkout
                .as_deref()
                .map(Path::new)
                .unwrap_or(&cwd),
            project: &workspace.project_root,
        })?;
    }
    placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            tab_anchor: placement::resolve_tab_anchor(
                placement,
                team_name.is_some(),
                crate::mux::own_pane_id(mux),
                checkout::pinned_room_root().as_deref(),
                &workspace.project_root,
            ),
            cwd: cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: crate::room::pane_identity_env(
                workspace,
                checkout.as_deref().map(Path::new).unwrap_or(&cwd),
                room_channel.as_deref(),
                !worktree_launch && loop_task.is_none(),
            ),
            focus,
            errors: LAUNCH_PLACEMENT_ERRORS,
        },
        || crate::harness::assist_log::record_tier_fallbacks(launch_batch.identities()),
    )
    .inspect_err(|_| fail_peer_prompt())?;
    Ok(LaunchOutcome::Opened(Box::new(OpenedLayout {
        identities: launch_batch.identities().to_vec(),
        leader_index: receipt_leader_index,
        team: team_name
            .as_deref()
            .zip(team)
            .map(|(name, team)| (name.to_owned(), team.clone())),
        channel: room_channel,
        cwd,
        in_place,
        pick_session,
        peer: peer_prompt.map(|(_, run)| run),
    })))
}
pub enum ResumeEntrance {
    Flag,
    Reconcile,
}

enum ResumeScope {
    Room,
    Worktree(PathBuf),
    Lane(String),
}

pub fn validate_resume_inputs(overrides: &LaunchOverrides, entrance: ResumeEntrance) -> Result<()> {
    let (input, message) = if overrides.preset.system_prompt_file.is_some() {
        (
            "--system-prompt-file",
            "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--system-prompt-file`",
        )
    } else if !overrides.preset.append_system_prompt_files.is_empty() {
        (
            "--append-system-prompt-file",
            "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--append-system-prompt-file`",
        )
    } else if !overrides.passthrough.is_empty() {
        (
            "passthrough arguments",
            "resume does not accept passthrough arguments after `--`; put supported settings in the profile or launch fresh",
        )
    } else {
        return Ok(());
    };
    match entrance {
        ResumeEntrance::Flag => bail!("{message}"),
        ResumeEntrance::Reconcile => {
            bail!("{message}; rerun without {input}, or choose fresh instead of resume")
        }
    }
}

fn team_hold_guidance(hold: &crate::harness::resume::TeamHold) -> (String, String, String) {
    let target = format!("{}#{}", hold.team, hold.channel);
    let (reason, release) = match &hold.reason {
        crate::harness::resume::TeamHoldReason::LiveMember => {
            ("a member is live".to_owned(), String::new())
        }
        crate::harness::resume::TeamHoldReason::BoardStage(stage) => (
            format!("its board is at `{stage}`"),
            format!(
                ", or release it with `{}`",
                shell_command(["rimz", "teams", "flip", &target, "Done", "<note>"])
            ),
        ),
    };
    let resume = format!("`{}`", shell_command(["rimz", "teams", "resume", &target]));
    (reason, release, resume)
}

fn prepare_peer_prompt(
    store: &crate::Store,
    identities: &[AgentLaunchIdentity],
    cwd: &Path,
    report_to: crate::store::run::ReportTo,
) -> Result<Option<(AgentState, crate::store::run::RunRecord)>> {
    let Some(identity) = identities.iter().find(|identity| {
        identity.launch.launched_by.is_some()
            && identity
                .prompt
                .as_deref()
                .is_some_and(|prompt| !prompt.trim().is_empty())
    }) else {
        return Ok(None);
    };
    let adapter = crate::agents::find_definition(identity.kind.as_str())
        .context("launched peer has no adapter")?;
    if !crate::harness::run::peer_can_report(adapter) {
        return Ok(None);
    }
    let agents = store.runtime_projection(crate::RuntimeScope::Audit)?.agents;
    let peer = agents
        .iter()
        .find(|peer| {
            peer.kind == identity.kind && peer.launch_id.as_ref() == Some(&identity.agent_id)
        })
        .context("launched peer has no provisional row")?;
    let launcher = peer
        .launcher()
        .and_then(|(kind, id)| crate::address::launch_row(&agents, kind, id));
    if launcher.is_none() {
        tracing::warn!(
            "agent launched, but its launcher could not be resolved; its response lands under its own name"
        );
    }
    let run = crate::harness::run::create_peer_prompt(
        store.paths(),
        peer,
        launcher.and_then(|launcher| launcher.name.as_deref()),
        adapter,
        identity.prompt.as_deref().unwrap_or_default(),
        cwd,
        report_to,
    )?;
    Ok(run.map(|run| (peer.clone(), run)))
}

fn queue_resume_prompt(
    store: &crate::Store,
    session_name: &str,
    seeds: &[crate::harness::plan::CohortSeed],
    identities: &[AgentLaunchIdentity],
    prompt: &str,
    leader: usize,
) -> Result<()> {
    use crate::harness::plan::CohortSeed;
    use crate::store::message::{DeliveryGate, MessageRecord, MessageSender};
    let (kind, agent_id, name) = match &seeds[leader] {
        CohortSeed::Resume(agent) => (&agent.kind, &agent.agent_id, agent.name.clone()),
        CohortSeed::Fresh => {
            let index = seeds[..leader]
                .iter()
                .filter(|seed| matches!(seed, CohortSeed::Fresh))
                .count();
            let identity = &identities[index];
            (
                &identity.kind,
                &identity.agent_id,
                Some(identity.name.clone()),
            )
        }
    };
    let message = MessageRecord::new_for_card(
        store.paths().workspace_id.clone(),
        kind.clone(),
        agent_id.clone(),
        name,
        prompt.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Human);
    store.queue_message(&message, session_name)?;
    Ok(())
}

/// What a preflighted cell starts: a launch with its optional prompt, or the
/// provider's session picker.
#[derive(Clone, Copy)]
enum CellStart<'a> {
    Launch(Option<&'a str>),
    Pick,
}

#[allow(clippy::too_many_arguments)]
fn check_cell(
    workspace: &crate::ResolvedWorkspace,
    runtime: &crate::RuntimePaths,
    cell: &crate::harness::spec::AgentCell,
    isolation: crate::config::Isolation,
    start: CellStart<'_>,
    checked_folder_trust: &mut HashSet<(crate::ids::LoginKey, PathBuf)>,
    noticed_hook_trust: &mut HashSet<crate::ids::LoginKey>,
    hook_fix: impl FnOnce() -> String,
    unattended: Option<trust::Unattended>,
    empty_dir: Option<(&[AgentState], Option<&str>)>,
    interaction: &mut dyn Interaction,
) -> Result<(
    crate::ids::LoginName,
    Option<crate::agents::empty_dir::Prepared>,
)> {
    let adapter = crate::agents::find_definition(cell.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", cell.kind))?;
    crate::sandbox::preflight_launch(
        isolation,
        &cell.kind,
        cell.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let mut request =
        crate::harness::launch::ExecRequest::bare_launch(cell.kind.clone(), Vec::new());
    let state = crate::StatePaths::for_workspace(runtime.workspace_id.clone())?;
    let machine = crate::config::MachineConfig::load_lenient();
    let login = cell
        .launch
        .login
        .clone()
        .map_or(
            crate::store::writer::LaunchLogin::RoomDefault,
            crate::store::writer::LaunchLogin::Pinned,
        )
        .resolve(
            &cell.kind,
            &crate::agents::room_accounts(
                &state.workspace_record,
                Some(&workspace.project_root),
                &machine,
            )?,
            &machine.accounts,
        )?;
    let decision = match empty_dir {
        Some((agents, name)) => crate::agents::empty_dir::decide(
            &state,
            agents,
            name,
            &cell.kind,
            &login,
            cell.empty_dir == Some(true),
        )?,
        None => crate::agents::empty_dir::Decision::Skip,
    };
    let prepared = match decision {
        crate::agents::empty_dir::Decision::Applied(prepared) => Some(prepared),
        crate::agents::empty_dir::Decision::Fallback(notice) => {
            interaction.notice(Notice::Warning(&notice.to_string()))?;
            None
        }
        crate::agents::empty_dir::Decision::Skip => None,
    };
    let cwd = prepared
        .as_ref()
        .map_or(workspace.worktree_root.as_path(), |prepared| {
            prepared.cwd.as_path()
        });
    request.identity.params.login = (!login.is_default()).then(|| login.name().clone());
    request.skills.clone_from(&cell.skills);
    request.isolation_default = cell.isolation_default;
    request.action = match start {
        CellStart::Pick => crate::harness::launch::ExecAction::Pick {
            extra_args: cell.args.clone(),
        },
        CellStart::Launch(prompt) => crate::harness::launch::ExecAction::Launch {
            prompt: prompt.map(str::to_owned),
            extra_args: cell.args.clone(),
        },
    };
    let process = crate::harness::launch::preflight_agent_process(
        &workspace.project_root,
        &request,
        cwd,
        (!isolation.is_scoped()).then_some(runtime),
    )?;
    if let Some(notice) = trust::preflight_hooks(
        login.health(&crate::agents::ambient_env()),
        || unattended,
        hook_fix,
        noticed_hook_trust,
    )? {
        interaction.notice(Notice::Trust(&notice))?;
    }
    let mut launch_env = crate::agents::ambient_env();
    launch_env.extend(process.env.clone());
    if let Some(error) = adapter.launch_blocker(cwd, &launch_env, &process.provider_argv) {
        return Err(error.into());
    }
    if adapter.min_version().is_some()
        && let Ok(Some(path)) = process.resolve_program_after_shell_rc()
    {
        crate::agents::version::check_launch_version_floor(adapter, &path)?;
    }
    if checked_folder_trust.insert((login.key(), cwd.to_path_buf()))
        && let Some(notice) = trust::preflight(
            cell.kind.as_str(),
            cwd,
            adapter.folder_trust(
                cwd,
                workspace
                    .cwd_project_root
                    .as_deref()
                    .filter(|_| prepared.is_none() && cell.launch.checkout.is_none()),
                &login.env(&crate::agents::ambient_env()),
            ),
            unattended,
        )?
    {
        interaction.notice(Notice::Trust(&notice))?;
    }
    Ok((login.name().clone(), prepared))
}

#[allow(clippy::too_many_arguments)]
fn launch_resume_layout(
    args: &LayoutRequest,
    machine_config: &crate::config::MachineConfig,
    teams: &crate::config::TeamsConfig,
    profiles: &crate::config::ProfilesConfig,
    mut layout: LayoutSpec,
    team_name: Option<String>,
    single_cell: bool,
    scope: ResumeScope,
    ancestry: Option<&crate::harness::ancestry::LaunchAncestry>,
    focus: LaunchFocus,
    mut checked_folder_trust: HashSet<(crate::ids::LoginKey, PathBuf)>,
    mut noticed_hook_trust: HashSet<crate::ids::LoginKey>,
    interaction: &mut dyn Interaction,
) -> Result<()> {
    let ctx = &args.context;
    let allow_in_place = args.allow_in_place;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let agents = match &scope {
        ResumeScope::Worktree(target) => {
            let target = crate::utils::path::normalize_path_lexical(target);
            projection
                .agents
                .iter()
                .filter(|agent| agent_matches_worktree_filter(agent, &target))
                .cloned()
                .collect::<Vec<_>>()
        }
        ResumeScope::Lane(lane) => projection
            .agents
            .iter()
            .filter(|agent| crate::address::agent_in_worktree(agent, lane))
            .cloned()
            .collect(),
        ResumeScope::Room => projection.agents.clone(),
    };
    let cells = cohort_cells(&layout);
    let spec = args.launch.spec.as_deref().unwrap_or("<spec>");
    let scope_label = match &scope {
        ResumeScope::Worktree(path) => worktree_scope_label(path),
        _ => None,
    };
    let logins = crate::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &crate::config::MachineConfig::load_lenient(),
    )?;
    let team = team_name.as_deref().and_then(|name| teams.0.get(name));
    let mut plan = crate::harness::resume::plan_cohort_resume(
        &agents,
        &logins,
        &crate::agents::machine_login_catalog(),
        crate::store::runtime::agent_liveness,
        &cells,
        team_name.as_deref(),
        |path| path.is_dir(),
        crate::harness::resume::resume_session_present,
    )
    .map_err(|err| {
        if args.launch.teams_scene {
            team_resume_error(
                err,
                spec,
                args.launch
                    .cohort
                    .worktree
                    .as_deref()
                    .filter(|lane| !lane.trim().is_empty()),
                &agents,
                &projection.agents,
                teams,
                |path| path.is_dir(),
                crate::store::runtime::agent_liveness,
            )
        } else {
            cohort_resume_error(err, spec, scope_label.as_deref(), &agents, teams)
        }
    })?;
    let availability = crate::harness::plan::LaunchAvailability::read(
        store.runtime_paths(),
        store.paths(),
        machine_config,
        jiff::Timestamp::now(),
    );
    let unavailable = |kind: &str, model: &str| availability.unavailable(kind, model);
    let mut overrides = resume_overrides(&args.launch.overrides, machine_config, &unavailable)?;
    overrides.team = team;
    crate::harness::resume::restore_routed_cells(&mut layout, &plan.seeds, profiles, &overrides)?;
    let cwd = plan
        .cwd
        .clone()
        .context("cohort resume matched no working directory")?;
    let preflight_workspace = workspace.at_launch_dir(&cwd)?;
    let mut preflighted_logins = Vec::new();
    for (cell, seed) in layout.agent_cells().zip(&plan.seeds) {
        let mut cell = cell.clone();
        let mut preflight_workspace = preflight_workspace.clone();
        if let crate::harness::plan::CohortSeed::Resume(agent) = seed {
            cell.launch.checkout.clone_from(&agent.checkout);
            cell.launch.login.clone_from(&agent.login);
            if agent.checkout.is_some() {
                let cwd = agent
                    .worktree_path
                    .as_deref()
                    .context("empty-dir agent has no recorded working directory")?;
                crate::agents::empty_dir::prepare_reentry(store.paths(), Path::new(cwd))?;
                preflight_workspace.worktree_root = cwd.into();
                lsp_admission::admit(Path::new(cwd), machine_config, interaction)?;
            }
        }
        let isolation = match seed {
            crate::harness::plan::CohortSeed::Resume(agent) => {
                crate::harness::plan::effective_resume_isolation(
                    cell.launch.isolation,
                    agent.isolation,
                )
            }
            crate::harness::plan::CohortSeed::Fresh => cell.launch.isolation,
        };
        let (login, _) = check_cell(
            &preflight_workspace,
            store.runtime_paths(),
            &cell,
            crate::config::Isolation::resolve(
                isolation,
                cell.isolation_default,
                machine_config.agents.isolation,
            ),
            CellStart::Launch(None),
            &mut checked_folder_trust,
            &mut noticed_hook_trust,
            || match seed {
                crate::harness::plan::CohortSeed::Resume(agent) => {
                    crate::config::definitions::hook_trust_fix(
                        &agent.kind,
                        agent.profile.as_deref(),
                        Some((agent, "resume")),
                    )
                }
                crate::harness::plan::CohortSeed::Fresh => {
                    crate::config::definitions::hook_trust_fix(
                        &cell.kind,
                        cell.launch.profile.as_deref(),
                        None,
                    )
                }
            },
            ancestry
                .map(|_| trust::Unattended::Agent)
                .or_else(|| team_name.as_ref().map(|_| trust::Unattended::Team)),
            None,
            interaction,
        )?;
        if matches!(seed, crate::harness::plan::CohortSeed::Fresh) {
            preflighted_logins.push(login);
        }
    }
    lsp_admission::admit(&cwd, machine_config, interaction)?;
    if let (Some(name), Some(team)) = (team_name.as_deref(), team) {
        crate::harness::team_notes::register(
            store,
            &cwd,
            name,
            team,
            crate::harness::team_notes::Launch::Resume,
        )?;
    }
    let lane_path = plan
        .seeds
        .iter()
        .find_map(|seed| match seed {
            crate::harness::plan::CohortSeed::Resume(agent) => {
                agent.checkout.as_deref().map(Path::new)
            }
            crate::harness::plan::CohortSeed::Fresh => None,
        })
        .unwrap_or(&cwd);
    let channel = crate::harness::spec::resolve_room_channel(
        &workspace.project_root,
        lane_path,
        team_name.as_deref(),
        plan.channel.as_deref(),
    );
    plan.channel = channel.clone();
    let scoped_resume = resume_outside_launch_dir(
        channel.as_deref(),
        &cwd,
        lane_path,
        &workspace.project_root,
        &workspace.worktree_root,
        std::env::current_dir().ok().as_deref(),
    );
    let placement = if single_cell {
        apply_in_place_downgrade(
            resolve_placement(
                args.launch.cohort.new_tab,
                args.launch.new_pane,
                machine_config.agents.placement,
                scoped_resume,
                single_cell,
                crate::mux::ambient_pane_id().is_some(),
            )?,
            focus,
            allow_in_place,
        )
    } else {
        Placement::NewTab
    };
    let in_place = placement == Placement::SamePane;
    let room = RoomContext::live_tab(
        workspace,
        std::sync::Arc::new(machine_config.clone()),
        ctx.mux,
    )?;
    let mux = room.mux_name();
    let backend = room.backend();
    let mut launch_requests = launch_identity_requests(
        &layout,
        None,
        None,
        team_name.as_deref(),
        team.map(|team| team.roles.as_slice()),
        channel.as_deref(),
        None,
        Some(&plan),
        ancestry,
    )?;
    for (request, login) in launch_requests.iter_mut().zip(preflighted_logins) {
        request.login = crate::store::writer::LaunchLogin::Pinned(login);
    }
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: None,
            description: None,
        },
    )?;

    let title = channel.as_deref().map_or_else(
        || crate::harness::spec::default_tab_title(&layout, None, team_name.as_deref()),
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd);
    let mut panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            runtime: store.runtime_paths(),
            cwd: &cwd,
            cleanup_worktree: false,
            in_place,
            resume_seeds: Some(&plan.seeds),
            pick_session: false,
            launch_identities: launch_batch.identities(),
            fallback_channel: channel.as_deref(),
            loop_reminder: None,
        },
    )?;
    panes.focused_pane = team_leader_pane(&layout, team);
    let leader = team.and_then(|team| team.leader.as_deref());
    let receipt = Notice::ResumeReceipt {
        plan: &plan,
        team: team_name.as_deref(),
        channel: channel.as_deref(),
        cwd: lane_path,
        project: &workspace.project_root,
        identities: launch_batch.identities(),
        leader,
    };
    if let Some(prompt) = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        queue_resume_prompt(
            store,
            &workspace.session_name,
            &plan.seeds,
            launch_batch.identities(),
            prompt,
            crate::harness::spec::prompt_leader(&layout, team)?,
        )?;
    }
    interaction.notice(Notice::ResumeAnnouncement(&plan))?;
    if in_place {
        interaction.notice(receipt)?;
    }
    placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            tab_anchor: None,
            cwd: cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: crate::room::pane_identity_env(
                workspace,
                lane_path,
                channel.as_deref(),
                false,
            ),
            focus,
            errors: LAUNCH_PLACEMENT_ERRORS,
        },
        || {
            if let Err(err) = crate::harness::rebirth::settle_refilled_seats(
                store,
                &workspace.session_name,
                &plan.refilled,
            ) {
                let _ = interaction.notice(Notice::Warning(&format!(
                    "rimz: replaced agents stay pending: {err}"
                )));
            }
            crate::harness::assist_log::record_tier_fallbacks(launch_batch.identities());
            if args.launch.overrides.tier.is_some() {
                for (cell, seed) in layout.agent_cells().zip(&plan.seeds) {
                    let crate::harness::plan::CohortSeed::Resume(agent) = seed else {
                        continue;
                    };
                    crate::harness::assist_log::record_tier_fallback(
                        &agent.kind,
                        &agent.agent_id,
                        agent.name.as_deref(),
                        &cell.launch,
                    );
                }
            }
        },
    )?;
    if !in_place {
        interaction.notice(receipt)?;
    }
    Ok(())
}

/// A team tab opens focused on its leader's pane; any other layout keeps the
/// leading pane. The leader is an agent-cell index, so command cells before it
/// still count toward its pane position.
fn team_leader_pane(layout: &LayoutSpec, team: Option<&crate::config::Team>) -> usize {
    let Some(leader) =
        team.and_then(|team| crate::harness::spec::prompt_leader(layout, Some(team)).ok())
    else {
        return 0;
    };
    layout
        .columns
        .iter()
        .flat_map(|column| column.rows.iter())
        .enumerate()
        .filter(|(_, cell)| matches!(cell, Cell::Agent(_)))
        .nth(leader)
        .map_or(0, |(position, _)| position)
}

fn agent_matches_worktree_filter(agent: &AgentState, target: &Path) -> bool {
    agent.lane_path().is_some_and(|worktree| {
        crate::utils::path::normalize_path_lexical(Path::new(worktree)) == target
    })
}

fn resume_worktree_scope(
    worktree_arg: Option<&str>,
    workspace: &crate::ResolvedWorkspace,
    machine_config: &crate::config::MachineConfig,
) -> Result<Option<PathBuf>> {
    resume_worktree_scope_with(
        worktree_arg,
        &workspace.worktree_root,
        workspace.launch_repo_root(),
        |name| {
            if workspace.cwd_project_root.is_none()
                && workspace.root_class != crate::workspace::RootClass::Repo
            {
                bail!("--worktree requires a git repository-backed room");
            }
            crate::worktree::worktree_path(
                workspace.launch_repo_root(),
                &machine_config.agents.worktree,
                name,
            )
            .map_err(Into::into)
        },
    )
}

fn resume_worktree_scope_with(
    worktree_arg: Option<&str>,
    worktree_root: &Path,
    project_root: &Path,
    resolve_named: impl FnOnce(&str) -> Result<PathBuf>,
) -> Result<Option<PathBuf>> {
    match worktree_arg.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => resolve_named(name).map(Some),
        None if worktree_root != project_root => Ok(Some(worktree_root.to_owned())),
        None => Ok(None),
    }
}

fn worktree_scope_label(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn cohort_resume_error(
    err: crate::harness::resume::CohortResumeErr,
    spec: &str,
    scope: Option<&str>,
    agents: &[AgentState],
    teams: &crate::config::TeamsConfig,
) -> anyhow::Error {
    let subject = cohort_resume_subject(spec, scope);
    match err {
        crate::harness::resume::CohortResumeErr::NothingToResume { .. } => {
            let resumable = crate::harness::resume::closed_cohort_specs(
                agents,
                crate::store::runtime::agent_liveness,
            );
            match resumable.first() {
                Some(first) => {
                    let retry = if teams.0.contains_key(first) {
                        format!("rimz teams resume {first}")
                    } else {
                        format!("rimz agents {first} --continue")
                    };
                    anyhow::anyhow!(
                        "nothing to resume for {subject}; resumable here: {} — retry with `{retry}`",
                        resumable.join(", ")
                    )
                }
                None => {
                    anyhow::anyhow!("nothing to resume for {subject}; launch without `--continue`")
                }
            }
        }
        crate::harness::resume::CohortResumeErr::MembersStillLive { labels } => {
            anyhow::anyhow!(
                "cannot resume {subject}; still live: {}; close them first or drop `--continue`",
                labels.join(", ")
            )
        }
        crate::harness::resume::CohortResumeErr::SessionNotResumable { label } => {
            anyhow::anyhow!(
                "cannot continue {subject}; {label} has no resumable session; use `rimz agents {spec} --resume` to choose a provider session, or `rimz agents {spec}` to launch fresh"
            )
        }
        crate::harness::resume::CohortResumeErr::LoginMismatch(mismatch) => mismatch.into(),
    }
}

fn cohort_resume_subject(spec: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("`{spec}` in worktree `{scope}`"),
        None => format!("`{spec}`"),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the hints need both rosters and injected checkout and liveness probes"
)]
fn team_resume_error(
    err: crate::harness::resume::CohortResumeErr,
    spec: &str,
    lane: Option<&str>,
    agents: &[AgentState],
    room_agents: &[AgentState],
    teams: &crate::config::TeamsConfig,
    checkout_exists: impl Fn(&Path) -> bool,
    liveness: impl Fn(&AgentState) -> crate::store::runtime::AgentLiveness,
) -> anyhow::Error {
    use crate::harness::resume::{CohortResumeErr, TeamHoldReason};
    use crate::store::runtime::AgentLiveness;

    let address = lane.map_or_else(|| spec.to_owned(), |lane| format!("{spec}#{lane}"));
    match err {
        CohortResumeErr::NothingToResume { .. } => {
            let mut seen = HashSet::new();
            let resumable = crate::harness::resume::closed_cohort_specs(agents, &liveness)
                .into_iter()
                .filter_map(|candidate| {
                    if teams.0.contains_key(&candidate) {
                        Some(candidate)
                    } else {
                        teams.role_spec(&candidate).map(|(team, _)| team.to_owned())
                    }
                })
                .filter(|team| team != spec && seen.insert(team.clone()))
                .collect::<Vec<_>>();
            if let Some(first) = resumable.first() {
                let target = lane.map_or_else(|| first.clone(), |lane| format!("{first}#{lane}"));
                let retry = shell_command(["rimz", "teams", "resume", &target]);
                return anyhow::anyhow!(
                    "nothing to resume for `{address}`; resumable here: {} — retry with `{retry}`",
                    resumable.join(", ")
                );
            }
            let excluded_lanes = agents
                .iter()
                .filter_map(AgentState::channel)
                .chain(
                    room_agents
                        .iter()
                        .filter(|agent| {
                            agent.parent_agent_id.is_none() && agent.team.as_deref() == Some(spec)
                        })
                        .filter(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }))
                        .filter_map(AgentState::channel),
                )
                .collect::<HashSet<_>>();
            let board_held = |agent: &AgentState| {
                let (Some(lane), Some(checkout)) = (agent.channel(), agent.lane_path()) else {
                    return false;
                };
                let lane_rows = room_agents
                    .iter()
                    .filter(|row| crate::address::agent_in_worktree(row, &lane))
                    .cloned()
                    .collect::<Vec<_>>();
                crate::harness::resume::inspect_team_hold(&lane_rows, Path::new(checkout))
                    .is_some_and(|hold| {
                        hold.team == spec && matches!(hold.reason, TeamHoldReason::BoardStage(_))
                    })
            };
            let lanes = room_agents
                .iter()
                .filter(|agent| {
                    agent.parent_agent_id.is_none()
                        && agent.team.as_deref() == Some(spec)
                        && !agent.agent_id.is_empty()
                        && (!agent.agent_id.is_provisional() || board_held(agent))
                })
                .filter(|agent| {
                    agent
                        .lane_path()
                        .is_some_and(|path| checkout_exists(Path::new(path)))
                })
                .filter_map(AgentState::channel)
                .filter(|lane| !excluded_lanes.contains(lane))
                .collect::<BTreeSet<_>>();
            if let Some(first) = lanes.first() {
                let target = format!("{spec}#{first}");
                let retry = shell_command(["rimz", "teams", "resume", &target]);
                let lanes = lanes
                    .iter()
                    .map(|lane| format!("#{lane}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return anyhow::anyhow!(
                    "nothing to resume for `{address}`; cohorts are in {lanes} — retry with `{retry}`"
                );
            }
            let launch = shell_command(["rimz", "teams", spec]);
            anyhow::anyhow!("nothing to resume for `{address}`; launch with `{launch}`")
        }
        CohortResumeErr::MembersStillLive { labels } => {
            let focus = shell_command(["rimz", "teams", "focus", &address]);
            let stop = shell_command(["rimz", "teams", "stop", &address]);
            anyhow::anyhow!(
                "cannot resume `{address}`; still live: {}; reach them with `{focus}` or stop them with `{stop}` before resuming",
                labels.join(", ")
            )
        }
        CohortResumeErr::SessionNotResumable { label } => {
            let launch = shell_command(["rimz", "teams", spec]);
            anyhow::anyhow!(
                "cannot resume `{address}`; {label} has no resumable session; launch with `{launch}`"
            )
        }
        CohortResumeErr::LoginMismatch(mismatch) => mismatch.into(),
    }
}

fn resume_overrides<'a>(
    overrides: &'a LaunchOverrides,
    machine: &'a crate::config::MachineConfig,
    unavailable: &'a dyn Fn(&str, &str) -> Option<crate::agents::TierSkipReason>,
) -> Result<crate::harness::resume::ResumeOverrides<'a>> {
    Ok(crate::harness::resume::ResumeOverrides {
        team: None,
        permission_mode: overrides.mode,
        preset: resolve::launch_override_preset(overrides)?,
        agent: overrides.agent.as_deref(),
        tier: overrides.tier.map(|tier| (tier, &machine.tiers)),
        unavailable: Some(unavailable),
    })
}

fn validate_picker(layout: &LayoutSpec, team: Option<&str>) -> Result<()> {
    let cell = layout.agent_cells().next().filter(|cell| {
        team.is_none() && cell.launch.team.is_none() && cell.launch.role.is_none()
            && layout.columns.iter().map(|column| column.rows.len()).sum::<usize>() == 1
    }).context("--resume takes a kind or solo profile, not a team, team role, or multi-cell spec; use --continue to reopen a cohort")?;
    let adapter = crate::agents::find_definition(&cell.kind)
        .with_context(|| format!("unknown provider {}", cell.kind))?;
    if adapter.spec().launch.picker.is_some() {
        return Ok(());
    }
    let alternative = if adapter.spec().launch.resume.is_some() {
        "use --resume=<id> for an exact session, or --continue for a cohort"
    } else {
        "use --continue for a cohort, or launch without --resume"
    };
    bail!("{} has no session picker; {alternative}", cell.kind)
}

fn shell_command<'a>(argv: impl IntoIterator<Item = &'a str>) -> String {
    // Process arguments cannot carry the NUL byte, the only input shlex refuses.
    shlex::try_join(argv).expect("argv words carry no NUL byte")
}

#[cfg(test)]
mod tests;
