//! Launch identity enrichment from environment and process state.

use super::*;

pub(super) fn env_run_id() -> Option<rimz::RunId> {
    let raw = std::env::var(rimz::harness::run::ENV_RUN_ID).ok()?;
    match raw.parse() {
        Ok(run_id) => Some(run_id),
        Err(err) => {
            warn!(
                run_id = %raw,
                error = %err,
                "lifecycle: ignoring invalid supervised run id",
            );
            None
        }
    }
}

type IdentityValidator = fn(String, &str, &str) -> Option<String>;

pub(super) fn agent_identity_env(
    observation: &AgentLifecycleObservation,
    var: &str,
    validate: IdentityValidator,
) -> Option<String> {
    if let Ok(raw) = std::env::var(var) {
        if raw.trim().is_empty() {
            let _ = validate(raw, "env", var);
            return None;
        }
        if let Some(value) = validate(raw, "env", var) {
            return Some(value);
        }
    }
    let raw = rimz::proc::env_var(observation.agent_pid?, var)?;
    validate(raw, "process", var)
}

pub(in crate::cli::hooks) fn fill_root_launch_identity(
    observation: &mut AgentLifecycleObservation,
    configured_identity: (Option<String>, Option<String>),
    mut identity_env: impl FnMut(&AgentLifecycleObservation, &'static str) -> Option<String>,
) {
    if observation.parent_agent_id.is_some() {
        return;
    }
    if observation.launch.role.is_none() {
        observation.launch.role = identity_env(observation, rimz::harness::run::ENV_AGENT_ROLE);
    }
    if observation.launch.team.is_none() {
        observation.launch.team = identity_env(observation, rimz::harness::run::ENV_TEAM);
    }
    if observation.launch.launch_group.is_none() {
        observation.launch.launch_group =
            identity_env(observation, rimz::harness::run::ENV_LAUNCH_GROUP);
    }
    if observation.launch.launch_ordinal.is_none() {
        observation.launch.launch_ordinal =
            identity_env(observation, rimz::harness::run::ENV_LAUNCH_ORDINAL)
                .and_then(|raw| raw.parse::<u32>().ok());
    }
    if observation.launch.channel.is_none() {
        observation.launch.channel = identity_env(observation, rimz::harness::run::ENV_CHANNEL);
    }
    if observation.launch.profile.is_none() {
        observation.launch.profile =
            identity_env(observation, rimz::harness::run::ENV_AGENT_PROFILE);
    }
    if observation.launch.model.is_none() {
        observation.launch.model = identity_env(observation, rimz::harness::run::ENV_AGENT_MODEL)
            .or(configured_identity.0);
    }
    if observation.launch.effort.is_none() {
        observation.launch.effort = identity_env(observation, rimz::harness::run::ENV_AGENT_EFFORT)
            .or(configured_identity.1);
    }
    if observation.launch.budget.is_none() {
        observation.launch.budget = identity_env(observation, rimz::harness::run::ENV_AGENT_BUDGET);
    }
}

pub(super) fn validate_agent_name_env(raw: String, source: &str, _var: &str) -> Option<String> {
    if rimz::harness::petname::valid_agent_name(&raw) {
        Some(raw)
    } else {
        warn!(
            agent_name = %raw,
            source,
            "lifecycle: ignoring invalid RimZ agent name",
        );
        None
    }
}

pub(super) fn validate_non_empty_identity_env(
    raw: String,
    source: &str,
    var: &str,
) -> Option<String> {
    let value = raw.trim();
    if !value.is_empty() {
        Some(value.to_owned())
    } else {
        warn!(
            env_var = var,
            source, "lifecycle: ignoring empty RimZ agent identity",
        );
        None
    }
}
