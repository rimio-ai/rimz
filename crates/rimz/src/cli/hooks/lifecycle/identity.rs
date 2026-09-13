//! Launch identity enrichment from environment and process state.

use super::*;

pub(super) fn env_run_id() -> Option<rimz::RunId> {
    let raw = std::env::var(rimz::harness::launch::ENV_RUN_ID).ok()?;
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
    agent_pid: Option<u32>,
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
    let raw = rimz::proc::env_var(agent_pid?, var)?;
    validate(raw, "process", var)
}

pub(in crate::cli::hooks) fn fill_root_launch_identity(
    observation: &mut AgentLifecycleObservation,
    configured_identity: (Option<String>, Option<String>),
    mut identity_env: impl FnMut(Option<u32>, &'static str) -> Option<String>,
) {
    if observation.parent_agent_id.is_some() {
        return;
    }
    let agent_pid = observation.agent_pid;
    rimz::harness::launch::fill_launch_identity_env(&mut observation.launch, |var| {
        identity_env(agent_pid, var)
    });
    observation.launch.model = observation.launch.model.take().or(configured_identity.0);
    observation.launch.effort = observation.launch.effort.take().or(configured_identity.1);
}

pub(super) fn validate_agent_name_env(raw: String, source: &str, _var: &str) -> Option<String> {
    if rimz::agents::petname::valid_agent_name(&raw) {
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
