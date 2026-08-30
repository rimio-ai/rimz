use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use super::render;
use rimz::config::effective::ProfileScope;
use rimz::harness::subagent_policy::{SubagentCatalog, SubagentSpec, SubagentSpecSource};

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

pub(crate) fn subagent_reports(
    catalog: SubagentCatalog,
    profiles: &rimz::config::ProfilesConfig,
    sources: &rimz::config::AgentSpecSources,
) -> Vec<AgentProfileReport> {
    let SubagentCatalog::Available(specs) = catalog else {
        return Vec::new();
    };
    specs
        .into_iter()
        .map(|spec| subagent_report(spec, profiles, sources))
        .collect()
}

fn subagent_report(
    spec: SubagentSpec,
    profiles: &rimz::config::ProfilesConfig,
    sources: &rimz::config::AgentSpecSources,
) -> AgentProfileReport {
    let (source, path) = match spec.source {
        SubagentSpecSource::Profile => (
            "profile",
            sources
                .profile(ProfileScope::Subagents, &spec.name)
                .map(PathBuf::from),
        ),
        SubagentSpecSource::Command => ("command", sources.command(&spec.name).map(PathBuf::from)),
    };
    let brand_kind = spec
        .agent
        .as_deref()
        .map(|agent| provider_brand_kind(agent, profiles).to_owned());
    AgentProfileReport {
        name: spec.name,
        source,
        brand_kind,
        agent: spec.agent,
        model: spec.model,
        effort: spec.effort,
        description: spec.description,
        path,
    }
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
    mut reports: Vec<AgentProfileReport>,
    scope: ProfileScope,
    json: bool,
    show_path: bool,
) -> Result<()> {
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
    fn disabled_subagent_catalog_has_no_reports() {
        assert!(
            subagent_reports(
                SubagentCatalog::Disabled,
                &rimz::config::ProfilesConfig::default(),
                &rimz::config::AgentSpecSources::default(),
            )
            .is_empty()
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
