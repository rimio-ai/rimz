//! What a `rimz subagents` caller may launch, and what `general` means for it.

use crate::agents::AgentState;
use crate::config::ProfilesConfig;
use crate::ids::AgentKind;
use crate::{agents::LaunchPreset, harness::spec::LayoutErr};

pub const GENERAL_SPEC: &str = "general";

pub fn general_launch(caller: &AgentState) -> (AgentKind, LaunchPreset) {
    (
        caller.kind.clone(),
        LaunchPreset {
            model: caller.model.clone(),
            effort: caller.effort.clone(),
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
        },
    )
}

pub fn allowed_specs<'a>(
    caller: &AgentState,
    profiles: &'a ProfilesConfig,
) -> Option<&'a [String]> {
    caller
        .profile
        .as_deref()
        .and_then(|name| profiles.0.get(name))
        .and_then(|profile| profile.subagents.as_deref())
}

pub fn check_allowed(
    caller: &AgentState,
    profiles: &ProfilesConfig,
    spec: &str,
    agent_override: Option<&str>,
) -> crate::harness::spec::Result<()> {
    let Some(allowed) = allowed_specs(caller, profiles) else {
        return Ok(());
    };
    for requested in [Some(spec), agent_override].into_iter().flatten() {
        let requested = requested.trim();
        if allowed.iter().any(|candidate| candidate == requested) {
            continue;
        }
        return Err(LayoutErr::SubagentSpecNotAllowed {
            profile: caller.profile.clone().unwrap_or_default(),
            spec: requested.to_owned(),
            allowed: format!("[{}]", allowed.join(", ")),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::config::Profile;
    use std::collections::BTreeMap;

    fn caller(allowed: Option<Vec<&str>>) -> (AgentState, ProfilesConfig) {
        let mut caller = AgentState::stub("claude", "parent", AgentStatus::Running);
        caller.profile = Some("planner".to_owned());
        caller.model = Some("opus".to_owned());
        caller.effort = Some("high".to_owned());
        let profiles = ProfilesConfig(BTreeMap::from([(
            "planner".to_owned(),
            Profile {
                agent: "claude".to_owned(),
                description: None,
                subagents: allowed.map(|values| values.into_iter().map(str::to_owned).collect()),
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                args: None,
            },
        )]));
        (caller, profiles)
    }

    #[test]
    fn general_inherits_current_model_and_effort() {
        let (caller, _) = caller(None);
        assert_eq!(
            general_launch(&caller),
            (
                AgentKind::new_unchecked("claude"),
                LaunchPreset {
                    model: Some("opus".to_owned()),
                    effort: Some("high".to_owned()),
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                }
            )
        );

        let mut bare = caller;
        bare.model = None;
        bare.effort = None;
        assert_eq!(general_launch(&bare).1.model, None);
        assert_eq!(general_launch(&bare).1.effort, None);
    }

    #[test]
    fn allowlist_checks_spec_and_agent_override() {
        let (caller, profiles) = caller(Some(vec!["general", "explorer"]));
        assert!(check_allowed(&caller, &profiles, " general ", None).is_ok());
        assert!(check_allowed(&caller, &profiles, "general", Some("explorer")).is_ok());

        let error = check_allowed(&caller, &profiles, "codex", None).unwrap_err();
        assert!(error.to_string().contains("profile `planner`"));
        assert!(error.to_string().contains("[general, explorer]"));
        assert!(check_allowed(&caller, &profiles, "general", Some("codex")).is_err());
    }

    #[test]
    fn missing_policy_allows_all_and_empty_policy_allows_none() {
        let (unrestricted, profiles) = caller(None);
        assert!(check_allowed(&unrestricted, &profiles, "codex", None).is_ok());

        let mut bare = unrestricted;
        bare.profile = None;
        assert!(check_allowed(&bare, &profiles, "codex", None).is_ok());

        let (caller, profiles) = caller(Some(Vec::new()));
        assert!(check_allowed(&caller, &profiles, "general", None).is_err());
    }
}
