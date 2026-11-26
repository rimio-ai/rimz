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
/// What a booker hears of a schedule's fires: nothing delivers one a message (the fire's
/// launch carries the booking chain for ownership only).
const NO_MESSAGE: &str = "no message comes when it fires; join does not wait for it";
const QUEUED: &str = "a message waits for you: read it, then join again";

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
        Named::Gone => Joined {
            hint: Some(GONE.to_owned()),
            ..Joined::new(id, "ended")
        },
        Named::Schedule { .. } if schedule_disabled(&root, name)? => Joined::new(id, "disabled"),
        Named::Schedule { .. } => Joined {
            report: Some(NO_MESSAGE),
            ..Joined::new(id, "running")
        },
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
                    let detail = (status != RunStatus::Completed)
                        .then(|| "the cohort ended before its board reached Done".to_owned());
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

/// Join a subagent or agent: its supervised run when the run still goes, the agent has ended,
/// or the run is the subagent's or the peer's latest turn, else its live turn, as
/// `rimz agents wait` picks; a settled run is claimed so the digest does not deliver it again.
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
