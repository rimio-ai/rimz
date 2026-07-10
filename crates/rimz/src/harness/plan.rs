//! Compile resolved layouts into launch identities and backend-neutral pane commands.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::{LaunchPlacement, RoleBinding};
use crate::harness::resume::{CohortCell, CohortResumePlan, CohortSeed};
use crate::harness::spec::{Cell, LayoutSpec};
use crate::ids::{AgentSessionId, EventId};
use crate::mux::{LayoutColumn, LayoutPanes, PaneCmd};
use crate::store::{AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};

/// Reduce a resolved layout to the agent cells used by cohort resume matching.
pub fn cohort_cells(layout: &LayoutSpec) -> Vec<CohortCell> {
    layout
        .agent_cells()
        .filter_map(|cell| match cell {
            Cell::Agent { kind, role, .. } => Some(CohortCell {
                kind: kind.clone(),
                role: role.clone(),
            }),
            Cell::Command { .. } => None,
        })
        .collect()
}

/// Build fresh launch requests for the cells a cohort resume cannot restore.
pub fn fresh_resume_launch_requests(
    layout: &LayoutSpec,
    plan: &CohortResumePlan,
    team: Option<&str>,
    team_roles: Option<&[RoleBinding]>,
    channel: Option<&str>,
) -> Result<Vec<AgentLaunchRequest>> {
    let mut requests =
        launch_identity_requests(layout, None, None, team, team_roles, channel, None)?;
    if team.is_none() {
        for request in &mut requests {
            if request.launch.launch_group.is_some()
                && let Some(group) = plan.launch_group.as_ref()
            {
                request.launch.launch_group = Some(group.clone());
            }
        }
    }
    Ok(requests
        .into_iter()
        .zip(plan.seeds.iter())
        .filter_map(|(request, seed)| matches!(seed, CohortSeed::Fresh).then_some(request))
        .collect())
}

/// Where a launch lands. The resolver derives it from the per-launch flags, the
/// `[agents] placement` default, and whether in-pane placement is feasible here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    SamePane,
    NewPane,
    NewTab,
}

/// Resolve launch placement. Explicit flags win; otherwise the config policy
/// decides, with `auto` running a single non-worktree cell in the current pane
/// and opening a new tab for a worktree or multi-cell layout. In-pane placement
/// needs a single cell and a launching pane; an explicit `--new-pane` that
/// cannot be honored fails fast, while a defaulted one falls back to a new tab.
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

pub fn apply_in_place_downgrade(placement: Placement, bg: bool, allow_in_place: bool) -> Placement {
    // In-place takes over the launching pane: it cannot honor --bg, and
    // create-on-miss must never replace the caller's pane. Downgrade to a split.
    if placement == Placement::SamePane && (bg || !allow_in_place) {
        Placement::NewPane
    } else {
        placement
    }
}

/// In-pane placement (same pane or new pane) needs a single cell and a
/// launching pane to take over or split; otherwise fall back to a new tab.
fn feasible_or_new(target: Placement, single_cell: bool, has_launching_pane: bool) -> Placement {
    if single_cell && has_launching_pane {
        target
    } else {
        Placement::NewTab
    }
}

/// Compile one launch request per agent cell in layout order.
pub fn launch_identity_requests(
    layout: &LayoutSpec,
    explicit_name: Option<&str>,
    generated_worktree_name: Option<&str>,
    team: Option<&str>,
    team_roles: Option<&[RoleBinding]>,
    channel: Option<&str>,
    prompt: Option<(&str, usize)>,
) -> Result<Vec<AgentLaunchRequest>> {
    let agent_cells: Vec<&Cell> = layout.agent_cells().collect();
    let agent_count = agent_cells.len();
    let inline_launch_group = (team.is_none() && agent_count >= 2).then(mint_launch_group);
    let mut requests = Vec::with_capacity(agent_cells.len());
    for (index, cell) in agent_cells.into_iter().enumerate() {
        let Cell::Agent {
            kind,
            profile,
            mode,
            role,
            model,
            effort,
            budget,
            ..
        } = cell
        else {
            continue;
        };
        let launch_ordinal = match team {
            Some(_) => role
                .as_deref()
                .and_then(|role| team_role_ordinal(team_roles, role)),
            None if inline_launch_group.is_some() => Some(index_to_launch_ordinal(index)),
            None => None,
        };
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
        requests.push(AgentLaunchRequest {
            kind: kind.clone(),
            agent_id: mint_launch_id(),
            name,
            launch: crate::agents::LaunchParams {
                profile: profile.clone(),
                mode: *mode,
                role: role.clone(),
                model: model.clone(),
                effort: effort.clone(),
                budget: budget.clone(),
                team: team.map(ToOwned::to_owned),
                launch_group: inline_launch_group.clone(),
                launch_ordinal,
                channel: channel.map(ToOwned::to_owned),
                kind_ordinal: None,
            },
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

fn team_role_ordinal(team_roles: Option<&[RoleBinding]>, role: &str) -> Option<u32> {
    let index = team_roles?
        .iter()
        .position(|binding| binding.role == role)?;
    Some(index_to_launch_ordinal(index))
}

fn index_to_launch_ordinal(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Compile backend-neutral pane commands for a resolved layout.
pub fn layout_panes_with_names(
    layout: &LayoutSpec,
    params: LayoutPaneParams<'_>,
    launch_identities: &[AgentLaunchIdentity],
) -> Result<LayoutPanes> {
    let rimz_bin = crate::proc::rimz_exe();
    let mut agent_index = 0usize;
    let mut launch_index = 0usize;
    let columns = layout
        .columns
        .iter()
        .map(|column| {
            let panes = column
                .rows
                .iter()
                .map(|cell| {
                    let cell_agent_index =
                        matches!(cell, Cell::Agent { .. }).then_some(agent_index);
                    let (resume_seed, launch) = if matches!(cell, Cell::Agent { .. }) {
                        let resume_seed = params
                            .resume_seeds
                            .map(|seeds| {
                                seeds.get(agent_index).with_context(|| {
                                    format!("resume plan missing seed for agent cell {agent_index}")
                                })
                            })
                            .transpose()?;
                        let resumes = matches!(resume_seed, Some(CohortSeed::Resume(_)));
                        let launch = if resumes {
                            None
                        } else {
                            let Some(launch) = launch_identities.get(launch_index) else {
                                bail!("launch plan missing identity for agent cell {agent_index}");
                            };
                            launch_index = launch_index.saturating_add(1);
                            Some(launch)
                        };
                        agent_index = agent_index.saturating_add(1);
                        (resume_seed, launch)
                    } else {
                        (None, None)
                    };
                    pane_cmd_with_name(
                        cell,
                        PaneCmdOptions {
                            rimz_bin: &rimz_bin,
                            cwd: params.cwd,
                            prompt: (params.prompt_agent_index == cell_agent_index)
                                .then_some(params.prompt)
                                .flatten(),
                            cleanup_worktree: params.cleanup_worktree,
                            in_place: params.in_place,
                            team: params.team,
                            channel: params.channel,
                            launch,
                            resume_seed,
                        },
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(LayoutColumn {
                panes,
                stacked: column.stacked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(LayoutPanes { columns })
}

#[derive(Clone, Copy)]
pub struct LayoutPaneParams<'a> {
    pub cwd: &'a Path,
    pub prompt: Option<&'a str>,
    pub prompt_agent_index: Option<usize>,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub team: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub resume_seeds: Option<&'a [CohortSeed]>,
}

pub struct PaneCmdOptions<'a> {
    pub rimz_bin: &'a Path,
    pub cwd: &'a Path,
    pub prompt: Option<&'a str>,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub team: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub launch: Option<&'a AgentLaunchIdentity>,
    pub resume_seed: Option<&'a CohortSeed>,
}

pub fn pane_cmd_with_name(cell: &Cell, options: PaneCmdOptions<'_>) -> Result<PaneCmd> {
    let argv = match cell {
        Cell::Command { argv } if argv.is_empty() => {
            vec![crate::harness::launch::user_shell_program()]
        }
        Cell::Command { argv } => argv.clone(),
        Cell::Agent {
            kind,
            args,
            mode,
            profile,
            role,
            model,
            effort,
            budget,
            ..
        } => {
            if let Some(CohortSeed::Resume(agent)) = options.resume_seed {
                return Ok(PaneCmd {
                    argv: crate::harness::resume::resume_command(
                        options.rimz_bin,
                        agent,
                        options.channel,
                    ),
                });
            }
            if let Some(launch) = options.launch {
                validate_agent_name(&launch.name)?;
            }
            crate::harness::launch::exec_argv(
                options.rimz_bin,
                &crate::harness::launch::ExecInvocation {
                    kind: kind.as_str(),
                    action: crate::harness::launch::ExecAction::Launch {
                        prompt: options.prompt,
                        extra_args: args,
                    },
                    run_id: None,
                    worktree_path: options.cleanup_worktree.then_some(options.cwd),
                    close_pane_on_exit: !options.cleanup_worktree && !options.in_place,
                    exit_on_run_completion: false,
                    identity: crate::harness::launch::ExecIdentity {
                        name: options.launch.map(|launch| launch.name.as_str()),
                        name_explicit: options.launch.is_some_and(|launch| launch.name_explicit),
                        launch_id: options.launch.map(|launch| launch.agent_id.as_str()),
                        profile: profile.as_deref(),
                        mode: *mode,
                        role: role.as_deref(),
                        team: options.team,
                        launch_group: options
                            .launch
                            .and_then(|launch| launch.launch.launch_group.as_deref()),
                        launch_ordinal: options
                            .launch
                            .and_then(|launch| launch.launch.launch_ordinal),
                        channel: options.channel,
                        model: model.as_deref(),
                        effort: effort.as_deref(),
                        budget: budget.as_deref(),
                    },
                },
            )
        }
    };
    Ok(PaneCmd { argv })
}

pub fn validate_agent_name(name: &str) -> Result<()> {
    if !valid_agent_name_candidate(name) {
        bail!("invalid agent name `{name}`; use ASCII letters, numbers, and `-`");
    }
    Ok(())
}

pub fn valid_agent_name_candidate(name: &str) -> bool {
    crate::harness::petname::valid_name(name)
        && !crate::harness::petname::collides_with_reserved_prefix(
            name,
            crate::agents::known_kinds(),
        )
}

#[cfg(test)]
mod tests;
