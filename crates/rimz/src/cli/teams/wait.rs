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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Progress {
    stage: Option<String>,
    owner: Option<String>,
    owner_live: Option<bool>,
    needs_you: Vec<(String, rimz::agents::AgentStatus)>,
}

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

struct Target {
    team: String,
    channel: String,
    worktree: PathBuf,
    stage: Option<String>,
    progress: Option<Progress>,
    outcome: Option<RunStatus>,
}

impl Target {
    fn instance(&self) -> String {
        format!("{}#{}", self.team, self.channel)
    }

    fn board(&self) -> PathBuf {
        rimz::harness::scratch::board_path(&self.worktree)
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

/// A target's last board stage and outcome at one poll: `Done` completes it,
/// and anything else (a missing board included) stays pending while the cohort
/// lives. A gone cohort fails only on a board read taken after the liveness
/// check, so a flip to `Done` that races the members' exit still completes.
fn settle(
    mut read_board: impl FnMut() -> Option<BoardStage>,
    cohort_live: impl FnOnce() -> Result<bool>,
) -> Result<(Option<BoardStage>, Option<RunStatus>)> {
    let is_done = |stage: &Option<BoardStage>| {
        stage
            .as_ref()
            .is_some_and(|stage| stage.name == rimz::config::DONE_STAGE)
    };
    let stage = read_board();
    if is_done(&stage) {
        return Ok((stage, Some(RunStatus::Completed)));
    }
    if cohort_live()? {
        return Ok((stage, None));
    }
    let stage = read_board();
    let status = if is_done(&stage) {
        RunStatus::Completed
    } else {
        RunStatus::Failed
    };
    Ok((stage, Some(status)))
}

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    if args.worktree.is_some() && args.references.len() > 1 {
        bail!("-w/--worktree selects one cohort; name each cohort as `team#lane` instead");
    }
    let ctx = Ctx::open(globals)?;
    let mut targets = resolve_targets(&ctx, args.references, args.worktree, globals)?;
    let single = targets.len() == 1 && !args.any;
    let started = Instant::now();
    let machine = crate::cli::machine_config();
    loop {
        let settled = poll(&ctx, &mut targets, &machine)?;
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
        if let Some(timeout) = args.timeout
            && started.elapsed() >= timeout
        {
            report_timeout(&targets, single, args.json, timeout)?;
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
    let (teams, profiles) = super::list::effective_catalog(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    references
        .into_iter()
        .map(|reference| {
            let (team, lane) = super::team_lane(reference, worktree.clone())?;
            super::validate_team_name(&team, &teams, &profiles)?;
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
                progress: None,
                outcome: None,
            })
        })
        .collect()
}

/// Settle every pending target that can settle now, returning their indices in
/// argument order. The snapshot is read only when some board is not yet `Done`.
fn poll(
    ctx: &Ctx,
    targets: &mut [Target],
    machine: &rimz::config::MachineConfig,
) -> Result<Vec<usize>> {
    let mut snapshot = None;
    let mut demotion = None;
    let no_demotion = rimz::agents::ParkDemotion::default();
    let mut settled = Vec::new();
    for (index, target) in targets.iter_mut().enumerate() {
        if target.outcome.is_some() {
            continue;
        }
        let (stage, outcome) = settle(
            || board_stage(&target.worktree),
            || {
                let snapshot = match &mut snapshot {
                    Some(snapshot) => snapshot,
                    None => snapshot.insert(ctx.published_snapshot()?),
                };
                Ok(rimz::address::team_cohorts(snapshot.agents.iter())
                    .iter()
                    .any(|cohort| cohort.team == target.team && cohort.channel == target.channel))
            },
        )?;
        let now = jiff::Timestamp::now();
        let mut progress = Progress {
            stage: stage.as_ref().map(|stage| stage.name.clone()),
            owner: stage.as_ref().and_then(|stage| stage.owner.clone()),
            owner_live: None,
            needs_you: Vec::new(),
        };
        let mut statuses = Vec::new();
        let mut activity = None;
        let cohort = snapshot.as_ref().and_then(|snapshot| {
            rimz::address::team_cohorts(snapshot.agents.iter())
                .into_iter()
                .find(|cohort| cohort.team == target.team && cohort.channel == target.channel)
        });
        if let Some(snapshot) = &snapshot
            && let Some(cohort) = &cohort
        {
            progress.owner_live = progress.owner.as_deref().map(|owner| {
                !rimz::address::role_holders(cohort.members.iter().copied(), owner).is_empty()
            });
            for agent in &cohort.members {
                let row = snapshot
                    .rows()
                    .find(|row| row.is_agent() && row.id == agent.agent_id.as_str());
                // Park demotion reads logins, capacity files, and the message queue,
                // and only a member without a row status consults it.
                let demotion = match row.and_then(|row| row.status()) {
                    Some(_) => &no_demotion,
                    None => &*demotion.get_or_insert_with(|| {
                        rimz::harness::park_demotion(
                            Some(&ctx.store),
                            ctx.store.paths(),
                            ctx.runtime(),
                            machine,
                            &snapshot.agents,
                            snapshot.resume_outcomes.as_deref().unwrap_or_default(),
                            now,
                        )
                    }),
                };
                let attention = rimz::store::snapshot::needs_you(agent, row, demotion, now);
                let status = row
                    .and_then(|row| row.status())
                    .unwrap_or_else(|| agent.rowless_status(demotion).0);
                statuses.push(status);
                activity = activity.max(Some(
                    row.map_or(agent.last_activity, |row| row.last_activity),
                ));
                if let Some(attention) = attention {
                    progress.needs_you.push((
                        rimz::address::agent_handle(agent, &cohort.members, false),
                        attention.status,
                    ));
                }
            }
        }
        progress
            .needs_you
            .sort_by(|left, right| left.0.cmp(&right.0));
        let state = snapshot
            .as_ref()
            .zip(cohort.as_ref())
            .map_or(super::list::CohortState::Idle, |(snapshot, cohort)| {
                super::list::CohortState::for_cohort(snapshot, cohort, statuses)
            });
        let state = super::show::state_with_age(state, activity, now);
        // A settling target's outcome follows at once, so it gets no progress line.
        if outcome.is_none()
            && let Some(line) = diagnostic_line(
                &target.instance(),
                target.progress.as_ref(),
                &progress,
                &state,
            )
        {
            writeln!(render::err(), "{line}")?;
        }
        target.progress = Some(progress);
        target.stage = stage.map(|stage| stage.name);
        target.outcome = outcome;
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

fn report_timeout(targets: &[Target], single: bool, json: bool, timeout: Duration) -> Result<()> {
    let mut err = render::err();
    for target in targets.iter().filter(|target| target.outcome.is_none()) {
        // The run loop polls every target before checking its timeout.
        let progress = target
            .progress
            .as_ref()
            .expect("polled target has progress");
        writeln!(
            err,
            "{}",
            timeout_line(&target.instance(), progress, timeout)
        )?;
    }
    if json {
        return if single {
            render::json_pretty(&targets[0].entry())
        } else {
            print_json(targets)
        };
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

    fn settle_over(
        boards: &[Option<&str>],
        live: bool,
    ) -> (Option<String>, Option<RunStatus>, usize) {
        let mut reads = boards.iter().map(|board| {
            board.map(|name| BoardStage {
                name: name.to_owned(),
                owner: None,
            })
        });
        let mut count = 0;
        let (last, status) = settle(
            || {
                count += 1;
                reads.next().expect("no extra board read")
            },
            || Ok(live),
        )
        .expect("settle");
        (last.map(|stage| stage.name), status, count)
    }

    #[test]
    fn settle_completes_on_exact_done_and_fails_only_a_dissolved_cohort() {
        for live in [true, false] {
            assert_eq!(
                settle_over(&[Some("Done")], live),
                (Some("Done".into()), Some(RunStatus::Completed), 1)
            );
        }
        for board in [None, Some("Review"), Some("Done (delta)")] {
            let name = board.map(str::to_owned);
            assert_eq!(settle_over(&[board], true), (name.clone(), None, 1));
            assert_eq!(
                settle_over(&[board, board], false),
                (name, Some(RunStatus::Failed), 2)
            );
        }
    }

    #[test]
    fn settle_rereads_the_board_before_failing_a_dissolved_cohort() {
        assert_eq!(
            settle_over(&[Some("Reflect"), Some("Done")], false),
            (Some("Done".into()), Some(RunStatus::Completed), 2)
        );
    }
}
