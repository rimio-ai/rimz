//! Team-declared standing subscriptions pinned to registered member sessions.

use std::collections::BTreeSet;

use super::arm::{
    self, DeliveryName, DeliveryPrompt, DeliveryProvenance, DeliverySpec, DeliveryTrigger,
    SubscriptionLifetime, TaskName,
};
use crate::agents::AgentState;
use crate::config::{TaskTarget, Team};
use crate::ids::TeamInstanceId;
use crate::workspace::ResolvedWorkspace;

#[derive(Debug, thiserror::Error)]
pub enum TeamBindingErr {
    #[error("team signals require a registered root team member")]
    InvalidMember,
    #[error(transparent)]
    Instance(#[from] crate::ids::InvalidTeamInstanceId),
    #[error("team `{team}` role `{role}` signal binding {index}: {source}")]
    Binding {
        team: String,
        role: String,
        index: usize,
        #[source]
        source: TeamBindingFailure,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TeamBindingFailure {
    #[error(transparent)]
    Schedule(#[from] super::ScheduleErr),
    #[error(transparent)]
    Scope(#[from] arm::DeliveryScopeFailure),
    #[error(transparent)]
    Arm(#[from] arm::ArmFailure),
    #[error(
        "CI on the root checkout is not watched: RimZ polls the forge for worktree branches. Set match = {{ branch = \"<name>\" }} or match = {{ path = \"<worktree-path>\" }}, launch with -w <worktree>, or watch it with: rimz wake -- gh run watch --exit-status"
    )]
    RootCheckout,
}

pub fn validate_launch(
    name: &str,
    team: &Team,
    workspace: &ResolvedWorkspace,
    worktree: Option<&str>,
) -> Result<(), TeamBindingErr> {
    if worktree.is_some() || workspace.worktree_root != workspace.project_root {
        return Ok(());
    }
    for (role, index, binding) in team.roles.iter().flat_map(|role| {
        role.signals
            .iter()
            .enumerate()
            .map(move |(index, binding)| (&role.role, index, binding))
    }) {
        let result = (|| -> Result<(), TeamBindingFailure> {
            let selector =
                super::parse_signal_selector(name, &binding.signal, Some(&binding.matches))?;
            if matches!(selector.family(), "ci" | "pr")
                && !binding.matches.contains_key("path")
                && !binding.matches.contains_key("branch")
            {
                return Err(TeamBindingFailure::RootCheckout);
            }
            Ok(())
        })();
        result.map_err(|source| TeamBindingErr::Binding {
            team: name.to_owned(),
            role: role.clone(),
            index: index + 1,
            source,
        })?;
    }
    Ok(())
}

pub fn arm_member(
    workspace: &ResolvedWorkspace,
    agents: &[AgentState],
    member: &AgentState,
    team: &Team,
) -> Result<usize, TeamBindingErr> {
    if member.agent_id.is_provisional() || member.parent_agent_id.is_some() {
        return Err(TeamBindingErr::InvalidMember);
    }
    let name = member
        .team
        .as_deref()
        .ok_or(TeamBindingErr::InvalidMember)?;
    let Some(role) = member.role.as_deref() else {
        return Ok(0);
    };
    let Some(declared_role) = team.roles.iter().find(|binding| binding.role == role) else {
        return Ok(0);
    };
    let channel = member.channel().unwrap_or_else(|| "external".to_owned());
    let instance: TeamInstanceId = format!("{name}#{channel}").parse()?;
    let peers: Vec<_> = agents.iter().collect();
    let target = TaskTarget {
        kind: member.kind.clone(),
        session: member.agent_id.clone(),
        handle: crate::address::agent_handle(member, &peers, true),
    };
    let mut names = BTreeSet::new();
    let mut specs = Vec::new();
    for (index, binding) in declared_role.signals.iter().enumerate() {
        let result = (|| -> Result<DeliverySpec, TeamBindingFailure> {
            let mut ordinal = 1;
            let task_name = loop {
                let candidate = member_task_name(name, &channel, role, &binding.signal, ordinal)?;
                if names.insert(candidate.to_string()) {
                    break candidate;
                }
                ordinal += 1;
            };
            let selector = super::parse_signal_selector(
                &task_name.to_string(),
                &binding.signal,
                Some(&binding.matches),
            )?;
            let mut matches = binding.matches.clone();
            arm::default_signal_matches(workspace, agents, member, &selector, &mut matches)?;
            arm::validate_self_signal(&selector, &matches, &target)?;
            Ok(DeliverySpec {
                name: DeliveryName::Named(task_name),
                target: target.clone(),
                trigger: DeliveryTrigger::Signal {
                    selector,
                    matches,
                    lifetime: SubscriptionLifetime::Standing,
                },
                prompt: binding
                    .prompt
                    .clone()
                    .map_or(DeliveryPrompt::None, DeliveryPrompt::Inline),
                provenance: DeliveryProvenance::Team(instance.clone()),
                check: None,
                deadline: None,
                max_strikes: None,
                surplus: None,
            })
        })();
        specs.push((
            index + 1,
            result.map_err(|source| TeamBindingErr::Binding {
                team: name.to_owned(),
                role: role.to_owned(),
                index: index + 1,
                source,
            })?,
        ));
    }
    let count = specs.len();
    for (index, spec) in specs {
        arm::arm_delivery(workspace, spec).map_err(|source| TeamBindingErr::Binding {
            team: name.to_owned(),
            role: role.to_owned(),
            index,
            source: source.into(),
        })?;
    }
    Ok(count)
}

fn member_task_name(
    team: &str,
    channel: &str,
    role: &str,
    signal: &str,
    ordinal: usize,
) -> Result<TaskName, super::ScheduleErr> {
    let mut name: String = format!("team-{team}-{channel}-{role}-{signal}")
        .chars()
        .map(|ch| match ch.to_ascii_lowercase() {
            ch @ ('a'..='z' | '0'..='9' | '_' | '-') => ch,
            _ => '-',
        })
        .collect();
    if ordinal > 1 {
        name.push_str(&format!("-{ordinal}"));
    }
    name.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_signal_names_are_deterministic_and_ordinal_on_repeats() {
        assert_eq!(
            member_task_name("Forge", "feat/x", "Coder", "ci.failed", 1)
                .unwrap()
                .to_string(),
            "team-forge-feat-x-coder-ci-failed"
        );
        assert_eq!(
            member_task_name("forge", "feat-x", "coder", "ci.failed", 2)
                .unwrap()
                .to_string(),
            "team-forge-feat-x-coder-ci-failed-2"
        );
    }

    #[test]
    fn team_signal_launch_scope_requires_explicit_root_checkout_matches() {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let mut team: Team = toml::from_str(
            r#"
            [[roles]]
            role = "coder"
            profile = "codex"
            signals = [{ signal = "ci.failed" }]
            "#,
        )
        .unwrap();
        assert!(validate_launch("forge", &team, &workspace, None).is_err());
        assert!(validate_launch("forge", &team, &workspace, Some("feature")).is_ok());
        for key in ["branch", "path"] {
            team.roles[0].signals[0]
                .matches
                .insert(key.to_owned(), "feature".to_owned());
            assert!(validate_launch("forge", &team, &workspace, None).is_ok());
            team.roles[0].signals[0].matches.clear();
        }
        team.roles[0].signals[0].signal = "pr.merged".to_owned();
        assert!(validate_launch("forge", &team, &workspace, None).is_err());
        workspace.worktree_root = root.path().join("feature");
        assert!(validate_launch("forge", &team, &workspace, None).is_ok());
    }
}
