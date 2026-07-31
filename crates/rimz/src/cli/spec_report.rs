use anyhow::Result;
use serde::Serialize;

use super::render;

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
        })
        .collect::<Vec<_>>();
    specs.extend(commands.0.keys().map(|name| AgentSpecReport {
        name: name.clone(),
        source: "command",
        agent: None,
        model: None,
        effort: None,
        description: None,
    }));
    specs
}

pub(crate) fn list_specs(
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    json: bool,
) -> Result<()> {
    let specs = available_specs(profiles, commands);
    if json {
        return render::json_pretty(&specs);
    }
    let mut table = render::Table::new(["SPEC", "SOURCE", "DETAIL", "DESCRIPTION"]);
    for agent_spec in specs {
        let detail = agent_spec.detail();
        table.row([
            render::cell(agent_spec.name),
            render::cell(agent_spec.source),
            render::cell(detail).dash(),
            render::cell(agent_spec.description.unwrap_or_else(|| "-".to_owned())).dash(),
        ]);
    }
    table.render(&mut render::out()).map_err(Into::into)
}
