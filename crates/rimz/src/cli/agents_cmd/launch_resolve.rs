//! Shared CLI launch resolution, override parsing, and validation.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rimz::agents::{LaunchPreset, PermissionMode};
use rimz::config::{MachineConfig, effective::LaunchAgents};
use rimz::harness::budget::BudgetSpec;
use rimz::harness::plan::{LaunchFinalizeOptions, LaunchFinalizeWarning, ResolvedLaunch};
use rimz::harness::spec::LayoutSpec;
use rimz::store::snapshot::SidebarSnapshot;

use super::LaunchOverrideArgs;

#[derive(Debug)]
pub(super) struct FinalizedLaunch {
    pub(super) resolved: ResolvedLaunch,
    pub(super) preset: LaunchPreset,
    pub(super) warnings: Vec<LaunchFinalizeWarning>,
    pub(super) inferred_lane: Option<String>,
    pub(super) qualified_spec: Option<String>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared launch inputs keep CLI overrides and optional room context explicit"
)]
pub(super) fn resolve_finalized_layout(
    snapshot: Option<&SidebarSnapshot>,
    machine_config: &MachineConfig,
    effective: &LaunchAgents,
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrideArgs,
    budget: Option<BudgetSpec>,
    max_turns: Option<u32>,
    lane: Option<&str>,
    enforce_name_cardinality: bool,
) -> Result<FinalizedLaunch> {
    // A bare role in a team's lane names that team's role, not a global profile.
    let mut qualified_spec = None;
    let mut inferred_lane = None;
    if let (Some(snapshot), Some(spec), Some(channel)) = (snapshot, spec, lane)
        && let Some(team) = rimz::address::channel_team(&snapshot.agents, channel)
    {
        let qualified = rimz::harness::spec::qualify_spec_in_channel(
            spec,
            channel,
            team,
            &effective.teams,
            &effective.profiles,
            &machine_config.agents.commands,
        )?;
        if let Cow::Owned(qualified) = qualified {
            qualified_spec = Some(qualified);
            inferred_lane = Some(channel.to_owned());
        }
    }
    let spec = qualified_spec.as_deref().or(spec);
    let mut resolved = rimz::harness::plan::resolve_launch(
        effective,
        rimz::config::effective::ProfileScope::Agents,
        &machine_config.agents.commands,
        spec,
        rimz::harness::plan::normalized_preset_value(overrides.agent.as_deref()).as_deref(),
    )?;
    let preset = validate_resolved_launch_inputs(
        spec,
        prompt,
        overrides,
        effective,
        &machine_config.agents.commands,
        &resolved.layout,
        enforce_name_cardinality,
    )?;
    let warnings = rimz::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: interactive_permission_mode_from_flags(overrides.ask, overrides.yolo)?,
            preset: &preset,
            passthrough: &overrides.passthrough,
            budget,
            max_turns,
        },
    )?;
    Ok(FinalizedLaunch {
        resolved,
        preset,
        warnings,
        inferred_lane,
        qualified_spec,
    })
}

pub(super) fn interactive_permission_mode_from_flags(
    ask: bool,
    yolo: bool,
) -> Result<Option<PermissionMode>> {
    if ask && yolo {
        bail!("choose at most one of --ask and --yolo");
    }
    Ok(if yolo {
        Some(PermissionMode::Yolo)
    } else if ask {
        Some(PermissionMode::Ask)
    } else {
        None
    })
}

/// Build the launch-override preset from shared launch flags. Prompt files are
/// resolved to absolute paths and required to exist here, at the entry point,
/// rather than downstream in the agent.
pub(super) fn launch_override_preset(
    overrides: &LaunchOverrideArgs,
) -> Result<rimz::agents::LaunchPreset> {
    let system_prompt_file = resolve_launch_prompt_file(
        overrides.system_prompt_file.as_deref(),
        "--system-prompt-file",
    )?;
    let append_system_prompt_files =
        resolve_launch_prompt_files(&overrides.append_system_prompt_files)?;
    Ok(rimz::agents::LaunchPreset {
        model: rimz::harness::plan::normalized_preset_value(overrides.model.as_deref()),
        effort: rimz::harness::plan::normalized_preset_value(overrides.effort.as_deref()),
        auto_compact: None,
        system_prompt_file,
        append_system_prompt_files,
    })
}

pub(super) fn resolve_launch_prompt_file(
    path: Option<&Path>,
    flag: &str,
) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let resolved = path
        .canonicalize()
        .with_context(|| format!("reading {flag} `{}`", path.display()))?;
    if !resolved.is_file() {
        bail!("{flag} `{}` is not a regular file", path.display());
    }
    Ok(Some(resolved))
}

pub(super) fn resolve_launch_prompt_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            resolve_launch_prompt_file(Some(path), "--append-system-prompt-file")
                .map(|path| path.expect("a supplied path resolves to one path"))
        })
        .collect()
}

/// Apply CLI-owned launch validation in its user-visible precedence order.
fn validate_resolved_launch_inputs(
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrideArgs,
    effective: &rimz::config::effective::LaunchAgents,
    commands: &rimz::config::CommandsConfig,
    layout: &LayoutSpec,
    enforce_name_cardinality: bool,
) -> Result<rimz::agents::LaunchPreset> {
    rimz::harness::plan::reject_prompt_that_looks_like_spec(
        spec,
        prompt,
        &effective.profiles,
        commands,
        &effective.teams,
    )?;
    if enforce_name_cardinality && layout.agent_kinds().count() != 1 {
        bail!("--name requires a layout with exactly one agent cell");
    }
    launch_override_preset(overrides)
}
