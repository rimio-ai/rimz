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
/// a `subagents` definition (no seat reads it), cache pings switched off, a
/// provider prompt-cache lifetime that is unknown or shorter than the floor, or a
/// horizon the keepalive maximum cuts short. A while-only policy needs a finite ping cap.
fn keep_warm_warnings(
    loaded: &LoadedDefinitions,
    harness: &rimz::config::HarnessConfig,
) -> Vec<Warning> {
    use rimz::config::KeepWarm;
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
        let kind = &row.kind;
        let ttl = harness.prompt_cache_ttl(&rimz::ids::AgentKind::new_unchecked(kind));
        let message = match ttl {
            _ if row.team.is_none() && row.namespace == "subagents" => {
                "keep-warm has no effect on a subagents definition; set it on the agents definition or team role the seat launches from".to_owned()
            }
            _ if !harness.cache_keepalive => {
                "keep-warm is ignored: `harness.cache_keepalive` is off; set it to `true`"
                    .to_owned()
            }
            None => format!(
                "keep-warm is ignored: no prompt-cache lifetime is known for {kind}; set `[harness.prompt_cache_ttl] {kind}`"
            ),
            Some(ttl) => match harness.keep_warm_min_ttl {
                Some(floor) if ttl < floor => format!(
                    "keep-warm is ignored: {kind}'s prompt-cache lifetime {} is below `harness.keep_warm_min_ttl` ({}); lower the floor or set it to `off`",
                    KeepWarm::For(ttl),
                    KeepWarm::For(floor),
                ),
                _ => match (setting.max, harness.cache_keepalive_max) {
                    (Some(KeepWarm::For(horizon)), Some(max)) if horizon > max => format!(
                        "keep-warm {} is capped at `harness.cache_keepalive_max` ({}): pings stop that long after the agent's last request; raise the maximum or set it to `off`",
                        KeepWarm::For(horizon),
                        KeepWarm::For(max),
                    ),
                    _ => continue,
                },
            },
        };
        warnings.push(Warning {
            path: row.source.clone(),
            message,
        });
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
mod tests {
    use super::*;

    #[test]
    fn keep_warm_rows_report_resolved_fields_and_warn_on_an_unbounded_while() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::create_dir(root.path().join("teams")).unwrap();
        for (name, fields) in [
            ("claude", ""),
            ("worker", "agent: claude\ntools: []\nkeep-warm: 2h\n"),
        ] {
            std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!("---\ndescription: Test\n{fields}---\nBase."),
            )
            .unwrap();
        }
        std::fs::write(root.path().join("teams/probe.md"), "---\nleader: lead\nstages: [Plan]\nkeep-warm: { while: 'team.stage!=Done' }\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]\n  - agent: worker\n    role: helper\n    keep-warm: 1h\n---\nPipeline.").unwrap();
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let rows_json = serde_json::to_value(&loaded.rows).unwrap();
        let row = |name| {
            rows_json
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .unwrap()
        };
        assert_eq!(
            row("worker")["keep_warm"],
            serde_json::json!({"max": "2h", "while": null})
        );
        assert_eq!(
            row("probe.lead")["keep_warm"],
            serde_json::json!({"max": null, "while": "team.stage!=Done"})
        );
        assert_eq!(
            row("probe.helper")["keep_warm"],
            serde_json::json!({"max": "1h", "while": "team.stage!=Done"})
        );
        let warnings = keep_warm_warnings(
            &loaded,
            &toml::from_str("cache_keepalive_max = 'off'").unwrap(),
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("holds nothing"));
        assert!(warnings[0].message.contains("set `max`"));
        for cap in ["6h", "off"] {
            for (config, expected) in [
                (
                    "cache_keepalive = false",
                    "`harness.cache_keepalive` is off",
                ),
                (
                    "[prompt_cache_ttl]\nclaude = 'off'",
                    "no prompt-cache lifetime is known",
                ),
                (
                    "[prompt_cache_ttl]\nclaude = '5m'",
                    "below `harness.keep_warm_min_ttl`",
                ),
            ] {
                let warnings = keep_warm_warnings(
                    &loaded,
                    &toml::from_str(&format!("cache_keepalive_max = '{cap}'\n{config}")).unwrap(),
                );
                let applicable: Vec<_> = warnings
                    .iter()
                    .filter(|warning| warning.message.contains(expected))
                    .collect();
                assert_eq!(
                    applicable.len(),
                    3,
                    "all max/while rows must warn: {config}"
                );
                assert_eq!(
                    applicable
                        .iter()
                        .filter(|warning| warning.path.ends_with("probe.md"))
                        .count(),
                    2,
                    "both the max role and the while-only role must warn"
                );
            }
        }
        let capped = keep_warm_warnings(
            &loaded,
            &toml::from_str("cache_keepalive_max = '30m'").unwrap(),
        );
        assert_eq!(capped.len(), 2, "only explicit maxima can exceed the cap");
        assert!(
            capped
                .iter()
                .all(|warning| warning.message.contains("is capped"))
        );
        let mut human = Vec::new();
        rows(&mut human, loaded.rows.iter()).unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(human.contains("team.stage!=Done"));
        assert!(human.contains("2h"));
    }

    #[test]
    fn keep_warm_warns_at_the_definition_for_an_unknown_or_short_cache_lifetime() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        for (name, fields) in [
            ("claude", ""),
            ("amp", ""),
            ("warm", "agent: claude\nkeep-warm: 2h\ntools: []\n"),
            ("unknown", "agent: amp\nkeep-warm: 2h\n"),
            ("cold", "agent: claude\nkeep-warm: off\ntools: []\n"),
        ] {
            std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!("---\ndescription: Test\n{fields}---\nBase."),
            )
            .unwrap();
        }
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let harness = |toml: &str| toml::from_str::<rimz::config::HarnessConfig>(toml).unwrap();

        let defaults = keep_warm_warnings(&loaded, &harness(""));
        assert_eq!(defaults.len(), 1, "a 60m Claude cache clears the 15m floor");
        assert!(defaults[0].path.ends_with("unknown.md"));
        assert!(
            defaults[0]
                .message
                .contains("set `[harness.prompt_cache_ttl] amp`"),
            "{}",
            defaults[0].message
        );

        let capped = keep_warm_warnings(&loaded, &harness("cache_keepalive_max = \"1h\""));
        let warm = capped
            .iter()
            .find(|warning| warning.path.ends_with("warm.md"))
            .expect("a horizon past the keepalive maximum warns");
        assert!(
            warm.message
                .contains("keep-warm 2h is capped at `harness.cache_keepalive_max` (1h)"),
            "{}",
            warm.message
        );
        assert_eq!(
            keep_warm_warnings(&loaded, &harness("cache_keepalive_max = \"off\"")).len(),
            1,
            "no maximum, no cap"
        );

        let short = "[prompt_cache_ttl]\nclaude = \"5m\"\namp = \"20m\"";
        let floored = keep_warm_warnings(&loaded, &harness(short));
        assert_eq!(floored.len(), 1, "an override makes amp known");
        assert!(floored[0].path.ends_with("warm.md"));
        assert!(
            floored[0]
                .message
                .contains("lifetime 5m is below `harness.keep_warm_min_ttl` (15m)"),
            "{}",
            floored[0].message
        );
        let open = format!("keep_warm_min_ttl = \"off\"\n{short}");
        assert!(keep_warm_warnings(&loaded, &harness(&open)).is_empty());

        let off = keep_warm_warnings(&loaded, &harness("cache_keepalive = false"));
        assert_eq!(off.len(), 2, "every set horizon is inert without pings");
        assert!(
            off.iter()
                .all(|warning| warning.message.contains("`harness.cache_keepalive` is off")),
            "{:?}",
            off.iter().map(|w| &w.message).collect::<Vec<_>>()
        );

        std::fs::create_dir(root.path().join("subagents")).unwrap();
        std::fs::write(
            root.path().join("subagents/child.md"),
            "---\ndescription: Test\nagent: claude\nkeep-warm: 2h\ntools: []\n---\nBase.",
        )
        .unwrap();
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let inert = keep_warm_warnings(&loaded, &harness(""));
        let child = inert
            .iter()
            .find(|warning| warning.path.ends_with("subagents/child.md"))
            .expect("a subagents definition that sets keep-warm warns");
        assert!(
            child
                .message
                .contains("keep-warm has no effect on a subagents definition"),
            "{}",
            child.message
        );
    }

    #[test]
    fn tier_exclusions_warn_once_per_definition_and_family() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        for (name, fields) in [
            ("claude", ""),
            ("routed", "tier: junior\ntools: []\n"),
            ("exact", "agent: claude\nmodel: claude-custom\ntools: []\n"),
            ("blocked", "model: astra\ntools: []\n"),
        ] {
            std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!("---\ndescription: Test\n{fields}---\nBase."),
            )
            .unwrap();
        }
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(loaded.errors[0].path.ends_with("blocked.md"));
        assert!(loaded.errors[0].message.contains("codex.md"));
        assert!(loaded.errors[0].message.contains("tier: senior"));
        let warnings = warnings(&loaded, Isolation::Host, &Default::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].path.ends_with("routed.md"));
        assert!(
            warnings[0]
                .message
                .contains("`junior` lists sol (codex), which cannot run this definition:")
        );
        assert!(warnings[0].message.contains("codex.md"));
    }

    #[test]
    fn allowed_tools_warnings_follow_resolved_kind_and_nonempty_rules() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        for kind in ["claude", "codex"] {
            std::fs::write(
                root.path().join(format!("agents/{kind}.md")),
                "---\ndescription: Base\n---\nBase.",
            )
            .unwrap();
            for (suffix, rules) in [("rules", "[Read]"), ("empty", "[]")] {
                std::fs::write(root.path().join(format!("agents/{kind}-{suffix}.md")), format!("---\ndescription: Worker\nagent: {kind}\ntools: [Read]\nallowed-tools: {rules}\n---\n")).unwrap();
            }
        }
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let warnings = warnings(&loaded, Isolation::Host, &Default::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].path.ends_with("codex-rules.md"));
        assert_eq!(
            warnings[0].message,
            "codex has no per-launch permission rules; allowed-tools is not applied and the agent prompts as usual: remove allowed-tools from the definition or run it on claude"
        );
    }

    #[test]
    fn host_skill_warnings_follow_definition_isolation_and_keep_success() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(
            root.path().join("agents/pi.md"),
            "---\ndescription: Pi base\n---\nBase.",
        )
        .unwrap();
        for (name, isolation, skills) in [
            ("host", "host", "[not-installed]"),
            ("boxed", "scoped", "[]"),
        ] {
            std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!(
                    "---\ndescription: Worker\nagent: pi\nisolation: {isolation}\nskills: {skills}\n---\n"
                ),
            )
            .unwrap();
        }
        let env = std::collections::BTreeMap::from([(
            "HOME".to_owned(),
            root.path().display().to_string(),
        )]);
        let loaded = load(
            root.path(),
            SkillCheck::Check {
                env: &env,
                library: &root.path().join("skills"),
                machine_isolation: Isolation::Scoped,
            },
            &Default::default(),
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let warnings = warnings(&loaded, Isolation::Scoped, &Default::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].path.ends_with("host.md"));
        let json = serde_json::to_value(&warnings).unwrap();
        assert!(
            json[0]["message"]
                .as_str()
                .unwrap()
                .contains("pi has no per-launch skill switch")
        );
    }

    #[test]
    fn shared_lsp_warning_requires_machine_servers_and_named_tool() {
        let root = tempfile::tempdir().unwrap();
        for namespace in ["agents", "subagents", "teams"] {
            std::fs::create_dir(root.path().join(namespace)).unwrap();
        }
        std::fs::write(
            root.path().join("agents/claude.md"),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
        for (name, namespace, tools, skills) in [
            ("navigator", "agents", "Bash, LSP, Skill", None),
            ("skillless", "agents", "Bash, LSP", None),
            ("writer", "agents", "Write, Read, Skill", None),
            ("native", "agents", "Bash, LSP, Skill", Some(vec!["commit"])),
            ("empty", "agents", "Bash, LSP, Skill", Some(vec![])),
            (
                "child",
                "subagents",
                "Bash, LSP, Skill",
                Some(vec!["commit"]),
            ),
        ] {
            let skills = skills.map_or_else(String::new, |skills| {
                format!("skills: {}\n", serde_json::to_string(&skills).unwrap())
            });
            std::fs::write(
                root.path().join(format!("{namespace}/{name}.md")),
                format!("---\ndescription: Test\nagent: claude\ntools: [{tools}]\n{skills}---\n"),
            )
            .unwrap();
        }
        std::fs::write(root.path().join("teams/probe.md"), "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: native\n    role: lead\n    owns: [Plan]\n---\nPipeline.").unwrap();
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(lsp_warnings(&loaded, &rimz::config::MachineConfig::default()).is_empty());
        let machine = toml::from_str("[lsp.servers.rust]\ncommand = ['rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
        let warnings = lsp_warnings(&loaded, &machine);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].path.ends_with("navigator.md"));
        assert_eq!(
            warnings[0].message,
            "profile navigator: tools names LSP, which a room with a shared language server strips"
        );
    }

    #[test]
    fn effort_cell_appends_fast_only_when_on() {
        let mut row = definitions::DefinitionRow {
            keep_warm: None,
            tier: None,
            tier_fallback: None,
            name: "quick".into(),
            namespace: "agents".into(),
            kind: "codex".into(),
            model: None,
            effort: Some("high".into()),
            fast: false,
            source: "agents/quick.md".into(),
            team: None,
            role: None,
            owns: Vec::new(),
            signals: Vec::new(),
        };
        assert_eq!(effort_label(&row), "high");
        row.fast = true;
        assert_eq!(effort_label(&row), "high · fast");
        row.effort = None;
        assert_eq!(effort_label(&row), "— · fast");
    }

    #[test]
    fn validation_keeps_good_rows_and_collects_definition_errors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(
            root.path().join("agents/claude.md"),
            "---\ndescription: Claude base\n---\nClaude base.",
        )
        .unwrap();
        std::fs::write(
            root.path().join("agents/good.md"),
            "---\ndescription: Good\nmodel: opus\ntools: [Read, LSP]\n---\n",
        )
        .unwrap();
        std::fs::write(root.path().join("agents/bad.md"), "no frontmatter").unwrap();
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.rows.iter().any(|row| row.name == "good"));
        assert_eq!(loaded.tools["good"], ["Read", "LSP"]);
        assert!(
            loaded
                .errors
                .iter()
                .any(|error| error.path.ends_with("bad.md"))
        );
        let machine = rimz::config::MachineConfig::parse_text(
            &root.path().join("config.toml"),
            "",
            root.path(),
        )
        .unwrap();
        let effective =
            rimz::config::effective::load_with_roots(&machine, root.path(), root.path()).unwrap();
        for name in loaded.failed.keys() {
            let error = effective
                .block_failed_reference(Some(name), None)
                .unwrap_err();
            let detail = loaded
                .errors
                .iter()
                .filter(|error| loaded.failed[name].contains(&error.path))
                .map(|error| format!("{}: {}", error.path.display(), error.message))
                .collect::<Vec<_>>()
                .join("\n\n");
            assert_eq!(error.to_string(), detail);
        }
    }

    #[test]
    fn validation_checks_skills_even_without_sandbox_config() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(
            root.path().join("agents/claude.md"),
            "---\ndescription: Claude base\n---\nClaude base.",
        )
        .unwrap();
        std::fs::write(
            root.path().join("agents/worker.md"),
            "---\ndescription: Worker\nmodel: opus\ntools: [Skill]\nskills: [missing]\n---\n",
        )
        .unwrap();
        assert!(
            load(root.path(), SkillCheck::Skip, &Default::default())
                .errors
                .is_empty()
        );
        let env = std::collections::BTreeMap::from([(
            "HOME".to_owned(),
            root.path().display().to_string(),
        )]);
        let check = SkillCheck::Check {
            env: &env,
            library: &root.path().join("skills"),
            machine_isolation: Isolation::Host,
        };
        let checked = load(root.path(), check, &Default::default());
        assert!(
            checked
                .errors
                .iter()
                .any(|error| error.message.contains("missing"))
        );
        std::fs::create_dir_all(root.path().join(".claude/skills/missing")).unwrap();
        std::fs::write(
            root.path().join(".claude/skills/missing/SKILL.md"),
            "---\ndescription: provider-root only\n---\nSkill.",
        )
        .unwrap();
        let checked = load(root.path(), check, &Default::default());
        assert!(checked.errors.is_empty(), "{:?}", checked.errors);
    }
}
