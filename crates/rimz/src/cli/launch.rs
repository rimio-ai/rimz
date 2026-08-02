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
    overrides: impl FnOnce(&LaunchAgents, &LayoutSpec, &str) -> Result<CohortOverrides>,
) -> Result<(
    ResolvedLaunch,
    Option<String>,
    Vec<LaunchFinalizeWarning>,
    String,
)> {
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
        overrides(&effective, &resolved.layout, &qualified_spec)?;
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
    Ok((
        resolved,
        inferred_lane,
        warnings,
        qualified_spec.into_owned(),
    ))
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
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd after checkout materialization.
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        resolved.team_name.as_deref(),
        explicit_channel.or(inferred_lane),
    );
    Ok((launch, room_channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::config::{Profile, RoleBinding, Team};

    #[test]
    fn lane_qualification_reaches_validation_and_the_caller() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut machine = rimz::config::MachineConfig::default();
        machine.agents.profiles.0.insert(
            "reviewer-profile".to_owned(),
            Profile {
                agent: "claude".to_owned(),
                description: None,
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                args: None,
            },
        );
        machine.agents.teams.0.insert(
            "forge".to_owned(),
            Team {
                roles: vec![RoleBinding {
                    role: "reviewer".to_owned(),
                    profile: "reviewer-profile".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                }],
                ..Team::default()
            },
        );
        let mut agent =
            rimz::testkit::agent_state("claude", "planner", jiff::Timestamp::UNIX_EPOCH);
        agent.channel = Some("forge".to_owned());
        agent.team = Some("forge".to_owned());
        let mut validated_spec = None;

        let (_, inferred_lane, _, qualified_spec) = prepare_cohort(
            &machine,
            dir.path(),
            &[agent],
            ProfileScope::Agents,
            Some("forge"),
            "reviewer",
            None,
            |_, _, spec| {
                validated_spec = Some(spec.to_owned());
                Ok((None, LaunchPreset::default(), Vec::new(), None, None))
            },
        )
        .expect("prepare qualified lane launch");

        assert_eq!(validated_spec.as_deref(), Some("forge.reviewer"));
        assert_eq!(qualified_spec, "forge.reviewer");
        assert_eq!(inferred_lane.as_deref(), Some("forge"));
    }
}
