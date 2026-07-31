use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use super::render;
use rimz::config::effective::ProfileScope;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentSpecReport {
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

impl AgentSpecReport {
    pub(crate) fn detail(&self) -> String {
        let Some(agent) = &self.agent else {
            return "-".to_owned();
        };
        let posture = match (self.model.as_deref(), self.effort.as_deref()) {
            (Some(model), Some(effort)) => Some(format!("{model}@{effort}")),
            (Some(model), None) => Some(model.to_owned()),
            (None, Some(effort)) => Some(format!("@{effort}")),
            (None, None) => None,
        };
        posture.map_or_else(|| agent.clone(), |posture| format!("{agent} · {posture}"))
    }
}

pub(crate) fn available_specs(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
) -> Vec<AgentSpecReport> {
    let mut specs = profiles
        .0
        .iter()
        .map(|(name, profile)| AgentSpecReport {
            name: name.clone(),
            source: "profile",
            agent: Some(profile.agent.clone()),
            model: profile.model.clone(),
            effort: profile.effort.clone(),
            description: profile.description.clone(),
            path: sources.profile(scope, name).map(PathBuf::from),
        })
        .collect::<Vec<_>>();
    specs.extend(commands.0.keys().map(|name| AgentSpecReport {
        name: name.clone(),
        source: "command",
        agent: None,
        model: None,
        effort: None,
        description: None,
        path: sources.command(name).map(PathBuf::from),
    }));
    specs
}

pub(crate) fn list_specs(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    sources: &rimz::config::AgentSpecSources,
    scope: ProfileScope,
    json: bool,
) -> Result<()> {
    let specs = available_specs(profiles, commands, sources, scope);
    if json {
        return render::json_pretty(&specs);
    }
    spec_table(specs)
        .render(&mut render::out())
        .map_err(Into::into)
}

fn spec_table(specs: Vec<AgentSpecReport>) -> render::Table {
    let mut table = render::Table::new(["SPEC", "SOURCE", "DETAIL", "DESCRIPTION", "PATH"]);
    for agent_spec in specs {
        let detail = agent_spec.detail();
        table.row([
            render::cell(agent_spec.name),
            render::cell(agent_spec.source),
            render::cell(detail).dash(),
            render::cell(agent_spec.description.unwrap_or_else(|| "-".to_owned())).dash(),
            render::cell(
                agent_spec
                    .path
                    .map(|path| render::home_relative(&path.to_string_lossy()))
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_the_final_spec_table_column() {
        let specs = vec![
            AgentSpecReport {
                name: "planner".to_owned(),
                source: "profile",
                agent: Some("codex".to_owned()),
                model: None,
                effort: None,
                description: Some("Plans the work".to_owned()),
                path: Some(PathBuf::from("/tmp/rimz/agents.toml")),
            },
            AgentSpecReport {
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

        spec_table(specs)
            .render(&mut anstream::StripStream::new(&mut output))
            .expect("render spec table");

        insta::assert_snapshot!(String::from_utf8(output).expect("utf-8"), @r"
        SPEC     SOURCE   DETAIL  DESCRIPTION     PATH
        planner  profile  codex   Plans the work  /tmp/rimz/agents.toml
        lint     command  -       -               /tmp/.agents/profiles/lint/agent.toml
        ");
    }
}
