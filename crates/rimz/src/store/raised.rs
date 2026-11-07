use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::AgentState;
use crate::agents::state::{CardAlert, RaisedAsk, RaisedAskState};
use crate::disk::{atomic, lock, paths::StatePaths};
use crate::ids::{AgentKind, AgentSessionId, AskId};

#[derive(Debug, thiserror::Error)]
pub enum RaisedErr {
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("the user already replied to this turn")]
    AlreadyAnswered,
    #[error("target has no recorded turn or registration")]
    NoBasis,
    #[error("an outside raise without TEXT requires an agent resting cleanly")]
    NotResting,
    #[error("no recorded reply; provide TEXT")]
    NoReply,
    #[error(transparent)]
    Transcript(#[from] crate::transcript::TranscriptLogErr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseCause {
    UserTurn,
    TurnFailed,
    TurnUnseen,
}

impl CloseCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserTurn => "user_turn",
            Self::TurnFailed => "turn_failed",
            Self::TurnUnseen => "turn_unseen",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry<T> {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(flatten)]
    pub value: T,
}

impl<T> Entry<T> {
    fn matches(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> bool {
        self.kind == *kind && self.agent_id == *agent_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanTurn {
    pub at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaisedRecord {
    version: u32,
    pub asks: Vec<Entry<RaisedAsk>>,
    pub alerts: Vec<Entry<CardAlert>>,
    pub human_turns: Vec<Entry<HumanTurn>>,
}

impl Default for RaisedRecord {
    fn default() -> Self {
        Self {
            version: 1,
            asks: Vec::new(),
            alerts: Vec::new(),
            human_turns: Vec::new(),
        }
    }
}

impl RaisedRecord {
    pub fn is_open(&self, ask: &Entry<RaisedAsk>) -> bool {
        ask.value.state == RaisedAskState::Open && !self.answered(ask)
    }

    fn answered(&self, ask: &Entry<RaisedAsk>) -> bool {
        self.human_turns.iter().any(|turn| {
            turn.matches(&ask.kind, &ask.agent_id) && turn.value.at > ask.value.basis_at
        })
    }

    fn prune_closed(&mut self, kind: &AgentKind, agent_id: &AgentSessionId) {
        let turns = &self.human_turns;
        self.asks.retain(|ask| {
            !ask.matches(kind, agent_id)
                || !turns
                    .iter()
                    .any(|turn| turn.matches(kind, agent_id) && turn.value.at > ask.value.basis_at)
        });
    }
}

pub fn read(paths: &StatePaths) -> RaisedRecord {
    std::fs::read(&paths.raised)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RaisedRecord>(&bytes).ok())
        .filter(|record| record.version == 1)
        .unwrap_or_default()
}

/// Fill each row's raised ask and alert from the record, copying the shared
/// rows only when some overlay changes, so a cached snapshot stays shared.
pub(crate) fn attach(agents: &mut Arc<Vec<AgentState>>, paths: &StatePaths) {
    let record = read(paths);
    let overlays = |agent: &AgentState| {
        let live = agent.ended_at.is_none();
        let ask = record
            .asks
            .iter()
            .find(|ask| live && ask.matches(&agent.kind, &agent.agent_id) && record.is_open(ask))
            .map(|ask| ask.value.clone());
        let alert = record
            .alerts
            .iter()
            .find(|alert| live && alert.matches(&agent.kind, &agent.agent_id))
            .map(|alert| alert.value.clone());
        (ask, alert)
    };
    if agents.iter().all(|agent| {
        let (ask, alert) = overlays(agent);
        agent.raised_ask == ask && agent.alert == alert
    }) {
        return;
    }
    for agent in Arc::make_mut(agents) {
        (agent.raised_ask, agent.alert) = overlays(agent);
    }
}

pub fn prepare_raise(
    paths: &StatePaths,
    agent: &AgentState,
    is_self: bool,
    text: Option<String>,
    options: Vec<String>,
    raised_by: Option<String>,
    at: Timestamp,
) -> Result<Entry<RaisedAsk>, RaisedErr> {
    let basis_at = if is_self {
        at
    } else {
        agent
            .last_prompt_at
            .max(agent.turn_started_at)
            .or(agent.registered_at)
            .ok_or(RaisedErr::NoBasis)?
    };
    let text = if text.is_none() && !is_self {
        let (status, phase) = agent.rowless_status(&Default::default(), at);
        if !matches!(
            status,
            crate::agents::AgentStatus::Idle | crate::agents::AgentStatus::Success
        ) || phase == crate::agents::TurnPhase::Parked
        {
            return Err(RaisedErr::NotResting);
        }
        let since = agent
            .last_prompt_at
            .or(agent.registered_at)
            .ok_or(RaisedErr::NoBasis)?;
        Some(
            crate::transcript::latest_assistant(paths, &agent.kind, &agent.agent_id, since)?
                .ok_or(RaisedErr::NoReply)?
                .text,
        )
    } else {
        text
    };
    Ok(Entry {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        value: RaisedAsk {
            ask_id: AskId::new(),
            state: if is_self {
                RaisedAskState::Armed
            } else {
                RaisedAskState::Open
            },
            raised_at: at,
            basis_at,
            opened_at: (!is_self).then_some(at),
            raised_by,
            text,
            options,
        },
    })
}

#[must_use = "durability barrier; check the result"]
pub fn raise(paths: &StatePaths, ask: Entry<RaisedAsk>) -> Result<(), RaisedErr> {
    update(paths, |record| {
        if record.answered(&ask) {
            return Err(RaisedErr::AlreadyAnswered);
        }
        record
            .asks
            .retain(|prior| !prior.matches(&ask.kind, &ask.agent_id));
        record.asks.push(ask);
        Ok(())
    })
}

#[must_use = "durability barrier; check the result"]
pub fn withdraw(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    ask_id: Option<&AskId>,
) -> Result<Option<RaisedAsk>, RaisedErr> {
    update(paths, |record| {
        record.prune_closed(kind, agent_id);
        let index = record.asks.iter().position(|ask| {
            ask.matches(kind, agent_id) && ask_id.is_none_or(|id| *id == ask.value.ask_id)
        });
        Ok(index.map(|index| record.asks.remove(index).value))
    })
}

#[must_use = "durability barrier; check the result"]
pub fn set_alert(paths: &StatePaths, alert: Entry<CardAlert>) -> Result<(), RaisedErr> {
    update(paths, |record| {
        record.prune_closed(&alert.kind, &alert.agent_id);
        record
            .alerts
            .retain(|prior| !prior.matches(&alert.kind, &alert.agent_id));
        record.alerts.push(alert);
        Ok(())
    })
}

#[must_use = "durability barrier; check the result"]
pub fn clear_alert(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<(), RaisedErr> {
    update(paths, |record| {
        record.prune_closed(kind, agent_id);
        record.alerts.retain(|alert| !alert.matches(kind, agent_id));
        Ok(())
    })
}

#[must_use = "durability barrier; check the result"]
pub(crate) fn retire(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<Option<RaisedAsk>, RaisedErr> {
    let record = read(paths);
    if !record
        .asks
        .iter()
        .any(|entry| entry.matches(kind, agent_id))
        && !record
            .alerts
            .iter()
            .any(|entry| entry.matches(kind, agent_id))
        && !record
            .human_turns
            .iter()
            .any(|entry| entry.matches(kind, agent_id))
    {
        return Ok(None);
    }
    update(paths, |record| {
        let removed = record
            .asks
            .iter()
            .find(|ask| ask.matches(kind, agent_id) && !record.answered(ask))
            .map(|ask| ask.value.clone());
        record.asks.retain(|ask| !ask.matches(kind, agent_id));
        record.alerts.retain(|alert| !alert.matches(kind, agent_id));
        record
            .human_turns
            .retain(|turn| !turn.matches(kind, agent_id));
        Ok(removed)
    })
}

fn update<T>(
    paths: &StatePaths,
    edit: impl FnOnce(&mut RaisedRecord) -> Result<T, RaisedErr>,
) -> Result<T, RaisedErr> {
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = read(paths);
    let before = record.clone();
    let result = edit(&mut record)?;
    if record != before {
        atomic::write_temp_then_rename(&paths.raised, &record)?;
    }
    Ok(result)
}

#[must_use = "durability barrier; check the result"]
pub fn observe(
    paths: &StatePaths,
    kind: &AgentKind,
    observation: &crate::agents::AgentLifecycleObservation,
    transition: Option<crate::agents::lifecycle::Transition>,
    at: Timestamp,
    human: bool,
    reply: Option<&str>,
) -> Result<Option<(RaisedAsk, CloseCause)>, RaisedErr> {
    use crate::agents::LifecycleSignal;
    use crate::agents::lifecycle::TransitionKind;

    let Some(agent_id) = observation.agent_id.as_ref() else {
        return Ok(None);
    };
    let Some(transition) =
        transition.filter(|step| !matches!(step.kind, TransitionKind::Ignored { .. }))
    else {
        return Ok(None);
    };
    if !matches!(
        observation.signal,
        LifecycleSignal::TurnStarted { .. }
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::TurnInterrupted { .. }
    ) {
        return Ok(None);
    }
    // Codex's async replies carry TurnStarted but no new task prompt.
    let question_reply = observation.prompt.is_none()
        && observation
            .ask_queue
            .as_ref()
            .is_some_and(|edit| !edit.clear_all);
    let stamps_human_turn = human
        && !question_reply
        && matches!(observation.signal, LifecycleSignal::TurnStarted { .. })
        && !observation
            .prompt
            .as_deref()
            .and_then(|prompt| prompt.split_whitespace().next())
            .is_some_and(|first| {
                crate::agents::spec_by_kind(kind.as_str())
                    .and_then(|spec| spec.launch.compact_command)
                    .is_some_and(|compact| first == compact.command)
            });
    if !stamps_human_turn
        && !read(paths)
            .asks
            .iter()
            .any(|ask| ask.matches(kind, agent_id))
    {
        return Ok(None);
    }
    update(paths, |record| {
        let prior = record
            .asks
            .iter()
            .find(|ask| ask.matches(kind, agent_id))
            .cloned();
        let was_open = prior.as_ref().is_some_and(|ask| record.is_open(ask));
        if stamps_human_turn {
            if let Some(turn) = record
                .human_turns
                .iter_mut()
                .find(|turn| turn.matches(kind, agent_id))
            {
                turn.value.at = turn.value.at.max(at);
            } else {
                record.human_turns.push(Entry {
                    kind: kind.clone(),
                    agent_id: agent_id.clone(),
                    value: HumanTurn { at },
                });
            }
        }
        let Some(prior) = prior else { return Ok(None) };
        if prior.value.state == RaisedAskState::Armed && at >= prior.value.raised_at {
            let cause = match observation.signal {
                LifecycleSignal::TurnEnded { errored: true, .. }
                | LifecycleSignal::TurnInterrupted { .. } => Some(CloseCause::TurnFailed),
                LifecycleSignal::TurnStarted { .. }
                    if transition.opened_turn && !question_reply && at > prior.value.raised_at =>
                {
                    Some(CloseCause::TurnUnseen)
                }
                _ => None,
            };
            if let Some(cause) = cause {
                record.asks.retain(|ask| !ask.matches(kind, agent_id));
                return Ok(Some((prior.value, cause)));
            }
            if matches!(
                observation.signal,
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    ..
                }
            ) && !record.answered(&prior)
            {
                let mut opened = prior.value;
                opened.state = RaisedAskState::Open;
                opened.opened_at = Some(at);
                opened.text = opened.text.or_else(|| reply.map(ToOwned::to_owned));
                if opened.text.is_none() {
                    opened.text = match crate::transcript::latest_assistant(
                        paths,
                        kind,
                        agent_id,
                        opened.raised_at,
                    ) {
                        Ok(entry) => entry.map(|entry| entry.text),
                        Err(error) => {
                            tracing::warn!(%error, "raised ask: could not read the final reply");
                            None
                        }
                    };
                }
                record.asks.retain(|ask| !ask.matches(kind, agent_id));
                record.asks.push(Entry {
                    kind: kind.clone(),
                    agent_id: agent_id.clone(),
                    value: opened,
                });
                return Ok(None);
            }
        }
        if was_open && record.answered(&prior) {
            return Ok(Some((prior.value, CloseCause::UserTurn)));
        }
        record.prune_closed(kind, agent_id);
        Ok(None)
    })
}

#[must_use = "durability barrier; check the result"]
pub fn audit(
    store: &super::Store,
    session_name: &str,
    method: &str,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    mut params: serde_json::Value,
) -> Result<(), super::StoreErr> {
    params["kind"] = serde_json::json!(kind);
    params["agent_id"] = serde_json::json!(agent_id);
    store
        .append_event(&super::event::EventEnvelope::new(
            store.paths().workspace_id.clone(),
            session_name,
            "rimz",
            "runtime",
            method,
            params,
        ))
        .map(|_| ())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;
