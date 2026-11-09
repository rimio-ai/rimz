//! Durable unread episodes for sidebar rows.
//!
//! `unread.json` is the workspace-wide set of open pending-look episodes. The
//! shared snapshot enrichment folds this set with read receipts to stamp
//! `SidebarRow::unread`; the room host reconciles it against current
//! rows and persists opens/prunes.
//!
//! Every writer (host reconcile, manual unread, CLI mark read) changes the
//! set inside one transaction under `unread.lock`, from before its load until
//! after its replace, so no writer publishes a map loaded before another's
//! change. No writer holds `read-marks.lock` at the same time.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentStatus;
use crate::disk::{atomic, paths::RuntimePaths};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::read_marks::{ReadMarks, lane_lock, prune_read_marks};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};

const UNREAD_EPISODES_VERSION: &str = "rimz.unread.v1";

pub(crate) fn reconcile_snapshot(
    runtime: &RuntimePaths,
    snapshot: &mut SidebarSnapshot,
) -> ReconcileOutcome {
    let marks = ReadMarks::load(runtime);
    if snapshot.panes_produced_at_ms.is_some() {
        prune_read_marks(
            runtime,
            &snapshot.rows().map(|row| row.id.clone()).collect(),
        );
    }
    let reconciled = update_episodes(runtime, |episodes| {
        let silent_opens = episodes.was_absent_on_load();
        let unread = episodes.reconcile(snapshot, &marks, silent_opens);
        let write = silent_opens || unread.changed;
        (unread, write)
    });
    match reconciled {
        Ok(unread) => unread,
        Err(err) => {
            // Opens wait for the next final fold rather than notifying ones
            // this pass could not record.
            debug!(error = %err, "unread episodes reconcile deferred");
            derive(snapshot, &UnreadEpisodes::load(runtime), &marks);
            ReconcileOutcome::default()
        }
    }
}

/// The episode transaction: holds `unread.lock` across a fresh load, the
/// caller's change, and the replace the change asks for with its `bool`.
pub(super) fn update_episodes<T>(
    runtime: &RuntimePaths,
    change: impl FnOnce(&mut UnreadEpisodes) -> (T, bool),
) -> atomic::Result<T> {
    let _lock = lane_lock(runtime.unread_lock_path())?;
    let mut episodes = UnreadEpisodes::load(runtime);
    let (out, write) = change(&mut episodes);
    if write {
        episodes.persist(runtime)?;
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnreadEpisodes {
    episodes: BTreeMap<String, i64>,
    absent_on_load: bool,
}

impl UnreadEpisodes {
    pub fn load(runtime: &RuntimePaths) -> Self {
        let path = runtime.unread_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    episodes: BTreeMap::new(),
                    absent_on_load: true,
                };
            }
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar unread episodes unreadable");
                return Self::default();
            }
        };
        let file: UnreadEpisodesFile = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar unread episodes invalid");
                return Self::default();
            }
        };
        if file.v != UNREAD_EPISODES_VERSION {
            debug!(
                path = %path.display(),
                version = file.v,
                "sidebar unread episodes version ignored",
            );
            return Self::default();
        }
        Self {
            episodes: file.episodes,
            absent_on_load: false,
        }
    }

    /// Writes an empty `unread.json` through the episode transaction, so a
    /// test's later opens are not cold-start silent.
    #[cfg(test)]
    pub(crate) fn materialize(runtime: &RuntimePaths) -> atomic::Result<()> {
        update_episodes(runtime, |_| ((), true))
    }

    pub(crate) fn was_absent_on_load(&self) -> bool {
        self.absent_on_load
    }

    fn persist(&mut self, runtime: &RuntimePaths) -> atomic::Result<()> {
        let path = runtime.unread_path();
        let file = UnreadEpisodesFile {
            v: UNREAD_EPISODES_VERSION.to_owned(),
            episodes: self.episodes.clone(),
        };
        atomic::write_temp_then_rename_cache(&path, &file)?;
        self.absent_on_load = false;
        Ok(())
    }

    fn open_for_row(&mut self, row: &SidebarRow, episode_ms: i64) -> OpenedUnread {
        self.episodes.insert(row.id.clone(), episode_ms);
        opened_unread(row, episode_ms, false)
    }

    pub fn remove_reached_for_row(&mut self, row: &SidebarRow, cleared_at_ms: i64) -> bool {
        let Some(episode_ms) = self.episodes.get(&row.id).copied() else {
            return false;
        };
        if cleared_at_ms < episode_ms {
            return false;
        }
        self.episodes.remove(&row.id);
        true
    }

    pub(super) fn unread_row_ids(&self, marks: &ReadMarks) -> BTreeSet<String> {
        self.episodes
            .iter()
            .filter_map(|(row_id, episode_ms)| {
                (!receipt_reaches_ms(marks, row_id, *episode_ms)).then_some(row_id.clone())
            })
            .collect()
    }

    pub(crate) fn reconcile(
        &mut self,
        snapshot: &mut SidebarSnapshot,
        marks: &ReadMarks,
        silent_opens: bool,
    ) -> ReconcileOutcome {
        if snapshot.panes_produced_at_ms.is_none() {
            derive(snapshot, self, marks);
            return ReconcileOutcome::default();
        }

        let mut rows = BTreeMap::new();
        for row in snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
        {
            rows.insert(row.id.clone(), row);
        }

        let mut cleared = Vec::new();
        let mut remove = Vec::new();
        let mut changed = false;
        for (row_id, episode_ms) in &self.episodes {
            if receipt_reaches_ms(marks, row_id, *episode_ms) {
                remove.push(row_id.clone());
                continue;
            }
            match rows.get(row_id) {
                Some(_) => {}
                None => {
                    remove.push(row_id.clone());
                    cleared.push(ClearedUnread {
                        row_id: row_id.clone(),
                        label: None,
                        agent_kind: None,
                        agent_id: None,
                        worktree: None,
                        pane_id: None,
                        cause: UnreadClearCause::RowGone,
                        cleared_at_ms: None,
                    });
                }
            }
        }
        for row_id in remove {
            self.episodes.remove(&row_id);
            changed = true;
        }

        let mut opened = Vec::new();
        for row in rows.values() {
            let Some(status) = row.status() else {
                continue;
            };
            let overlay = row.as_agent().and_then(|agent| agent.overlay_raised_at);
            if !status.needs_a_look()
                || self.episodes.get(&row.id).is_some_and(|episode_ms| {
                    overlay.is_none_or(|at| at.as_millisecond() <= *episode_ms)
                })
            {
                continue;
            }
            let episode_at = overlay
                .or_else(|| {
                    (status == AgentStatus::Waiting)
                        .then(|| {
                            snapshot
                                .agents
                                .iter()
                                .find(|agent| agent.kind == row.name && agent.agent_id == row.id)
                                .and_then(crate::agents::AgentState::permission_review_until)
                        })
                        .flatten()
                })
                .unwrap_or(row.last_activity);
            if receipt_reaches(marks, &row.id, episode_at) {
                continue;
            }
            let episode_ms = episode_at.as_millisecond();
            self.episodes.insert(row.id.clone(), episode_ms);
            changed = true;
            let mut unread = opened_unread(row, episode_ms, silent_opens);
            unread.focused = unread
                .pane_id
                .as_ref()
                .is_some_and(|pane| snapshot.viewed_panes.contains(pane));
            opened.push(unread);
        }

        derive(snapshot, self, marks);
        ReconcileOutcome {
            opened,
            cleared,
            changed,
        }
    }
}

/// Open an unread episode for each row and persist `unread.json` — the one
/// durable manual-unread write path, shared by `rimz agents mark TARGET unread`
/// and the renderer's `m` key on a read row. Each episode opens at
/// `max(last_activity, now_ms, receipt + 1)`, the receipt read inside the
/// episode transaction, so no receipt that exists when the mark is made
/// reaches it and the host's reconcile cannot prune it. A held `unread.lock`
/// past the lane budget fails the mark. Callers trace and wake; this owns only
/// the persistence.
pub fn mark_rows_unread(
    runtime: &RuntimePaths,
    rows: &[SidebarRow],
    now_ms: i64,
) -> atomic::Result<Vec<OpenedUnread>> {
    update_episodes(runtime, |episodes| {
        let opened = open_manual(episodes, &ReadMarks::load(runtime), rows, now_ms);
        let write = !opened.is_empty();
        (opened, write)
    })
}

fn open_manual(
    episodes: &mut UnreadEpisodes,
    marks: &ReadMarks,
    rows: &[SidebarRow],
    now_ms: i64,
) -> Vec<OpenedUnread> {
    // Unread is an agent-row concept: an episode opens on a `needs_a_look`
    // status, so a process row (no status) can never be unread. Skip non-agent
    // rows so the renderer's `m` key stays panic-free at `opened_unread`.
    rows.iter()
        .filter(|row| row.status().is_some())
        .map(|row| {
            let after_receipt = marks
                .cleared_at_ms(&row.id)
                .map_or(i64::MIN, |receipt| receipt.saturating_add(1));
            let episode_ms = row
                .last_activity
                .as_millisecond()
                .max(now_ms)
                .max(after_receipt);
            episodes.open_for_row(row, episode_ms)
        })
        .collect()
}

pub(super) fn derive(snapshot: &mut SidebarSnapshot, episodes: &UnreadEpisodes, marks: &ReadMarks) {
    let unread = |row: &SidebarRow| {
        episodes
            .episodes
            .get(&row.id)
            .is_some_and(|episode_ms| !receipt_reaches_ms(marks, &row.id, *episode_ms))
    };
    for index in 0..snapshot.worktree_groups.len() {
        let group = &snapshot.worktree_groups[index];
        if !group.rows.iter().any(|row| row.unread != unread(row)) {
            continue;
        }
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[index];
        for row in std::sync::Arc::make_mut(&mut group.rows) {
            row.unread = unread(row);
        }
    }
}

pub(crate) fn receipt_reaches(marks: &ReadMarks, row_id: &str, stamp: jiff::Timestamp) -> bool {
    receipt_reaches_ms(marks, row_id, stamp.as_millisecond())
}

fn receipt_reaches_ms(marks: &ReadMarks, row_id: &str, stamp_ms: i64) -> bool {
    marks
        .cleared_at_ms(row_id)
        .is_some_and(|cleared_at_ms| cleared_at_ms >= stamp_ms)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcileOutcome {
    pub(crate) opened: Vec<OpenedUnread>,
    pub(crate) cleared: Vec<ClearedUnread>,
    pub(crate) changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedUnread {
    pub row_id: String,
    pub label: String,
    pub handle: String,
    pub agent_kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub worktree: Option<String>,
    pub task: Option<String>,
    pub pane_id: Option<PaneId>,
    pub root: Option<String>,
    pub status: AgentStatus,
    pub episode_ms: i64,
    pub focused: bool,
    pub silent: bool,
}

impl OpenedUnread {
    pub(crate) fn trace_event(&self) -> crate::diag::notify::NotifyTraceEvent {
        crate::diag::notify::NotifyTraceEvent::UnreadMarked {
            row_id: self.row_id.clone(),
            label: Some(self.label.clone()),
            agent_kind: Some(self.agent_kind.clone()),
            agent_id: Some(self.agent_id.clone()),
            worktree: self.worktree.clone(),
            pane_id: self.pane_id.clone(),
            status: self.status.as_str().to_owned(),
            episode_ms: self.episode_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClearedUnread {
    pub(crate) row_id: String,
    pub(crate) label: Option<String>,
    pub(crate) agent_kind: Option<AgentKind>,
    pub(crate) agent_id: Option<AgentSessionId>,
    pub(crate) worktree: Option<String>,
    pub(crate) pane_id: Option<PaneId>,
    pub(crate) cause: UnreadClearCause,
    pub(crate) cleared_at_ms: Option<i64>,
}

impl ClearedUnread {
    pub(crate) fn trace_event(&self) -> crate::diag::notify::NotifyTraceEvent {
        crate::diag::notify::NotifyTraceEvent::UnreadCleared {
            row_id: self.row_id.clone(),
            label: self.label.clone(),
            agent_kind: self.agent_kind.clone(),
            agent_id: self.agent_id.clone(),
            worktree: self.worktree.clone(),
            pane_id: self.pane_id.clone(),
            cause: self.cause.as_str().to_owned(),
            cleared_at_ms: self.cleared_at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadClearCause {
    Focus,
    MarkRead,
    TabView,
    RowGone,
}

impl UnreadClearCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::MarkRead => "mark_read",
            Self::TabView => "tab_view",
            Self::RowGone => "row_gone",
        }
    }
}

pub(super) fn opened_unread(row: &SidebarRow, episode_ms: i64, silent: bool) -> OpenedUnread {
    let status = row
        .status()
        .expect("opened unread rows are agent rows with a status");
    OpenedUnread {
        row_id: row.id.clone(),
        label: row_label(row),
        handle: row.display_name().to_owned(),
        agent_kind: AgentKind::new_unchecked(row.name.clone()),
        agent_id: AgentSessionId::from(row.id.clone()),
        worktree: row_worktree(row),
        task: row.task().map(str::to_owned),
        pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
        root: row.lane_path().map(ToOwned::to_owned),
        status,
        episode_ms,
        focused: false,
        silent,
    }
}

pub(crate) fn cleared_unread(
    row: &SidebarRow,
    cause: UnreadClearCause,
    cleared_at_ms: Option<i64>,
) -> ClearedUnread {
    ClearedUnread {
        row_id: row.id.clone(),
        label: Some(row_label(row)),
        agent_kind: Some(AgentKind::new_unchecked(row.name.clone())),
        agent_id: Some(AgentSessionId::from(row.id.clone())),
        worktree: row_worktree(row),
        pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
        cause,
        cleared_at_ms,
    }
}

pub fn row_label(row: &SidebarRow) -> String {
    row.task()
        .or_else(|| row.as_agent().and_then(|agent| agent.prompt.as_deref()))
        .filter(|value| !value.trim().is_empty())
        .map(trim_label)
        .unwrap_or_else(|| format!("{} {}", row.display_name(), short_id(&row.id)))
}

fn row_worktree(row: &SidebarRow) -> Option<String> {
    row.worktree_branch
        .clone()
        .or_else(|| row.lane_path().map(ToOwned::to_owned))
}

fn trim_label(value: &str) -> String {
    const MAX: usize = 48;
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_owned();
    }
    let mut out = trimmed
        .chars()
        .take(MAX.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn short_id(raw: &str) -> &str {
    raw.get(..8).unwrap_or(raw)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct UnreadEpisodesFile {
    v: String,
    #[serde(default)]
    episodes: BTreeMap<String, i64>,
}

#[cfg(test)]
mod tests;
