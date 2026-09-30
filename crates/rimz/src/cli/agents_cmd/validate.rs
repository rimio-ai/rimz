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
    let in_sandbox = Isolation::ambient(&env) == Some(Isolation::Sandbox);
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
            "rimz: skill listings were not checked because this shell runs inside a sandbox view; run `rimz agents validate` from a host shell"
        )?;
    }
    let loaded = load(&home, check, &machine);
    let mut warnings = warnings(&loaded, machine.agents.isolation, &machine.tiers);
    warnings.extend(lsp_warnings(&loaded, &machine));
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
    let mut loaded = definitions::load(home, check, &machine.agents.commands, &machine.tiers);
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
        if profile.skills.is_some()
            && Isolation::resolve(None, profile.isolation, machine) == Isolation::Host
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
    let mut table = render::Table::new(["NAME", "KIND", "MODEL", "EFFORT", "SOURCE"]);
    for row in rows {
        table.row([
            render::cell(&row.name).fg(render::palette::identity(&row.kind)),
            render::cell(&row.kind).fg(render::palette::identity(&row.kind)),
            render::cell(crate::cli::profile_report::model_label(
                row.model.as_deref().unwrap_or("—"),
                row.tier,
                row.tier_fallback,
            )),
            render::cell(row.effort.as_deref().unwrap_or("—")),
            render::cell(row.source.display().to_string()),
        ]);
    }
    table.render(out)?;
    Ok(())
}

fn lsp_warnings(loaded: &LoadedDefinitions, machine: &rimz::config::MachineConfig) -> Vec<Warning> {
    if machine.lsp.servers.is_empty() {
        return Vec::new();
    }
    loaded
        .rows
        .iter()
        .filter(|row| {
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
    fn tier_exclusions_warn_once_per_definition_and_family() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        for (name, fields) in [
            ("claude", ""),
            ("routed", "tier: junior\ntools: []\n"),
            ("exact", "agent: claude\nmodel: claude-custom\ntools: []\n"),
        ] {
            std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!("---\ndescription: Test\n{fields}---\nBase."),
            )
            .unwrap();
        }
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
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
            ("boxed", "sandbox", "[]"),
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
                machine_isolation: Isolation::Sandbox,
            },
            &Default::default(),
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let warnings = warnings(&loaded, Isolation::Sandbox, &Default::default());
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
        let mut loaded = LoadedDefinitions::default();
        for (name, tools) in [
            ("navigator", ["Bash", "LSP"]),
            ("writer", ["Write", "Read"]),
        ] {
            loaded
                .tools
                .insert(name.into(), tools.map(str::to_owned).to_vec());
            loaded.rows.push(definitions::DefinitionRow {
                tier: None,
                tier_fallback: None,
                name: name.into(),
                namespace: "agents".into(),
                kind: "claude".into(),
                model: None,
                effort: None,
                source: format!("agents/{name}.md").into(),
                team: None,
                role: None,
                owns: Vec::new(),
                signals: Vec::new(),
            });
        }
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
