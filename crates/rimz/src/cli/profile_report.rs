use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use super::render;
use rimz::config::effective::ProfileScope;
use rimz::harness::subagent_policy::GENERAL_SPEC;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentProfileReport {
    pub(crate) name: String,
    pub(crate) source: &'static str,
    #[serde(skip)]
    brand_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<PathBuf>,
}

pub(crate) fn available_profiles(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
) -> Vec<AgentProfileReport> {
    let mut reports = profiles
        .0
        .iter()
        .map(|(name, profile)| AgentProfileReport {
            name: name.clone(),
            source: "profile",
            brand_kind: Some(provider_brand_kind(&profile.agent, profiles).to_owned()),
            agent: Some(profile.agent.clone()),
            model: profile.model.clone(),
            effort: profile.effort.clone(),
            description: profile.description.clone(),
            path: sources.profile(scope, name).map(PathBuf::from),
        })
        .collect::<Vec<_>>();
    reports.extend(commands.0.keys().map(|name| AgentProfileReport {
        name: name.clone(),
        source: "command",
        brand_kind: None,
        agent: None,
        model: None,
        effort: None,
        description: None,
        path: sources.command(name).map(PathBuf::from),
    }));
    reports
}

pub(crate) fn general_report(caller: Option<&rimz::agents::AgentState>) -> AgentProfileReport {
    let kind = caller.map(|caller| caller.kind.to_string());
    AgentProfileReport {
        name: GENERAL_SPEC.to_owned(),
        source: "builtin",
        brand_kind: kind.clone(),
        agent: kind,
        model: caller.and_then(|caller| caller.model.clone()),
        effort: caller.and_then(|caller| caller.effort.clone()),
        description: Some(
            "General-purpose child of the caller's kind; inherits its model and effort unless overridden"
                .to_owned(),
        ),
        path: None,
    }
}

pub(crate) fn retain_allowed(reports: &mut Vec<AgentProfileReport>, allowed: Option<&[String]>) {
    let Some(allowed) = allowed else {
        return;
    };
    reports.retain(|report| allowed.contains(&report.name));
}

fn provider_brand_kind<'a>(
    raw_agent: &'a str,
    profiles: &'a rimz::config::ProfilesConfig,
) -> &'a str {
    let mut current = raw_agent;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let Some(profile) = profiles.0.get(current) else {
            return if rimz::agents::find_definition(current).is_some() {
                current
            } else {
                raw_agent
            };
        };
        let next = profile.agent.as_str();
        if next == current && rimz::agents::find_definition(next).is_some() {
            return next;
        }
        current = next;
    }
    raw_agent
}

pub(crate) fn list_profiles(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
    prepend: Vec<AgentProfileReport>,
    allowed: Option<&[String]>,
    json: bool,
    show_path: bool,
) -> Result<()> {
    let mut reports = available_profiles(profiles, commands, sources, scope);
    reports.splice(0..0, prepend);
    retain_allowed(&mut reports, allowed);
    apply_path_visibility(&mut reports, show_path);
    if json {
        return render::json_pretty(&reports);
    }
    render::finish(profile_cards(&reports, scope, &mut render::out()))
}

fn apply_path_visibility(reports: &mut [AgentProfileReport], show_path: bool) {
    if !show_path {
        for report in reports {
            report.path = None;
        }
    }
}

fn profile_cards(
    reports: &[AgentProfileReport],
    scope: ProfileScope,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if reports.is_empty() {
        let profile_section = match scope {
            ProfileScope::Agents => "agents.profiles",
            ProfileScope::Subagents => "subagents.profiles",
        };
        writeln!(out, "No profiles or commands configured.")?;
        writeln!(
            out,
            "Add one under [{profile_section}] or [agents.commands]."
        )?;
        return Ok(());
    }

    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }

        let (name_style, segments) = match &report.agent {
            Some(agent) => {
                let mut segments = vec![agent.as_str()];
                segments.extend(report.model.as_deref());
                segments.extend(report.effort.as_deref());
                let brand_kind = report.brand_kind.as_deref().unwrap_or(agent);
                (render::palette::identity(brand_kind).bold(), segments)
            }
            None if report.source == "builtin" => (render::palette::muted(), vec!["caller's kind"]),
            None => (render::palette::muted(), vec!["command"]),
        };
        writeln!(
            out,
            "{} — {}",
            render::paint(name_style, &report.name),
            segments.join(" · ")
        )?;
        if let Some(description) = &report.description {
            writeln!(out, "  {description}")?;
        }
        if let Some(path) = &report.path {
            writeln!(out, "  {}", render::home_relative(&path.to_string_lossy()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_render_as_cards_with_independent_model_and_effort_segments() {
        let reports = vec![
            AgentProfileReport {
                name: "planner".to_owned(),
                source: "profile",
                brand_kind: Some("codex".to_owned()),
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: Some("high".to_owned()),
                description: Some("Plans the work".to_owned()),
                path: Some(PathBuf::from("/tmp/rimz/agents.toml")),
            },
            AgentProfileReport {
                name: "reviewer".to_owned(),
                source: "profile",
                brand_kind: Some("claude".to_owned()),
                agent: Some("claude".to_owned()),
                model: None,
                effort: Some("max".to_owned()),
                description: Some("Reviews the result".to_owned()),
                path: None,
            },
            AgentProfileReport {
                name: "coder".to_owned(),
                source: "profile",
                brand_kind: Some("codex".to_owned()),
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: None,
                description: None,
                path: None,
            },
            AgentProfileReport {
                name: "lint".to_owned(),
                source: "command",
                brand_kind: None,
                agent: None,
                model: None,
                effort: None,
                description: None,
                path: Some(PathBuf::from("/tmp/.agents/profiles/lint/agent.toml")),
            },
        ];
        let mut output = Vec::new();

        profile_cards(
            &reports,
            ProfileScope::Agents,
            &mut anstream::StripStream::new(&mut output),
        )
        .expect("render profile cards");

        insta::assert_snapshot!(String::from_utf8(output).expect("utf-8"), @r"
        planner — codex · gpt-5.6 · high
          Plans the work
          /tmp/rimz/agents.toml

        reviewer — claude · max
          Reviews the result

        coder — codex · gpt-5.6

        lint — command
          /tmp/.agents/profiles/lint/agent.toml
        ");
    }

    #[test]
    fn paths_are_opt_in_for_json() {
        let mut reports = vec![AgentProfileReport {
            name: "planner".to_owned(),
            source: "profile",
            brand_kind: Some("codex".to_owned()),
            agent: Some("codex".to_owned()),
            model: None,
            effort: None,
            description: None,
            path: Some(PathBuf::from("/tmp/rimz/agents.toml")),
        }];

        let json = serde_json::to_value(&reports).expect("serialize profile reports with paths");
        assert_eq!(json[0]["path"], "/tmp/rimz/agents.toml");

        apply_path_visibility(&mut reports, false);

        let json = serde_json::to_value(&reports).expect("serialize profile reports without paths");
        assert!(json[0].get("path").is_none());
    }

    #[test]
    fn empty_catalog_names_the_configuration_section() {
        let mut output = Vec::new();
        profile_cards(&[], ProfileScope::Subagents, &mut output).expect("render empty catalog");

        assert_eq!(
            String::from_utf8(output).expect("utf-8"),
            "No profiles or commands configured.\n\
             Add one under [subagents.profiles] or [agents.commands].\n"
        );
    }

    #[test]
    fn general_report_uses_caller_identity_or_labels_the_unknown_kind() {
        let mut caller =
            rimz::agents::AgentState::stub("claude", "caller", rimz::agents::AgentStatus::Running);
        caller.model = Some("opus".to_owned());
        caller.effort = Some("high".to_owned());
        let reports = [general_report(Some(&caller)), general_report(None)];
        let mut output = Vec::new();

        profile_cards(
            &reports,
            ProfileScope::Subagents,
            &mut anstream::StripStream::new(&mut output),
        )
        .expect("render general cards");

        insta::assert_snapshot!(String::from_utf8(output).expect("utf-8"), @r"
        general — claude · opus · high
          General-purpose child of the caller's kind; inherits its model and effort unless overridden

        general — caller's kind
          General-purpose child of the caller's kind; inherits its model and effort unless overridden
        ");
        assert_eq!(reports[0].source, "builtin");
    }

    #[test]
    fn profile_allowlist_filters_builtin_profiles_and_commands() {
        let mut reports = vec![
            general_report(None),
            AgentProfileReport {
                name: "explorer".to_owned(),
                source: "profile",
                brand_kind: Some("claude".to_owned()),
                agent: Some("claude".to_owned()),
                model: None,
                effort: None,
                description: None,
                path: None,
            },
            AgentProfileReport {
                name: "lint".to_owned(),
                source: "command",
                brand_kind: None,
                agent: None,
                model: None,
                effort: None,
                description: None,
                path: None,
            },
        ];

        retain_allowed(
            &mut reports,
            Some(&["general".to_owned(), "lint".to_owned()]),
        );

        assert_eq!(
            reports
                .iter()
                .map(|report| report.name.as_str())
                .collect::<Vec<_>>(),
            ["general", "lint"]
        );
    }

    #[test]
    fn chained_profiles_resolve_the_provider_for_brand_tint() {
        let profiles = rimz::config::ProfilesConfig(
            [
                ("deep".to_owned(), profile("planner")),
                ("planner".to_owned(), profile("claude")),
                ("claude".to_owned(), profile("claude")),
            ]
            .into(),
        );

        let reports = available_profiles(
            &profiles,
            &rimz::config::CommandsConfig::default(),
            &rimz::config::AgentSpecSources::default(),
            ProfileScope::Agents,
        );
        let deep = reports
            .iter()
            .find(|report| report.name == "deep")
            .expect("deep profile");
        assert_eq!(deep.agent.as_deref(), Some("planner"));
        assert_eq!(deep.brand_kind.as_deref(), Some("claude"));
        assert!(
            serde_json::to_value(deep)
                .expect("serialize profile")
                .get("brand_kind")
                .is_none()
        );
    }

    #[test]
    fn cyclic_profile_brand_falls_back_to_the_raw_agent() {
        let profiles = rimz::config::ProfilesConfig(
            [
                ("planner".to_owned(), profile("reviewer")),
                ("reviewer".to_owned(), profile("planner")),
            ]
            .into(),
        );

        assert_eq!(provider_brand_kind("reviewer", &profiles), "reviewer");
    }

    fn profile(agent: &str) -> rimz::config::Profile {
        rimz::config::Profile {
            agent: agent.to_owned(),
            description: None,
            subagents: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        }
    }
}
