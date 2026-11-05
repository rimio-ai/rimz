//! Launch-keyed attention-zone crossings, retained for the room's lifetime.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::{AgentState, AgentStatus};
use crate::disk::{atomic, paths::RuntimePaths, retention::DEFAULT_RETENTION};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};

const ATTENTION_VERSION: &str = "rimz.attention.v1";

pub(crate) fn reconcile_snapshot(runtime: &RuntimePaths, snapshot: &mut SidebarSnapshot) {
    let mut stamps = AttentionStamps::load(runtime);
    if stamps.reconcile(snapshot)
        && let Err(err) = stamps.persist(runtime)
    {
        debug!(error = %err, "sidebar attention stamps persist failed");
    }
    stamps.derive(snapshot);
    snapshot.sort_groups_for_presentation();
}

#[derive(Default)]
pub(super) struct AttentionStamps {
    cards: BTreeMap<String, CardStamp>,
}

impl AttentionStamps {
    pub(super) fn load(runtime: &RuntimePaths) -> Self {
        let path = runtime.attention_path();
        match fs::read(&path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    debug!(
                        path = %path.display(),
                        error = %err,
                        "sidebar attention stamps unreadable",
                    );
                }
                Self::default()
            }
        }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        serde_json::from_slice::<AttentionFile>(bytes)
            .ok()
            .filter(|file| file.v == ATTENTION_VERSION)
            .map(|file| Self { cards: file.cards })
            .unwrap_or_default()
    }

    fn persist(&self, runtime: &RuntimePaths) -> atomic::Result<()> {
        atomic::write_temp_then_rename_cache(
            &runtime.attention_path(),
            &AttentionFile {
                v: ATTENTION_VERSION.to_owned(),
                cards: self.cards.clone(),
            },
        )
    }

    fn reconcile(&mut self, snapshot: &SidebarSnapshot) -> bool {
        if snapshot.panes_produced_at_ms.is_none() {
            return false;
        }
        let launches = launch_keys(&snapshot.agents);
        let now_ms = snapshot.now.as_millisecond();
        let mut present = BTreeSet::new();
        let mut changed = false;
        for row in snapshot.rows().filter(|row| row.is_agent()) {
            let key = card_key(row, &launches);
            present.insert(key);
            if let Some(next) = next_stamp(self.cards.get(key), row, now_ms) {
                self.cards.insert(key.to_owned(), next);
                changed = true;
            }
        }
        let before = self.cards.len();
        let oldest_ms = now_ms.saturating_sub(DEFAULT_RETENTION.as_millis() as i64);
        self.cards
            .retain(|key, stamp| present.contains(key.as_str()) || stamp.at_ms >= oldest_ms);
        changed || before != self.cards.len()
    }

    /// Ranks every card as the next reconcile will stamp it: a fold that does not reconcile
    /// (an interim publication, a published-cache reader) derives a pending crossing at the
    /// snapshot clock without persisting it.
    pub(super) fn derive(&self, snapshot: &mut SidebarSnapshot) {
        let launches = launch_keys(&snapshot.agents);
        let now_ms = snapshot.now.as_millisecond();
        let stamp = |row: &SidebarRow| {
            let stored = self.cards.get(card_key(row, &launches));
            next_stamp(stored, row, now_ms)
                .or_else(|| stored.cloned())
                .map(|stamp| stamp.at_ms)
        };
        for index in 0..snapshot.worktree_groups.len() {
            let group = &snapshot.worktree_groups[index];
            if !group
                .rows
                .iter()
                .any(|row| row.is_agent() && row.attention_at_ms != stamp(row))
            {
                continue;
            }
            let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[index];
            for row in std::sync::Arc::make_mut(&mut group.rows) {
                let at_ms = stamp(row);
                if row.is_agent() {
                    row.attention_at_ms = at_ms;
                }
            }
        }
    }
}

/// The stamp a framed fold records for `row`, or `None` while its displayed zone is unchanged:
/// birth on first sighting, the fold's clock on a top/calm crossing.
fn next_stamp(stored: Option<&CardStamp>, row: &SidebarRow, now_ms: i64) -> Option<CardStamp> {
    let zone = if row
        .attention_status()
        .is_some_and(AgentStatus::is_actionable)
    {
        Zone::Top
    } else {
        Zone::Calm
    };
    let at_ms = match stored {
        Some(stamp) if stamp.zone == zone => return None,
        Some(_) => now_ms,
        None => row
            .as_agent()
            .and_then(|agent| agent.registered_at)
            .map_or(now_ms, |birth| birth.as_millisecond()),
    };
    Some(CardStamp { zone, at_ms })
}

fn launch_keys(agents: &[AgentState]) -> BTreeMap<(&str, &str), &str> {
    agents
        .iter()
        .filter_map(|agent| {
            Some((
                (agent.kind.as_str(), agent.agent_id.as_str()),
                agent.launch_id.as_deref()?,
            ))
        })
        .collect()
}

fn card_key<'a>(row: &'a SidebarRow, launches: &BTreeMap<(&str, &str), &'a str>) -> &'a str {
    launches
        .get(&(row.name.as_str(), row.id.as_str()))
        .copied()
        .unwrap_or(&row.id)
}

#[derive(Serialize, Deserialize)]
struct AttentionFile {
    v: String,
    cards: BTreeMap<String, CardStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CardStamp {
    zone: Zone,
    at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Zone {
    Top,
    Calm,
}

#[cfg(test)]
mod tests;
