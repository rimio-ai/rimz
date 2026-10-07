//! CLI boundary for place-first lane recovery in a live room.
//!
//! Harness resume policy resolves and qualifies lanes. This module gathers
//! machine facts, preflights providers, applies mux/store actions, and renders
//! their user-facing result.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::agents::LocalSessionObservation;
use rimz::harness::ancestry::{LaunchFocus, resolve_caller};
use rimz::harness::resume::{
    LaneRestoreConfig, LaneResumeAction, LaneResumeError, LaneResumeRequest, LaneResumeSelector,
    LaneSummary, LaneWorktree, MaterializedRecovery, ResumeSkip, plan_lane_resume,
    resume_session_present,
};
use rimz::mux::{ResumeTab, SplitPaneOptions, SplitPlacement, SplitTarget};

use super::{GlobalFlags, RoomContext, RoomSizing};

pub(super) fn resume_lane(
    scope: Option<String>,
    from_pr: Option<rimz::forge::PrTarget>,
    bg: bool,
    fresh: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone())
            .context("resolving current workspace")?;
    let mux = rimz::room::require_live_mux(globals.mux, &workspace)?;
    let machine_config = crate::cli::machine_config();
    let room = RoomContext::from_resolved(
        &workspace,
        machine_config.clone(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let backend = room.backend();
    let store = crate::cli::open_store(&workspace)?;
    let projection = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let focus = LaunchFocus::resolve(bg, resolve_caller(&projection.agents).as_ref());
    let worktrees = local_worktrees(&workspace)?;
    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    let catalog = rimz::agents::LoginCatalog::room_view(&machine_config.accounts);
    let selector = lane_selector(
        scope,
        from_pr.map(|target| target.number),
        workspace.worktree_root == workspace.project_root,
    );
    let action = plan_lane_resume(
        LaneResumeRequest {
            selector,
            fresh,
            agents: &projection.agents,
            worktrees: &worktrees,
            current_root: &workspace.worktree_root,
            project_root: &workspace.project_root,
            workspace_id: &workspace.workspace_id,
            max: machine_config.resume.max,
            rimz_bin: &rimz::proc::rimz_exe(),
            runtime: store.runtime_paths(),
            logins: &logins,
            catalog: &catalog,
        },
        Path::is_dir,
        resume_session_present,
        rimz::store::runtime::agent_liveness,
        |path| discover_lane_sessions(path, &machine_config.accounts, &logins),
        || {
            let availability = rimz::harness::plan::LaunchAvailability::read(
                store.runtime_paths(),
                store.paths(),
                &machine_config,
                jiff::Timestamp::now(),
            );
            LaneRestoreConfig::load(&machine_config, &workspace.project_root, |kind, model| {
                availability.unavailable(kind, model)
            })
            .map_err(|error| LaneResumeError::RestoreConfig {
                message: error.to_string(),
            })
        },
    )?;

    let cwd = match &action {
        LaneResumeAction::SplitClosed { cwd, .. } | LaneResumeAction::RestoreClosed { cwd, .. } => {
            crate::cli::lsp_admission::admit(cwd, &machine_config)?;
            cwd.as_path()
        }
        LaneResumeAction::List { .. } | LaneResumeAction::Focus { .. } => {
            workspace.worktree_root.as_path()
        }
    };
    for kind in action.agent_kinds_needing_preflight() {
        rimz::harness::launch::preflight_agent_kind(&workspace.project_root, kind.as_str(), cwd)?;
        logins
            .login(kind, &machine_config.accounts)?
            .health(&catalog.native_ambient(kind, &rimz::agents::ambient_env()))?;
    }

    match action {
        LaneResumeAction::List { lanes } => render_lane_list(lanes),
        LaneResumeAction::Focus {
            lane_label,
            pane_id,
        } => {
            if !focus.takes_focus() {
                writeln!(
                    std::io::stdout().lock(),
                    "lane '{lane_label}' is already live"
                )?;
                return Ok(());
            }
            let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
            rimz::mux::focus_anchor::execute_action(
                backend,
                &runtime,
                &workspace.session_name,
                pane_id,
            )?;
            writeln!(
                std::io::stdout().lock(),
                "lane '{lane_label}' is already live — focused"
            )?;
            Ok(())
        }
        LaneResumeAction::SplitClosed {
            lane_label,
            cwd,
            channel,
            target_pane_id,
            commands,
            skipped,
            warnings,
            live_labels,
            ..
        } => {
            report_resume_skips(&skipped)?;
            report_posture_warnings(&warnings)?;
            let resumed = commands.len();
            let direction = rimz::mux::detect_terminal_size()
                .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
                .unwrap_or_default();
            for pane in commands {
                backend.split_pane(SplitPaneOptions {
                    target: SplitTarget::SessionPane {
                        session_name: workspace.session_name.clone(),
                        pane_id: target_pane_id.clone(),
                    },
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    command: Some(pane.argv),
                    env: rimz::room::pane_identity_env(&workspace, &cwd, channel.as_deref(), false),
                    title: pane.name,
                    close_on_exit: false,
                    placement: SplitPlacement::Directional(direction),
                    focus: focus.takes_focus(),
                })?;
            }
            for label in live_labels {
                writeln!(std::io::stdout().lock(), "skipped live {label}")?;
            }
            writeln!(
                std::io::stdout().lock(),
                "resumed {resumed} closed agent{} in '{lane_label}'",
                if resumed == 1 { "" } else { "s" }
            )?;
            Ok(())
        }
        LaneResumeAction::RestoreClosed {
            lane_label, plan, ..
        } => {
            report_discovery_skips(plan.discovery_skipped())?;
            report_resume_skips(plan.skipped())?;
            report_posture_warnings(plan.warnings())?;
            let MaterializedRecovery {
                resume: plan,
                tab_agents,
            } = plan.materialize(&store, &workspace.session_name)?;
            let count = plan.tabs.iter().map(ResumeTab::pane_count).sum::<usize>();
            for (index, (tab, agents)) in plan.tabs.into_iter().zip(tab_agents).enumerate() {
                if let Err(error) = room.open_resume_tab(tab, focus.takes_focus()) {
                    for launch in plan
                        .team_launches
                        .iter()
                        .filter(|launch| launch.tab >= index)
                    {
                        let _ = store.fail_agent_launch_batch(&launch.batch);
                    }
                    return Err(error.into());
                }
                if let Err(err) = rimz::harness::rebirth::settle_refilled_seats(
                    &store,
                    &workspace.session_name,
                    &agents.refilled,
                ) {
                    let _ = writeln!(
                        std::io::stderr(),
                        "rimz: replaced agents stay pending: {err}"
                    );
                }
                if let Some(launch) = plan.team_launches.iter().find(|launch| launch.tab == index) {
                    rimz::harness::assist_log::record_tier_fallbacks(launch.batch.identities());
                }
            }
            writeln!(
                std::io::stdout().lock(),
                "{} {count} agent{} in '{lane_label}'",
                if fresh { "relaunched" } else { "resumed" },
                if count == 1 { "" } else { "s" }
            )?;
            Ok(())
        }
    }
}

fn lane_selector(
    scope: Option<String>,
    from_pr: Option<u64>,
    at_project_root: bool,
) -> LaneResumeSelector {
    match (scope, from_pr, at_project_root) {
        (_, Some(number), _) => LaneResumeSelector::PullRequest(number),
        (Some(scope), None, _) => LaneResumeSelector::Scope(scope),
        (None, None, true) => LaneResumeSelector::List,
        (None, None, false) => LaneResumeSelector::Current,
    }
}

fn local_worktrees(workspace: &rimz::ResolvedWorkspace) -> Result<Vec<LaneWorktree>> {
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return Ok(Vec::new());
    }
    rimz::worktree::discover_owned(&workspace.project_root)?
        .into_iter()
        .map(|entry| {
            Ok(LaneWorktree {
                name: entry.marker.name,
                path: rimz::utils::path::normalize_path_lexical(&entry.path),
                branch: entry.branch,
                from_pr: entry.marker.from_pr,
            })
        })
        .collect()
}

/// Sessions under `path` in the room's own account homes, so every discovered
/// session is one the room may resume.
fn discover_lane_sessions(
    path: &Path,
    accounts: &rimz::config::AccountsConfig,
    logins: &rimz::agents::RoomAccounts,
) -> Vec<LocalSessionObservation> {
    let ambient = rimz::agents::ambient_env();
    rimz::agents::all_definitions()
        .filter(|adapter| adapter.spec().capabilities.local_session_discovery)
        .flat_map(|adapter| {
            let kind = rimz::ids::AgentKind::new_unchecked(adapter.spec().kind);
            match logins.login(&kind, accounts) {
                Ok(login) => login.local_sessions(adapter, &[path], &ambient),
                Err(_) => Vec::new(),
            }
        })
        .filter(|observation| {
            std::fs::metadata(&observation.transcript_path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        })
        .collect()
}

fn report_resume_skips(skips: &[ResumeSkip]) -> Result<()> {
    let mut out = std::io::stderr().lock();
    for skip in skips {
        writeln!(
            out,
            "rimz: not resumed: {} ({})",
            skip.label,
            skip.reason.label()
        )?;
    }
    Ok(())
}

fn report_posture_warnings(warnings: &[String]) -> Result<()> {
    let mut out = std::io::stderr().lock();
    for warning in warnings {
        writeln!(out, "rimz: {warning}")?;
    }
    Ok(())
}

fn report_discovery_skips(skips: &[LocalSessionObservation]) -> Result<()> {
    let mut out = std::io::stderr().lock();
    for observation in skips {
        writeln!(
            out,
            "rimz: not resumed: {} {} (older run)",
            observation.kind, observation.session_id
        )?;
    }
    Ok(())
}

fn render_lane_list(lanes: Vec<LaneSummary>) -> Result<()> {
    let mut table = crate::cli::render::Table::new(["LANE", "MEMBERS", "LIVE", "CLOSED", "AGE"])
        .right(&[1, 2, 3, 4]);
    let now = Timestamp::now();
    for lane in lanes {
        table.row([
            crate::cli::render::cell(lane.label),
            crate::cli::render::cell(lane.members.to_string()),
            crate::cli::render::cell(lane.live.to_string()),
            crate::cli::render::cell((lane.members - lane.live).to_string()),
            crate::cli::render::cell(crate::cli::render::rel_age(lane.freshest, now)),
        ]);
    }
    table.render(&mut crate::cli::render::out())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_translation_preserves_cli_precedence() {
        assert_eq!(
            lane_selector(Some("docs".to_owned()), Some(42), true),
            LaneResumeSelector::PullRequest(42)
        );
        assert_eq!(
            lane_selector(Some("docs".to_owned()), None, true),
            LaneResumeSelector::Scope("docs".to_owned())
        );
        assert_eq!(lane_selector(None, None, true), LaneResumeSelector::List);
        assert_eq!(
            lane_selector(None, None, false),
            LaneResumeSelector::Current
        );
    }
}
