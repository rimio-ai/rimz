//! Durable unread episodes for sidebar rows.
//!
//! `unread.json` is the workspace-wide set of open pending-look episodes. The
//! shared snapshot enrichment folds this set with read receipts to stamp
//! `SidebarRow::unread`; the elected producer reconciles it against current
//! rows and persists opens/prunes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentStatus;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::{RuntimePaths, atomic};
use crate::sidebar::read_marks::ReadMarks;
use crate::{SidebarRow, SidebarSnapshot};

pub const UNREAD_EPISODES_VERSION: &str = "rimz.unread.v1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnreadEpisodes {
    episodes: BTreeMap<String, i64>,
    absent_on_load: bool,
}

impl UnreadEpisodes {
    pub fn empty() -> Self {
        Self::default()
    }

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
                return Self::empty();
            }
        };
        let file: UnreadEpisodesFile = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar unread episodes invalid");
                return Self::empty();
            }
        };
        if file.v != UNREAD_EPISODES_VERSION {
            debug!(
                path = %path.display(),
                version = file.v,
                "sidebar unread episodes version ignored",
            );
            return Self::empty();
        }
        Self {
            episodes: file.episodes,
            absent_on_load: false,
        }
    }

    pub fn was_absent_on_load(&self) -> bool {
        self.absent_on_load
    }

    pub fn persist(&mut self, runtime: &RuntimePaths) -> atomic::Result<()> {
        let path = runtime.unread_path();
        let file = UnreadEpisodesFile {
            v: UNREAD_EPISODES_VERSION.to_owned(),
            episodes: self.episodes.clone(),
        };
        atomic::write_temp_then_rename_cache(&path, &file)?;
        self.absent_on_load = false;
        Ok(())
    }

    pub fn open_for_row(&mut self, row: &SidebarRow, episode_ms: i64) -> OpenedUnread {
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

    pub(crate) fn unread_row_ids(&self, marks: &ReadMarks) -> BTreeSet<String> {
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
            .flat_map(|group| &group.rows)
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
        let live_open: BTreeSet<_> = self.episodes.keys().cloned().collect();
        for row in rows.values() {
            let Some(status) = row.status() else {
                continue;
            };
            if !status.needs_a_look()
                || live_open.contains(&row.id)
                || receipt_reaches(marks, &row.id, row.last_activity)
            {
                continue;
            }
            let episode_ms = row.last_activity.as_millisecond();
            self.episodes.insert(row.id.clone(), episode_ms);
            changed = true;
            opened.push(opened_unread(row, episode_ms, silent_opens));
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
/// durable mark-unread write path, shared by the `rimz sidebar mark-unread` CLI
/// and the renderer's `M` key. Each episode opens at `last_activity.max(now_ms)`
/// so no read receipt can reach it, which keeps the elder's reconcile from
/// pruning it and makes the write safe from any process, elder or not. Callers
/// trace and wake; this owns only the persistence.
pub fn mark_rows_unread(
    runtime: &RuntimePaths,
    rows: &[SidebarRow],
    now_ms: i64,
) -> atomic::Result<Vec<OpenedUnread>> {
    let mut episodes = UnreadEpisodes::load(runtime);
    // Unread is an agent-row concept: an episode opens on a `needs_a_look`
    // status, so a process row (no status) can never be unread. Skip non-agent
    // rows so every caller — the renderer's `M` key and `rimz sidebar
    // mark-unread @process-pane` alike — stays panic-free at `opened_unread`.
    let opened = rows
        .iter()
        .filter(|row| row.status().is_some())
        .map(|row| {
            let episode_ms = row.last_activity.as_millisecond().max(now_ms);
            episodes.open_for_row(row, episode_ms)
        })
        .collect::<Vec<_>>();
    if !opened.is_empty() {
        episodes.persist(runtime)?;
    }
    Ok(opened)
}

pub(crate) fn derive(snapshot: &mut SidebarSnapshot, episodes: &UnreadEpisodes, marks: &ReadMarks) {
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        row.unread = episodes
            .episodes
            .get(&row.id)
            .is_some_and(|episode_ms| !receipt_reaches_ms(marks, &row.id, *episode_ms));
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
    pub agent_kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub worktree: Option<String>,
    pub pane_id: Option<PaneId>,
    pub status: AgentStatus,
    pub episode_ms: i64,
    pub focused: bool,
    pub silent: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadClearCause {
    Focus,
    MarkRead,
    RowGone,
}

impl UnreadClearCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::MarkRead => "mark_read",
            Self::RowGone => "row_gone",
        }
    }
}

pub(crate) fn opened_unread(row: &SidebarRow, episode_ms: i64, silent: bool) -> OpenedUnread {
    let status = row
        .status()
        .expect("opened unread rows are agent rows with a status");
    OpenedUnread {
        row_id: row.id.clone(),
        label: row_label(row),
        agent_kind: AgentKind::new_unchecked(row.name.clone()),
        agent_id: AgentSessionId::from(row.id.clone()),
        worktree: row_worktree(row),
        pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
        status,
        episode_ms,
        focused: row.pane.as_ref().is_some_and(|pane| pane.is_focused),
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
        .or_else(|| row.worktree_path.clone())
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
mod tests {
    use super::*;
    use crate::pane::PaneRef;
    use crate::{AgentCard, MuxName, RowCard, SidebarStatusCount, SidebarWorktreeGroup};
    use std::path::Path;
    use tempfile::TempDir;

    fn runtime() -> (TempDir, RuntimePaths) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        (dir, runtime)
    }

    fn row(id: &str, status: AgentStatus, last_activity: i64) -> SidebarRow {
        SidebarRow {
            id: id.to_owned(),
            name: "claude".to_owned(),
            pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%1"))),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            unread: false,
            inactive: false,
            last_activity: jiff::Timestamp::from_millisecond(last_activity).expect("timestamp"),
            card: RowCard::Agent(Box::new(AgentCard {
                status: Some(status),
                ..AgentCard::default()
            })),
        }
    }

    fn process_row(id: &str, last_activity: i64) -> SidebarRow {
        SidebarRow {
            card: RowCard::Process(crate::ProcessCard::default()),
            name: "zsh".to_owned(),
            ..row(id, AgentStatus::Idle, last_activity)
        }
    }

    fn snapshot(rows: Vec<SidebarRow>) -> SidebarSnapshot {
        let status_counts = rows
            .iter()
            .filter_map(|row| row.status())
            .map(|status| SidebarStatusCount { status, count: 1 })
            .collect();
        let workspace = crate::ids::WorkspaceId::from_project_root(Path::new("/repo/main"));
        let mut snapshot = SidebarSnapshot::build_with_agents(
            workspace,
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::now(),
        );
        snapshot.panes_produced_at_ms = Some(1);
        snapshot.worktree_groups = vec![SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts,
            rows,
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
        }];
        snapshot
    }

    #[test]
    fn opens_for_every_needs_a_look_status_including_paused() {
        for status in [
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
            AgentStatus::Success,
        ] {
            let mut episodes = UnreadEpisodes::empty();
            let mut snapshot = snapshot(vec![row("a", status, 1_000)]);
            let out = episodes.reconcile(&mut snapshot, &ReadMarks::empty(), false);

            assert_eq!(out.opened.len(), 1, "{status:?} opens");
            assert!(snapshot.worktree_groups[0].rows[0].unread);
        }
    }

    #[test]
    fn stays_unread_across_return_to_running() {
        let mut episodes = UnreadEpisodes::empty();
        let mut waiting = snapshot(vec![row("a", AgentStatus::Waiting, 1_000)]);
        episodes.reconcile(&mut waiting, &ReadMarks::empty(), false);

        let mut running = snapshot(vec![row("a", AgentStatus::Running, 2_000)]);
        let out = episodes.reconcile(&mut running, &ReadMarks::empty(), false);

        assert!(out.opened.is_empty());
        assert!(running.worktree_groups[0].rows[0].unread);
    }

    #[test]
    fn read_mark_clears_only_when_it_reaches_episode() {
        let mut episodes = UnreadEpisodes::empty();
        let mut first = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
        episodes.reconcile(&mut first, &ReadMarks::empty(), false);

        let mut old_mark = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
        episodes.reconcile(
            &mut old_mark,
            &ReadMarks::from_entries([("a".to_owned(), 999)]),
            false,
        );
        assert!(old_mark.worktree_groups[0].rows[0].unread);

        let mut reached = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
        let out = episodes.reconcile(
            &mut reached,
            &ReadMarks::from_entries([("a".to_owned(), 1_000)]),
            false,
        );
        assert!(!reached.worktree_groups[0].rows[0].unread);
        assert!(out.changed);
        assert!(
            out.cleared.is_empty(),
            "the read-mark writer owns focus/mark_read clear tracing"
        );
    }

    #[test]
    fn later_activity_after_read_opens_a_new_episode() {
        let mut episodes = UnreadEpisodes::empty();
        let mut first = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
        episodes.reconcile(&mut first, &ReadMarks::empty(), false);

        let marks = ReadMarks::from_entries([("a".to_owned(), 1_500)]);
        let mut later = snapshot(vec![row("a", AgentStatus::Success, 2_000)]);
        let out = episodes.reconcile(&mut later, &marks, false);

        assert!(out.changed);
        assert!(
            out.cleared.is_empty(),
            "read-reached pruning must not emit a second clear trace"
        );
        assert_eq!(out.opened.len(), 1);
        assert_eq!(out.opened[0].episode_ms, 2_000);
        assert!(later.worktree_groups[0].rows[0].unread);
    }

    #[test]
    fn read_reached_prune_wins_over_row_gone_trace() {
        let mut episodes = UnreadEpisodes::empty();
        let row = row("a", AgentStatus::Success, 1_000);
        episodes.open_for_row(&row, 1_000);
        let mut empty = snapshot(Vec::new());

        let out = episodes.reconcile(
            &mut empty,
            &ReadMarks::from_entries([("a".to_owned(), 1_000)]),
            false,
        );

        assert!(out.changed);
        assert!(
            out.cleared.is_empty(),
            "a human read receipt owns the clear trace even if the row vanished before pruning"
        );
    }

    #[test]
    fn row_gone_clear_does_not_guess_an_agent_id_from_row_id() {
        let mut episodes = UnreadEpisodes::empty();
        let row = row("a", AgentStatus::Success, 1_000);
        episodes.open_for_row(&row, 1_000);
        let mut empty = snapshot(Vec::new());

        let out = episodes.reconcile(&mut empty, &ReadMarks::empty(), false);

        assert_eq!(out.cleared.len(), 1);
        assert_eq!(out.cleared[0].cause, UnreadClearCause::RowGone);
        assert!(out.cleared[0].agent_id.is_none());
    }

    #[test]
    fn row_label_fallback_uses_agent_handle() {
        let mut row = row("abcdefghi", AgentStatus::Waiting, 1_000);
        row.as_agent_mut().unwrap().handle = Some("planner".to_owned());

        assert_eq!(row_label(&row), "planner abcdefgh");
    }

    #[test]
    fn frameless_reconcile_neither_opens_nor_prunes() {
        let mut episodes = UnreadEpisodes::empty();
        let open_row = row("a", AgentStatus::Success, 1_000);
        episodes.open_for_row(&open_row, 1_000);
        let mut frameless = snapshot(vec![row("b", AgentStatus::Waiting, 2_000)]);
        frameless.panes_produced_at_ms = None;

        let out = episodes.reconcile(&mut frameless, &ReadMarks::empty(), false);

        assert!(!out.changed);
        assert!(out.opened.is_empty());
        assert!(out.cleared.is_empty());
        assert!(episodes.episodes.contains_key("a"));
        assert!(!episodes.episodes.contains_key("b"));
    }

    #[test]
    fn cold_start_opens_silently() {
        let mut episodes = UnreadEpisodes::empty();
        let mut snapshot = snapshot(vec![row("a", AgentStatus::Waiting, 1_000)]);
        let out = episodes.reconcile(&mut snapshot, &ReadMarks::empty(), true);

        assert!(out.opened[0].silent);
        assert!(snapshot.worktree_groups[0].rows[0].unread);
    }

    #[test]
    fn mark_rows_unread_skips_process_rows_without_panicking() {
        // `opened_unread` asserts an agent status; a process row carries none.
        // The durable path must skip it rather than unwind, whether the target
        // came from the `M` key or a `rimz sidebar mark-unread @process-pane`.
        // (Regression: R1-01.)
        let (_dir, runtime) = runtime();
        let rows = vec![
            process_row("term", 1_000),
            row("a", AgentStatus::Waiting, 2_000),
        ];

        let opened = mark_rows_unread(&runtime, &rows, 5_000).expect("mark unread");

        assert_eq!(opened.len(), 1, "only the agent row opens an episode");
        assert_eq!(opened[0].row_id, "a");
        let loaded = UnreadEpisodes::load(&runtime);
        assert!(loaded.episodes.contains_key("a"));
        assert!(!loaded.episodes.contains_key("term"));
    }

    #[test]
    fn mark_rows_unread_writes_nothing_for_only_process_rows() {
        // No agent row means no episode and no file write — the unread file stays
        // absent rather than being rewritten with an empty map.
        let (_dir, runtime) = runtime();

        let opened =
            mark_rows_unread(&runtime, &[process_row("term", 1_000)], 5_000).expect("mark unread");

        assert!(opened.is_empty());
        assert!(
            !runtime.unread_path().exists(),
            "no agent rows means no durable write",
        );
    }

    #[test]
    fn load_persist_round_trips_and_garbage_reads_empty() {
        let (_dir, runtime) = runtime();
        let mut episodes = UnreadEpisodes::empty();
        let row = row("a", AgentStatus::Waiting, 1_000);
        episodes.open_for_row(&row, 1_000);
        episodes.persist(&runtime).expect("persist");

        let loaded = UnreadEpisodes::load(&runtime);
        assert_eq!(loaded.episodes.get("a"), Some(&1_000));

        fs::write(runtime.unread_path(), b"{ not json").expect("garbage");
        assert_eq!(UnreadEpisodes::load(&runtime), UnreadEpisodes::empty());
    }
}
