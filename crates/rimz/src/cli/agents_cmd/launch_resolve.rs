//! CLI posture parsing for the library's read-only layout resolver.

use super::LaunchOverrideArgs;
use anyhow::{Result, bail};
use rimz::agents::PermissionMode;
use rimz::config::{
    MachineConfig,
    effective::{LaunchAgents, ProfileScope},
};
use rimz::harness::budget::BudgetSpec;
pub(super) use rimz::launcher::resolve::{
    FinalizedLaunch, resolve_launch_prompt_file, resolve_launch_prompt_files,
};
use rimz::store::snapshot::SidebarSnapshot;

pub(super) fn typed_overrides(
    overrides: &LaunchOverrideArgs,
) -> Result<rimz::launcher::LaunchOverrides> {
    Ok(rimz::launcher::LaunchOverrides {
        login: overrides.account.clone(),
        mode: interactive_permission_mode_from_flags(overrides.ask, overrides.yolo)?,
        tier: overrides.tier,
        agent: overrides.agent.clone(),
        preset: rimz::agents::LaunchPreset {
            model: rimz::harness::plan::normalized_preset_value(overrides.model.as_deref()),
            effort: rimz::harness::plan::normalized_preset_value(overrides.effort.as_deref()),
            fast: overrides.fast_override(),
            auto_compact: None,
            system_prompt_file: overrides.system_prompt_file.clone(),
            append_system_prompt_files: overrides.append_system_prompt_files.clone(),
        },
        isolation: overrides.isolation,
        passthrough: overrides.passthrough.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_finalized_layout(
    snapshot: Option<&SidebarSnapshot>,
    machine_config: &MachineConfig,
    effective: &LaunchAgents,
    scope: ProfileScope,
    spec: Option<&str>,
    prompt: Option<&str>,
    overrides: &LaunchOverrideArgs,
    budget: Option<BudgetSpec>,
    max_turns: Option<u32>,
    lane: Option<&str>,
    enforce_name_cardinality: bool,
    room: Option<(&rimz::RuntimePaths, &rimz::StatePaths)>,
) -> Result<FinalizedLaunch> {
    rimz::launcher::resolve::resolve_finalized_layout(
        snapshot,
        machine_config,
        effective,
        scope,
        spec,
        prompt,
        &typed_overrides(overrides)?,
        budget,
        max_turns,
        lane,
        enforce_name_cardinality,
        room,
    )
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

pub(super) fn resume_overrides<'a>(
    overrides: &'a LaunchOverrideArgs,
    machine: &'a MachineConfig,
    unavailable: &'a dyn Fn(&str, &str) -> Option<rimz::agents::TierSkipReason>,
) -> Result<rimz::harness::resume::ResumeOverrides<'a>> {
    Ok(rimz::harness::resume::ResumeOverrides {
        team: None,
        permission_mode: interactive_permission_mode_from_flags(overrides.ask, overrides.yolo)?,
        preset: launch_override_preset(overrides)?,
        agent: overrides.agent.as_deref(),
        tier: overrides.tier.map(|tier| (tier, &machine.tiers)),
        unavailable: Some(unavailable),
    })
}

pub(super) fn launch_override_preset(
    overrides: &LaunchOverrideArgs,
) -> Result<rimz::agents::LaunchPreset> {
    rimz::launcher::resolve::launch_override_preset(&typed_overrides(overrides)?)
}
