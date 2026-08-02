//! Shared cohort launch orchestration for CLI doorways.

use std::borrow::Cow;
use std::path::Path;

use anyhow::Result;

use rimz::agents::{AgentState, LaunchPreset};
use rimz::config::effective::{LaunchAgents, ProfileScope};
use rimz::harness::budget::BudgetSpec;
use rimz::harness::plan::{LaunchFinalizeOptions, LaunchFinalizeWarning, ResolvedLaunch};
use rimz::harness::run::PermissionMode;
use rimz::harness::spec::LayoutSpec;

#[expect(
    clippy::too_many_arguments,
    reason = "single typed boundary for shared launch preparation"
)]
pub(super) fn prepare_cohort(
    machine_config: &rimz::config::MachineConfig,
    project_root: &Path,
    agents: &[AgentState],
    scope: ProfileScope,
    lane: Option<&str>,
    spec: &str,
    agent_override: Option<&str>,
    overrides: impl FnOnce(&LaunchAgents, &LayoutSpec) -> Result<CohortOverrides>,
) -> Result<(ResolvedLaunch, Option<String>, Vec<LaunchFinalizeWarning>)> {
    let effective = rimz::config::effective::load(
        &machine_config.agents,
        &machine_config.subagents.profiles,
        project_root,
        &rimz::store::paths::config_home(),
    )?;
    let mut qualified_spec = Cow::Borrowed(spec);
    let mut inferred_lane = None;
    if let Some(channel) = lane
        && let Some(team) = rimz::harness::target::channel_team(agents, channel)
    {
        qualified_spec = rimz::harness::spec::qualify_spec_in_channel(
            spec,
            channel,
            team,
            &effective.teams,
            effective.profiles_for(scope),
            &machine_config.agents.commands,
        )?;
        if matches!(qualified_spec, Cow::Owned(_)) {
            inferred_lane = Some(channel.to_owned());
        }
    }
    let mut resolved = rimz::harness::plan::resolve_launch(
        &effective,
        scope,
        &machine_config.agents.commands,
        Some(&qualified_spec),
        agent_override,
    )?;
    let (permission_mode, preset, passthrough, budget, max_turns) =
        overrides(&effective, &resolved.layout)?;
    let warnings = rimz::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode,
            preset: &preset,
            passthrough: &passthrough,
            budget,
            max_turns,
        },
    )?;
    Ok((resolved, inferred_lane, warnings))
}

type CohortOverrides = (
    Option<PermissionMode>,
    LaunchPreset,
    Vec<String>,
    Option<BudgetSpec>,
    Option<u32>,
);

pub(super) fn materialize(
    resolved: &ResolvedLaunch,
    inferred_lane: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    worktree: Option<&str>,
    from_pr: Option<&rimz::forge::PrTarget>,
    explicit_channel: Option<&str>,
) -> Result<(rimz::worktree::LaunchCheckout, Option<String>)> {
    let launch = rimz::worktree::resolve_launch_checkout(
        workspace,
        &machine_config.agents.worktree,
        worktree,
        from_pr,
    )?;
    if let Some(team) = resolved
        .team_name
        .as_deref()
        .and_then(|name| resolved.teams.0.get(name))
    {
        rimz::worktree::exclude_team_scratch(&launch.cwd, &team.scratch_files);
    }
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        resolved.team_name.as_deref(),
        explicit_channel.or(inferred_lane),
    );
    Ok((launch, room_channel))
}
