//! Definition model defaults and provider-neutral tool vocabulary.

use super::definition::{DefinitionSpec, DefinitionTools};
use super::{PermissionMode, all_definitions, spec_by_kind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSet {
    bases: Vec<String>,
    agent_types: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolErr {
    #[error("invalid tool entry `{entry}`: expected a tool name")]
    Invalid { entry: String },
    #[error("tools are required for this agent kind")]
    Missing,
    #[error("agent kind `{kind}` does not support tools")]
    Unsupported { kind: String },
    #[error("unknown subagent type `{name}`; known types: {known}")]
    UnknownAgent { name: String, known: String },
}

impl ToolSet {
    pub fn parse(entries: &[String]) -> Result<Self, ToolErr> {
        let mut tools = Self {
            bases: Vec::new(),
            agent_types: Vec::new(),
        };
        for entry in entries {
            let entry_text = entry.trim();
            let (base, suffix) = match entry_text.split_once('(') {
                Some((base, tail)) => {
                    let Some(suffix) = tail.strip_suffix(')') else {
                        return Err(ToolErr::Invalid {
                            entry: entry.clone(),
                        });
                    };
                    if suffix.contains(['(', ')']) {
                        return Err(ToolErr::Invalid {
                            entry: entry.clone(),
                        });
                    }
                    (base.trim(), Some(suffix))
                }
                None => (entry_text, None),
            };
            if !base.starts_with(|c: char| c.is_ascii_alphabetic())
                || !base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(ToolErr::Invalid {
                    entry: entry.clone(),
                });
            }
            if !tools.has(base) {
                tools.bases.push(base.to_owned());
            }
            if base == "Agent" {
                for name in suffix
                    .into_iter()
                    .flat_map(|s| s.split(','))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if !tools.agent_types.iter().any(|known| known == name) {
                        tools.agent_types.push(name.to_owned());
                    }
                }
            }
        }
        Ok(tools)
    }

    pub fn has(&self, base: &str) -> bool {
        self.bases.iter().any(|name| name == base)
    }

    pub fn without(&self, base: &str) -> Self {
        let mut tools = self.clone();
        tools.bases.retain(|name| name != base);
        if base == "Agent" {
            tools.agent_types.clear();
        }
        tools
    }

    pub fn bases(&self) -> &[String] {
        &self.bases
    }

    pub(super) fn agent_types(&self) -> &[String] {
        &self.agent_types
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefinitionDefaults {
    pub mode: Option<PermissionMode>,
    pub effort: Option<&'static str>,
}

fn definitions(kind: &str) -> DefinitionSpec {
    spec_by_kind(kind).map_or(DefinitionSpec::EMPTY, |spec| spec.launch.definitions)
}

pub fn definition_model_kind(model: &str) -> Option<&'static str> {
    all_definitions()
        .find(|definition| {
            definition
                .spec()
                .launch
                .definitions
                .models
                .iter()
                .any(|entry| entry.name == model || entry.id == model)
        })
        .or_else(|| {
            all_definitions().find(|definition| {
                definition
                    .spec()
                    .launch
                    .definitions
                    .prefixes
                    .iter()
                    .any(|prefix| model.starts_with(prefix))
            })
        })
        .map(|definition| definition.spec().kind)
}

pub fn expand_model_alias(kind: &str, model: &str) -> String {
    definitions(kind)
        .models
        .iter()
        .find(|entry| entry.name == model)
        .map_or(model, |entry| entry.id)
        .to_owned()
}

pub fn definition_defaults(kind: &str, requested_model: Option<&str>) -> DefinitionDefaults {
    let spec = definitions(kind);
    let effort = requested_model
        .and_then(|model| {
            let expanded = expand_model_alias(kind, model);
            spec.models
                .iter()
                .find(|entry| entry.name == model)
                .and_then(|entry| entry.effort)
                .or_else(|| {
                    spec.models
                        .iter()
                        .find(|entry| entry.id == expanded)
                        .and_then(|entry| entry.effort)
                })
        })
        .or(spec.effort);
    DefinitionDefaults {
        mode: spec.mode,
        effort,
    }
}

pub fn tools_required(kind: &str) -> bool {
    matches!(definitions(kind).tools, DefinitionTools::Required(_))
}

pub fn render_tool_args(kind: &str, tools: Option<&ToolSet>) -> Result<Vec<String>, ToolErr> {
    match (definitions(kind).tools, tools) {
        (DefinitionTools::Required(render), tools) => render(tools.ok_or(ToolErr::Missing)?),
        (DefinitionTools::Unsupported, Some(_)) => Err(ToolErr::Unsupported {
            kind: kind.to_owned(),
        }),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(entries: &[&str]) -> ToolSet {
        ToolSet::parse(&entries.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn claude_tools_and_generated_planner_parity() {
        let plain = tools(&[
            "Bash",
            "Read",
            "Grep",
            "Glob",
            "Edit",
            "Write",
            "AskUserQuestion",
            "LSP",
            "Skill",
        ]);
        // Literal args from ~/.config/rimz/profiles/planner/agent.toml.
        let expected = shlex::split("--strict-mcp-config --tools 'Bash,Read,Grep,Glob,Edit,Write,AskUserQuestion,LSP,Skill'").unwrap();
        assert_eq!(render_tool_args("claude", Some(&plain)).unwrap(), expected);
        let named = tools(&["Agent(Explore, Plan)", "Bash", "Agent(Explore)"]);
        assert_eq!(
            render_tool_args("claude", Some(&named)).unwrap(),
            [
                "--strict-mcp-config",
                "--disallowedTools",
                "Agent(general-purpose)",
                "Agent(statusline-setup)",
                "Agent(fork)",
                "--tools",
                "Agent,Bash"
            ]
        );
        let error = render_tool_args("claude", Some(&tools(&["Agent(unknown)"]))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("known types: Explore, Plan, general-purpose, statusline-setup, fork")
        );
    }

    #[test]
    fn codex_tool_switches() {
        let ask = tools(&["AskUserQuestion", "Bash", "Skill"]);
        let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c agents.enabled=false -c features.goals=false -c features.multi_agent=false -c features.multi_agent_v2=false -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=true -c tools.experimental_request_user_input.enabled=true -c skills.include_instructions=true"#).unwrap();
        assert_eq!(render_tool_args("codex", Some(&ask)).unwrap(), expected);
        let web = tools(&["Bash", "Read", "WebFetch", "Agent"]);
        let expected = shlex::split(r#"--strict-config -c 'web_search="cached"' -c agents.enabled=true -c features.goals=false -c features.multi_agent=true -c features.multi_agent_v2=true -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=false -c tools.experimental_request_user_input.enabled=false -c skills.include_instructions=false"#).unwrap();
        assert_eq!(render_tool_args("codex", Some(&web)).unwrap(), expected);
    }

    #[test]
    fn generated_implementer_parity() {
        // Literal args from ~/.config/rimz/profiles/implementer/agent.toml.
        let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c 'agents.enabled=false' -c 'features.goals=false' -c 'features.multi_agent=false' -c 'features.multi_agent_v2=false' -c 'features.shell_snapshot=true' -c 'features.shell_tool=true' -c 'features.skill_mcp_dependency_install=false' -c 'features.tool_call_mcp_elicitation=false' -c 'features.browser_use=false' -c 'features.browser_use_external=false' -c 'features.computer_use=false' -c 'features.in_app_browser=false' -c 'features.image_generation=false' -c 'features.tool_suggest=false' -c 'features.memories=false' -c 'features.default_mode_request_user_input=false' -c 'tools.experimental_request_user_input.enabled=false' -c 'skills.include_instructions=false'"#).unwrap();
        assert_eq!(
            render_tool_args(
                "codex",
                Some(&tools(&[
                    "Bash", "Read", "Grep", "Glob", "Edit", "Write", "LSP"
                ]))
            )
            .unwrap(),
            expected
        );
    }

    #[test]
    fn tool_vocabulary_and_support() {
        let parsed = tools(&[" Bash ", "Agent(Explore, Plan)", "Bash", "Agent(Plan)"]);
        assert_eq!(parsed.bases(), ["Bash", "Agent"]);
        assert_eq!(parsed.agent_types(), ["Explore", "Plan"]);
        assert!(!parsed.without("Agent").has("Agent"));
        assert!(parsed.without("Agent").agent_types().is_empty());
        for entry in [
            "",
            " ",
            "--Bash",
            "Bash extra",
            "123",
            "Agent(Explore",
            "Bash)",
        ] {
            assert!(ToolSet::parse(&[entry.to_owned()]).is_err(), "{entry}");
        }
        for kind in ["claude", "codex"] {
            assert!(tools_required(kind));
            assert!(matches!(
                render_tool_args(kind, None),
                Err(ToolErr::Missing)
            ));
        }
        assert!(!tools_required("pi"));
        assert!(render_tool_args("pi", None).unwrap().is_empty());
        assert!(render_tool_args("pi", Some(&parsed)).unwrap().is_empty());
        assert!(
            matches!(render_tool_args("unregistered", Some(&parsed)), Err(ToolErr::Unsupported { kind }) if kind == "unregistered")
        );
        assert!(render_tool_args("unregistered", None).unwrap().is_empty());
    }

    #[test]
    fn model_catalog_and_defaults() {
        for (model, kind) in [
            ("fable", Some("claude")),
            ("gpt-6-astra", Some("codex")),
            ("astra", Some("codex")),
            ("claude-opus-4-6", Some("claude")),
            ("gpt-x", Some("codex")),
            ("custom", None),
        ] {
            assert_eq!(definition_model_kind(model), kind);
        }
        for (alias, id) in [
            ("astra", "gpt-6-astra"),
            ("luna", "gpt-5.6-luna"),
            ("terra", "gpt-5.6-terra"),
        ] {
            assert_eq!(expand_model_alias("codex", alias), id);
            assert_eq!(definition_model_kind(id), Some("codex"));
        }
        assert_eq!(expand_model_alias("pi", "astra"), "astra");
        assert_eq!(
            definition_defaults("claude", Some("fable")),
            DefinitionDefaults {
                mode: Some(PermissionMode::Auto),
                effort: Some("high")
            }
        );
        assert_eq!(
            definition_defaults("codex", Some("astra")),
            DefinitionDefaults {
                mode: None,
                effort: Some("xhigh")
            }
        );
        assert_eq!(definition_defaults("pi", None).effort, Some("xhigh"));
        assert_eq!(
            definition_defaults("unregistered", None),
            DefinitionDefaults {
                mode: None,
                effort: None
            }
        );
    }
}
