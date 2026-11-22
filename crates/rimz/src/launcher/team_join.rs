//! Waiting on team cohorts: poll each cohort's board until it reaches `Done` or the cohort
//! dissolves, with the caller's interrupt and deadline. Nothing here prints or exits; the caller
//! renders each poll it is shown and the value that comes back.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::agents::AgentStatus;
use crate::config::MachineConfig;
use crate::harness::scratch::{BoardStage, board_stage};
use crate::ids::TeamInstanceId;
use crate::store::Store;
use crate::store::run::RunStatus;
use crate::store::snapshot::SidebarSnapshot;
use crate::workspace::ResolvedWorkspace;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A cohort the join watches: its board's worktree, the last stage and progress read, and its
/// outcome once settled.
#[derive(Clone, Debug)]
pub struct TeamTarget {
    pub cohort: TeamInstanceId,
    pub worktree: PathBuf,
    pub stage: Option<String>,
    pub progress: Option<Progress>,
    pub outcome: Option<RunStatus>,
}

impl TeamTarget {
    pub fn new(cohort: TeamInstanceId, worktree: PathBuf) -> Self {
        Self {
            cohort,
            worktree,
            stage: None,
            progress: None,
            outcome: None,
        }
    }

    /// The settled outcome, or `TimedOut` while the cohort is still going.
    pub fn status(&self) -> RunStatus {
        self.outcome.unwrap_or(RunStatus::TimedOut)
    }
}

/// What one poll read of a pending cohort: its board stage and owner, whether the owner holds
/// a live member, and the members that need a person, by handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: Option<String>,
    pub owner: Option<String>,
    pub owner_live: Option<bool>,
    pub needs_you: Vec<(String, AgentStatus)>,
}

/// One cohort as a poll saw it, shown to the caller before the target takes the new reading.
pub struct Seen<'a> {
    pub target: &'a TeamTarget,
    pub progress: &'a Progress,
    /// Set when the board read settled the cohort this poll.
    pub outcome: Option<RunStatus>,
    /// The room snapshot, read only when some board was not yet `Done`.
    pub snapshot: Option<&'a SidebarSnapshot>,
    /// The cohort's live members in that snapshot, with their statuses.
    pub members: Option<&'a crate::address::TeamCohort<'a>>,
    pub statuses: &'a [AgentStatus],
    pub idle_since: Option<jiff::Timestamp>,
    pub now: jiff::Timestamp,
}

/// What a poll shows the caller.
pub enum Polled<'a> {
    /// One cohort read, pending or settling.
    Seen(Seen<'a>),
    /// The poll ended; these indices settled during it, in argument order.
    Settled {
        targets: &'a [TeamTarget],
        settled: &'a [usize],
    },
}

/// Why [`join`] returned. The targets hold each cohort's latest reading.
#[derive(Debug)]
pub enum Joined<I> {
    /// Every cohort settled, or under `any` the one at `first` did; `status` is the first
    /// status other than `Completed` among those reported, else `Completed`.
    Finished {
        status: RunStatus,
        first: Option<usize>,
    },
    /// The caller's interrupt answered this.
    Interrupted(I),
    /// The deadline passed with cohorts still going; they are left running.
    StillRunning,
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
            .is_some_and(|stage| stage.name == crate::config::DONE_STAGE)
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

/// Polls every pending target until all settle (under `any`, until one does), `interrupt`
/// answers, or `timeout` passes since the call. `observe` sees each poll; `interrupt` is read
/// once per poll, after it; nothing is held locked across the sleep.
#[allow(clippy::too_many_arguments)]
pub fn join<I>(
    workspace: &ResolvedWorkspace,
    store: &Store,
    machine: &MachineConfig,
    targets: &mut [TeamTarget],
    any: bool,
    timeout: Option<Duration>,
    interrupt: &mut dyn FnMut() -> Option<I>,
    observe: &mut dyn FnMut(Polled<'_>) -> Result<()>,
) -> Result<Joined<I>> {
    let started = Instant::now();
    loop {
        let settled = poll(workspace, store, machine, targets, observe)?;
        observe(Polled::Settled {
            targets,
            settled: &settled,
        })?;
        let selected = settled.first().copied();
        let finished = match selected {
            Some(index) if any => Some((targets[index].status(), Some(index))),
            _ if targets.iter().all(|target| target.outcome.is_some()) => Some((
                targets
                    .iter()
                    .map(TeamTarget::status)
                    .find(|status| *status != RunStatus::Completed)
                    .unwrap_or(RunStatus::Completed),
                None,
            )),
            _ => None,
        };
        if let Some((status, first)) = finished {
            return Ok(Joined::Finished { status, first });
        }
        if let Some(answer) = interrupt() {
            return Ok(Joined::Interrupted(answer));
        }
        if let Some(timeout) = timeout
            && started.elapsed() >= timeout
        {
            return Ok(Joined::StillRunning);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn published_snapshot(workspace: &ResolvedWorkspace, store: &Store) -> Result<SidebarSnapshot> {
    crate::sidebar::consumer::PublishedSnapshotReader::new(
        store.runtime_paths().clone(),
        workspace.session_name.clone(),
        None,
    )
    .read(store.paths())
    .context("reading the room snapshot")
}

/// Settle every pending target that can settle now, returning their indices in
/// argument order. The snapshot is read only when some board is not yet `Done`.
fn poll(
    workspace: &ResolvedWorkspace,
    store: &Store,
    machine: &MachineConfig,
    targets: &mut [TeamTarget],
    observe: &mut dyn FnMut(Polled<'_>) -> Result<()>,
) -> Result<Vec<usize>> {
    let mut snapshot = None;
    let mut demotion = None;
    let no_demotion = crate::agents::ParkDemotion::default();
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
                    None => snapshot.insert(published_snapshot(workspace, store)?),
                };
                Ok(crate::address::team_cohorts(snapshot.agents.iter())
                    .iter()
                    .any(|cohort| cohort.instance == target.cohort))
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
        let mut idle_since = None;
        let cohort = snapshot.as_ref().and_then(|snapshot| {
            crate::address::team_cohorts(snapshot.agents.iter())
                .into_iter()
                .find(|cohort| cohort.instance == target.cohort)
        });
        if let Some(snapshot) = &snapshot
            && let Some(cohort) = &cohort
        {
            let peers = crate::address::addressable_agents(snapshot);
            progress.owner_live = progress.owner.as_deref().map(|owner| {
                !crate::address::role_holders(cohort.members.iter().copied(), owner).is_empty()
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
                        crate::harness::park_demotion(
                            Some(store),
                            store.paths(),
                            store.runtime_paths(),
                            machine,
                            &snapshot.agents,
                            snapshot.resume_outcomes.as_deref().unwrap_or_default(),
                            now,
                        )
                    }),
                };
                let attention = crate::store::snapshot::needs_you(agent, row, None, demotion, now);
                let status = row
                    .and_then(|row| row.status())
                    .unwrap_or_else(|| agent.rowless_status(demotion, snapshot.now).0);
                statuses.push(status);
                idle_since =
                    idle_since.max(Some(crate::store::snapshot::agent_idle_since(agent, row)));
                if let Some(attention) = attention {
                    progress.needs_you.push((
                        crate::address::agent_handle(agent, &peers, false),
                        attention.status,
                    ));
                }
            }
        }
        progress
            .needs_you
            .sort_by(|left, right| left.0.cmp(&right.0));
        observe(Polled::Seen(Seen {
            target,
            progress: &progress,
            outcome,
            snapshot: snapshot.as_ref(),
            members: cohort.as_ref(),
            statuses: &statuses,
            idle_since,
            now,
        }))?;
        target.progress = Some(progress);
        target.stage = stage.map(|stage| stage.name);
        target.outcome = outcome;
        if target.outcome.is_some() {
            settled.push(index);
        }
    }
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use super::*;

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
