//! Read-only layout resolution and typed launch overrides.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::agents::LaunchPreset;
use crate::config::{
    MachineConfig,
    effective::{LaunchAgents, ProfileScope},
};
use crate::harness::budget::BudgetSpec;
use crate::harness::plan::{
    LaunchFinalizeOptions, LaunchFinalizeWarning, PermissionModeChoice, ResolvedLaunch,
};
use crate::harness::spec::{Cell, LayoutSpec};
use crate::store::snapshot::SidebarSnapshot;
use anyhow::{Context, Result, bail};

use super::LaunchOverrides;

#[derive(Debug)]
pub struct FinalizedLaunch {
    pub profiles: crate::config::ProfilesConfig,
    pub resolved: ResolvedLaunch,
    pub preset: LaunchPreset,
    pub warnings: Vec<LaunchFinalizeWarning>,
    pub inferred_lane: Option<String>,
    pub qualified_spec: Option<String>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared launch inputs keep typed overrides and optional room context explicit"
)]
pub fn resolve_finalized_layout(
    snapshot: Option<&SidebarSnapshot>,
    machine_config: &MachineConfig,
    effective: &LaunchAgents,
    scope: ProfileScope,
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrides,
    budget: Option<BudgetSpec>,
    max_turns: Option<u32>,
    lane: Option<&str>,
    enforce_name_cardinality: bool,
    room: Option<(&crate::RuntimePaths, &crate::StatePaths)>,
) -> Result<FinalizedLaunch> {
    // A bare role in a team's lane names that team's role, not a global profile.
    let mut qualified_spec = None;
    let mut inferred_lane = None;
    if let (Some(snapshot), Some(spec), Some(channel)) = (snapshot, spec, lane)
        && let Some(team) = crate::address::channel_team(&snapshot.agents, channel)
    {
        let qualified = crate::harness::spec::qualify_spec_in_channel(
            spec,
            channel,
            team,
            &effective.teams,
            effective.profiles_for(scope),
            &machine_config.agents.commands,
        )?;
        if let Cow::Owned(qualified) = qualified {
            qualified_spec = Some(qualified);
            inferred_lane = Some(channel.to_owned());
        }
    }
    let spec = qualified_spec.as_deref().or(spec);
    let mut effective = effective.clone();
    let availability = room
        .filter(|_| overrides.login.is_none())
        .map(|(runtime, state)| {
            crate::harness::plan::LaunchAvailability::read(
                runtime,
                state,
                machine_config,
                jiff::Timestamp::now(),
            )
        });
    let routed = effective.route(
        &machine_config.tiers,
        scope,
        spec,
        overrides.tier,
        overrides.preset.model.as_deref(),
        overrides.agent.as_deref(),
        |kind, model| {
            availability
                .as_ref()
                .and_then(|usage| usage.unavailable(kind, model))
        },
    )?;
    let mut resolved = crate::harness::plan::resolve_launch(
        &effective,
        scope,
        &machine_config.agents.commands,
        spec,
        if routed {
            None
        } else {
            overrides.agent.as_deref()
        },
    )?;
    let preset = validate_resolved_launch_inputs(
        spec,
        prompt,
        overrides,
        &effective,
        scope,
        &machine_config.agents.commands,
        &resolved.layout,
        enforce_name_cardinality,
    )?;
    let warnings = crate::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            agent_base: if routed {
                None
            } else {
                overrides.agent.as_deref()
            },
            permission_mode: overrides.mode.map(PermissionModeChoice::Explicit),
            isolation: overrides.isolation,
            preset: &preset,
            passthrough: &overrides.passthrough,
            budget,
            max_turns,
        },
    )?;
    if let Some(login) = &overrides.login {
        for cell in resolved
            .layout
            .columns
            .iter_mut()
            .flat_map(|column| &mut column.rows)
        {
            let Cell::Agent(cell) = cell else {
                continue;
            };
            cell.launch.login = Some(login.clone());
        }
    }
    Ok(FinalizedLaunch {
        profiles: match scope {
            ProfileScope::Agents => effective.profiles,
            ProfileScope::Subagents => effective.subagent_profiles,
        },
        resolved,
        preset,
        warnings,
        inferred_lane,
        qualified_spec,
    })
}

/// Resolve the typed override preset. Prompt files are
/// resolved to absolute paths and required to exist here, at the entry point,
/// rather than downstream in the agent.
pub fn launch_override_preset(overrides: &LaunchOverrides) -> Result<LaunchPreset> {
    let mut preset = overrides.preset.clone();
    preset.system_prompt_file =
        resolve_launch_prompt_file(preset.system_prompt_file.as_deref(), "--system-prompt-file")?;
    preset.append_system_prompt_files =
        resolve_launch_prompt_files(&preset.append_system_prompt_files)?;
    Ok(preset)
}

pub fn resolve_launch_prompt_file(path: Option<&Path>, flag: &str) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let absolute = std::env::current_dir()
        .context("reading the caller cwd")?
        .join(path);
    let resolved = super::checkout::caller_host_path(&absolute, None)?
        .canonicalize()
        .with_context(|| format!("reading {flag} `{}`", path.display()))?;
    if !resolved.is_file() {
        bail!("{flag} `{}` is not a regular file", path.display());
    }
    Ok(Some(resolved))
}

pub fn resolve_launch_prompt_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            resolve_launch_prompt_file(Some(path), "--append-system-prompt-file")
                .map(|path| path.expect("a supplied path resolves to one path"))
        })
        .collect()
}

/// Apply layout launch validation in its user-visible precedence order.
#[expect(
    clippy::too_many_arguments,
    reason = "validation keeps the selected profile scope beside its effective configuration"
)]
fn validate_resolved_launch_inputs(
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrides,
    effective: &crate::config::effective::LaunchAgents,
    scope: ProfileScope,
    commands: &crate::config::CommandsConfig,
    layout: &LayoutSpec,
    enforce_name_cardinality: bool,
) -> Result<crate::agents::LaunchPreset> {
    crate::harness::plan::reject_prompt_that_looks_like_spec(
        spec,
        prompt,
        effective.profiles_for(scope),
        commands,
        &effective.teams,
    )?;
    if enforce_name_cardinality && layout.agent_kinds().count() != 1 {
        bail!("--name requires a layout with exactly one agent cell");
    }
    if overrides.login.is_some() {
        let kinds = layout
            .agent_kinds()
            .collect::<std::collections::BTreeSet<_>>();
        if kinds.len() > 1 {
            bail!(
                concat!(
                    "--account names one provider's account, but this layout launches {}; ",
                    "launch each provider's cells separately, or switch the room with ",
                    "`rimz accounts use`"
                ),
                kinds.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
    }
    launch_override_preset(overrides)
}

use crate::harness::run::SupervisedRunRequest;
use crate::ids::AgentKind;
use crate::store::writer::LaunchLogin;

pub fn supervised_prompt<'a>(
    request: &'a SupervisedRunRequest,
    adapter: &crate::agents::AgentDefinition,
) -> Cow<'a, str> {
    if request.subagent && adapter.append_system_text_channel().is_none() {
        Cow::Owned(format!(
            "{}\n\n{}",
            request.prompt,
            crate::harness::launch_reminders::subagent_reminder()
        ))
    } else {
        Cow::Borrowed(&request.prompt)
    }
}

pub fn check_supervised_subagent_allowed(
    request: &SupervisedRunRequest,
    caller: Option<&crate::agents::AgentState>,
    profiles: &crate::config::ProfilesConfig,
) -> Result<()> {
    if !request.subagent {
        return Ok(());
    }
    let Some(caller) = caller else {
        return Ok(());
    };
    crate::harness::subagent_policy::check_allowed(
        caller,
        profiles,
        &request.spec,
        request.agent.as_deref(),
    )?;
    Ok(())
}

/// The account a supervised launch of `kind` runs under: the request's pin,
/// else a subagent's same-kind parent's account, else the room default.
pub fn launch_login(
    request: &SupervisedRunRequest,
    caller: Option<&crate::agents::AgentState>,
    kind: &AgentKind,
) -> LaunchLogin {
    if request.login != LaunchLogin::RoomDefault {
        return request.login.clone();
    }
    match caller.filter(|caller| request.subagent && &caller.kind == kind) {
        Some(caller) => LaunchLogin::Pinned(caller.login.clone().unwrap_or_default()),
        None => LaunchLogin::RoomDefault,
    }
}
