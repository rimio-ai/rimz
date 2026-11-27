//! What a `rimz subagents` caller may launch.

use crate::agents::AgentState;
use crate::config::effective::{ProfileScope, resolve_seat_setting};
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
    /// The caller's profile names no subagent (`subagents = []`), so every launch is refused.
    NoneAllowed,
    /// Nothing to list: an allowlist naming nothing loaded, or no allowlist and nothing in the
    /// home to launch.
    Disabled,
    /// Never empty: `catalog` returns `Disabled` when nothing survives its filter.
    Available(Vec<SubagentProfile>),
}

/// Kind bases serve both namespaces. Their variants are listed beside them, or when explicitly
/// allowed; a home with neither profiles nor commands otherwise lists nothing.
pub fn catalog(
    caller_profile: Option<&str>,
    caller_agent: Option<&str>,
    caller_kind: Option<&str>,
    scope: ProfileScope,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> SubagentCatalog {
    let allowed = allowed_specs(
        caller_profile,
        caller_agent,
        caller_kind,
        scope,
        profiles,
        subagent_profiles,
    );
    if allowed.as_ref().is_some_and(Vec::is_empty) {
        return SubagentCatalog::NoneAllowed;
    }

    let mut specs = subagent_profiles
        .0
        .iter()
        .map(|(name, profile)| SubagentProfile {
            name: name.clone(),
            source: SubagentProfileSource::Profile,
            agent: Some(profile.agent.clone()),
            model: profile.model.clone(),
            effort: crate::harness::spec::listed_tuning(name, subagent_profiles).0,
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
    for runtime in crate::agents::runtime_names().filter_map(crate::agents::find_runtime) {
        if runtime.variant.is_none() || specs.iter().any(|spec| spec.name == runtime.name()) {
            continue;
        }
        let base = subagent_profiles.0.get(runtime.kind());
        if base.is_none()
            && !allowed
                .as_ref()
                .is_some_and(|allowed| allowed.iter().any(|name| name == runtime.name()))
        {
            continue;
        }
        specs.push(SubagentProfile {
            name: runtime.name().to_owned(),
            source: SubagentProfileSource::Profile,
            agent: Some(runtime.kind().to_owned()),
            model: base.and_then(|base| base.model.clone()),
            effort: base.and_then(|_| {
                crate::harness::spec::listed_tuning(runtime.name(), subagent_profiles).0
            }),
            description: base.and_then(|base| base.description.clone()),
        });
    }
    if let Some(allowed) = allowed {
        specs.retain(|spec| {
            allowed
                .iter()
                .any(|name| name == &spec.name || subagent_profiles.key(name) == spec.name)
        });
    }
    if specs.is_empty() {
        return SubagentCatalog::Disabled;
    }
    SubagentCatalog::Available(specs)
}

const AVAILABLE_PREAMBLE: &str = concat!(
    "### Subagents\n\n",
    "Whether you could do a piece of work yourself settles nothing; you could do all of it. ",
    "What decides is what the work leaves in your window: when you need its result but not the ",
    "output behind it (a gate run, a log, a sweep across files, a long command), a profile below ",
    "takes it. The launch costs you one turn, and the output you read yourself costs you every turn after.\n\n",
);

/// The Subagents paragraph, naming `via`, the tool or skill the launch launches subagents
/// through.
pub(super) fn reminder(catalog: &SubagentCatalog, via: &str) -> String {
    match catalog {
        SubagentCatalog::Disabled => "### Subagents\n\nNo subagent profile is configured for you, so none is listed here. Do the work yourself with your direct tools.".to_owned(),
        SubagentCatalog::NoneAllowed => "### Subagents\n\nSubagents are disabled for this agent: its profile allows none, so any subagent launch is refused. Do the work yourself with your direct tools.".to_owned(),
        SubagentCatalog::Available(specs) => {
            let list = specs
                .iter()
                .map(reminder_profile)
                .collect::<Vec<_>>()
                .join("\n");
            // The skill owns the how (batching, the settle digest, joins); this only says what.
            format!("{AVAILABLE_PREAMBLE}Launch them through {via}. `subagent(x)` in any text means the profile `x` in this list:\n\n{list}")
        }
    }
}

/// Name and description only: bare model codenames and effort levels do not tell the caller
/// which profile is cheaper or stronger, so the description carries that choice.
fn reminder_profile(profile: &SubagentProfile) -> String {
    let description = profile
        .description
        .as_deref()
        .map(|description| format!(": {description}"));
    format!(
        "- `{}`{}",
        profile.name,
        description.as_deref().unwrap_or_default()
    )
}

/// The effective `subagents:` allowlist for a caller, resolved in launch chain order under
/// its recorded `--agent` and kind.
pub fn allowed_specs(
    caller_profile: Option<&str>,
    caller_agent: Option<&str>,
    caller_kind: Option<&str>,
    scope: ProfileScope,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
) -> Option<Vec<String>> {
    resolve_seat_setting(
        caller_profile,
        caller_agent,
        caller_kind,
        scope,
        profiles,
        subagent_profiles,
        true,
        |profile| {
            // A Markdown definition listing the native Agent tool refuses any `subagents:`
            // list from lower layers (override or final kind base).
            if profile.definition.is_some()
                && profile
                    .tools
                    .as_ref()
                    .is_some_and(|tools| tools.has("Agent"))
            {
                Some(profile.subagents.clone())
            } else {
                profile.subagents.clone().map(Some)
            }
        },
    )
    .flatten()
}

pub fn check_allowed(
    caller: &AgentState,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
    profile: &str,
) -> crate::harness::spec::Result<()> {
    let scope = ProfileScope::for_launch(caller.profile_scope, caller.is_launched_child());
    let Some(allowed) = allowed_specs(
        caller.profile.as_deref(),
        caller
            .record
            .as_ref()
            .and_then(|record| record.agent.as_deref()),
        Some(caller.kind.as_str()),
        scope,
        profiles,
        subagent_profiles,
    ) else {
        return Ok(());
    };
    let requested = profile.trim();
    if allowed
        .iter()
        .any(|candidate| profiles.key(candidate) == profiles.key(requested))
    {
        return Ok(());
    }
    Err(LayoutErr::SubagentProfileNotAllowed {
        caller_profile: caller
            .profile
            .clone()
            .or_else(|| {
                caller
                    .record
                    .as_ref()
                    .and_then(|record| record.agent.clone())
            })
            .unwrap_or_else(|| caller.kind.to_string()),
        profile: requested.to_owned(),
        allowed: format!("[{}]", allowed.join(", ")),
    })
}

#[cfg(test)]
mod tests;
