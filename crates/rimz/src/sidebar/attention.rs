//! Launch-keyed attention crossings and pane-keyed slots, retained for the room's lifetime.
//! Slots follow a pane's first sighting, independent of its tenant's order stamp.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::{AgentState, AgentStatus};
use crate::disk::{atomic, paths::RuntimePaths, retention::DEFAULT_RETENTION};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};

const ATTENTION_VERSION: &str = "rimz.attention.v1";

pub(super) fn reconcile_snapshot(runtime: &RuntimePaths, snapshot: &mut SidebarSnapshot) {
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
    panes: BTreeMap<String, PaneStamp>,
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
            .map(|file| Self {
                cards: file.cards,
                panes: file.panes,
            })
            .unwrap_or_default()
    }

    fn persist(&self, runtime: &RuntimePaths) -> atomic::Result<()> {
        atomic::write_temp_then_rename_cache(
            &runtime.attention_path(),
            &AttentionFile {
                v: ATTENTION_VERSION.to_owned(),
                cards: self.cards.clone(),
                panes: self.panes.clone(),
            },
        )
    }

    fn reconcile(&mut self, snapshot: &SidebarSnapshot) -> bool {
        if snapshot.panes_produced_at_ms.is_none() {
            return false;
        }
        let launches = launch_keys(&snapshot.agents);
        let now_ms = snapshot.now.as_millisecond();
        let mut present_cards = BTreeSet::new();
        let mut present_panes = BTreeSet::new();
        let mut changed = false;
        for row in snapshot.rows() {
            let pane_id = row.pane.as_ref().map(|pane| pane.pane_id.as_str());
            let stored = pane_id.and_then(|key| self.panes.get(key));
            present_panes.extend(pane_id);
            let card = if row.is_agent() {
                let key = card_key(row, &launches);
                if pane_id == Some(key) && holds_process(stored, row) {
                    changed |= self.cards.remove(key).is_some();
                    continue;
                }
                present_cards.insert(key);
                let next = next_stamp(self.cards.get(key), stored, row, now_ms);
                let at_ms = next.at_ms;
                if self.cards.get(key) != Some(&next) {
                    self.cards.insert(key.to_owned(), next);
                    changed = true;
                }
                Some(at_ms)
            } else {
                None
            };
            let Some(pane) = &row.pane else {
                continue;
            };
            let key = pane.pane_id.as_str();
            let next = if let Some(at_ms) = card {
                PaneStamp {
                    at_ms,
                    seen_ms: pane.first_seen_at_ms,
                    tenant: PaneTenant::Agent {
                        active_ms: (row.inactive || row.archived)
                            .then_some(row.last_activity.as_millisecond()),
                    },
                }
            } else {
                let (at_ms, active_ms) = process_stamp(stored, row, now_ms);
                PaneStamp {
                    at_ms,
                    seen_ms: pane
                        .first_seen_at_ms
                        .or(stored.and_then(|stamp| stamp.seen_ms)),
                    tenant: PaneTenant::Process { active_ms },
                }
            };
            if self.panes.get(key) != Some(&next) {
                self.panes.insert(key.to_owned(), next);
                changed = true;
            }
        }
        let before = (self.cards.len(), self.panes.len());
        let oldest_ms = now_ms.saturating_sub(DEFAULT_RETENTION.as_millis() as i64);
        self.cards
            .retain(|key, stamp| present_cards.contains(key.as_str()) || stamp.at_ms >= oldest_ms);
        self.panes
            .retain(|key, stamp| present_panes.contains(key.as_str()) || stamp.at_ms >= oldest_ms);
        changed || before != (self.cards.len(), self.panes.len())
    }

    /// Derive the next reconciliation's stamps and process sinks without writing:
    /// interim publications and published-cache readers see pending crossings and exits.
    pub(super) fn derive(&self, snapshot: &mut SidebarSnapshot) {
        let launches = launch_keys(&snapshot.agents);
        let now_ms = snapshot.now.as_millisecond();
        for group_index in 0..snapshot.worktree_groups.len() {
            for row_index in 0..snapshot.worktree_groups[group_index].rows.len() {
                let row = &snapshot.worktree_groups[group_index].rows[row_index];
                let (at_ms, facts) = if row.is_agent() {
                    let stored = self.cards.get(card_key(row, &launches));
                    let pane = row
                        .pane
                        .as_ref()
                        .and_then(|pane| self.panes.get(pane.pane_id.as_str()));
                    (
                        next_stamp(stored, pane, row, now_ms).at_ms,
                        (row.inactive, row.archived, row.attention_score),
                    )
                } else {
                    let Some(pane) = &row.pane else {
                        continue;
                    };
                    let (at_ms, active_ms) =
                        process_stamp(self.panes.get(pane.pane_id.as_str()), row, now_ms);
                    (at_ms, snapshot.process_attention_facts(active_ms))
                };
                if row.attention_at_ms == Some(at_ms)
                    && (row.inactive, row.archived, row.attention_score) == facts
                {
                    continue;
                }
                let group =
                    &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[group_index];
                let row = &mut std::sync::Arc::make_mut(&mut group.rows)[row_index];
                row.attention_at_ms = Some(at_ms);
                (row.inactive, row.archived, row.attention_score) = facts;
            }
        }
    }
}

/// A calm first sighting inherits a shell slot, else births; a zone crossing takes the fold clock.
fn next_stamp(
    stored: Option<&CardStamp>,
    pane: Option<&PaneStamp>,
    row: &SidebarRow,
    now_ms: i64,
) -> CardStamp {
    let zone = if row
        .attention_status()
        .is_some_and(AgentStatus::is_actionable)
    {
        Zone::Top
    } else {
        Zone::Calm
    };
    let at_ms = match stored {
        Some(stamp) if stamp.zone == zone => return stamp.clone(),
        Some(_) => now_ms,
        None => pane
            .filter(|_| zone == Zone::Calm && holds_process(pane, row))
            .map(|stamp| stamp.at_ms)
            .or_else(|| {
                row.as_agent()
                    .and_then(|agent| agent.registered_at)
                    .map(|birth| birth.as_millisecond())
            })
            .unwrap_or(now_ms),
    };
    CardStamp { zone, at_ms }
}

fn process_stamp(stored: Option<&PaneStamp>, row: &SidebarRow, now_ms: i64) -> (i64, i64) {
    let birth_ms = row
        .pane
        .as_ref()
        .filter(|pane| pane.pane_process_start.is_some() || pane.first_seen_at_ms.is_some())
        .map(|_| row.last_activity.as_millisecond());
    match matching_pane_stamp(stored, row) {
        Some(PaneStamp {
            at_ms,
            tenant: PaneTenant::Process { active_ms },
            ..
        }) => (*at_ms, *active_ms),
        Some(PaneStamp {
            at_ms,
            tenant: PaneTenant::Agent { active_ms },
            ..
        }) => (*at_ms, active_ms.unwrap_or(now_ms)),
        None => {
            let birth_ms = birth_ms.unwrap_or(now_ms);
            (birth_ms, birth_ms)
        }
    }
}

/// Whether the pane's slot still belongs to its process. An unbound agent row keyed by its pane
/// records nothing over such a slot, so the card that binds next inherits the shell's place.
fn holds_process(stored: Option<&PaneStamp>, row: &SidebarRow) -> bool {
    matching_pane_stamp(stored, row)
        .is_some_and(|stamp| matches!(stamp.tenant, PaneTenant::Process { .. }))
}

fn matching_pane_stamp<'a>(
    stored: Option<&'a PaneStamp>,
    row: &SidebarRow,
) -> Option<&'a PaneStamp> {
    let seen_ms = row.pane.as_ref().and_then(|pane| pane.first_seen_at_ms);
    stored.filter(|stamp| match (stamp.seen_ms, seen_ms) {
        (Some(stored), Some(current)) => stored == current,
        _ => true,
    })
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
    #[serde(default)]
    panes: BTreeMap<String, PaneStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CardStamp {
    zone: Zone,
    at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PaneStamp {
    at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen_ms: Option<u64>,
    #[serde(flatten)]
    tenant: PaneTenant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tenant", rename_all = "snake_case")]
enum PaneTenant {
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_ms: Option<i64>,
    },
    Process {
        active_ms: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Zone {
    Top,
    Calm,
}

#[cfg(test)]
mod tests;
