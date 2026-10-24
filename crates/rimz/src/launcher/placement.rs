//! Shared mux placement for fresh, resumed, and forked agent launches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::LaunchPlacement;
use crate::harness::ancestry::LaunchFocus;
use crate::ids::{AgentKind, MuxName, PaneId};
use crate::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, SplitPaneOptions, SplitPlacement,
    SplitTarget, TabOptions, own_pane_id,
};
use crate::store::writer::AgentLaunchBatch;
use anyhow::{Context, Result, bail};

/// Where a launch lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    SamePane,
    NewPane,
    NewTab,
}

/// Resolve launch placement from explicit flags, config policy, and current
/// pane feasibility.
pub fn resolve_placement(
    new_tab: bool,
    new_pane: bool,
    policy: LaunchPlacement,
    is_worktree: bool,
    single_cell: bool,
    has_launching_pane: bool,
) -> Result<Placement> {
    if new_tab {
        return Ok(Placement::NewTab);
    }
    if new_pane {
        if !single_cell {
            bail!(
                "--new-pane opens a single agent cell; a multi-cell layout opens a new tab — drop --new-pane or pass --new-tab"
            );
        }
        if !has_launching_pane {
            bail!(
                "--new-pane splits the current pane, so run it from inside the room; drop it to open a new tab"
            );
        }
        return Ok(Placement::NewPane);
    }
    Ok(match policy {
        LaunchPlacement::Tab => Placement::NewTab,
        LaunchPlacement::Pane if is_worktree => Placement::NewTab,
        LaunchPlacement::Pane => {
            feasible_or_new(Placement::NewPane, single_cell, has_launching_pane)
        }
        LaunchPlacement::Auto if is_worktree => Placement::NewTab,
        LaunchPlacement::Auto => {
            feasible_or_new(Placement::SamePane, single_cell, has_launching_pane)
        }
    })
}

pub fn resolve_fork_placement(
    new_tab: bool,
    new_pane: bool,
    focus: LaunchFocus,
    has_launching_pane: bool,
) -> Result<Placement> {
    let placement = resolve_placement(
        new_tab,
        new_pane,
        LaunchPlacement::Auto,
        false,
        true,
        has_launching_pane,
    )?;
    Ok(apply_in_place_downgrade(placement, focus, true))
}

pub fn resolve_tab_anchor(
    placement: Placement,
    has_team: bool,
    caller_pane: Option<PaneId>,
    caller_room_root: Option<&Path>,
    project_root: &Path,
) -> Option<PaneId> {
    if placement != Placement::NewTab || has_team || caller_room_root != Some(project_root) {
        return None;
    }
    caller_pane
}

pub fn apply_in_place_downgrade(
    placement: Placement,
    focus: LaunchFocus,
    allow_in_place: bool,
) -> Placement {
    if placement == Placement::SamePane && (!focus.takes_focus() || !allow_in_place) {
        Placement::NewPane
    } else {
        placement
    }
}

fn feasible_or_new(target: Placement, single_cell: bool, has_launching_pane: bool) -> Placement {
    if single_cell && has_launching_pane {
        target
    } else {
        Placement::NewTab
    }
}

/// Whether a resolved resume lands outside the pane the command runs in. A
/// launch pane already sitting in the cohort's working directory is the origin
/// pane — an agent that dropped to a shell there resumes in place instead of
/// opening a lane tab.
pub fn resume_outside_launch_dir(
    channel: Option<&str>,
    cwd: &Path,
    lane_path: &Path,
    project_root: &Path,
    worktree_root: &Path,
    launch_dir: Option<&Path>,
) -> bool {
    let target = crate::utils::path::normalize_path_lexical(cwd);
    if launch_dir.is_some_and(|dir| crate::utils::path::normalize_path_lexical(dir) == target) {
        return false;
    }
    channel.is_some() || (lane_path != project_root && lane_path != worktree_root)
}

pub struct PlacementRequest {
    pub placement: Placement,
    pub mux: MuxName,
    pub tab_anchor: Option<PaneId>,
    pub cwd: PathBuf,
    pub title: String,
    pub panes: LayoutPanes,
    pub sidebar: SidebarPaneOptions,
    pub identity_env: BTreeMap<String, String>,
    pub focus: LaunchFocus,
    pub errors: PlacementErrors,
}

#[derive(Clone, Copy)]
pub struct PlacementErrors {
    pub new_tab: &'static str,
    pub new_pane: &'static str,
    pub same_pane: &'static str,
}

enum PreparedPlacement {
    NewTab(Box<TabOptions>),
    NewPane(SplitPaneOptions),
    SamePane {
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
}

pub fn execute(
    backend: &dyn MuxBackend,
    store: &crate::Store,
    batch: &AgentLaunchBatch,
    request: PlacementRequest,
    mut on_launched: impl FnMut(),
) -> Result<()> {
    if request.placement == Placement::SamePane
        && let Some(anchor) = own_pane_id(request.mux)
        && let Some(pane_name) = single_pane(&request.panes)?.name.clone()
        && let Err(err) = backend.rename_tab(
            &request.sidebar.session_name,
            &anchor,
            &request.title,
            crate::mux::tab_name::TabNameIntent::Claim { pane_name },
        )
    {
        tracing::warn!(
            error = &err as &dyn std::error::Error,
            "could not name the agent's tab",
        );
    }
    let errors = request.errors;
    let context = match request.placement {
        Placement::NewTab => errors.new_tab,
        Placement::NewPane => errors.new_pane,
        Placement::SamePane => errors.same_pane,
    };
    let in_place = request.placement == Placement::SamePane;
    let result = prepare(request).and_then(|prepared| match prepared {
        PreparedPlacement::NewTab(options) => backend.open_tab(&options).map_err(Into::into),
        PreparedPlacement::NewPane(options) => {
            backend.split_pane(options).map(|_| ()).map_err(Into::into)
        }
        PreparedPlacement::SamePane { argv, env, cwd } => {
            // A successful exec never returns, so the launch counts as placed here.
            on_launched();
            Err(exec_wrapper_in_place(&argv, env, &cwd))
        }
    });
    if let Err(err) = result {
        let _ = store.fail_agent_launch_batch(batch);
        return Err(err).context(context);
    }
    if !in_place {
        on_launched();
    }
    Ok(())
}

fn prepare(request: PlacementRequest) -> Result<PreparedPlacement> {
    let detected_size = crate::mux::detect_terminal_size();
    let target_pane_id = (request.placement == Placement::NewPane)
        .then(|| own_pane_id(request.mux))
        .flatten();
    prepare_resolved(request, detected_size, target_pane_id)
}

fn prepare_resolved(
    request: PlacementRequest,
    detected_size: Option<(u16, u16)>,
    target_pane_id: Option<PaneId>,
) -> Result<PreparedPlacement> {
    let direction = detected_size
        .map(|(cols, rows)| crate::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let PlacementRequest {
        placement,
        tab_anchor,
        cwd,
        title,
        panes,
        sidebar,
        identity_env,
        focus,
        ..
    } = request;
    Ok(match placement {
        Placement::NewTab => PreparedPlacement::NewTab(Box::new(TabOptions {
            env: identity_env,
            title,
            panes,
            focus: focus.takes_focus(),
            dock_sidebar: true,
            after: tab_anchor,
            sidebar,
        })),
        Placement::NewPane => {
            let pane = single_pane(&panes)?;
            PreparedPlacement::NewPane(SplitPaneOptions {
                target: target_pane_id.map_or(SplitTarget::Ambient, SplitTarget::Pane),
                env: identity_env,
                placement: SplitPlacement::Directional(direction),
                focus: focus.takes_focus(),
                ..SplitPaneOptions::from_command(pane, &cwd)
            })
        }
        Placement::SamePane => PreparedPlacement::SamePane {
            argv: single_pane(&panes)?.argv.clone(),
            env: identity_env,
            cwd,
        },
    })
}

/// The one pane command of an in-pane launch (the resolver guarantees a single
/// cell before this is reached).
fn single_pane(panes: &LayoutPanes) -> Result<&PaneCmd> {
    panes
        .columns
        .first()
        .and_then(|column| column.panes.first())
        .context("in-pane launch produced no pane command")
}

#[cfg(unix)]
fn exec_wrapper_in_place(
    argv: &[String],
    env: BTreeMap<String, String>,
    cwd: &Path,
) -> anyhow::Error {
    use std::os::unix::process::CommandExt;

    let Some((program, rest)) = argv.split_first() else {
        return anyhow::anyhow!("in-place launch produced no command");
    };
    let mut command = Command::new(program);
    command.args(rest).envs(&env).current_dir(cwd);
    command.exec().into()
}

#[cfg(not(unix))]
fn exec_wrapper_in_place(
    _argv: &[String],
    _env: BTreeMap<String, String>,
    _cwd: &Path,
) -> anyhow::Error {
    anyhow::anyhow!("in-place launch is only supported on Unix")
}

mod zone;
pub use zone::{
    PaneBindWait, SubagentZoneOpen, launch_has_bound_pane, lock_subagent_zone,
    split_into_subagent_zone, wait_for_subagent_pane_bind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPlacement {
    Split,
    SubagentZone,
    Tab,
}

/// A supervised `-p` run normally splits the current tab so focus stays with
/// the caller. Subagents use their dedicated zone; forced and out-of-pane
/// launches open a new tab.
pub fn run_placement(force_new_tab: bool, has_ambient_pane: bool, subagent: bool) -> RunPlacement {
    if force_new_tab || !has_ambient_pane {
        RunPlacement::Tab
    } else if subagent {
        RunPlacement::SubagentZone
    } else {
        RunPlacement::Split
    }
}

pub fn run_tab_title(
    store: &crate::Store,
    channel: Option<&str>,
    subagent: bool,
    kind: &AgentKind,
) -> String {
    if subagent {
        zone::subagent_companion_title(store, channel)
    } else {
        format!("run {kind}")
    }
}

#[cfg(test)]
mod tests;
