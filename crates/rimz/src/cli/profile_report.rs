use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use super::render;
use rimz::config::effective::ProfileScope;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentProfileReport {
    pub(crate) name: String,
    pub(crate) source: &'static str,
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
        agent: None,
        model: None,
        effort: None,
        description: None,
        path: sources.command(name).map(PathBuf::from),
    }));
    reports
}

pub(crate) fn list_profiles(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
    json: bool,
    show_path: bool,
) -> Result<()> {
    let mut reports = available_profiles(profiles, commands, sources, scope);
    apply_path_visibility(&mut reports, show_path);
    if json {
        return render::json_pretty(&reports);
    }
    render::finish(profile_cards(&reports, &mut render::out()))
}

fn apply_path_visibility(reports: &mut [AgentProfileReport], show_path: bool) {
    if !show_path {
        for report in reports {
            report.path = None;
        }
    }
}

fn profile_cards(reports: &[AgentProfileReport], out: &mut impl Write) -> std::io::Result<()> {
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }

        let (name_style, segments) = match &report.agent {
            Some(agent) => {
                let mut segments = vec![agent.as_str()];
                segments.extend(report.model.as_deref());
                segments.extend(report.effort.as_deref());
                (render::palette::identity(agent).bold(), segments)
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
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: Some("high".to_owned()),
                description: Some("Plans the work".to_owned()),
                path: Some(PathBuf::from("/tmp/rimz/agents.toml")),
            },
            AgentProfileReport {
                name: "reviewer".to_owned(),
                source: "profile",
                agent: Some("claude".to_owned()),
                model: None,
                effort: Some("max".to_owned()),
                description: Some("Reviews the result".to_owned()),
                path: None,
            },
            AgentProfileReport {
                name: "coder".to_owned(),
                source: "profile",
                agent: Some("codex".to_owned()),
                model: Some("gpt-5.6".to_owned()),
                effort: None,
                description: None,
                path: None,
            },
            AgentProfileReport {
                name: "lint".to_owned(),
                source: "command",
                agent: None,
                model: None,
                effort: None,
                description: None,
                path: Some(PathBuf::from("/tmp/.agents/profiles/lint/agent.toml")),
            },
        ];
        let mut output = Vec::new();

        profile_cards(&reports, &mut anstream::StripStream::new(&mut output))
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
}
