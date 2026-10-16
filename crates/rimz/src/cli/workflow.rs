use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::definitions::{self, SkillCheck};
use rimz::disk::paths;
use rimz::harness::workflow::{self, Validation};
use rimz::ids::WorkflowRunId;
use rimz::store::workflow::{self as records, RunRecord};

use super::render;

#[derive(Debug, Args)]
pub(super) struct WorkflowArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a workflow in the current checkout or an existing owned worktree.
    Run {
        name: PathBuf,
        #[arg(short = 'w', long)]
        worktree: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        prompt: Option<String>,
    },
    /// List active and parked workflow runs across rooms.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a run's work directory, current position, and history.
    Show {
        id: WorkflowRunId,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run and interrupt its live script.
    Cancel { id: WorkflowRunId },
    /// Re-enter a parked node or extend a run's bounds.
    Resume {
        id: WorkflowRunId,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        extend: Vec<String>,
    },
    #[command(hide = true)]
    Advance {
        #[arg(required_unless_present = "wake_signal")]
        id: Option<WorkflowRunId>,
        #[arg(long, conflicts_with = "id")]
        wake_signal: Option<String>,
    },
    /// Validate one workflow directory, or every machine workflow.
    Validate {
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Print the workflow specification's JSON Schema.
    Schema,
}

pub(super) fn run(args: WorkflowArgs, globals: &super::GlobalFlags) -> Result<()> {
    match args.command {
        Command::Run {
            name,
            worktree,
            prompt,
        } => start(name, worktree, prompt, globals),
        Command::List { json } => list(json),
        Command::Show { id, json } => show(&id, json),
        Command::Cancel { id } => {
            let (state, _) = locate(&id)?;
            let changed = workflow::cancel(&state, &id)?;
            writeln!(
                render::out(),
                "{id}: {}",
                if changed {
                    "canceled"
                } else {
                    "already terminal; unchanged"
                }
            )?;
            Ok(())
        }
        Command::Resume { id, at, extend } => {
            let (state, _) = locate(&id)?;
            workflow::resume(&state, &id, at.as_deref(), &extend)?;
            writeln!(render::out(), "{id}: resumed")?;
            Ok(())
        }
        Command::Advance { id: Some(id), .. } => {
            let (state, _) = locate(&id)?;
            Ok(workflow::advance(&state, &id)?)
        }
        Command::Advance {
            wake_signal: Some(signal),
            ..
        } => {
            let ctx = super::ctx::Ctx::open(globals)?;
            Ok(workflow::wake(ctx.store.paths(), &signal)?)
        }
        Command::Advance { .. } => unreachable!("clap requires a run id or wake signal"),
        Command::Schema => render::json_pretty(&workflow::schema()),
        Command::Validate { dir, json } => validate(dir, json),
    }
}

fn definitions() -> rimz::config::definitions::LoadedDefinitions {
    let machine = super::machine_config();
    definitions::load(
        &paths::agents_home(),
        SkillCheck::Skip,
        &machine.agents.commands,
        &machine.tiers,
        machine.harness.auto_compact,
    )
}

fn start(
    name: PathBuf,
    worktree: Option<String>,
    prompt: Option<String>,
    globals: &super::GlobalFlags,
) -> Result<()> {
    let mut components = name.components();
    let dir = if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        paths::workflows_dir().join(name)
    } else {
        name
    };
    let report = workflow::validate(&dir, &paths::workflows_dir(), &definitions());
    if !report.errors.is_empty() {
        bail!(
            "{}",
            report
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let ctx = super::ctx::Ctx::open(globals)?;
    let scope = if let Some(name) = worktree {
        rimz::worktree::resolve_owned(
            &ctx.workspace.project_root,
            &super::machine_config().agents.worktree,
            &name,
        )?
        .path
    } else {
        ctx.workspace.worktree_root.clone()
    };
    let record = workflow::create(
        ctx.store.paths(),
        &dir,
        &scope,
        &ctx.workspace.project_root,
        prompt,
    )?;
    writeln!(
        render::out(),
        "{}\n{}",
        record.id,
        ctx.store.paths().workflow(&record.id).work.display()
    )?;
    Ok(())
}

fn locate(id: &WorkflowRunId) -> Result<(rimz::StatePaths, paths::WorkflowPaths)> {
    paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .find(|(state, run)| state.workflow(id).home == run.home)
        .with_context(|| format!("unknown workflow run {id}"))
}

fn state_word(record: &RunRecord) -> &'static str {
    use records::RunState;
    match record.state {
        RunState::Active => "active",
        RunState::Parked => "parked",
        RunState::Done => "done",
        RunState::Failed => "failed",
        RunState::Canceled => "canceled",
    }
}

fn waiting(record: &RunRecord) -> Option<String> {
    record.wait.as_ref().map(|w| {
        format!(
            "waiting: {}, said {}, since {}, ×{}, recheck {}",
            w.node,
            w.outcome,
            w.first,
            w.count,
            w.recheck_at
                .map_or_else(|| "next wake".into(), |at| at.to_string())
        )
    })
}

fn list(json: bool) -> Result<()> {
    let mut rows = Vec::new();
    let mut table = render::Table::new([
        "RUN", "WORKFLOW", "CHECKOUT", "STATE", "NODE", "SINCE", "DETAIL",
    ]);
    let rooms: BTreeMap<_, _> = paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .map(|(state, _)| (state.root.clone(), state))
        .collect();
    for state in rooms.values() {
        for r in records::list(state)? {
            if r.state.is_terminal() {
                continue;
            }
            let detail = waiting(&r)
                .or_else(|| {
                    r.park.as_ref().map(|p| {
                        format!(
                            "{}{}",
                            reason_word(p.reason),
                            p.text
                                .as_ref()
                                .map_or_else(String::new, |s| format!(": {s}"))
                        )
                    })
                })
                .unwrap_or_default();
            rows.push(serde_json::json!({"id":r.id,"workflow":r.workflow,"scope":r.scope.path,"state":r.state,"node":r.node,"since":r.entered_at,"detail":detail}));
            table.row([
                render::cell(r.id.as_str()),
                render::cell(&r.workflow),
                render::cell(r.scope.path.display().to_string()),
                render::cell(state_word(&r)).fg(render::status::workflow(r.state)),
                render::cell(&r.node),
                render::cell(r.entered_at.to_string()),
                render::cell(detail),
            ]);
        }
    }
    if json {
        render::json_pretty(&rows)
    } else {
        Ok(table.render(&mut render::out())?)
    }
}

fn reason_word(reason: records::ParkReason) -> &'static str {
    use records::ParkReason;
    match reason {
        ParkReason::User => "user",
        ParkReason::Cap => "cap",
        ParkReason::Deadline => "deadline",
        ParkReason::Timeout => "timeout",
        ParkReason::Invalid => "invalid",
        ParkReason::Asked => "asked",
        ParkReason::Check => "check",
    }
}

fn show(id: &WorkflowRunId, json: bool) -> Result<()> {
    let (_, paths) = locate(id)?;
    let record = records::load(&paths)?;
    let journal = records::journal(&paths)?;
    if json {
        return render::json_pretty(
            &serde_json::json!({"dir":paths.work,"record":record,"journal":journal}),
        );
    }
    let spec = workflow::load(&paths.home)?;
    let mut out = render::out();
    writeln!(out, "{}", paths.work.display())?;
    let mut header = render::KeyVals::new();
    header.push("workflow", render::cell(&record.workflow));
    header.push(
        "scope checkout",
        render::cell(record.scope.path.display().to_string()),
    );
    let state = if let Some(park) = &record.park {
        format!(
            "{} ({}{})",
            state_word(&record),
            reason_word(park.reason),
            park.text
                .as_ref()
                .map_or_else(String::new, |s| format!(": {s}"))
        )
    } else {
        state_word(&record).into()
    };
    header.push(
        "state",
        render::cell(state).fg(render::status::workflow(record.state)),
    );
    header.push(
        "current node",
        render::cell(format!("{}, since {}", record.node, record.entered_at)),
    );
    header.push(
        "deadline",
        render::cell(record.deadline.map_or_else(
            || "none".into(),
            |at| {
                format!(
                    "{at} ({})",
                    if record.deadline_fired {
                        "fired"
                    } else {
                        "armed"
                    }
                )
            },
        )),
    );
    header.push(
        "prompt",
        render::cell(record.prompt.as_deref().unwrap_or("none")),
    );
    header.render(&mut out)?;
    if let Some(waiting) = waiting(&record) {
        writeln!(out, "{waiting}")?;
    }
    for (name, visits) in &record.visits {
        let max = spec.steps.get(name).and_then(|s| s.max_visits).map_or_else(
            || "unbounded".into(),
            |max| format!("{} + {} grant", max, record.visit_grant),
        );
        writeln!(out, "step {name}: {} visits of {max}", visits.total)?;
    }
    writeln!(out, "history:")?;
    for entry in journal {
        let value = serde_json::to_value(&entry)?;
        let event = value["event"].as_str().unwrap_or_default();
        let outcome = ["outcome", "state", "reason", "signal"]
            .iter()
            .find_map(|field| value.get(*field).and_then(|v| v.as_str()))
            .map_or_else(String::new, |word| format!(" {word}"));
        let count = value
            .get("count")
            .map_or_else(String::new, |count| format!(" ×{count}"));
        let payload = value
            .get("object")
            .or_else(|| value.get("input"))
            .map_or_else(String::new, |v| format!(" {v}"));
        writeln!(
            out,
            "  {} #{} {} {event}{outcome}{count}{payload}",
            entry.at,
            entry.seq.map_or_else(|| "—".into(), |seq| seq.to_string()),
            entry.node
        )?;
    }
    Ok(())
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
    let loaded = definitions();
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
