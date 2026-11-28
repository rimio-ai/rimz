//! `join`: block on one thing the caller started until it ends or the deadline passes, through
//! the poll the command of the same job runs (`rimz agents wait`, `rimz subagents wait`,
//! `rimz teams wait`, `rimz workflow wait`), and answer its result or that it still runs.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::Tools;
use super::owned::{self, GONE, Named, Owners};
use super::room::Room;
use super::start::TaskKind;
use crate::agents::AgentState;
use crate::harness::schedule::catalog::TaskCatalog;
use crate::harness::workflow::join::{self as workflow_join, JoinTarget, Reading};
use crate::launcher::agent_join::{self, TerminalPayload, WaitSet, WaitTarget};
use crate::launcher::context::{current_channel, resolve_me};
use crate::launcher::team_join::{self, TeamTarget};
use crate::mcp::catalog::JoinArgs;
use crate::mcp::{Answer, Failure};
use crate::message::announce::WaitCaller;
use crate::store::run::RunStatus;
use crate::store::snapshot::SidebarSnapshot;

const TIMEOUT_DEFAULT: Duration = Duration::from_secs(4 * 60);
/// Under a provider's own tool-call limit: Codex's 600 s, Pi's 600 s registration timeout,
/// Claude's 600000 ms script limit.
const TIMEOUT_MAX: Duration = Duration::from_secs(9 * 60);
/// How a wait's outcome reaches the caller.
const BY_MESSAGE: &str = "its outcome comes to you as a message; join does not wait for it";
/// What a booker hears of a schedule's fires: each one's outcome, as a message.
const EACH_FIRE: &str = "a message comes after each fire; join does not wait for it";
const QUEUED: &str = "a message waits for you: read it, then join again";
/// A shell's outcome a fire sent before the join's claim landed.
const ALSO_SENT: &str = "this outcome also came to you as a message";
/// A shell canceled before its command exited.
const CANCELED: &str = "it was canceled before its command exited";
/// The detail of a team its call's `timeout` stopped.
const TEAM_TIMED_OUT: &str =
    "timed_out: its timeout stopped the team before its board reached Done";
/// How often a shell join reads the shell's row, and its history once the row is gone.
const SHELL_POLL: Duration = Duration::from_millis(100);
/// How long past its deadline a join waits for history when dropping its claim finds a fire
/// took it: that fire writes the history just after it removes the row.
const CLAIM_SETTLE: Duration = Duration::from_secs(10);

/// What `join` answers: `done` with the result, `running` at the deadline, or `ended` for a
/// thing with no record left to read.
#[derive(Default, Serialize)]
struct Joined {
    id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

impl Joined {
    fn new(id: &str, status: &'static str) -> Self {
        Self {
            id: id.to_owned(),
            status,
            ..Self::default()
        }
    }

    fn done(id: &str, result: Option<String>, detail: Option<String>) -> Self {
        Self {
            result,
            detail,
            ..Self::new(id, "done")
        }
    }

    /// Still running when the join stopped early: at the deadline, on the call's cancel, or on a
    /// message queued for the caller.
    fn stopped(id: &str, why: Option<Interrupt>) -> Self {
        Self {
            hint: matches!(why, Some(Interrupt::Queued)).then(|| QUEUED.to_owned()),
            ..Self::new(id, "running")
        }
    }
}

/// Why a join stopped before its deadline.
#[derive(Clone, Copy)]
enum Interrupt {
    Cancelled,
    Queued,
}

pub(super) fn join(
    tools: &Tools,
    args: &JoinArgs,
    cancelled: &dyn Fn() -> bool,
) -> Result<Answer, Failure> {
    let (kind, name) = TaskKind::parse(&args.id)?;
    let id = args.id.as_str();
    let timeout = match args.timeout.as_deref() {
        None => TIMEOUT_DEFAULT,
        Some(raw) => crate::utils::time::parse_compact_duration(raw)
            .map_err(|err| Failure::new(format!("invalid timeout `{raw}`: {err}")))?
            .min(TIMEOUT_MAX),
    };
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.alive_snapshot()?;
    let caller = resolve_me(&room.store, &snapshot)?;
    let audit = room.store.audit_rows().map_err(anyhow::Error::from)?;
    let owners = Owners::new(caller, &snapshot.agents, &audit, room.store.paths());
    let root = room.workspace.project_root.clone();
    let named = owned::resolve(&owners, kind, name, || TaskCatalog::load(Some(&root)))?;
    let mut announcer = WaitCaller::starting_now();
    let session = room.workspace.session_name.clone();
    let store = &room.store;
    let mut interrupt = || {
        if cancelled() {
            return Some(Interrupt::Cancelled);
        }
        announcer.queued(store, &session).map(|_| Interrupt::Queued)
    };
    let joined = match named {
        Named::Gone if kind == TaskKind::Shell => ended_shell(&owners, &root, id, name)?,
        Named::Gone
            if kind == TaskKind::Team
                && owned::ended_team_run(&owners, name)
                    .is_some_and(|run| run.status == RunStatus::TimedOut) =>
        {
            Joined::done(id, None, Some(TEAM_TIMED_OUT.to_owned()))
        }
        Named::Gone => Joined {
            hint: Some(GONE.to_owned()),
            ..Joined::new(id, "ended")
        },
        Named::Schedule { .. } if schedule_disabled(&root, name)? => Joined::new(id, "disabled"),
        Named::Schedule { .. } => Joined {
            report: Some(EACH_FIRE),
            ..Joined::new(id, "running")
        },
        Named::Wait { .. } if kind == TaskKind::Shell => {
            match crate::harness::schedule::wait::ShellJoin::open(&root, name)? {
                Some(mut shell) => {
                    let claims = owners.wakes_caller(shell.target());
                    join_shell(id, &mut shell, timeout, claims, &mut interrupt)?
                }
                None => ended_shell(&owners, &root, id, name)?,
            }
        }
        Named::Wait { .. } => Joined {
            report: Some(BY_MESSAGE),
            ..Joined::new(id, "running")
        },
        Named::Agent { row, live } => join_agent(
            &room,
            &snapshot,
            id,
            (row, live),
            timeout,
            (&mut interrupt, cancelled),
        )?,
        Named::Team(cohort) => {
            let checkouts: Vec<_> = cohort
                .members
                .iter()
                .map(|member| member.worktree_path.as_deref())
                .collect();
            let Some(Some(checkout)) = checkouts.first().copied() else {
                return Err(Failure::new(format!(
                    "{id} has no checkout to read a board from"
                )));
            };
            if checkouts.iter().any(|other| *other != Some(checkout)) {
                return Err(Failure::new(format!(
                    "{id} has no single checkout to read a board from"
                )));
            }
            let worktree = crate::utils::path::normalize_path_lexical(Path::new(checkout));
            let mut targets = [TeamTarget::new(cohort.instance.clone(), worktree.clone())];
            let joined = team_join::join(
                &room.workspace,
                &room.store,
                &crate::config::MachineConfig::load_lenient(),
                &mut targets,
                false,
                Some(timeout),
                &mut interrupt,
                &mut |_| Ok(()),
            )?;
            match joined {
                team_join::Joined::Finished { status, .. } => {
                    let result = (status == RunStatus::Completed)
                        .then(|| crate::harness::scratch::board_section(&worktree, "Result"))
                        .flatten();
                    // A cohort its call's timeout stopped dissolves short of Done after the
                    // timeout settled its team run.
                    let timed_out = || {
                        owned::ended_team_run(&owners, name)
                            .is_some_and(|run| run.status == RunStatus::TimedOut)
                    };
                    let detail = match status {
                        RunStatus::Completed => None,
                        _ if timed_out() => Some(TEAM_TIMED_OUT.to_owned()),
                        _ => Some("the cohort ended before its board reached Done".to_owned()),
                    };
                    Joined::done(id, result, detail)
                }
                team_join::Joined::Interrupted(why) => Joined::stopped(id, Some(why)),
                team_join::Joined::StillRunning => Joined::stopped(id, None),
            }
        }
        Named::Workflow { id: run, .. } => {
            let reading = match workflow_join::look(&run)? {
                None => Reading::Ended(workflow_join::Ended::quiet()),
                Some(Ok(reading)) => reading,
                Some(Err(_)) => {
                    return Err(
                        Failure::new(format!("{id} has a record this RimZ cannot read"))
                            .with_hint("`rimz workflow show` names the RimZ that can"),
                    );
                }
            };
            let mut targets = [JoinTarget { id: run, reading }];
            let joined = workflow_join::join(&mut targets, false, Some(timeout), &mut interrupt)?;
            let [target] = targets;
            match (joined, target.reading) {
                (_, Reading::Ended(ended)) => {
                    let state =
                        crate::store::workflow::state_word(ended.state, ended.ending.as_ref());
                    let detail = match &ended.checkout {
                        Some(checkout) => format!("{state} · checkout {}", checkout.display()),
                        None => state.to_owned(),
                    };
                    Joined::done(
                        id,
                        ended.notes.map(|notes| notes.display().to_string()),
                        Some(detail),
                    )
                }
                (_, Reading::NeedsYou { .. }) => Joined {
                    hint: Some(format!(
                        "the run waits on a person; `rimz workflow show {}` says why",
                        target.id
                    )),
                    ..Joined::new(id, "running")
                },
                (workflow_join::Joined::Interrupted(why), _) => Joined::stopped(id, Some(why)),
                (workflow_join::Joined::Unreadable(_), _) => {
                    return Err(Failure::new(format!(
                        "{id} has a record this RimZ cannot read"
                    )));
                }
                _ => Joined::stopped(id, None),
            }
        }
    };
    Ok(Answer::data(&joined))
}

/// What a shell join reads and writes of a shell, apart from the store.
trait ShellSeam {
    /// Claim the shell's outcome until `until`, or drop the claim; `false` once its row is gone.
    fn claim(&mut self, until: Option<jiff::Timestamp>) -> anyhow::Result<bool>;
    /// Whether the shell's row is still there.
    fn present(&mut self) -> anyhow::Result<bool>;
    /// The shell's terminal history record, once one is written.
    fn ended(&mut self)
    -> anyhow::Result<Option<crate::harness::schedule::run_log::LoopRunRecord>>;
}

impl ShellSeam for crate::harness::schedule::wait::ShellJoin {
    fn claim(&mut self, until: Option<jiff::Timestamp>) -> anyhow::Result<bool> {
        crate::harness::schedule::wait::ShellJoin::claim(self, until)
    }

    fn present(&mut self) -> anyhow::Result<bool> {
        Ok(crate::harness::schedule::wait::ShellJoin::present(self))
    }

    fn ended(
        &mut self,
    ) -> anyhow::Result<Option<crate::harness::schedule::run_log::LoopRunRecord>> {
        Ok(crate::harness::schedule::wait::ShellJoin::ended(self))
    }
}

/// Join a shell: claim its outcome until the deadline when `claims` (the caller is the agent
/// its message goes to), so its exit comes to this join and not as a message, then read its
/// history once its row is gone. A deadline or an interrupt while the row stands drops the
/// claim; a row gone under the claim (a fire took it, or a cancel) is waited out to the
/// deadline, and a claim found taken at the deadline waits up to `CLAIM_SETTLE` more for the
/// history the taking fire writes next.
fn join_shell(
    id: &str,
    seam: &mut dyn ShellSeam,
    timeout: Duration,
    claims: bool,
    interrupt: &mut dyn FnMut() -> Option<Interrupt>,
) -> anyhow::Result<Joined> {
    let mut deadline = std::time::Instant::now() + timeout;
    let mut present = if claims {
        seam.claim(Some(jiff::Timestamp::now().checked_add(timeout)?))?
    } else {
        seam.present()?
    };
    loop {
        if !present && let Some(record) = seam.ended()? {
            return Ok(shell_outcome(id, &record, claims));
        }
        let why = interrupt();
        let at_deadline = std::time::Instant::now() >= deadline;
        if present && (why.is_some() || at_deadline) {
            if !claims || seam.claim(None)? {
                return Ok(Joined::stopped(id, why));
            }
            present = false;
            if at_deadline {
                deadline = std::time::Instant::now() + CLAIM_SETTLE;
            }
            continue;
        }
        if !present && matches!(why, Some(Interrupt::Cancelled)) {
            return Ok(Joined::stopped(id, why));
        }
        if !present && at_deadline {
            return Ok(Joined {
                hint: Some(GONE.to_owned()),
                ..Joined::new(id, "ended")
            });
        }
        std::thread::sleep(SHELL_POLL);
        if present {
            present = seam.present()?;
        }
    }
}

/// A shell whose row is gone: its last outcome from history, for a caller that armed it, else
/// `ended`.
fn ended_shell(owners: &Owners, root: &Path, id: &str, name: &str) -> Result<Joined, Failure> {
    let Some((record, target)) =
        crate::harness::schedule::wait::ShellJoin::last_outcome(root, name)
            .and_then(|record| record.wakes.clone().map(|target| (record, target)))
    else {
        return Ok(Joined {
            hint: Some(GONE.to_owned()),
            ..Joined::new(id, "ended")
        });
    };
    if !owners.armed_self_wait(&target) {
        return Err(owned::refused(id));
    }
    Ok(shell_outcome(id, &record, owners.wakes_caller(&target)))
}

/// A shell's outcome from its history: the exit or verdict, the output file and its size, and,
/// for the agent the shell's message goes to (`own`), whether a message carried it too; or
/// `ended` for a shell canceled first.
fn shell_outcome(
    id: &str,
    record: &crate::harness::schedule::run_log::LoopRunRecord,
    own: bool,
) -> Joined {
    use crate::harness::schedule::run_log::LoopRunResult;
    if record.result == LoopRunResult::Canceled {
        return Joined {
            hint: Some(CANCELED.to_owned()),
            ..Joined::new(id, "ended")
        };
    }
    let verdict = record
        .watch
        .as_ref()
        .map_or_else(|| record.result.label().to_owned(), |watch| watch.label());
    let output = record
        .check
        .as_ref()
        .and_then(|check| check.output_path.clone());
    let detail = match &output {
        Some(path) => {
            let size = crate::disk::summary::FileSummary::measure(path)
                .ok()
                .filter(|summary| !summary.is_empty())
                .map_or_else(|| "no output".to_owned(), |summary| summary.label());
            format!("{verdict} · {size}")
        }
        None => verdict,
    };
    Joined {
        hint: (own && record.result == LoopRunResult::Delivered).then(|| ALSO_SENT.to_owned()),
        ..Joined::done(
            id,
            output.map(|path| path.display().to_string()),
            Some(detail),
        )
    }
}

/// Whether schedule `name` is switched off, so no fire and no message comes while it stays so.
fn schedule_disabled(root: &Path, name: &str) -> anyhow::Result<bool> {
    use crate::harness::schedule::arming::ArmState;
    let catalog = TaskCatalog::load(Some(root))?;
    let Some(task) = catalog.visible().get(name) else {
        return Ok(false);
    };
    let records = crate::harness::schedule::task_record::load()?;
    let arming = records
        .live(&task.key_for_root(name, root))
        .and_then(|(_, record)| record.arming.as_ref());
    Ok(matches!(
        ArmState::resolve(arming, task.source(), jiff::Timestamp::now()),
        ArmState::Disabled(_)
    ))
}

/// Whether `run` is the self-cleaning run `row`'s own launch opened (a `stay: false` call's,
/// say), which no later turn of that launch supersedes, so its record is the answer while the
/// pane closes. A kept run's agent works on, and its later turns supersede the run.
fn own_run(run: &crate::store::run::RunRecord, row: &AgentState) -> bool {
    run.self_cleanup
        && run.peer.is_none()
        && run.team.is_none()
        && run.launch_id.is_some()
        && run.launch_id == row.launch_id
}

/// Join a subagent or agent: its supervised run when the run still goes, the agent has ended,
/// or the run is the subagent's or the peer's latest turn or the launch's own, else its live
/// turn, as `rimz agents wait` picks; a settled run is claimed so the digest does not deliver it
/// again.
fn join_agent(
    room: &Room,
    snapshot: &SidebarSnapshot,
    id: &str,
    (row, live): (&AgentState, Option<&AgentState>),
    timeout: Duration,
    (interrupt, cancelled): (&mut dyn FnMut() -> Option<Interrupt>, &dyn Fn() -> bool),
) -> Result<Joined, Failure> {
    let store = &room.store;
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| row.launch_key().to_string());
    let run = crate::launcher::stop::newest_run_for_agent(store, row)?;
    let latest_turn = |run: &crate::store::run::RunRecord| -> Result<bool, Failure> {
        let Some(peer) = live.filter(|_| run.peer.is_some()) else {
            return Ok(false);
        };
        let view =
            crate::message::turn_wait::TurnWaitView::load(store).map_err(anyhow::Error::from)?;
        Ok(crate::harness::run::describes_latest_turn(
            run,
            peer,
            view.completion(peer),
        ))
    };
    let reader = current_channel(&room.workspace, Some(store)).address_context();
    let target = match run {
        Some(run)
            if !run.status.is_terminal()
                || live.is_none()
                || (run.subagent && row.is_launched_child())
                || own_run(&run, row)
                || latest_turn(&run)? =>
        {
            WaitTarget::Run {
                run_id: run.run_id,
                name,
                typed_reference: id.to_owned(),
                reason: None,
            }
        }
        _ => {
            let Some(live) = live else {
                return Ok(Joined {
                    hint: Some("it ended with no result recorded".to_owned()),
                    ..Joined::new(id, "ended")
                });
            };
            WaitTarget::Agent {
                reference: crate::address::paste_address(live, snapshot, &reader),
                typed_reference: id.to_owned(),
                name,
                reason: None,
                kind: live.kind.clone(),
                completed_seen_at: None,
            }
        }
    };
    let mut waits = WaitSet::new(vec![target], false, Some(timeout));
    let joined = agent_join::join(store, &reader, &mut waits, interrupt, &mut |_| Ok(()))?;
    let answer = match joined {
        agent_join::Joined::Settled { selected, .. } => {
            let Some(outcome) = waits.outcome(selected) else {
                return Err(Failure::new(format!("{id} settled with no outcome")));
            };
            // A cancelled call's answer reaches no one, so the digest keeps the run to deliver.
            // A test holds the call before the claim and after it.
            #[cfg(feature = "testkit")]
            crate::testkit::rendezvous("RIMZ_TEST_MCP_JOIN_CLAIM_GATE");
            if !cancelled() {
                agent_join::mark_joined(store, &room.workspace.session_name, [outcome]);
            }
            #[cfg(feature = "testkit")]
            crate::testkit::rendezvous("RIMZ_TEST_MCP_JOIN_CLAIM_GATE");
            match &outcome.payload {
                TerminalPayload::Run(record) => {
                    let detail = (record.status != RunStatus::Completed).then(|| {
                        let status = record.status.as_str();
                        record
                            .failure_tail
                            .as_deref()
                            .map_or_else(|| status.to_owned(), |tail| format!("{status}: {tail}"))
                    });
                    Joined::done(id, record.last_message.clone(), detail)
                }
                TerminalPayload::Agent {
                    completion,
                    last_message,
                    ..
                } => {
                    let detail = (*completion == crate::agents::TurnCompletion::Failed)
                        .then(|| "its turn failed".to_owned());
                    Joined::done(id, last_message.clone(), detail)
                }
                TerminalPayload::Disappeared => Joined {
                    hint: Some("it left the room while the join waited".to_owned()),
                    ..Joined::new(id, "ended")
                },
            }
        }
        agent_join::Joined::StillRunning { .. } => Joined::stopped(id, None),
        agent_join::Joined::Interrupted { answer, .. } => Joined::stopped(id, Some(answer)),
    };
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::harness::schedule::run_log::{
        CheckRecord, LoopRunMode, LoopRunRecord, LoopRunResult,
    };
    use crate::harness::signal::WatchVerdict;

    /// A shell whose row reads `rows` and whose history reads `ends`, poll by poll. A claim
    /// answers `claimed`, and dropping it answers `dropped` (`false`: a fire took the row).
    struct Fake {
        claimed: bool,
        dropped: bool,
        rows: VecDeque<bool>,
        ends: VecDeque<Option<LoopRunRecord>>,
        claims: Vec<bool>,
        present: bool,
        /// History reads made while the row still read present.
        early_reads: usize,
    }

    impl Fake {
        fn new(claimed: bool, rows: Vec<bool>, ends: Vec<Option<LoopRunRecord>>) -> Self {
            Self {
                claimed,
                dropped: true,
                rows: rows.into(),
                ends: ends.into(),
                claims: Vec::new(),
                present: true,
                early_reads: 0,
            }
        }
    }

    impl ShellSeam for Fake {
        fn claim(&mut self, until: Option<jiff::Timestamp>) -> anyhow::Result<bool> {
            self.claims.push(until.is_some());
            let held = if until.is_some() {
                self.claimed
            } else {
                self.dropped
            };
            self.present = held;
            Ok(held)
        }

        fn present(&mut self) -> anyhow::Result<bool> {
            self.present = self.rows.pop_front().unwrap_or(false);
            Ok(self.present)
        }

        fn ended(&mut self) -> anyhow::Result<Option<LoopRunRecord>> {
            if self.present {
                self.early_reads += 1;
            }
            Ok(self.ends.pop_front().flatten())
        }
    }

    fn ended(result: LoopRunResult, output: &std::path::Path) -> LoopRunRecord {
        let mut record = LoopRunRecord::new("shell-otter", result, LoopRunMode::Scheduled, 0);
        record.watch = Some(WatchVerdict::Exited {
            code: Some(3),
            elapsed_ms: 2_000,
        });
        record.check = Some(CheckRecord {
            code: Some(3),
            timed_out: false,
            output: String::new(),
            output_path: Some(output.to_owned()),
            duration_ms: None,
            agent: None,
        });
        record
    }

    fn join(
        fake: &mut Fake,
        claims: bool,
        timeout: Duration,
        interrupt: Option<Interrupt>,
    ) -> serde_json::Value {
        let joined = join_shell("shell:otter", fake, timeout, claims, &mut || interrupt).unwrap();
        serde_json::to_value(joined).unwrap()
    }

    fn output_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("shell-otter.output");
        std::fs::write(&output, "failed here\n").unwrap();
        (dir, output)
    }

    fn done(output: &std::path::Path, hint: Option<&str>) -> serde_json::Value {
        let mut expected = serde_json::json!({
            "id": "shell:otter",
            "status": "done",
            "result": output.display().to_string(),
            "detail": "exit 3 after 2s · <1k tokens, 1 line",
        });
        if let Some(hint) = hint {
            expected["hint"] = serde_json::json!(hint);
        }
        expected
    }

    #[test]
    fn a_shell_join_answers_the_outcome_its_claim_took_or_a_fire_sent_first() {
        let (_dir, output) = output_file();
        let mut answers = Vec::new();
        for (case, claimed, claims, rows, result, hint) in [
            (
                "claimed",
                true,
                true,
                vec![true, true, true],
                LoopRunResult::Joined,
                None,
            ),
            // The fire ran before the claim landed: its message carried the outcome to the caller.
            (
                "sent first",
                false,
                true,
                Vec::new(),
                LoopRunResult::Delivered,
                Some(ALSO_SENT),
            ),
            // Another owner's join claims nothing, and the message went to the shell's agent.
            (
                "not its agent",
                true,
                false,
                vec![true, true],
                LoopRunResult::Delivered,
                None,
            ),
        ] {
            let mut fake = Fake::new(
                claimed,
                rows,
                vec![None, None, Some(ended(result, &output))],
            );
            let answer = join(&mut fake, claims, Duration::from_secs(5), None);
            answers.push((
                case,
                answer == done(&output, hint),
                fake.claims.clone(),
                fake.early_reads,
            ));
        }
        assert_eq!(
            answers,
            [
                ("claimed", true, vec![true], 0),
                ("sent first", true, vec![true], 0),
                ("not its agent", true, Vec::new(), 0),
            ]
        );
    }

    #[test]
    fn a_shell_join_waits_out_a_row_a_fire_took_and_never_answers_running_for_it() {
        let (_dir, output) = output_file();
        let joined = || Some(ended(LoopRunResult::Joined, &output));
        // History lands well after the row is gone, short of the deadline.
        let mut late = Fake::new(true, vec![false], [vec![None; 8], vec![joined()]].concat());
        // A queued message interrupts the join after a fire took its claim.
        let mut taken = Fake::new(true, vec![true], vec![None, None, joined()]);
        taken.dropped = false;
        let answers = [
            join(&mut late, true, Duration::from_secs(5), None),
            join(
                &mut taken,
                true,
                Duration::from_secs(5),
                Some(Interrupt::Queued),
            ),
        ];
        assert_eq!(answers, [done(&output, None), done(&output, None)]);
        assert_eq!(taken.claims, [true, false]);
    }

    #[test]
    fn a_shell_join_answers_a_cancel_under_its_claim_at_once() {
        let mut canceled = LoopRunRecord::new(
            "shell-otter",
            LoopRunResult::Canceled,
            LoopRunMode::Manual,
            0,
        );
        canceled.wakes = Some(crate::config::TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "launch-session".into(),
            handle: "@otter".into(),
        });
        let mut fake = Fake::new(true, vec![false], vec![None, None, Some(canceled)]);
        let started = std::time::Instant::now();
        assert_eq!(
            join(&mut fake, true, Duration::from_secs(5), None),
            serde_json::json!({"id": "shell:otter", "status": "ended", "hint": CANCELED})
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a canceled shell ends its join before the deadline: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_shell_join_ends_on_a_row_gone_without_history_and_drops_its_claim_at_the_deadline() {
        let mut gone = Fake::new(true, Vec::new(), Vec::new());
        let started = std::time::Instant::now();
        assert_eq!(
            join(&mut gone, true, Duration::from_millis(400), None),
            serde_json::json!({"id": "shell:otter", "status": "ended", "hint": GONE})
        );
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "a row gone under the claim is waited out to the deadline: {:?}",
            started.elapsed()
        );
        let mut running = Fake::new(true, vec![true; 100], Vec::new());
        assert_eq!(
            join(&mut running, true, Duration::from_millis(400), None),
            serde_json::json!({"id": "shell:otter", "status": "running"})
        );
        assert_eq!(running.claims, [true, false]);
        assert_eq!(
            running.early_reads, 0,
            "history is read only once the row is gone"
        );
    }
}
