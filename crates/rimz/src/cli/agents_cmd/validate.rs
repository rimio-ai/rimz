use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use rimz::config::definitions::{self, DefinitionErr, LoadedDefinitions, SkillLibraryCheck};

use crate::cli::render;

pub(super) fn run(json: bool) -> Result<()> {
    let home = rimz::disk::paths::agents_home();
    let skills = home.join("skills");
    let check = if skills.is_dir() {
        SkillLibraryCheck::Check(&skills)
    } else {
        writeln!(
            std::io::stderr(),
            "warning: skill library {} is missing; skipping skill checks",
            skills.display()
        )?;
        SkillLibraryCheck::Skip
    };
    let loaded = load(&home, check);
    if json {
        render::json_pretty(&serde_json::json!({
            "rows": loaded.rows,
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
    }
    if !loaded.errors.is_empty() {
        bail!("{} definition error(s)", loaded.errors.len());
    }
    Ok(())
}

fn load(home: &Path, check: SkillLibraryCheck<'_>) -> LoadedDefinitions {
    let mut loaded = definitions::load(home, check);
    let agents = rimz::config::AgentsConfig {
        profiles: loaded.agent_profiles.clone(),
        teams: loaded.teams.clone(),
        ..Default::default()
    };
    let subagents = rimz::config::SubagentProfilesConfig {
        profiles: loaded.subagent_profiles.clone(),
    };
    if let Err(error) = rimz::config::validate_agents_file(&agents, &subagents, home) {
        loaded.errors.push(DefinitionErr {
            path: home.to_owned(),
            message: error.to_string(),
        });
    }
    loaded
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
    fn validation_keeps_good_rows_and_collects_definition_errors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(
            root.path().join("agents/good.md"),
            "---\ndescription: Good\nmodel: opus\ntools: [Read]\n---\n",
        )
        .unwrap();
        std::fs::write(root.path().join("agents/bad.md"), "no frontmatter").unwrap();
        let loaded = load(root.path(), SkillLibraryCheck::Skip);
        assert!(loaded.rows.iter().any(|row| row.name == "good"));
        assert!(
            loaded
                .errors
                .iter()
                .any(|error| error.path.ends_with("bad.md"))
        );
    }

    #[test]
    fn validation_checks_skills_even_without_sandbox_config() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(
            root.path().join("agents/worker.md"),
            "---\ndescription: Worker\nmodel: opus\ntools: [Skill]\nskills: [missing]\n---\n",
        )
        .unwrap();
        assert!(load(root.path(), SkillLibraryCheck::Skip).errors.is_empty());
        let checked = load(
            root.path(),
            SkillLibraryCheck::Check(&root.path().join("skills")),
        );
        assert!(
            checked
                .errors
                .iter()
                .any(|error| error.message.contains("missing"))
        );
    }
}
