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
    let in_sandbox = Isolation::ambient(&env).is_some_and(Isolation::is_scoped);
    let check = if !in_sandbox {
        SkillCheck::Check {
            env: &env,
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
    let mut loaded = load(&home, check, &machine);
    let chain = rimz::agents::skills::SkillChain::from_home(&home);
    loaded
        .errors
        .extend(capability_errors(&loaded, &machine, |kind| {
            (!in_sandbox).then(|| {
                rimz::agents::skills::rimz_skills(
                    rimz::agents::find_definition(kind)
                        .and_then(|definition| definition.skills_home(&env))
                        .as_deref(),
                    Some(&chain),
                )
            })
        }));
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
                rimz::packs::one_line(&warning.path.display().to_string()),
                rimz::packs::one_line(&warning.message)
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
        &rimz::config::MachineConfig::config_path(),
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
    let mut warnings: Vec<_> = loaded
        .warnings
        .iter()
        .map(|warning| Warning {
            path: warning.path.clone(),
            message: warning.message.clone(),
        })
        .collect();
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
            let engine = renders
                .preference
                .family
                .as_deref()
                .filter(|kind| rimz::agents::runs_other_providers_models(kind));
            for (family, reason) in &renders.exclusions {
                if engine.is_some_and(|engine| engine != family) {
                    continue;
                }
                if let Some((listed_tier, _, model)) =
                    tiers.entries(tier.tier).find(|(_, kind, _)| {
                        engine == Some(family.as_str()) || *kind == Some(family.as_str())
                    })
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
        let kind = rimz::ids::AgentKind::new_unchecked(&row.kind);
        let mode_args = profile
            .mode
            .map(|mode| adapter.spec().launch.permission_args(mode))
            .unwrap_or_default();
        if profile.isolation == Some(Isolation::Sandbox) && !adapter.has_native_sandbox() {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::NoSandboxFlag {
                    kind: kind.clone(),
                }
                .to_string(),
            });
        } else if Isolation::resolve(None, profile.isolation, machine) == Isolation::Sandbox
            && adapter.native_sandbox_overridden(&mode_args)
        {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::SandboxOverridden {
                    kind: kind.clone(),
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
                    kind: kind.clone(),
                }
                .to_string(),
            });
        }
        for rule in adapter
            .spec()
            .tool_rules
            .untranslated(profile.allowed_tools.as_deref().unwrap_or_default())
        {
            warnings.push(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::ToolRuleUntranslated {
                    kind: kind.clone(),
                    rule: rule.clone(),
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
    let mut table = render::Table::new([
        "NAME",
        "KIND",
        "MODEL",
        "EFFORT",
        "KEEP-WARM",
        "ORIGIN",
        "SOURCE",
    ]);
    for row in rows {
        table.row([
            render::cell(rimz::packs::one_line(&row.name)).fg(render::palette::identity(&row.kind)),
            render::cell(&row.kind).fg(render::palette::identity(&row.kind)),
            render::cell(rimz::packs::one_line(
                &crate::cli::profile_report::model_label(
                    row.model.as_deref().unwrap_or("—"),
                    row.tier,
                    row.tier_fallback,
                ),
            )),
            render::cell(effort_label(row)),
            render::cell(row.keep_warm.as_ref().map_or_else(
                || "—".to_owned(),
                |policy| {
                    rimz::packs::one_line(&format!(
                        "max {}; while {}",
                        policy
                            .max
                            .map_or_else(|| "—".to_owned(), |max| max.to_string()),
                        policy.while_clause.as_deref().unwrap_or("—")
                    ))
                },
            )),
            render::cell(crate::cli::profile_report::origin_label(&row.origin)),
            render::cell(rimz::packs::one_line(&row.source.display().to_string())),
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
/// A while with no resolved maximum needs a finite ping cap.
fn keep_warm_warnings(
    loaded: &LoadedDefinitions,
    harness: &rimz::config::HarnessConfig,
) -> Vec<Warning> {
    use rimz::agents::context::SignIn;
    use rimz::config::{KeepWarm, KeepWarmFields, LifetimeSource};
    let mut warnings = Vec::new();
    for row in &loaded.rows {
        let setting = if let Some(team) = &row.team {
            loaded.teams.0.get(team).and_then(|team| {
                team.keep_warm_with_default(row.role.as_deref(), harness.keep_warm)
            })
        } else if row.namespace == "subagents" {
            row.keep_warm.clone()
        } else {
            let max = loaded
                .agent_profiles
                .keep_warm(Some(&row.name), harness.keep_warm);
            (max != KeepWarm::Off).then_some(KeepWarmFields {
                max: Some(max),
                while_clause: None,
            })
        };
        let Some(setting) = setting else {
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

/// The profile behind each definition row `validate` judges on its own: seat rows
/// are skipped, since their definition's row carries what the definition does,
/// and a kind base, which has a row in each table, is judged once, on its agents row.
fn own_rows(
    loaded: &LoadedDefinitions,
) -> impl Iterator<Item = (&definitions::DefinitionRow, &rimz::config::Profile)> {
    loaded
        .rows
        .iter()
        .filter(|row| row.namespace != "team")
        .filter_map(|row| {
            let profiles = if row.namespace == "subagents" {
                &loaded.subagent_profiles
            } else {
                &loaded.agent_profiles
            };
            let profile = profiles.0.get(&row.name)?;
            (!(profile.kind_base && row.namespace == "subagents")).then_some((row, profile))
        })
}

/// What shuts a profile out of RimZ capability `name`, judged offline by the
/// machine config and the profile's `tools`, `skills`, and `mcps`: the reasons
/// its MCP tools are out of reach, then the reasons its skill is. An empty side is
/// a way in; the skill side does not look at the disk. A profile whose tools carry
/// a `capabilities:` grant reaches only what the grant names, on either side.
fn shut_out(
    machine: &rimz::config::MachineConfig,
    kind: &str,
    profile: &rimz::config::Profile,
    subagent: bool,
    name: &str,
) -> (Vec<String>, Vec<String>) {
    use rimz::mcp::catalog::Capability;
    if subagent && name == Capability::Subagents.name() {
        let reason = vec!["a subagent cannot launch subagents".to_owned()];
        return (reason.clone(), reason);
    }
    let grant = profile.tools.as_ref().and_then(rimz::agents::ToolSet::rimz);
    if let Some(grant) = grant
        && !grant.iter().any(|entry| entry.name() == name)
    {
        let reason = vec![format!("its capabilities leave out {name}")];
        return (reason.clone(), reason);
    }
    let tool = match Capability::from_name(name) {
        None => vec![format!("{name} has no MCP tools")],
        Some(Capability::Loop) if subagent => {
            vec!["a subagent is not served the rimz-loop tools".to_owned()]
        }
        Some(_) if !machine.agents.rimz_mcp_server => {
            vec!["the machine's rimz-mcp-server is off".to_owned()]
        }
        Some(_)
            if !rimz::agents::find_definition(kind)
                .is_some_and(|definition| definition.takes_mcp_servers()) =>
        {
            vec![format!("{kind} takes no MCP servers")]
        }
        Some(capability)
            if grant.is_none()
                && !rimz::config::mcp_capabilities(profile.mcps.as_deref())
                    .contains(&capability) =>
        {
            vec![format!("its mcps leave out {name}")]
        }
        Some(_) => Vec::new(),
    };
    let skill = if rimz::agents::skills_lack_skill_tool(kind, profile.tools.as_ref()) {
        vec!["its tools leave out Skill".to_owned()]
    } else if grant.is_none() && !rimz::config::skill_is_callable(profile.skills.as_deref(), name) {
        vec![format!("its skills leave out {name}")]
    } else {
        Vec::new()
    };
    (tool, skill)
}

/// Each `rimz(x)` in a profile's resolved prompt sources that names no capability
/// and no skill on disk for its kind, or that the profile cannot reach. `on_disk`
/// gives a kind's `rimz-*` skills, or `None` when the disk is not checked, and
/// then only the lists judge. The launch only warns for these, so they stay out of
/// the definition loader, whose errors refuse a launch.
fn capability_errors(
    loaded: &LoadedDefinitions,
    machine: &rimz::config::MachineConfig,
    on_disk: impl Fn(&str) -> Option<std::collections::BTreeSet<String>>,
) -> Vec<definitions::DefinitionErr> {
    let mut inventories = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    for (row, profile) in own_rows(loaded) {
        let names: std::collections::BTreeSet<String> = profile
            .system_prompt_file
            .iter()
            .chain(&profile.append_system_prompt_files)
            .filter_map(|source| match source {
                rimz::config::PromptSource::Text { text, .. } => Some(text.clone()),
                rimz::config::PromptSource::File(path) => std::fs::read_to_string(path).ok(),
            })
            .flat_map(|text| rimz::config::rimz_references(&text))
            .collect();
        let inventory = inventories
            .entry(row.kind.clone())
            .or_insert_with(|| on_disk(&row.kind));
        for name in names {
            let present = inventory
                .as_ref()
                .is_none_or(|skills| skills.contains(&name));
            let short = &name["rimz-".len()..];
            let known = rimz::mcp::catalog::Capability::from_name(&name).is_some() || present;
            let (tool, mut skill) = shut_out(
                machine,
                &row.kind,
                profile,
                row.namespace == "subagents",
                &name,
            );
            if skill.is_empty() && !present {
                skill.push(format!("no {name} skill is on disk for {}", row.kind));
            }
            let message = if !known {
                format!(
                    "profile {}: rimz({short}) names `{name}`, which is neither a RimZ \
                     capability nor a skill on disk for {}",
                    row.name, row.kind
                )
            } else if tool.is_empty() || skill.is_empty() {
                continue;
            } else {
                // A skills-only name is shut out by its skill side alone.
                let mut reasons = if rimz::mcp::catalog::Capability::from_name(&name).is_some() {
                    tool
                } else {
                    Vec::new()
                };
                reasons.extend(skill);
                reasons.dedup();
                format!(
                    "profile {}: rimz({short}) names `{name}`, which this profile cannot reach: {}",
                    row.name,
                    reasons.join("; ")
                )
            };
            errors.push(definitions::DefinitionErr {
                path: row.source.clone(),
                message,
                cause: definitions::DefinitionCause::Invalid,
            });
        }
    }
    errors
}

fn lsp_warnings(loaded: &LoadedDefinitions, machine: &rimz::config::MachineConfig) -> Vec<Warning> {
    if machine.lsp.servers.is_empty() {
        return Vec::new();
    }
    own_rows(loaded)
        .filter(|(row, profile)| {
            let (tool, skill) = shut_out(
                machine,
                &row.kind,
                profile,
                row.namespace == "subagents",
                rimz::mcp::catalog::Capability::Lsp.name(),
            );
            (tool.is_empty() || skill.is_empty())
                && profile
                    .tools
                    .as_ref()
                    .is_some_and(|tools| tools.bases().iter().any(|tool| tool == "LSP"))
        })
        .map(|(row, _)| Warning {
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
