//! `rimz teams wait` — block until live team cohorts' boards reach `Done`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use super::super::{Ctx, GlobalFlags, render};
use super::WaitArgs;
use rimz::harness::scratch::{BoardStage, board_section, board_stage};
use rimz::store::run::RunStatus;
use rimz::utils::path::normalize_path_lexical;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const ENDED_BEFORE_DONE: &str = "cohort ended before Done";

struct Target {
    team: String,
    channel: String,
    worktree: PathBuf,
    stage: Option<String>,
    outcome: Option<RunStatus>,
}

impl Target {
    fn instance(&self) -> String {
        format!("{}#{}", self.team, self.channel)
    }

    fn board(&self) -> PathBuf {
        self.worktree.join("blackboard.md")
    }

    fn status(&self) -> RunStatus {
        self.outcome.unwrap_or(RunStatus::TimedOut)
    }

    fn entry(&self) -> EntryJson {
        let status = self.status();
        EntryJson {
            status,
            exit: status.exit_code(),
            stage: self.stage.clone(),
            board: self.board(),
            result: (status == RunStatus::Completed)
                .then(|| board_section(&self.worktree, "Result"))
                .flatten(),
            error: (status == RunStatus::Failed).then_some(ENDED_BEFORE_DONE),
        }
    }
}

#[derive(Serialize)]
struct EntryJson {
    status: RunStatus,
    exit: i32,
    stage: Option<String>,
    board: PathBuf,
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

/// A target's outcome at one poll: `Done` completes it, a cohort gone before
/// `Done` fails it, and anything else (a missing board included) stays pending.
fn settle(stage: Option<&BoardStage>, cohort_live: bool) -> Option<RunStatus> {
    if stage.is_some_and(|stage| stage.name == rimz::config::DONE_STAGE) {
        return Some(RunStatus::Completed);
    }
    (!cohort_live).then_some(RunStatus::Failed)
}

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    if args.worktree.is_some() && args.references.len() > 1 {
        bail!("-w/--worktree selects one cohort; name each cohort as `team#lane` instead");
    }
    let ctx = Ctx::open(globals)?;
    let mut targets = resolve_targets(&ctx, args.references, args.worktree, globals)?;
    let single = targets.len() == 1 && !args.any;
    let deadline = args.timeout.map(|timeout| Instant::now() + timeout);
    loop {
        let settled = poll(&ctx, &mut targets)?;
        let selected = settled.first().copied();
        if !args.json && !single {
            let settled = if args.any {
                &settled[..settled.len().min(1)]
            } else {
                &settled
            };
            for &index in settled {
                print_block(&targets[index])?;
            }
        }
        let finished = match selected {
            Some(index) if args.any => Some(targets[index].status()),
            _ if targets.iter().all(|target| target.outcome.is_some()) => Some(
                targets
                    .iter()
                    .map(Target::status)
                    .find(|status| *status != RunStatus::Completed)
                    .unwrap_or(RunStatus::Completed),
            ),
            _ => None,
        };
        if let Some(status) = finished {
            let reported = match selected {
                Some(index) if args.any => std::slice::from_ref(&targets[index]),
                _ => &targets[..],
            };
            report_finished(reported, single, args.json)?;
            std::process::exit(status.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            report_timeout(&targets, single, args.json)?;
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn resolve_targets(
    ctx: &Ctx,
    references: Vec<String>,
    worktree: Option<String>,
    globals: &GlobalFlags,
) -> Result<Vec<Target>> {
    let teams = super::list::effective_teams(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    references
        .into_iter()
        .map(|reference| {
            let (team, lane) = super::team_lane(reference, worktree.clone())?;
            super::validate_team_name(&team, &teams)?;
            let cohort =
                super::cohort::select(&team, lane.as_deref(), ctx.channel(), &snapshot.agents)?;
            let mut worktrees = cohort.members.iter().map(|agent| {
                agent
                    .worktree_path
                    .as_deref()
                    .map(|path| normalize_path_lexical(Path::new(path)))
            });
            let first = worktrees.next().flatten();
            let Some(worktree) =
                first.filter(|first| worktrees.all(|path| path.as_ref() == Some(first)))
            else {
                bail!(
                    "team cohort `{team}#{}` has no single worktree to read a board from",
                    cohort.channel
                );
            };
            Ok(Target {
                team,
                channel: cohort.channel,
                worktree,
                stage: None,
                outcome: None,
            })
        })
        .collect()
}

/// Settle every pending target that can settle now, returning their indices in
/// argument order. The snapshot is read only when some board is not yet `Done`.
fn poll(ctx: &Ctx, targets: &mut [Target]) -> Result<Vec<usize>> {
    let mut snapshot = None;
    let mut settled = Vec::new();
    for (index, target) in targets.iter_mut().enumerate() {
        if target.outcome.is_some() {
            continue;
        }
        let stage = board_stage(&target.worktree);
        target.stage = stage.as_ref().map(|stage| stage.name.clone());
        let cohort_live = if settle(stage.as_ref(), true).is_some() {
            true
        } else {
            let snapshot = match &mut snapshot {
                Some(snapshot) => snapshot,
                None => snapshot.insert(ctx.alive_snapshot()?),
            };
            rimz::address::team_cohorts(&snapshot.agents)
                .iter()
                .any(|cohort| cohort.team == target.team && cohort.channel == target.channel)
        };
        target.outcome = settle(stage.as_ref(), cohort_live);
        if target.outcome.is_some() {
            settled.push(index);
        }
    }
    Ok(settled)
}

fn report_finished(targets: &[Target], single: bool, json: bool) -> Result<()> {
    match (single, json) {
        (true, true) => render::json_pretty(&targets[0].entry()),
        (true, false) => print_outcome(&targets[0], &mut render::err()),
        (false, true) => print_json(targets),
        (false, false) => Ok(()),
    }
}

fn report_timeout(targets: &[Target], single: bool, json: bool) -> Result<()> {
    if json {
        return if single {
            render::json_pretty(&targets[0].entry())
        } else {
            print_json(targets)
        };
    }
    let mut err = render::err();
    for target in targets.iter().filter(|target| target.outcome.is_none()) {
        write_header(&mut err, target)?;
    }
    Ok(())
}

fn print_json(targets: &[Target]) -> Result<()> {
    let entries = targets
        .iter()
        .map(|target| (target.instance(), target.entry()))
        .collect::<BTreeMap<_, _>>();
    render::json(&entries)
}

fn print_block(target: &Target) -> Result<()> {
    let mut out = render::out();
    let mut err = render::err();
    write_header(&mut out, target)?;
    let mut diagnostics = Vec::new();
    print_outcome(target, &mut diagnostics)?;
    if !diagnostics.is_empty() {
        write_header(&mut err, target)?;
        err.write_all(&diagnostics)?;
    }
    writeln!(out)?;
    Ok(())
}

/// The Result section on stdout, or the reason there is none on `err`.
fn print_outcome(target: &Target, err: &mut impl Write) -> Result<()> {
    let instance = target.instance();
    if target.status() == RunStatus::Failed {
        let stage = target.stage.as_deref().unwrap_or("no board");
        writeln!(
            err,
            "rimz: {instance}: {ENDED_BEFORE_DONE} (stage: {stage})"
        )?;
        return Ok(());
    }
    let Some(result) = board_section(&target.worktree, "Result") else {
        writeln!(
            err,
            "rimz: {instance} reached Done; the board has no Result section"
        )?;
        return Ok(());
    };
    let mut out = render::out();
    for line in render::prose::Prose::for_stdout().lines(&result, render::prose::prose_width(0)) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

fn write_header(out: &mut impl Write, target: &Target) -> Result<()> {
    let instance = render::paint(render::palette::body(), &target.instance());
    let status = target.status();
    if status == RunStatus::Completed {
        writeln!(out, "--- {instance} ---")?;
    } else {
        writeln!(
            out,
            "--- {instance} ({}) ---",
            render::paint(
                render::status::run(status),
                super::super::supervised::output::status_label(status)
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(name: &str) -> BoardStage {
        BoardStage {
            name: name.to_owned(),
            owner: None,
        }
    }

    #[test]
    fn settle_completes_on_exact_done_and_fails_only_a_dissolved_cohort() {
        for live in [true, false] {
            assert_eq!(
                settle(Some(&stage("Done")), live),
                Some(RunStatus::Completed)
            );
        }
        for board in [None, Some(stage("Review")), Some(stage("Done (delta)"))] {
            assert_eq!(settle(board.as_ref(), true), None);
            assert_eq!(settle(board.as_ref(), false), Some(RunStatus::Failed));
        }
    }
}
