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
            .find(|ask| ask.matches(kind, agent_id))
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

#[cfg(test)]
mod tests;
