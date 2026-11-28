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
use crate::ids::LaneName;
use crate::store::snapshot::SidebarSnapshot;
use anyhow::{Context, Result, bail};

use super::LaunchOverrides;

#[derive(Debug)]
pub struct FinalizedLaunch {
    pub resolved: ResolvedLaunch,
    pub preset: LaunchPreset,
    pub warnings: Vec<LaunchFinalizeWarning>,
    pub inferred_lane: Option<LaneName>,
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
    lane: Option<&LaneName>,
    enforce_name_cardinality: bool,
    room: Option<(&crate::RuntimePaths, &crate::StatePaths)>,
) -> Result<FinalizedLaunch> {
    let mut selection = selection_of(machine_config, overrides);
    if let Some((runtime, state)) = room.filter(|_| overrides.login.is_none()) {
        let availability = crate::harness::plan::LaunchAvailability::read(
            runtime,
            state,
            machine_config,
            jiff::Timestamp::now(),
        );
        selection =
            selection.availability(move |kind, model| availability.unavailable(kind, model));
    }
    resolve_routed(
        snapshot,
        machine_config,
        effective,
        scope,
        spec,
        prompt,
        overrides,
        budget,
        max_turns,
        lane,
        enforce_name_cardinality,
        &selection,
    )
}

/// The launch's `--agent`, `--model` and `--tier` over the machine's tier table.
fn selection_of(
    machine_config: &MachineConfig,
    overrides: &LaunchOverrides,
) -> crate::config::Selection {
    crate::config::Selection::new(&machine_config.tiers)
        .agent(overrides.agent.as_deref())
        .model(overrides.preset.model.as_deref())
        .tier(overrides.tier)
}

/// Every agent kind a launch of `spec` can run on, each once, in layout order: the kinds its
/// first pick names, then each kind a tier route falls back to while every kind found so far
/// reads unavailable. A caller that must read one kind before any launch (a loop task's window
/// provider) refuses more than one.
#[expect(
    clippy::too_many_arguments,
    reason = "the same launch inputs `resolve_finalized_layout` takes, without a room"
)]
pub fn routable_kinds(
    snapshot: Option<&SidebarSnapshot>,
    machine_config: &MachineConfig,
    effective: &LaunchAgents,
    scope: ProfileScope,
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrides,
    budget: Option<BudgetSpec>,
    max_turns: Option<u32>,
    lane: Option<&LaneName>,
    enforce_name_cardinality: bool,
) -> Result<Vec<String>> {
    let mut kinds = Vec::<String>::new();
    loop {
        let seen = kinds.clone();
        let selection = selection_of(machine_config, overrides).availability(move |kind, _| {
            seen.iter()
                .any(|seen| seen == kind)
                .then_some(crate::agents::TierSkipReason::LoggedOut)
        });
        let finalized = resolve_routed(
            snapshot,
            machine_config,
            effective,
            scope,
            spec,
            prompt,
            overrides,
            budget,
            max_turns,
            lane,
            enforce_name_cardinality,
            &selection,
        )?;
        let found = kinds.len();
        for kind in finalized.resolved.layout.agent_kinds() {
            if !kinds.iter().any(|seen| seen == kind) {
                kinds.push(kind.to_owned());
            }
        }
        if kinds.len() == found {
            return Ok(kinds);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared launch inputs keep typed overrides and the routing's availability explicit"
)]
fn resolve_routed(
    snapshot: Option<&SidebarSnapshot>,
    machine_config: &MachineConfig,
    effective: &LaunchAgents,
    scope: ProfileScope,
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrides,
    budget: Option<BudgetSpec>,
    max_turns: Option<u32>,
    lane: Option<&LaneName>,
    enforce_name_cardinality: bool,
    selection: &crate::config::Selection,
) -> Result<FinalizedLaunch> {
    // A bare role in a team's lane names that team's role, not a global profile.
    let mut qualified_spec = None;
    let mut inferred_lane = None;
    if let (Some(snapshot), Some(spec), Some(channel)) = (snapshot, spec, lane)
        && let Some(team) = crate::address::channel_team(&snapshot.agents, channel)
    {
        let qualified = crate::harness::spec::qualify_spec_in_channel(
            spec,
            channel.as_str(),
            team,
            &effective.teams,
            effective.profiles_for(scope),
            &machine_config.agents.commands,
        )?;
        if let Cow::Owned(qualified) = qualified {
            qualified_spec = Some(qualified);
            inferred_lane = Some(channel.clone());
        }
    }
    let spec = qualified_spec.as_deref().or(spec);
    let mut resolved = crate::harness::plan::resolve_launch(
        effective,
        scope,
        &machine_config.agents.commands,
        spec,
        selection,
    )?;
    let preset = validate_resolved_launch_inputs(
        spec,
        prompt,
        overrides,
        effective,
        scope,
        &machine_config.agents.commands,
        &resolved.layout,
        enforce_name_cardinality,
    )?;
    let warnings = crate::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            smart_compact: Some(crate::harness::plan::LaunchCompactPolicy::new(
                effective,
                &machine_config.harness,
                scope,
                resolved.team_name.as_deref(),
            )),
            agent_base: overrides.agent.as_deref(),
            permission_mode: overrides
                .mode
                .map(PermissionModeChoice::Explicit)
                .or(overrides.default_mode.map(PermissionModeChoice::Default)),
            isolation: overrides.isolation,
            preset: &preset,
            passthrough: &overrides.passthrough,
            budget,
            max_turns,
            machine_config: Some(machine_config),
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

/// Whether the adapter's system text channel delivers the launch text to `cell`. An agent-file
/// channel carries it only for a launch whose prompt sources plan the file; a resumed
/// conversation keeps what its first launch wrote.
pub(super) fn launch_text_delivered(
    adapter: &crate::agents::AgentDefinition,
    cell: &crate::harness::spec::AgentCell,
) -> bool {
    match adapter.append_system_text_channel() {
        None => false,
        Some(crate::agents::capabilities::SystemTextChannel::AgentFile) => {
            !crate::harness::prompt_compose::SystemPromptSources::from_cell(cell).is_empty()
        }
        Some(_) => true,
    }
}

pub fn supervised_prompt<'a>(
    request: &'a SupervisedRunRequest,
    adapter: &crate::agents::AgentDefinition,
    cell: &crate::harness::spec::AgentCell,
) -> Cow<'a, str> {
    if request.subagent && !launch_text_delivered(adapter, cell) {
        Cow::Owned(format!(
            "{}\n\n{}",
            request.prompt,
            crate::harness::launch_reminders::subagent_reminder()
        ))
    } else {
        Cow::Borrowed(&request.prompt)
    }
}

/// A call's launch prompt `prompt` for leader `cell`: with no channel for the launch text, it
/// carries the call's reminder section (its Answer), which would otherwise reach no one. A retry's
/// prompt, built on the first attempt's, already carries it, so it carries it once.
pub(super) fn call_prompt(
    prompt: String,
    call: &crate::harness::call::CallSpec,
    adapter: &crate::agents::AgentDefinition,
    cell: &crate::harness::spec::AgentCell,
) -> String {
    let Some(section) = call.reminder() else {
        return prompt;
    };
    let reminder = crate::harness::launch_reminders::section_reminder(&section);
    if launch_text_delivered(adapter, cell) || prompt.contains(&reminder) {
        return prompt;
    }
    format!("{prompt}\n\n{reminder}")
}

pub fn check_supervised_subagent_allowed(
    request: &SupervisedRunRequest,
    caller: Option<&crate::agents::AgentState>,
    profiles: &crate::config::ProfilesConfig,
    subagent_profiles: &crate::config::ProfilesConfig,
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
        subagent_profiles,
        &request.spec,
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
