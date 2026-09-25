use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use rimz::config::Isolation;
use rimz::config::definitions::{self, DefinitionErr, LoadedDefinitions, SkillCheck};

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
    let warnings = warnings(&loaded, machine.agents.isolation);
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
    let mut loaded = definitions::load(home, check, &machine.agents.commands);
    for error in &machine.notices.definition_errors {
        if !loaded
            .errors
            .iter()
            .any(|seen| seen.path == error.path && seen.message == error.message)
        {
            loaded.errors.push(DefinitionErr {
                path: error.path.clone(),
                message: error.message.clone(),
            });
        }
    }
    loaded
}

#[derive(serde::Serialize)]
struct Warning {
    path: std::path::PathBuf,
    message: String,
}

fn warnings(loaded: &LoadedDefinitions, machine: Isolation) -> Vec<Warning> {
    loaded
        .rows
        .iter()
        .filter_map(|row| {
            let profiles = if row.namespace == "subagents" {
                &loaded.subagent_profiles
            } else {
                &loaded.agent_profiles
            };
            let profile = profiles.0.get(&row.name)?;
            if profile.skills.is_none()
                || Isolation::resolve(None, profile.isolation, machine) != Isolation::Host
            {
                return None;
            }
            let adapter = rimz::agents::find_definition(&row.kind)?;
            if !matches!(
                adapter.spec().host_skills,
                rimz::agents::skills::HostSkills::Unsupported
            ) {
                return None;
            }
            Some(Warning {
                path: row.source.clone(),
                message: rimz::harness::launch_plan::LaunchPlanWarning::HostSkillsUnenforced {
                    kind: rimz::ids::AgentKind::new_unchecked(&row.kind),
                }
                .to_string(),
            })
        })
        .collect()
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
            render::cell(row.model.as_deref().unwrap_or("—")),
            render::cell(row.effort.as_deref().unwrap_or("—")),
            render::cell(row.source.display().to_string()),
        ]);
    }
    table.render(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let warnings = warnings(&loaded, Isolation::Sandbox);
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
            "---\ndescription: Good\nmodel: opus\ntools: [Read]\n---\n",
        )
        .unwrap();
        std::fs::write(root.path().join("agents/bad.md"), "no frontmatter").unwrap();
        let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
        assert!(loaded.rows.iter().any(|row| row.name == "good"));
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
