use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use rimz::config::Isolation;
use rimz::config::definitions::{self, LoadedDefinitions, SkillCheck};

use crate::cli::render;

pub(super) fn run(json: bool) -> Result<()> {
    let home = rimz::disk::paths::agents_home();
    let machine = crate::cli::machine_config();
    let env = rimz::agents::ambient_env();
    let library = home.join("skills");
    let in_sandbox = Isolation::ambient(&env).is_some_and(Isolation::is_scoped);
    let check = if !in_sandbox {
        SkillCheck::Check {
            env: &env,
            library: &library,
            machine_isolation: machine.agents.isolation,
        }
    } else {
        SkillCheck::Skip
    };
    if in_sandbox {
        writeln!(
            render::err(),
            "rimz: skill listings were not checked because this shell runs inside a scoped view; run `rimz agents validate` from a host shell"
        )?;
    }
    let loaded = load(&home, check, &machine);
    let mut warnings = warnings(&loaded, machine.agents.isolation, &machine.tiers);
    warnings.extend(lsp_warnings(&loaded, &machine));
    warnings.extend(keep_warm_warnings(&loaded, &machine.harness));
    if json {
        render::json_pretty(&serde_json::json!({
            "rows": loaded.rows,
            "warnings": warnings,
            "errors": loaded.errors.iter().map(|error| serde_json::json!({
                "path": error.path, "message": error.message,
            })).collect::<Vec<_>>(),
        }))?;
    } else {
        let mut out = render::out();
        for namespace in ["agents", "subagents", "team"] {
            writeln!(
                out,
                "{}",
                if namespace == "team" {
                    "teams"
                } else {
                    namespace
                }
            )?;
            if namespace == "team" {
                for (name, team) in &loaded.teams.0 {
                    writeln!(
                        out,
                        "  {name}: stages {}; leader {}",
                        team.stages.join(" → "),
                        team.leader.as_deref().unwrap_or("—")
                    )?;
                    rows(
                        &mut out,
                        loaded
                            .rows
                            .iter()
                            .filter(|row| row.team.as_deref() == Some(name)),
                    )?;
                }
            } else {
                rows(
                    &mut out,
                    loaded.rows.iter().filter(|row| row.namespace == namespace),
                )?;
            }
        }
        for error in &loaded.errors {
            writeln!(out, "{error}")?;
        }
        for warning in &warnings {
            writeln!(
                out,
                "{}: warning: {}",
                warning.path.display(),
                warning.message
            )?;
        }
    }
    if !loaded.errors.is_empty() {
        bail!("{} definition error(s)", loaded.errors.len());
    }
    Ok(())
}

/// The definitions as `validate` prints them, plus every error the launch path's
/// own config load recorded, so validate never passes a set a launch refuses.
fn load(
    home: &Path,
    check: SkillCheck<'_>,
    machine: &rimz::config::MachineConfig,
) -> LoadedDefinitions {
    let mut loaded = definitions::load(
        home,
        check,
        &machine.agents.commands,
        &machine.tiers,
        machine.harness.auto_compact,
    );
    for error in &machine.notices.definition_errors {
        if !loaded
            .errors
            .iter()
            .any(|seen| seen.path == error.path && seen.message == error.message)
        {
            loaded.errors.push(error.clone());
        }
    }
    loaded
}

#[derive(serde::Serialize)]
struct Warning {
    path: std::path::PathBuf,
    message: String,
}

fn warnings(
    loaded: &LoadedDefinitions,
    machine: Isolation,
    tiers: &rimz::config::tiers::TierConfig,
) -> Vec<Warning> {
    let mut warnings = Vec::new();
    for row in &loaded.rows {
        let profiles = if row.namespace == "subagents" {
            &loaded.subagent_profiles
        } else {
            &loaded.agent_profiles
        };
        let Some(profile) = profiles.0.get(&row.name) else {
            continue;
        };
        if let (Some(tier), Some(renders)) = (&profile.model_tier, &profile.definition_renders) {
            for (family, reason) in &renders.exclusions {
                if let Some((listed_tier, _, model)) =
                    tiers.entries(tier.tier).find(|(_, kind, _)| kind == family)
                {
                    warnings.push(Warning {
                        path: row.source.clone(),
                        message: format!(
                            "`{listed_tier}` lists {model} ({family}), which cannot run this definition: {reason}"
                        ),
                    });
                }
            }
        }
        let Some(adapter) = rimz::agents::find_definition(&row.kind) else {
            continue;
        };
        if profile.isolation == Some(Isolation::Sandbox) && !adapter.has_native_sandbox() {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::NoSandboxFlag {
                    kind: rimz::ids::AgentKind::new_unchecked(&row.kind),
                }
                .to_string(),
            });
        }
        if profile.skills.is_some()
            && !Isolation::resolve(None, profile.isolation, machine).is_scoped()
            && matches!(
                adapter.spec().host_skills,
                rimz::agents::skills::HostSkills::Unsupported
            )
        {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::HostSkillsUnenforced {
                    kind: rimz::ids::AgentKind::new_unchecked(&row.kind),
                }
                .to_string(),
            });
        }
        if profile
            .allowed_tools
            .as_ref()
            .is_some_and(|rules| !rules.is_empty())
            && matches!(
                adapter.spec().tool_rules,
                rimz::agents::skills::ToolRules::Unsupported
            )
        {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::ToolRulesUnsupported {
                    kind: rimz::ids::AgentKind::new_unchecked(&row.kind),
                }
                .to_string(),
            });
        }
    }
    warnings
}

fn rows<'a>(
    out: &mut impl Write,
    rows: impl Iterator<Item = &'a definitions::DefinitionRow>,
) -> Result<()> {
    let mut table = render::Table::new(["NAME", "KIND", "MODEL", "EFFORT", "KEEP-WARM", "SOURCE"]);
    for row in rows {
        table.row([
            render::cell(&row.name).fg(render::palette::identity(&row.kind)),
            render::cell(&row.kind).fg(render::palette::identity(&row.kind)),
            render::cell(crate::cli::profile_report::model_label(
                row.model.as_deref().unwrap_or("—"),
                row.tier,
                row.tier_fallback,
            )),
            render::cell(effort_label(row)),
            render::cell(row.keep_warm.as_ref().map_or_else(
                || "—".to_owned(),
                |policy| {
                    format!(
                        "max {}; while {}",
                        policy
                            .max
                            .map_or_else(|| "—".to_owned(), |max| max.to_string()),
                        policy.while_clause.as_deref().unwrap_or("—")
                    )
                },
            )),
            render::cell(row.source.display().to_string()),
        ]);
    }
    table.render(out)?;
    Ok(())
}

/// The EFFORT cell: the effort, with `fast` appended when the definition
/// launches in the provider's fast mode.
fn effort_label(row: &definitions::DefinitionRow) -> String {
    let effort = row.effort.as_deref().unwrap_or("—");
    if row.fast {
        format!("{effort} · fast")
    } else {
        effort.to_owned()
    }
}

/// A `keep-warm` horizon that will not hold, with the setting that decides it:
/// a `subagents` definition (no seat reads it), cache pings switched off, a static
/// lifetime that is off or shorter than the floor, or a horizon the ping cap cuts
/// short. Sign-in modes are checked separately; an agent's writes can override them.
/// A while-only policy needs a finite ping cap.
fn keep_warm_warnings(
    loaded: &LoadedDefinitions,
    harness: &rimz::config::HarnessConfig,
) -> Vec<Warning> {
    use rimz::agents::context::SignIn;
    use rimz::config::{KeepWarm, LifetimeSource};
    let mut warnings = Vec::new();
    for row in &loaded.rows {
        let Some(setting) = &row.keep_warm else {
            continue;
        };
        if let (None, Some(clause), None) = (
            setting.max,
            &setting.while_clause,
            harness.cache_keepalive_max,
        ) {
            warnings.push(Warning {
                path: row.source.clone(),
                message: format!("{}: keep-warm while `{clause}` holds nothing: `harness.cache_keepalive_max` is off; set `max`", row.name),
            });
        }
        let message = if row.team.is_none() && row.namespace == "subagents" {
            Some(
                "keep-warm has no effect on a subagents definition; set it on the agents definition or team role the seat launches from".to_owned()
            )
        } else if !harness.cache_keepalive {
            Some(
                "keep-warm is ignored: `harness.cache_keepalive` is off; set it to `true`"
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some(message) = message {
            warnings.push(Warning {
                path: row.source.clone(),
                message,
            });
            continue;
        }
        let kind = rimz::ids::AgentKind::new_unchecked(&row.kind);
        let lifetimes = [None, Some(SignIn::Subscription), Some(SignIn::ApiKey)].map(|sign_in| {
            (
                sign_in.map_or("unknown sign-in", SignIn::as_str),
                harness.configured_lifetime(&kind, sign_in),
            )
        });
        let differs = lifetimes.iter().any(|(_, value)| *value != lifetimes[0].1);
        let mut admits_hold = false;
        for (label, lifetime) in &lifetimes[..if differs { 3 } else { 1 }] {
            let kind = if differs {
                format!("{kind} {label}")
            } else {
                kind.to_string()
            };
            let message = match lifetime.ttl {
                None if lifetime.source == LifetimeSource::Fallback => format!(
                    "keep-warm is ignored: no prompt-cache lifetime is known for {kind}; set `[harness.prompt_cache_ttl] {}`",
                    row.kind,
                ),
                None => format!(
                    "keep-warm is ignored: {kind}'s prompt-cache lifetime is off; set `[harness.prompt_cache_ttl] {}`",
                    row.kind,
                ),
                Some(ttl) => {
                    let Some(floor) = harness.keep_warm_min_ttl.filter(|floor| ttl < *floor) else {
                        admits_hold |= ttl > harness.prompt_cache_margin;
                        continue;
                    };
                    format!(
                        "keep-warm is ignored: {kind}'s prompt-cache lifetime {} is below `harness.keep_warm_min_ttl` ({}); lower the floor or set it to `off`",
                        KeepWarm::For(ttl),
                        KeepWarm::For(floor),
                    )
                }
            };
            warnings.push(Warning {
                path: row.source.clone(),
                message: format!(
                    "{message}; this checks settings and defaults only; an agent's own cache writes can change the lifetime"
                ),
            });
        }
        if let (true, Some(KeepWarm::For(horizon)), Some(max)) =
            (admits_hold, setting.max, harness.cache_keepalive_max)
            && horizon > max
        {
            warnings.push(Warning {
                path: row.source.clone(),
                message: format!(
                    "keep-warm {} is capped at `harness.cache_keepalive_max` ({}): pings stop that long after the agent's last request; raise the maximum or set it to `off`",
                    KeepWarm::For(horizon),
                    KeepWarm::For(max),
                ),
            });
        }
    }
    warnings
}

fn lsp_warnings(loaded: &LoadedDefinitions, machine: &rimz::config::MachineConfig) -> Vec<Warning> {
    if machine.lsp.servers.is_empty() {
        return Vec::new();
    }
    loaded
        .rows
        .iter()
        .filter(|row| {
            let profiles = if row.namespace == "subagents" {
                &loaded.subagent_profiles
            } else {
                &loaded.agent_profiles
            };
            let Some(profile) = profiles.0.get(&row.name) else {
                return false;
            };
            if rimz::agents::skills_lack_skill_tool(&row.kind, profile.tools.as_ref())
                || !rimz::config::skill_is_callable(profile.skills.as_deref(), "rimz-lsp")
            {
                return false;
            }
            loaded
                .tools
                .get(&row.name)
                .is_some_and(|tools| tools.iter().any(|tool| tool == "LSP"))
        })
        .map(|row| Warning {
            path: row.source.clone(),
            message: format!(
                "profile {}: tools names LSP, which a room with a shared language server strips",
                row.name
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests;
