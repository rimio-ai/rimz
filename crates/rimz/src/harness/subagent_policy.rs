//! What a `rimz subagents` caller may launch.

use crate::agents::AgentState;
use crate::config::{CommandsConfig, ProfilesConfig};
use crate::harness::spec::LayoutErr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentProfileSource {
    Profile,
    Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentProfile {
    pub name: String,
    pub source: SubagentProfileSource,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentCatalog {
    Disabled,
    Available(Vec<SubagentProfile>),
}

pub fn catalog(
    caller_profile: Option<&str>,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> SubagentCatalog {
    let allowed = allowed_specs(caller_profile, profiles);
    if allowed.is_some_and(|allowed| allowed.is_empty()) {
        return SubagentCatalog::Disabled;
    }

    let mut specs = subagent_profiles
        .0
        .iter()
        .map(|(name, profile)| SubagentProfile {
            name: name.clone(),
            source: SubagentProfileSource::Profile,
            agent: Some(profile.agent.clone()),
            model: profile.model.clone(),
            effort: profile.effort.clone(),
            description: profile.description.clone(),
        })
        .collect::<Vec<_>>();
    specs.extend(commands.0.keys().map(|name| SubagentProfile {
        name: name.clone(),
        source: SubagentProfileSource::Command,
        agent: None,
        model: None,
        effort: None,
        description: None,
    }));
    if let Some(allowed) = allowed {
        specs.retain(|spec| allowed.contains(&spec.name));
    }
    SubagentCatalog::Available(specs)
}

pub fn reminder(catalog: &SubagentCatalog) -> String {
    let body = match catalog {
        SubagentCatalog::Disabled => "Subagents are disabled for this agent: its profile allows none, so any `rimz subagents` launch is refused. Do the work yourself with your direct tools.".to_owned(),
        SubagentCatalog::Available(specs) if specs.is_empty() => "No subagent profiles are configured for this agent; Skill(rimz-subagents) has nothing configured to launch. The user enables subagents by adding `[subagents.profiles]` entries to agents.toml.".to_owned(),
        SubagentCatalog::Available(specs) => {
            let list = specs
                .iter()
                .map(reminder_profile)
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Subagents are available to you; launch them with Skill(rimz-subagents). Use them to run independent work in parallel, fan out searches or audits, or keep a large exploration out of your own context: delegate it and keep the conclusion, not the file dumps.\n\nAvailable subagent profiles you may launch:\n{list}"
            )
        }
    };
    format!("<system_reminder>\n{body}\n</system_reminder>")
}

fn reminder_profile(profile: &SubagentProfile) -> String {
    let details = [
        profile.agent.as_deref(),
        profile.model.as_deref(),
        profile.effort.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let details = (!details.is_empty()).then(|| format!(" ({details})"));
    let description = profile
        .description
        .as_deref()
        .map(|description| format!(": {description}"));
    format!(
        "- `{}`{}{}",
        profile.name,
        details.as_deref().unwrap_or_default(),
        description.as_deref().unwrap_or_default()
    )
}

pub fn allowed_specs<'a>(
    caller_profile: Option<&str>,
    profiles: &'a ProfilesConfig,
) -> Option<&'a [String]> {
    caller_profile
        .and_then(|name| profiles.0.get(name))
        .and_then(|profile| profile.subagents.as_deref())
}

pub fn check_allowed(
    caller: &AgentState,
    profiles: &ProfilesConfig,
    profile: &str,
    agent_override: Option<&str>,
) -> crate::harness::spec::Result<()> {
    let Some(allowed) = allowed_specs(caller.profile.as_deref(), profiles) else {
        return Ok(());
    };
    for requested in [Some(profile), agent_override].into_iter().flatten() {
        let requested = requested.trim();
        if allowed.iter().any(|candidate| candidate == requested) {
            continue;
        }
        return Err(LayoutErr::SubagentProfileNotAllowed {
            caller_profile: caller.profile.clone().unwrap_or_default(),
            profile: requested.to_owned(),
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
    fn allowlist_checks_spec_and_agent_override() {
        let (caller, profiles) = caller(Some(vec!["explorer", "designer"]));
        assert!(check_allowed(&caller, &profiles, " explorer ", None).is_ok());
        assert!(check_allowed(&caller, &profiles, "explorer", Some("designer")).is_ok());

        let error = check_allowed(&caller, &profiles, "codex", None).unwrap_err();
        assert!(error.to_string().contains("profile `planner`"));
        assert!(error.to_string().contains("[explorer, designer]"));
        assert!(check_allowed(&caller, &profiles, "explorer", Some("codex")).is_err());
    }

    #[test]
    fn missing_policy_allows_all_and_empty_policy_allows_none() {
        let (unrestricted, profiles) = caller(None);
        assert!(check_allowed(&unrestricted, &profiles, "codex", None).is_ok());

        let mut bare = unrestricted;
        bare.profile = None;
        assert!(check_allowed(&bare, &profiles, "codex", None).is_ok());

        let (caller, profiles) = caller(Some(Vec::new()));
        assert!(check_allowed(&caller, &profiles, "explorer", None).is_err());
    }

    #[test]
    fn catalog_combines_profiles_and_commands() {
        let (_, profiles) = caller(None);
        let subagent_profiles = ProfilesConfig(BTreeMap::from([(
            "explorer".to_owned(),
            Profile {
                agent: "claude".to_owned(),
                description: Some("Finds files and traces code paths".to_owned()),
                subagents: None,
                mode: None,
                model: Some("sonnet".to_owned()),
                effort: Some("low".to_owned()),
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                args: None,
            },
        )]));
        let commands = CommandsConfig(BTreeMap::from([(
            "lint".to_owned(),
            "cargo clippy".to_owned(),
        )]));

        let SubagentCatalog::Available(specs) =
            catalog(None, &profiles, &subagent_profiles, &commands)
        else {
            panic!("unrestricted catalog is available");
        };
        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["explorer", "lint"]
        );
        assert_eq!(specs[0].source, SubagentProfileSource::Profile);
        assert_eq!(specs[1].source, SubagentProfileSource::Command);
    }

    #[test]
    fn catalog_filters_by_literal_name_and_distinguishes_disabled() {
        let (_, profiles) = caller(Some(vec!["lint"]));
        let commands = CommandsConfig(BTreeMap::from([(
            "lint".to_owned(),
            "cargo clippy".to_owned(),
        )]));
        let SubagentCatalog::Available(specs) = catalog(
            Some("planner"),
            &profiles,
            &ProfilesConfig::default(),
            &commands,
        ) else {
            panic!("non-empty allowlist is available");
        };
        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["lint"]
        );

        let (_, profiles) = caller(Some(Vec::new()));
        assert_eq!(
            catalog(
                Some("planner"),
                &profiles,
                &ProfilesConfig::default(),
                &CommandsConfig::default()
            ),
            SubagentCatalog::Disabled
        );
    }

    #[test]
    fn reminder_renders_available_specs_and_disabled_policy() {
        let available = SubagentCatalog::Available(vec![
            SubagentProfile {
                name: "explorer".to_owned(),
                source: SubagentProfileSource::Profile,
                agent: Some("claude".to_owned()),
                model: Some("sonnet".to_owned()),
                effort: Some("low".to_owned()),
                description: Some("Finds files and traces code paths".to_owned()),
            },
            SubagentProfile {
                name: "lint".to_owned(),
                source: SubagentProfileSource::Command,
                agent: None,
                model: None,
                effort: None,
                description: None,
            },
        ]);
        let text = reminder(&available);
        assert_eq!(
            text,
            "<system_reminder>\n\
             Subagents are available to you; launch them with Skill(rimz-subagents). Use them to run independent work in parallel, fan out searches or audits, or keep a large exploration out of your own context: delegate it and keep the conclusion, not the file dumps.\n\n\
             Available subagent profiles you may launch:\n\
             - `explorer` (claude · sonnet · low): Finds files and traces code paths\n\
             - `lint`\n\
             </system_reminder>"
        );
        assert_eq!(
            reminder(&SubagentCatalog::Disabled),
            "<system_reminder>\n\
             Subagents are disabled for this agent: its profile allows none, so any `rimz subagents` launch is refused. Do the work yourself with your direct tools.\n\
             </system_reminder>"
        );
        assert_eq!(
            reminder(&SubagentCatalog::Available(Vec::new())),
            "<system_reminder>\n\
             No subagent profiles are configured for this agent; Skill(rimz-subagents) has nothing configured to launch. The user enables subagents by adding `[subagents.profiles]` entries to agents.toml.\n\
             </system_reminder>"
        );
    }
}
