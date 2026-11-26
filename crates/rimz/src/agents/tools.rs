//! Definition model defaults and provider-neutral tool vocabulary.

use super::definition::{CodeModeSwitch, DefinitionSpec, DefinitionTools};
use super::{all_definitions, spec_by_kind};

/// RimZ tool names beside Pi's, read forward by Pi's renderer and backward by parsing.
pub(super) const PI_TOOL_NAMES: &[(&str, &str)] = &[
    ("Bash", "bash"),
    ("Read", "read"),
    ("Edit", "edit"),
    ("MultiEdit", "edit"),
    ("Write", "write"),
    ("Grep", "grep"),
    ("Glob", "find"),
    ("LS", "ls"),
    ("AskUserQuestion", "ask_user_question"),
    ("powershell", "powershell"),
];

/// The RimZ spelling of a Pi-native tool name; any other name as written.
pub(crate) fn canonical_tool_name(name: &str) -> &str {
    PI_TOOL_NAMES
        .iter()
        .find_map(|&(canonical, native)| (name == native).then_some(canonical))
        .unwrap_or(name)
}

/// Parsed definition tools, rendered for the final launch provider.
///
/// Restriction text inside `Base(...)` is dropped at parse: only the bases and the `Agent(...)`
/// type list reach a launch, and `restrictions` only records which bases carried one, for the
/// notice that says it was dropped. A pack update's widening review compares those two lists
/// alone, so a restriction that changes grants nothing it can see.
///
/// `rimz` is the RimZ grant of a `capabilities:` list, which rides on its tools value so that
/// every rule that moves the tools moves the grant with them; a `tools:` list has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSet {
    bases: Vec<String>,
    agent_types: Vec<String>,
    restrictions: Vec<String>,
    rimz: Option<Vec<crate::config::RimzEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolNotice {
    Dropped(Vec<String>),
    Restrictions(Vec<String>),
    NotApplied,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolErr {
    #[error("invalid tool entry `{entry}`: expected a tool name")]
    Invalid { entry: String },
    #[error("tools are required for this agent kind")]
    Missing,
    #[error("unknown subagent type `{name}`; known types: {known}")]
    UnknownAgent { name: String, known: String },
}

impl ToolSet {
    pub(crate) fn parse(entries: &[String]) -> Result<Self, ToolErr> {
        let mut tools = Self {
            bases: Vec::new(),
            agent_types: Vec::new(),
            restrictions: Vec::new(),
            rimz: None,
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
            let base = canonical_tool_name(base);
            if !tools.has(base) {
                tools.bases.push(base.to_owned());
            }
            if suffix.is_some() && !tools.restrictions.iter().any(|name| name == base) {
                tools.restrictions.push(base.to_owned());
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

    pub(crate) fn has(&self, base: &str) -> bool {
        self.bases.iter().any(|name| name == base)
    }

    pub fn bases(&self) -> &[String] {
        &self.bases
    }

    /// The RimZ capabilities a `capabilities:` list grants; `None` for a `tools:` list.
    pub fn rimz(&self) -> Option<&[crate::config::RimzEntry]> {
        self.rimz.as_deref()
    }

    /// These tools as the lowering of a `capabilities:` list that grants `grant`.
    #[must_use]
    pub fn with_rimz(mut self, grant: Vec<crate::config::RimzEntry>) -> Self {
        self.rimz = Some(grant);
        self
    }

    pub(crate) fn agent_types(&self) -> &[String] {
        &self.agent_types
    }

    pub(super) fn restrictions(&self) -> &[String] {
        &self.restrictions
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DefinitionDefaults {
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

pub(crate) fn definition_defaults(kind: &str, requested_model: Option<&str>) -> DefinitionDefaults {
    spec_by_kind(kind).map_or(DefinitionDefaults { effort: None }, |spec| {
        defaults_for_launch(&spec.launch, requested_model, None)
    })
}

/// The defaults for `pinned`, the id a machine pin maps `name` to. On a kind with a kind-wide
/// effort, an id the catalog does not list keeps `name`'s own effort.
pub(crate) fn pinned_definition_defaults(
    kind: &str,
    name: &str,
    pinned: &str,
) -> DefinitionDefaults {
    spec_by_kind(kind).map_or(DefinitionDefaults { effort: None }, |spec| {
        defaults_for_launch(&spec.launch, Some(pinned), Some(name))
    })
}

fn defaults_for_launch(
    launch: &super::LaunchSpec,
    requested_model: Option<&str>,
    pinned_from: Option<&str>,
) -> DefinitionDefaults {
    let spec = launch.definitions;
    let listed = |model: &str| {
        spec.models
            .iter()
            .any(|entry| entry.name == model || entry.id == model)
    };
    let effort = requested_model
        .map(|model| match pinned_from {
            Some(name) if spec.effort.is_some() && !listed(model) => name,
            _ => model,
        })
        .and_then(|model| {
            spec.models
                .iter()
                .find(|entry| entry.name == model)
                .and_then(|entry| entry.effort)
                .or_else(|| {
                    spec.models
                        .iter()
                        .find(|entry| entry.id == model)
                        .and_then(|entry| entry.effort)
                })
        })
        .or(spec.effort)
        .filter(|_| {
            launch
                .preset_arg_matcher(super::PresetField::Effort)
                .is_some()
        });
    DefinitionDefaults { effort }
}

pub(crate) fn tools_required(kind: &str) -> bool {
    matches!(
        definitions(kind).tools,
        DefinitionTools::Required(_) | DefinitionTools::RequiredFile(_)
    )
}

/// The entry of a tools list's `bases` that is `kind`'s preset, as written; `None` on a kind
/// that declares none, which reads every entry as a tool name.
pub(crate) fn tool_preset<'a>(kind: &str, bases: &'a [String]) -> Option<&'a str> {
    let preset = definitions(kind).tool_preset?;
    bases
        .iter()
        .map(String::as_str)
        .find(|base| base.eq_ignore_ascii_case(preset))
}

/// Whether a tools list's `bases` grant `tool` on `kind`: they name it, or hold the kind's
/// preset, which RimZ reads as granting every tool.
pub(crate) fn grants_tool(kind: &str, bases: &[String], tool: &str) -> bool {
    bases.iter().any(|base| base == tool) || tool_preset(kind, bases).is_some()
}

/// Whether a supplied tools list applies to this kind and grants no `Skill`.
pub fn skills_lack_skill_tool(kind: &str, tools: Option<&ToolSet>) -> bool {
    tools.is_some_and(|tools| !grants_tool(kind, tools.bases(), "Skill"))
        && matches!(
            definitions(kind).tools,
            DefinitionTools::Required(_)
                | DefinitionTools::Optional(_)
                | DefinitionTools::RequiredFile(_)
        )
}

/// Whether `kind` applies a tools list only inside the agent file its system prompt rides.
pub(crate) fn tools_ride_prompt(kind: &str) -> bool {
    matches!(definitions(kind).tools, DefinitionTools::RequiredFile(_))
}

/// Whether a definition's system prompt on `kind` appends to the provider's own prompt.
pub(crate) fn system_prompt_appends(kind: &str) -> bool {
    definitions(kind).prompt_appends
}

/// Whether `kind` runs models named by other providers' vocabularies.
pub fn runs_other_providers_models(kind: &str) -> bool {
    definitions(kind).runs_other_providers_models
}

/// Whether `kind`, named in `agent:`, keeps `model` though the model's id routes to another
/// kind: the kind runs every provider's models, or the id carries one of its foreign prefixes.
pub(crate) fn keeps_model(kind: &str, model: &str) -> bool {
    let definition = definitions(kind);
    definition.runs_other_providers_models
        || definition
            .foreign_prefixes
            .iter()
            .any(|(prefix, _)| model.starts_with(prefix))
}

pub(crate) fn render_tool_args(
    kind: &str,
    tools: Option<&ToolSet>,
) -> Result<Vec<String>, ToolErr> {
    match (definitions(kind).tools, tools) {
        (DefinitionTools::Required(render), tools) => render(tools.ok_or(ToolErr::Missing)?, &[]),
        (DefinitionTools::Optional(render), Some(tools)) => Ok(render(tools, &[]).0),
        (DefinitionTools::RequiredFile(_), tools) => {
            tools.map(|_| Vec::new()).ok_or(ToolErr::Missing)
        }
        _ => Ok(Vec::new()),
    }
}

/// A launch's tool argv, with the kind's code-mode tools or argv when `code_mode` is on, and
/// its off argv when the kind switches off by argv.
pub(crate) fn render_launch_tool_args(
    kind: &str,
    tools: Option<&ToolSet>,
    code_mode: Option<bool>,
) -> Result<(Vec<String>, Vec<ToolNotice>), ToolErr> {
    let switch = spec_by_kind(kind).map(|spec| spec.launch.presets.code_mode);
    let switch_args = match (switch, code_mode) {
        (Some(CodeModeSwitch::Args { on, .. }), Some(true)) => on,
        (Some(CodeModeSwitch::Args { off, .. }), Some(false)) => off,
        _ => &[],
    };
    let code_mode = code_mode == Some(true);
    let Some(tools) = tools else {
        let argv = match switch {
            Some(CodeModeSwitch::Supported { without_tools, .. }) if code_mode => without_tools,
            _ => switch_args,
        };
        return Ok((
            argv.iter().map(|arg| (*arg).to_owned()).collect(),
            Vec::new(),
        ));
    };
    let extra = switch.map_or(&[][..], |switch| switch.tools(code_mode));
    let (mut argv, notices) = match definitions(kind).tools {
        DefinitionTools::Required(render) => (render(tools, extra)?, Vec::new()),
        DefinitionTools::Optional(render) => render(tools, extra),
        DefinitionTools::RequiredFile(render) => (Vec::new(), render(tools, extra)),
        DefinitionTools::Unsupported => (Vec::new(), vec![ToolNotice::NotApplied]),
        DefinitionTools::Ignored => (Vec::new(), Vec::new()),
    };
    argv.extend(switch_args.iter().map(|arg| (*arg).to_owned()));
    Ok((argv, notices))
}

/// The entries of a launch's `tools` list that a `kind` launch applies: the list less the
/// entries whose base its argv drops, or `None` where the kind applies no list and runs its own
/// default.
pub fn applied_tools(kind: &str, tools: &[String]) -> Option<Vec<String>> {
    let notices = ToolSet::parse(tools)
        .and_then(|set| render_launch_tool_args(kind, Some(&set), None))
        .map_or_else(|_| Vec::new(), |(_, notices)| notices);
    if matches!(definitions(kind).tools, DefinitionTools::Ignored)
        || notices.contains(&ToolNotice::NotApplied)
    {
        return None;
    }
    let dropped: Vec<&String> = notices
        .iter()
        .flat_map(|notice| match notice {
            ToolNotice::Dropped(bases) => bases.as_slice(),
            _ => &[],
        })
        .collect();
    Some(
        tools
            .iter()
            .filter(|entry| {
                let base = entry
                    .split_once('(')
                    .map_or(entry.as_str(), |(base, _)| base);
                !dropped.iter().any(|name| *name == base.trim())
            })
            .cloned()
            .collect(),
    )
}

#[cfg(test)]
mod tests;
