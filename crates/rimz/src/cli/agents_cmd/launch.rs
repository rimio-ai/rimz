//! CLI boundary for layout launches.

use super::*;
use crate::cli::launch_interaction::{CliInteraction, LayoutReceipt};
use crate::cli::{machine_config, render};
use rimz::harness::ancestry;
pub(super) use rimz::launcher::ResumeEntrance;

pub(super) fn validate_resume_inputs(
    args: &AgentLaunchArgs,
    entrance: ResumeEntrance,
) -> Result<()> {
    rimz::launcher::validate_resume_inputs(&typed_overrides(&args.overrides)?, entrance)
}

pub(super) fn launch_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
) -> Result<()> {
    if args.launch.cohort.continue_cohort {
        validate_resume_inputs(&args.launch, ResumeEntrance::Flag)?;
    }
    args.launch.cohort.validate_fresh_worktree()?;
    let machine_config = machine_config();
    if args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some() {
        crate::cli::require_worktree_config(&machine_config)?;
    }
    crate::cli::check_launch_room(globals)?;
    let mut ctx = Ctx::open(globals)?;
    let snapshot = ctx.cached_snapshot()?;
    ctx.workspace = crate::cli::anchor_caller_workspace(
        ctx.workspace,
        ancestry::resolve_calling_agent(&snapshot.agents).ok(),
        globals,
    )?;
    let cwd = crate::cli::resolve_launch_cwd(args.launch.cwd.as_deref(), ctx.store.paths())?;

    let mut launch = args.launch;
    if matches!(launch.resume, Some(Some(_))) {
        if let Some(name) = launch
            .cohort
            .worktree
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            rimz::worktree::parse_requested_name(name)?;
        }
        crate::cli::report_unknown_config_keys(&machine_config)?;
        let effective =
            rimz::config::effective::load(&machine_config, &ctx.workspace.project_root)?;
        let lane = launch.cohort.channel.as_deref().or_else(|| ctx.channel());
        let resolved_snapshot = if launch.spec.is_some() && lane.is_some() {
            Some(ctx.cached_snapshot()?)
        } else {
            None
        };
        let print_warning = |warning: &rimz::harness::plan::LaunchFinalizeWarning| {
            if matches!(
                warning,
                rimz::harness::plan::LaunchFinalizeWarning::TierMoved { .. }
            ) {
                return Ok(());
            }
            writeln!(render::err(), "{warning}")
        };
        let finalized = resolve_finalized_layout(
            resolved_snapshot.as_ref(),
            &machine_config,
            &effective,
            rimz::config::effective::ProfileScope::Agents,
            launch.spec.as_deref(),
            launch.prompt.as_deref(),
            &LaunchOverrideArgs::default(),
            launch.cohort.budget,
            launch.max_turns,
            lane,
            launch.name.is_some(),
            Some((ctx.store.runtime_paths(), ctx.store.paths())),
        )
        .inspect_err(|err| {
            if let Some(err) = err.downcast_ref::<rimz::harness::plan::LaunchFinalizeError>() {
                for warning in err.warnings() {
                    let _ = print_warning(warning);
                }
            }
        })?;
        for warning in &finalized.warnings {
            print_warning(warning)?;
        }
        return super::resume::resume_provider(
            &launch,
            &finalized.resolved.layout,
            finalized.resolved.team_name.as_deref(),
            &ctx,
            globals,
        )
        .map(|_| ());
    }
    let selection = if launch.cohort.continue_cohort {
        rimz::launcher::Selection::Cohort
    } else if launch.resume.is_some() {
        rimz::launcher::Selection::Picker
    } else {
        rimz::launcher::Selection::Fresh
    };
    let overrides = typed_overrides(&launch.overrides)?;
    let request = rimz::launcher::LayoutRequest {
        context: rimz::launcher::LaunchContext {
            workspace: ctx.workspace.clone(),
            store: ctx.store.clone(),
            machine: machine_config,
            mux: globals.mux,
            root: globals.root.clone(),
            channel: ctx.channel().map(str::to_owned),
        },
        launch: rimz::launcher::LayoutLaunch {
            spec: launch.spec.take(),
            prompt: launch.prompt.take(),
            name: launch.name.take(),
            cwd: launch.cwd.take(),
            cohort: rimz::launcher::CohortRequest {
                worktree: launch.cohort.worktree,
                from_pr: launch.cohort.from_pr,
                channel: launch.cohort.channel,
                description: launch.cohort.description,
                budget: launch.cohort.budget,
                fresh: launch.cohort.fresh,
                bg: launch.cohort.bg,
                new_tab: launch.cohort.new_tab,
            },
            selection,
            new_pane: launch.new_pane,
            overrides,
            max_turns: launch.max_turns,
            teams_scene: launch.teams_scene,
            report_to: report_to(launch.cohort.detach),
        },
        cwd,
        allow_in_place,
        entry: rimz::launcher::RoomEntry::Live,
        starter: rimz::launcher::Starter::Human,
        resident_loop: None,
        workflow: None,
        leader_run: None,
    };
    let mut interaction = CliInteraction;
    let outcome = match rimz::launcher::prepare_layout(request, &mut interaction)? {
        rimz::launcher::Preparation::Prepared(prepared) => {
            rimz::launcher::execute(*prepared, &mut interaction)?
        }
        rimz::launcher::Preparation::Handled(outcome) => outcome,
    };
    if let rimz::launcher::LaunchOutcome::Opened(launched) = outcome {
        launched.write_receipt(&mut render::out(), &ctx.store, &ctx.workspace.project_root)?;
    }
    Ok(())
}
