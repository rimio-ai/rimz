//! Waiting on agents and supervised runs: each target is polled until its run ends or its
//! turn settles, under the caller's interrupt and deadline. Nothing here prints or exits; the
//! caller renders what each poll shows it and the value that comes back.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::address::AddressContext;
use crate::agents::{AgentState, TurnCompletion};
use crate::ids::{AgentKind, RunId};
use crate::message::turn_wait::{TurnWaitReason, TurnWaitView};
use crate::store::Store;
use crate::store::run::RunRecord;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One thing a join waits on: a supervised run by id, or a live agent's current turn by the
/// reference it is resolved again by on every poll. `reason` is the last wait reason seen.
pub enum WaitTarget {
    Run {
        run_id: RunId,
        name: String,
        typed_reference: String,
        reason: Option<TurnWaitReason>,
    },
    Agent {
        reference: String,
        typed_reference: String,
        name: String,
        reason: Option<TurnWaitReason>,
        kind: AgentKind,
        completed_seen_at: Option<Instant>,
    },
}

impl WaitTarget {
    /// The reference as the caller typed it.
    pub fn typed_reference(&self) -> &str {
        match self {
            Self::Run {
                typed_reference, ..
            }
            | Self::Agent {
                typed_reference, ..
            } => typed_reference,
        }
    }
}

/// A settled target: the reference it was typed as, its agent name, and what ended it.
pub struct TargetOutcome {
    pub reference: String,
    pub name: String,
    pub payload: TerminalPayload,
}

pub enum TerminalPayload {
    /// The run ended; its record as it ended.
    Run(Box<RunRecord>),
    /// The agent's turn settled, with the turn's final message when one was recorded.
    Agent {
        agent: Box<AgentState>,
        completion: TurnCompletion,
        last_message: Option<String>,
    },
    /// The agent no longer resolves.
    Disappeared,
}

/// How long a completed turn may wait for its final message to reach the
/// transcript. The hook helper records the turn's end before it appends the
/// message, and a provider whose hook carries none settles after this grace.
const FINAL_MESSAGE_GRACE: Duration = Duration::from_secs(3);

/// Decide whether an interactive turn has settled, and with which message.
/// `None` keeps waiting; `Some(message)` settles. Only a message recorded
/// during the current turn counts. A wait deadline cuts the grace short, since
/// the turn itself has already completed.
pub fn settle_agent_turn(
    completion: TurnCompletion,
    turn_started_at: Option<jiff::Timestamp>,
    latest: Option<crate::transcript::TranscriptEntry>,
    completed_seen_at: &mut Option<Instant>,
    (now, deadline): (Instant, Option<Instant>),
) -> Option<Option<String>> {
    if completion == TurnCompletion::Open {
        *completed_seen_at = None;
        return None;
    }
    let message = latest
        .filter(|entry| turn_started_at.is_some_and(|started| entry.at >= started))
        .map(|entry| entry.text);
    if message.is_some() || completion == TurnCompletion::Failed {
        return Some(message);
    }
    let seen = *completed_seen_at.get_or_insert(now);
    (now.duration_since(seen) >= FINAL_MESSAGE_GRACE
        || deadline.is_some_and(|deadline| now >= deadline))
    .then_some(None)
}

/// The one agent a reference names now: `@me` is the caller, anything else must match
/// exactly one agent.
fn resolve_one<'a>(
    store: &Store,
    snapshot: &'a crate::store::snapshot::SidebarSnapshot,
    reference: &str,
    current_channel: &AddressContext,
) -> Option<&'a AgentState> {
    if reference == "@me" {
        return crate::launcher::context::resolve_me(store, snapshot).ok();
    }
    match crate::address::resolve_many(snapshot, reference, None, current_channel) {
        Ok(matches) => match matches.as_slice() {
            [agent] => Some(agent),
            _ => None,
        },
        Err(_) => None,
    }
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

/// What a poll shows the caller.
pub enum Observed<'a> {
    /// A pending target's wait reason, read this poll, beside the one read before it.
    Reason {
        address: &'a str,
        previous: Option<TurnWaitReason>,
        reason: Option<TurnWaitReason>,
    },
    /// A poll that left targets pending; these indices settled during it.
    Pending {
        waits: &'a WaitSet,
        settled: &'a [usize],
    },
}

fn observe_reason(
    observe: &mut dyn FnMut(Observed<'_>) -> Result<()>,
    slot: &mut Option<TurnWaitReason>,
    reason: Option<TurnWaitReason>,
    address: &str,
) -> Result<()> {
    let previous = slot.take();
    *slot = reason.clone();
    observe(Observed::Reason {
        address,
        previous,
        reason,
    })
}

fn poll_target(
    store: &Store,
    agent_view: Option<&TurnWaitView>,
    target: &mut WaitTarget,
    current_channel: &AddressContext,
    deadline: Option<Instant>,
    observe: &mut dyn FnMut(Observed<'_>) -> Result<()>,
) -> Result<Option<TargetOutcome>> {
    match target {
        WaitTarget::Run {
            run_id,
            name,
            typed_reference,
            reason,
        } => {
            let record = crate::harness::run::load(store.paths(), run_id)?;
            *name = record
                .agent_name
                .clone()
                .unwrap_or_else(|| run_id.to_string());
            if !record.status.is_terminal() {
                let agent = agent_view.and_then(|view| {
                    view.snapshot
                        .agents
                        .iter()
                        .find(|agent| record.matches_agent(agent))
                        .map(|agent| (view, agent))
                });
                match agent {
                    Some((view, agent)) => {
                        let address =
                            crate::address::paste_address(agent, &view.snapshot, current_channel);
                        observe_reason(observe, reason, view.reason(agent), &address)?;
                    }
                    None => *reason = None,
                }
                return Ok(None);
            }
            Ok(Some(TargetOutcome {
                reference: typed_reference.clone(),
                name: name.clone(),
                payload: TerminalPayload::Run(Box::new(record)),
            }))
        }
        WaitTarget::Agent {
            reference,
            typed_reference,
            name,
            reason,
            completed_seen_at,
            ..
        } => {
            let view = agent_view.context("pending agent target without snapshot")?;
            let Some(agent) = resolve_one(store, &view.snapshot, reference, current_channel) else {
                return Ok(Some(TargetOutcome {
                    reference: typed_reference.clone(),
                    name: name.clone(),
                    payload: TerminalPayload::Disappeared,
                }));
            };
            let address = crate::address::paste_address(agent, &view.snapshot, current_channel);
            observe_reason(observe, reason, view.reason(agent), &address)?;
            *name = agent_name(agent).to_owned();
            let completion = view.completion(agent);
            let latest = match agent.turn_started_at {
                Some(since) if completion != TurnCompletion::Open => {
                    crate::transcript::latest_assistant(
                        store.paths(),
                        &agent.kind,
                        &agent.agent_id,
                        since,
                    )
                    .context("reading the agent's final message")?
                }
                _ => None,
            };
            let settled = settle_agent_turn(
                completion,
                agent.turn_started_at,
                latest,
                completed_seen_at,
                (Instant::now(), deadline),
            );
            Ok(settled.map(|last_message| TargetOutcome {
                reference: typed_reference.clone(),
                name: agent_name(agent).to_owned(),
                payload: TerminalPayload::Agent {
                    agent: Box::new(agent.clone()),
                    completion,
                    last_message,
                },
            }))
        }
    }
}

/// The targets of one join and the outcome of each that settled, in argument order.
pub struct WaitSet {
    pub targets: Vec<WaitTarget>,
    pub outcomes: Vec<Option<TargetOutcome>>,
    deadline: Option<Instant>,
    any: bool,
    /// Polls every target once and stops, as a join deferring to the caller's report does.
    once: bool,
}

/// Why [`join`] returned; `settled` holds the indices that settled in its last poll.
pub enum Joined<I> {
    /// Every target settled, or under `any` the one at `selected` did (`0` otherwise).
    Settled {
        selected: usize,
        settled: Vec<usize>,
    },
    /// The deadline passed with targets pending; they are left running.
    StillRunning { settled: Vec<usize> },
    /// The caller's interrupt answered this.
    Interrupted { settled: Vec<usize>, answer: I },
}

impl WaitSet {
    /// A join over `targets`, settling on all of them or under `any` on the first, with its
    /// deadline `timeout` from now.
    pub fn new(targets: Vec<WaitTarget>, any: bool, timeout: Option<Duration>) -> Self {
        let outcomes = std::iter::repeat_with(|| None)
            .take(targets.len())
            .collect();
        Self {
            targets,
            outcomes,
            deadline: timeout.map(|duration| Instant::now() + duration),
            any,
            once: false,
        }
    }

    /// A set polled once: every target is read, `any` is met by the first settled one in
    /// order, and an unmet join ends as [`Joined::StillRunning`] unless the interrupt answers.
    pub fn once(targets: Vec<WaitTarget>, any: bool) -> Self {
        Self {
            once: true,
            ..Self::new(targets, any, None)
        }
    }

    pub fn outcome(&self, index: usize) -> Option<&TargetOutcome> {
        self.outcomes.get(index)?.as_ref()
    }

    /// One poll: the settled indices, and under `any` the one that settled first.
    fn poll(
        &mut self,
        store: &Store,
        current_channel: &AddressContext,
        observe: &mut dyn FnMut(Observed<'_>) -> Result<()>,
    ) -> Result<(Vec<usize>, Option<usize>)> {
        let pending_agent = self
            .targets
            .iter()
            .zip(&self.outcomes)
            .any(|(target, outcome)| {
                matches!(target, WaitTarget::Agent { .. }) && outcome.is_none()
            });
        let agent_view = if pending_agent {
            Some(TurnWaitView::load(store).context("reading agent snapshot")?)
        } else if self.outcomes.iter().any(Option::is_none) {
            TurnWaitView::load(store).ok()
        } else {
            None
        };
        let mut settled = Vec::new();
        for (index, target) in self.targets.iter_mut().enumerate() {
            if self.outcomes[index].is_some() {
                continue;
            }
            let Some(outcome) = poll_target(
                store,
                agent_view.as_ref(),
                target,
                current_channel,
                self.deadline,
                observe,
            )?
            else {
                continue;
            };
            self.outcomes[index] = Some(outcome);
            settled.push(index);
            if self.any && !self.once {
                return Ok((settled, Some(index)));
            }
        }
        let selected = self
            .any
            .then(|| self.outcomes.iter().position(Option::is_some))
            .flatten();
        Ok((settled, selected))
    }
}

/// Polls the pending targets until all settle (under `any`, until one does), `interrupt`
/// answers, or the set's deadline passes. `observe` sees each wait reason and each poll that
/// leaves targets pending; `interrupt` is read once per poll; nothing is held locked across
/// the sleep.
pub fn join<I>(
    store: &Store,
    current_channel: &AddressContext,
    waits: &mut WaitSet,
    interrupt: &mut dyn FnMut() -> Option<I>,
    observe: &mut dyn FnMut(Observed<'_>) -> Result<()>,
) -> Result<Joined<I>> {
    loop {
        let (settled, selected) = waits.poll(store, current_channel, observe)?;
        if let Some(selected) = selected {
            return Ok(Joined::Settled { selected, settled });
        }
        if waits.outcomes.iter().all(Option::is_some) {
            return Ok(Joined::Settled {
                selected: 0,
                settled,
            });
        }
        if let Some(answer) = interrupt() {
            return Ok(Joined::Interrupted { settled, answer });
        }
        if waits.once
            || waits
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Ok(Joined::StillRunning { settled });
        }
        observe(Observed::Pending {
            waits,
            settled: &settled,
        })?;
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Marks each settled run the caller may claim as joined, so its result is not delivered
/// again in the digest. A run outside its launcher's turn stays unjoined. Failures are logged.
pub fn mark_joined<'a>(
    store: &Store,
    session_name: &str,
    outcomes: impl IntoIterator<Item = &'a TargetOutcome>,
) {
    let snapshot = std::cell::LazyCell::new(|| {
        store
            .snapshot_cached()
            .inspect_err(|err| {
                tracing::warn!(error = %err, "could not read the caller for the joined run");
            })
            .ok()
    });
    let caller = std::cell::LazyCell::new(|| {
        snapshot
            .as_ref()
            .and_then(|snapshot| crate::harness::ancestry::resolve_caller(&snapshot.agents))
    });
    let audit = std::cell::LazyCell::new(|| {
        store
            .runtime_projection(crate::RuntimeScope::Audit)
            .inspect_err(|err| {
                tracing::warn!(error = %err, "could not read the joined run's agent rows");
            })
            .ok()
    });
    for outcome in outcomes {
        let TerminalPayload::Run(record) = &outcome.payload else {
            continue;
        };
        let claims = snapshot
            .as_ref()
            .zip(audit.as_ref())
            .is_some_and(|(snapshot, audit)| {
                crate::harness::run::report::may_claim(
                    snapshot,
                    &audit.agents,
                    caller.as_ref(),
                    record,
                )
            });
        if !claims {
            tracing::debug!(
                run_id = %record.run_id,
                "result printed outside its launcher's turn; left unjoined for the digest",
            );
            continue;
        }
        let result = crate::harness::run::report::join_and_settle_digest(
            store,
            session_name,
            &record.run_id,
            Some(record.follow_ups + 1),
            "joined inline",
        );
        if let Err(err) = result {
            tracing::warn!(
                run_id = %record.run_id,
                error = %err,
                "could not mark the joined run",
            );
        }
    }
}
