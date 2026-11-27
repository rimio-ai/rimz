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
    #[error("`{entry}` in `tools:` {reason}")]
    Refused { entry: String, reason: &'static str },
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

/// How a profile's lists reach a RimZ capability that also has a native tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RimzClaim {
    /// An entry names the capability, in `capabilities:`, `mcps`, or `skills`, or for
    /// `rimz-subagents` any `subagents:` list.
    Named,
    /// Only `rimz` in `capabilities:`, an `mcps` holding `*` or `rimz`, or no `mcps` at all.
    Blanket,
}

/// Which side gets a capability that both a tools list and RimZ's lists claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Contest {
    /// The launch keeps the native tool and gets neither the capability's MCP tools nor its
    /// skill.
    NativeWins,
    /// Two named entries contradict each other, so the definition is refused.
    Refused,
}

/// The provider tool `capability` stands in for: `AskUserQuestion` for `rimz-ask`, `Agent` for
/// `rimz-subagents`; `None` for every other capability.
pub(crate) fn native_tool(capability: crate::mcp::catalog::Capability) -> Option<&'static str> {
    use crate::mcp::catalog::Capability;
    match capability {
        Capability::Ask => Some("AskUserQuestion"),
        Capability::Subagents => Some("Agent"),
        _ => None,
    }
}

/// The native tool `capability` stands in for, where a tools list's `bases` name it and a
/// `kind` launch renders it (`applied_tools`): none on a kind that applies no tools list, nor
/// for a tool the kind's renderer drops, as Pi drops `Agent`.
pub(crate) fn native_claim(
    kind: &str,
    bases: &[String],
    capability: crate::mcp::catalog::Capability,
) -> Option<&'static str> {
    let tool = native_tool(capability).filter(|tool| bases.iter().any(|base| base == tool))?;
    applied_tools(kind, &[tool.to_owned()])
        .is_some_and(|applied| !applied.is_empty())
        .then_some(tool)
}

/// The outcome where a tools list names the native tool `native` and RimZ's lists also claim
/// its capability: the named native tool outranks a blanket grant, and two named entries refuse.
pub(crate) fn contest(native: Option<&str>, rimz: Option<RimzClaim>) -> Option<Contest> {
    native?;
    match rimz? {
        RimzClaim::Named => Some(Contest::Refused),
        RimzClaim::Blanket => Some(Contest::NativeWins),
    }
}

/// Whether a supplied tools list applies to this kind and excludes `Skill`.
pub fn skills_lack_skill_tool(kind: &str, tools: Option<&ToolSet>) -> bool {
    tools.is_some_and(|tools| !tools.has("Skill"))
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
