//! `rimz teams wait` — block until live team cohorts' boards reach `Done`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Serialize;

use super::super::{Ctx, GlobalFlags, render};
use super::WaitArgs;
use crate::cli::render::announce::{MESSAGE_STOP_EXIT, write_message_stop};
use rimz::harness::scratch::board_section;
use rimz::launcher::team_join::{self, Joined, Polled, Progress, Seen, TeamTarget as Target};
use rimz::message::announce::WaitCaller;
use rimz::store::run::RunStatus;
use rimz::utils::path::normalize_path_lexical;

const ENDED_BEFORE_DONE: &str = "cohort ended before Done";

fn diagnostic_line(
    instance: &str,
    previous: Option<&Progress>,
    current: &Progress,
    state: &str,
) -> Option<String> {
    if previous == Some(current) {
        return None;
    }
    let label = progress_stage(current);
    let Some(previous) = previous else {
        return Some(format!("waiting on {instance}: {label}, {state}"));
    };
    let focus = crate::cli::usage::shell_command(["rimz", "teams", "focus", instance]);
    let mut changes = Vec::new();
    if previous.stage != current.stage || previous.owner != current.owner {
        changes.push(format!(
            "{} -> {label}",
            previous.stage.as_deref().unwrap_or("none")
        ));
    }
    if current.owner_live == Some(false)
        && let Some(owner) = &current.owner
    {
        changes.push(format!("@{owner} not live"));
    }
    let raised = current
        .needs_you
        .iter()
        .filter(|entry| !previous.needs_you.contains(entry));
    for (handle, status) in raised {
        let status = if *status == rimz::agents::AgentStatus::Waiting {
            "waiting on you"
        } else {
            status.as_str()
        };
        changes.push(format!("{handle} {status} ({focus})"));
    }
    if changes.is_empty() {
        changes.push(format!("{label}, {state}"));
    }
    Some(format!("{instance}: {}", changes.join("; ")))
}

fn progress_stage(progress: &Progress) -> String {
    let stage = progress.stage.as_deref().unwrap_or("none");
    progress.owner.as_ref().map_or_else(
        || stage.to_owned(),
        |owner| {
            format!(
                "{stage} (@{owner}{})",
                if progress.owner_live == Some(false) {
                    ", not live"
                } else {
                    ""
                }
            )
        },
    )
}

fn timeout_line(instance: &str, progress: &Progress, timeout: Duration) -> String {
    format!(
        "{instance}: timed out after {} at {}; cohort left running",
        render::age_label(timeout.as_secs()),
        progress_stage(progress)
    )
}

/// How a wait that stops before every cohort settles ends.
enum Unfinished {
    TimedOut(Duration),
    /// Stopped on a message queued for the caller.
    Message,
}

impl Unfinished {
    fn line(&self, instance: &str, progress: &Progress) -> String {
        match self {
            Self::TimedOut(timeout) => timeout_line(instance, progress, *timeout),
            Self::Message => format!(
                "{instance}: stopped at {}; cohort left running",
                progress_stage(progress)
            ),
        }
    }

    /// The entry of a cohort still pending when the wait stops.
    fn entry(&self, target: &Target) -> EntryJson {
        let mut entry = entry(target);
        if target.outcome.is_none()
            && let Self::Message = self
        {
            entry.status = RunStatus::Running;
            entry.exit = MESSAGE_STOP_EXIT;
        }
        entry
    }
}

fn instance(target: &Target) -> String {
    target.cohort.to_string()
}

fn entry(target: &Target) -> EntryJson {
    let status = target.status();
    EntryJson {
        status,
        exit: status.exit_code(),
        stage: target.stage.clone(),
        board: rimz::harness::scratch::board_path(&target.worktree),
        result: (status == RunStatus::Completed)
            .then(|| board_section(&target.worktree, "Result"))
            .flatten(),
        error: (status == RunStatus::Failed).then_some(ENDED_BEFORE_DONE),
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

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    let mut caller = WaitCaller::starting_now();
    if args.worktree.is_some() && args.references.len() > 1 {
        bail!("-w/--worktree selects one cohort; name each cohort as `team#lane` instead");
    }
    let ctx = Ctx::open(globals)?;
    let mut targets = resolve_targets(&ctx, args.references, args.worktree, globals)?;
    let single = targets.len() == 1 && !args.any;
    let machine = crate::cli::machine_config();
    let joined = team_join::join(
        &ctx.workspace,
        &ctx.store,
        &machine,
        &mut targets,
        args.any,
        args.timeout,
        &mut || caller.queued(&ctx.store, &ctx.workspace.session_name),
        &mut |polled| match polled {
            // A settling target's outcome follows at once, so it gets no progress line.
            Polled::Seen(seen) if seen.outcome.is_none() => progress_line(&seen),
            Polled::Seen(_) => Ok(()),
            Polled::Settled { targets, settled } if !args.json && !single => {
                let settled = if args.any {
                    &settled[..settled.len().min(1)]
                } else {
                    settled
                };
                for &index in settled {
                    print_block(&targets[index])?;
                }
                Ok(())
            }
            Polled::Settled { .. } => Ok(()),
        },
    )?;
    match joined {
        Joined::Finished { status, first } => {
            let reported = match first {
                Some(index) => std::slice::from_ref(&targets[index]),
                None => &targets[..],
            };
            report_finished(reported, single, args.json)?;
            std::process::exit(status.exit_code());
        }
        Joined::Interrupted(announcement) => {
            report_unfinished(&targets, single, args.json, &Unfinished::Message)?;
            write_message_stop(&mut render::err(), &announcement)?;
            std::process::exit(MESSAGE_STOP_EXIT);
        }
        Joined::StillRunning => {
            let timeout = args.timeout.unwrap_or_default();
            report_unfinished(&targets, single, args.json, &Unfinished::TimedOut(timeout))?;
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
    }
}

/// The stderr line for a pending cohort whose progress changed since the last poll.
fn progress_line(seen: &Seen<'_>) -> Result<()> {
    let state = seen.snapshot.zip(seen.members).map_or(
        super::list::CohortState::Idle,
        |(snapshot, cohort)| {
            super::list::CohortState::for_cohort(snapshot, cohort, seen.statuses.iter().copied())
        },
    );
    let state = super::show::state_with_age(state, seen.idle_since, seen.now);
    if let Some(line) = diagnostic_line(
        &instance(seen.target),
        seen.target.progress.as_ref(),
        seen.progress,
        &state,
    ) {
        writeln!(render::err(), "{line}")?;
    }
    Ok(())
}

fn resolve_targets(
    ctx: &Ctx,
    references: Vec<String>,
    worktree: Option<String>,
    globals: &GlobalFlags,
) -> Result<Vec<Target>> {
    let effective = super::list::effective_catalog(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    references
        .into_iter()
        .map(|reference| {
            let (team, lane) = super::team_lane(reference, worktree.clone())?;
            if !effective.teams.0.contains_key(&team)
                && let Some(detail) = effective.definition_failure_for(&team)
            {
                bail!("{detail}");
            }
            super::validate_team_name(&team, &effective.teams, &effective.profiles)?;
            let target = super::cohort::Target::Team {
                team: &team,
                lane: lane.as_deref(),
            };
            let cohort = super::cohort::select(target, ctx.channel(), &snapshot.agents)?;
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
                    "team cohort `{}` has no single worktree to read a board from",
                    cohort.instance
                );
            };
            Ok(Target::new(cohort.instance, worktree))
        })
        .collect()
}

fn report_finished(targets: &[Target], single: bool, json: bool) -> Result<()> {
    match (single, json) {
        (true, true) => render::json_pretty(&entry(&targets[0])),
        (true, false) => print_outcome(&targets[0], &mut render::err()),
        (false, true) => print_json(targets, entry),
        (false, false) => Ok(()),
    }
}

fn report_unfinished(
    targets: &[Target],
    single: bool,
    json: bool,
    ending: &Unfinished,
) -> Result<()> {
    let mut err = render::err();
    for target in targets.iter().filter(|target| target.outcome.is_none()) {
        // The run loop polls every target before it checks a stop.
        let progress = target
            .progress
            .as_ref()
            .expect("polled target has progress");
        writeln!(err, "{}", ending.line(&instance(target), progress))?;
    }
    if json {
        return if single {
            render::json_pretty(&ending.entry(&targets[0]))
        } else {
            print_json(targets, |target| ending.entry(target))
        };
    }
    Ok(())
}

fn print_json(targets: &[Target], entry: impl Fn(&Target) -> EntryJson) -> Result<()> {
    let entries = targets
        .iter()
        .map(|target| (instance(target), entry(target)))
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
    let instance = instance(target);
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
    let instance = render::paint(render::palette::body(), &instance(target));
    let status = target.status();
    if status == RunStatus::Completed {
        writeln!(out, "--- {instance} ---")?;
    } else {
        writeln!(
            out,
            "--- {instance} ({}) ---",
            render::paint(render::status::run(status), status.label()),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> Progress {
        Progress {
            stage: Some("Build".into()),
            owner: Some("coder".into()),
            owner_live: Some(true),
            needs_you: Vec::new(),
        }
    }

    #[test]
    fn diagnostics_track_stage_owner_and_attention_not_age() {
        let before = progress();
        assert_eq!(
            diagnostic_line("forge#a", None, &before, "idle 1d").as_deref(),
            Some("waiting on forge#a: Build (@coder), idle 1d")
        );
        assert_eq!(
            diagnostic_line("forge#a", Some(&before), &before, "idle 2d"),
            None
        );
        let mut after = before.clone();
        after.stage = Some("Review".into());
        after.owner = Some("reviewer".into());
        assert_eq!(
            diagnostic_line("forge#a", Some(&before), &after, "working").as_deref(),
            Some("forge#a: Build -> Review (@reviewer)")
        );
        let before = after.clone();
        after
            .needs_you
            .push(("@reviewer".into(), rimz::agents::AgentStatus::Waiting));
        assert_eq!(
            diagnostic_line("forge#a", Some(&before), &after, "blocked 1m").as_deref(),
            Some("forge#a: @reviewer waiting on you (rimz teams focus 'forge#a')")
        );
        let before = after.clone();
        after.stage = Some("Reflect".into());
        assert_eq!(
            diagnostic_line("forge#a", Some(&before), &after, "blocked 1m").as_deref(),
            Some("forge#a: Review -> Reflect (@reviewer)")
        );
        let before = after.clone();
        after.needs_you.clear();
        after.owner_live = Some(false);
        assert_eq!(
            diagnostic_line("forge#a", Some(&before), &after, "idle 1m").as_deref(),
            Some("forge#a: @reviewer not live")
        );
    }

    #[test]
    fn diagnostics_timeout_names_the_stage_and_preserves_the_cohort() {
        assert_eq!(
            timeout_line("forge#a", &progress(), Duration::from_secs(2)),
            "forge#a: timed out after 2s at Build (@coder); cohort left running"
        );
    }
}
