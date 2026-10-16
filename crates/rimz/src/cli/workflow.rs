use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::definitions::{self, SkillCheck};
use rimz::disk::paths;
use rimz::harness::workflow::{self, Validation};

use super::render;

#[derive(Debug, Args)]
pub(super) struct WorkflowArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate one workflow directory, or every machine workflow.
    Validate {
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Print the workflow specification's JSON Schema.
    Schema,
}

pub(super) fn run(args: WorkflowArgs) -> Result<()> {
    match args.command {
        Command::Schema => render::json_pretty(&workflow::schema()),
        Command::Validate { dir, json } => validate(dir, json),
    }
}

fn validate(dir: Option<PathBuf>, json: bool) -> Result<()> {
    let home = paths::workflows_dir();
    let dirs = if let Some(dir) = dir {
        vec![dir]
    } else {
        let mut dirs = Vec::new();
        match std::fs::read_dir(&home) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry
                        .with_context(|| format!("reading {}", home.display()))?
                        .path();
                    if path.is_dir() {
                        dirs.push(path);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("reading {}", home.display())),
        }
        dirs.sort();
        dirs
    };
    let machine = super::machine_config();
    let loaded = definitions::load(
        &paths::agents_home(),
        SkillCheck::Skip,
        &machine.agents.commands,
        &machine.tiers,
        machine.harness.auto_compact,
    );
    let mut report = Validation::default();
    for dir in dirs {
        let result = workflow::validate(&dir, &home, &loaded);
        report.rows.extend(result.rows);
        report.errors.extend(result.errors);
        report.warnings.extend(result.warnings);
    }
    if json {
        render::json_pretty(&report)?;
    } else {
        let mut table = render::Table::new(["WORKFLOW", "NODE", "KIND", "RUNNER", "EDGES"]);
        for row in &report.rows {
            table.row([
                render::cell(&row.workflow),
                render::cell(&row.node),
                render::cell(&row.kind),
                render::cell(row.run.as_deref().or(row.agent.as_deref()).unwrap_or("—")),
                render::cell(
                    row.edges
                        .iter()
                        .map(|edge| format!("{} → {} ({})", edge.outcome, edge.target, edge.source))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ]);
        }
        let mut out = render::out();
        table.render(&mut out)?;
        for error in &report.errors {
            writeln!(out, "{}", error.message)?;
        }
        for warning in &report.warnings {
            writeln!(out, "warning: {}", warning.message)?;
        }
    }
    if !report.errors.is_empty() {
        bail!("{} workflow error(s)", report.errors.len());
    }
    Ok(())
}
