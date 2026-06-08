//! Typed live pane topology published by the sidebar producer.
//!
//! The mux seam remains a flat [`PaneRef`](crate::feed::PaneRef) list because
//! non-sidebar callers route by pane. The sidebar producer lifts that list into
//! tabs/windows, keeps process state as one record, and publishes the topology
//! as cache-class `snapshot.json`. The frame admits every rendered sidebar
//! card; ledger, sidecars, and realtime events only enrich cards whose pane is
//! present here.

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::{AgentSessionId, MuxName, PaneId, ViewId, ViewKind};
use crate::ledger::snapshot::SidebarOwnView;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneFrame {
    pub produced_at_ms: u64,
    pub session_name: String,
    pub tabs: Vec<TabFrame>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabFrame {
    pub view_id: ViewId,
    pub kind: ViewKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane: Option<PaneId>,
    pub panes: Vec<PaneState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub pane_id: PaneId,
    pub current: PaneProcess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PaneProcess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<u32>,
    #[serde(default, skip_serializing_if = "PaneMetrics::is_empty")]
    pub metrics: PaneMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneProcess {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_session_id: Option<AgentSessionId>,
}

/// Producer-sampled resource figures for one pane's foreground process —
/// display-only, written by the metrics cadence and projected onto process
/// rows. The CPU/memory/IO figures publish together once two same-tenant
/// `/proc` samples complete them, never as a partial set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMetrics {
    /// The sampler's stuck verdict only — `Some(Stuck)` when `/proc` reported a
    /// zombie or repeated uninterruptible sleep, else `None`. Idle-vs-busy is
    /// never carried here; the fold classifies it from the pane's program
    /// (`ledger::snapshot::process`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_state: Option<crate::ProcessState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_bps: Option<u64>,
}

impl PaneMetrics {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl PaneFrame {
    pub fn to_pane_refs(&self) -> Vec<PaneRef> {
        self.tabs
            .iter()
            .flat_map(|tab| {
                tab.panes
                    .iter()
                    .map(move |pane| self.pane_ref_for_state(tab, pane))
            })
            .collect()
    }

    pub fn pane_metrics(&self) -> impl Iterator<Item = (PaneId, PaneMetrics)> + '_ {
        self.pane_states()
            .map(|pane| (pane.pane_id.clone(), pane.metrics))
    }

    pub fn pane_states(&self) -> impl Iterator<Item = &PaneState> {
        self.tabs.iter().flat_map(|tab| tab.panes.iter())
    }

    pub fn pane_states_mut(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.tabs.iter_mut().flat_map(|tab| tab.panes.iter_mut())
    }

    pub fn rotate_against_prior(&mut self, prior: &PaneFrame) {
        let prior_by_pane: HashMap<PaneId, &PaneState> = prior
            .pane_states()
            .map(|pane| (pane.pane_id.clone(), pane))
            .collect();
        for pane in self.pane_states_mut() {
            if let Some(prior) = prior_by_pane.get(&pane.pane_id) {
                pane.rotate_on_process_change(prior);
            }
        }
    }

    fn pane_ref_for_state(&self, tab: &TabFrame, pane: &PaneState) -> PaneRef {
        PaneRef {
            pane_id: pane.pane_id.clone(),
            session_name: self.session_name.clone(),
            view_id: Some(tab.view_id.to_string()),
            view_kind: Some(tab.kind),
            view_name: tab.name.clone(),
            is_focused: tab.active_pane.as_ref() == Some(&pane.pane_id),
            command: pane.current.command.clone(),
            spawn_command: pane.current.spawn_command.clone(),
            cwd: pane.current.cwd.clone(),
            pane_pid: pane.current.pid,
            pane_process_start: pane.current.started_at,
            resumed_session_id: pane.current.resumed_session_id.clone(),
        }
    }
}

impl PaneState {
    /// Join this fresh pane state to the prior frame's state for the same pane
    /// id. A changed spawn command, pid, or process start is a new tenant: the
    /// prior current process rotates to `previous` and the fresh record stands
    /// clean. A stable identity repairs raced-null mux fields (`command`,
    /// `spawn_command`, `cwd`, `started_at`) from the prior read and carries
    /// `previous` along.
    ///
    /// `current.pid` is never backfilled here: on Zellij the pid is a
    /// metrics-layer derivation, and only that layer's `starttime` pid-reuse
    /// guard may restore it ([`super::produce`]'s metrics module) — a rotation
    /// carry would republish a stale binding without ever revalidating it.
    pub fn rotate_on_process_change(&mut self, prior: &PaneState) {
        let spawn_changed = match (
            self.current.spawn_command.as_deref(),
            prior.current.spawn_command.as_deref(),
        ) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        let pid_changed = match (self.current.pid, prior.current.pid) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        let start_changed = match (self.current.started_at, prior.current.started_at) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        if spawn_changed || pid_changed || start_changed {
            self.previous = Some(prior.current.clone());
            return;
        }

        self.previous = prior.previous.clone();
        if self.current.command.is_none() {
            self.current.command = prior.current.command.clone();
        }
        if self.current.spawn_command.is_none() {
            self.current.spawn_command = prior.current.spawn_command.clone();
        }
        if self.current.cwd.is_none() {
            self.current.cwd = prior.current.cwd.clone();
        }
        if self.current.started_at.is_none() {
            self.current.started_at = prior.current.started_at;
        }
        if self.current.resumed_session_id.is_none() {
            self.current.resumed_session_id = prior.current.resumed_session_id.clone();
        }
    }
}

// The constructor lives here, beside the frame it consumes, rather than with
// the `SidebarOwnView` type in `ledger/snapshot` — the ledger read path stays
// free of sidebar imports and only the sidebar fold derives an own-view.
impl SidebarOwnView {
    pub fn from_frame(own: &PaneId, frame: &PaneFrame) -> Option<Self> {
        let tab = frame
            .tabs
            .iter()
            .find(|tab| tab.panes.iter().any(|pane| pane.pane_id == *own))?;
        let own_pane = tab.panes.iter().find(|pane| pane.pane_id == *own)?;
        let siblings = tab
            .panes
            .iter()
            .filter(|pane| pane.pane_id != *own)
            .collect::<Vec<_>>();
        let non_sidebar_siblings = siblings
            .iter()
            .copied()
            .filter(|pane| !pane_is_sidebar_chrome(pane))
            .collect::<Vec<_>>();
        let active_pane_id = tab.active_pane.as_ref().and_then(|active| {
            non_sidebar_siblings
                .iter()
                .any(|pane| pane.pane_id == *active)
                .then(|| active.clone())
        });
        let working_pane_ids = non_sidebar_siblings
            .iter()
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let own_view_is_daemon = !non_sidebar_siblings.is_empty()
            && non_sidebar_siblings.iter().all(|pane| {
                crate::remote_control::pane_is_host(&frame.pane_ref_for_state(tab, pane))
            });
        Some(Self {
            sibling_count: siblings.len(),
            own_is_active: tab.active_pane.as_ref() == Some(&own_pane.pane_id),
            active_pane_id,
            working_pane_ids,
            own_view_is_daemon,
        })
    }
}

pub fn assemble_frame(
    panes: Vec<PaneRef>,
    produced_at_ms: u64,
    session_name: impl Into<String>,
) -> PaneFrame {
    let mut tabs: BTreeMap<ViewId, TabFrame> = BTreeMap::new();
    for pane in panes {
        let view_id = pane
            .view_id
            .clone()
            .map(ViewId::new_unchecked)
            .unwrap_or_else(|| ViewId::new_unchecked(format!("pane:{}", pane.pane_id)));
        let kind = pane
            .view_kind
            .unwrap_or_else(|| default_view_kind(pane.pane_id.mux()));
        let tab = tabs.entry(view_id.clone()).or_insert_with(|| TabFrame {
            view_id,
            kind,
            name: pane.view_name.clone(),
            active_pane: None,
            panes: Vec::new(),
        });
        if tab.name.is_none() {
            tab.name = pane.view_name.clone();
        }
        if pane.is_focused {
            if tab.active_pane.is_none() {
                tab.active_pane = Some(pane.pane_id.clone());
            } else {
                tracing::debug!(
                    view_id = %tab.view_id,
                    pane_id = %pane.pane_id,
                    "dropping extra active pane mark in one view"
                );
            }
        }
        let resumed_session_id = pane.resumed_session_id.or_else(|| {
            pane.command
                .as_deref()
                .and_then(crate::remote_control::codex_resumed_session_id_from_cmdline)
        });
        tab.panes.push(PaneState {
            pane_id: pane.pane_id,
            current: PaneProcess {
                pid: pane.pane_pid,
                command: pane.command,
                spawn_command: pane.spawn_command,
                cwd: pane.cwd,
                started_at: pane.pane_process_start,
                resumed_session_id,
            },
            previous: None,
            children: Vec::new(),
            metrics: PaneMetrics::default(),
        });
    }
    PaneFrame {
        produced_at_ms,
        session_name: session_name.into(),
        tabs: tabs.into_values().collect(),
    }
}

fn default_view_kind(mux: MuxName) -> ViewKind {
    match mux {
        MuxName::Zellij => ViewKind::Tab,
        MuxName::Tmux => ViewKind::Window,
    }
}

fn pane_is_sidebar_chrome(pane: &PaneState) -> bool {
    pane.current
        .command
        .as_deref()
        .is_some_and(crate::ledger::snapshot::command_is_sidebar_chrome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn pane(raw: &str, view: &str, command: Option<&str>, focused: bool) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(ViewKind::Tab),
            view_name: None,
            is_focused: focused,
            command: command.map(ToOwned::to_owned),
            spawn_command: None,
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
        }
    }

    #[test]
    fn active_pane_is_structural_one_per_tab() {
        let frame = assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), true),
                pane("terminal_2", "tab_0", Some("cargo build"), true),
                pane("terminal_3", "tab_1", Some("zsh"), true),
            ],
            7,
            "rimz-test",
        );

        assert_eq!(
            frame.tabs[0].active_pane,
            Some(frame.tabs[0].panes[0].pane_id.clone())
        );
        assert_eq!(
            frame.tabs[1].active_pane,
            Some(frame.tabs[1].panes[0].pane_id.clone())
        );
        let projected = frame.to_pane_refs();
        assert!(projected[0].is_focused);
        assert!(!projected[1].is_focused);
        assert!(projected[2].is_focused);
    }

    #[test]
    fn foreground_change_with_stable_spawn_does_not_rotate() {
        let old_start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
        let mut prior = assemble_frame(
            vec![PaneRef {
                command: Some("codex".to_owned()),
                spawn_command: Some(
                    "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main"
                        .to_owned(),
                ),
                ..pane("terminal_1", "tab_0", Some("codex"), false)
            }],
            1,
            "rimz-test",
        );
        prior.tabs[0].panes[0].current.started_at = Some(old_start);
        let mut fresh = assemble_frame(
            vec![PaneRef {
                command: Some("/usr/bin/codex".to_owned()),
                spawn_command: Some(
                    "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main"
                        .to_owned(),
                ),
                ..pane("terminal_1", "tab_0", Some("codex"), false)
            }],
            2,
            "rimz-test",
        );
        fresh.tabs[0].panes[0].current.started_at = Some(old_start);

        fresh.rotate_against_prior(&prior);

        let state = &fresh.tabs[0].panes[0];
        assert_eq!(state.current.command.as_deref(), Some("/usr/bin/codex"));
        assert_eq!(state.current.started_at, Some(old_start));
        assert!(state.previous.is_none());
    }

    #[test]
    fn unchanged_command_repairs_raced_nulls_and_keeps_previous() {
        let mut prior = assemble_frame(
            vec![pane("terminal_1", "tab_0", Some("claude"), false)],
            1,
            "rimz-test",
        );
        prior.tabs[0].panes[0].current.pid = Some(42);
        prior.tabs[0].panes[0].current.spawn_command = Some("rimz agents exec claude".to_owned());
        prior.tabs[0].panes[0].previous = Some(PaneProcess {
            pid: Some(41),
            command: Some("zsh".to_owned()),
            spawn_command: None,
            cwd: Some("/repo/main".to_owned()),
            started_at: None,
            resumed_session_id: None,
        });
        let mut fresh = assemble_frame(
            vec![PaneRef {
                command: None,
                cwd: None,
                pane_pid: None,
                ..pane("terminal_1", "tab_0", None, false)
            }],
            2,
            "rimz-test",
        );

        fresh.rotate_against_prior(&prior);

        let state = &fresh.tabs[0].panes[0];
        assert_eq!(state.current.command.as_deref(), Some("claude"));
        assert_eq!(
            state.current.spawn_command.as_deref(),
            Some("rimz agents exec claude")
        );
        assert_eq!(state.current.cwd.as_deref(), Some("/repo/main"));
        // The pid is never rotation-carried: only the metrics layer restores
        // it, behind its starttime pid-reuse guard.
        assert_eq!(state.current.pid, None);
        assert_eq!(
            state
                .previous
                .as_ref()
                .and_then(|previous| previous.command.as_deref()),
            Some("zsh")
        );
    }

    #[test]
    fn pid_change_rejects_prior_tenant_stamp_even_with_same_command() {
        let old_start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
        let mut prior = assemble_frame(
            vec![pane("terminal_1", "tab_0", Some("codex"), false)],
            1,
            "rimz-test",
        );
        prior.tabs[0].panes[0].current.pid = Some(100);
        prior.tabs[0].panes[0].current.started_at = Some(old_start);
        let mut fresh = assemble_frame(
            vec![pane("terminal_1", "tab_0", Some("codex"), false)],
            2,
            "rimz-test",
        );
        fresh.tabs[0].panes[0].current.pid = Some(200);

        fresh.rotate_against_prior(&prior);

        let state = &fresh.tabs[0].panes[0];
        assert_eq!(state.current.pid, Some(200));
        assert_eq!(state.current.started_at, None);
        assert_eq!(
            state.previous.as_ref().and_then(|previous| previous.pid),
            Some(100)
        );
    }

    #[test]
    fn own_view_derives_from_the_own_tab() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let frame = assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("rimz-sidebar"), false),
                pane("terminal_2", "tab_0", Some("zsh"), true),
                pane("terminal_3", "tab_1", Some("cargo build"), true),
            ],
            1,
            "rimz-test",
        );

        let view = SidebarOwnView::from_frame(&own, &frame).expect("own pane is present");

        assert_eq!(view.sibling_count, 1);
        assert!(!view.own_is_active);
        assert_eq!(view.active_pane_id, Some(active.clone()));
        assert_eq!(view.working_pane_ids, vec![active]);
    }

    fn own_view(own: &str, panes: Vec<PaneRef>) -> Option<SidebarOwnView> {
        let own = PaneId::from_parts(MuxName::Zellij, own);
        SidebarOwnView::from_frame(&own, &assemble_frame(panes, 1, "rimz-test"))
    }

    #[test]
    fn own_view_counts_only_siblings_sharing_the_tab() {
        let focused_here = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let view = own_view(
            "terminal_1",
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), false),
                pane("terminal_2", "tab_0", Some("zsh"), true),
                // Another tab — not a sibling.
                pane("terminal_3", "tab_1", Some("zsh"), true),
            ],
        )
        .expect("own pane is present");

        assert_eq!(view.sibling_count, 1);
        assert!(!view.own_is_active);
        assert_eq!(view.active_pane_id, Some(focused_here.clone()));
        assert_eq!(
            view.working_pane_ids,
            vec![focused_here],
            "the working set names only this tab's siblings — the fused \
             focus filter rides it"
        );
    }

    #[test]
    fn own_view_marks_when_the_sidebar_itself_is_active() {
        let view = own_view(
            "terminal_1",
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), true),
                pane("terminal_2", "tab_0", Some("zsh"), false),
            ],
        )
        .expect("own pane is present");

        assert!(view.own_is_active);
        assert_eq!(view.active_pane_id, None);
    }

    #[test]
    fn own_view_is_none_when_own_pane_is_absent() {
        // A view the caller cannot find itself in is unknowable — never close.
        let panes = vec![pane("terminal_1", "tab_0", Some("zsh"), true)];
        assert!(own_view("terminal_404", panes).is_none());
    }

    #[test]
    fn own_view_picks_the_tab_active_pane_without_a_client() {
        // The tab has an active pane but no client is looking at it. The
        // baseline is the tab's active pane, defined regardless of where any
        // client is — so the sidebar in an unviewed tab still points at the
        // pane the user would land on.
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_53");
        let view = own_view(
            "terminal_52",
            vec![
                pane("terminal_52", "tab_11", Some("zsh"), false),
                pane("terminal_53", "tab_11", Some("zsh"), true),
            ],
        )
        .expect("own pane is present");

        assert!(!view.own_is_active);
        assert_eq!(view.active_pane_id, Some(active));
    }

    /// A pane fixture with a view name, so a test can build the `rimzd` daemon
    /// view the tmux window-name fallback recognises.
    fn pane_named(raw: &str, view: &str, command: &str, view_name: &str) -> PaneRef {
        PaneRef {
            view_name: Some(view_name.to_owned()),
            ..pane(raw, view, Some(command), false)
        }
    }

    #[test]
    fn own_view_is_daemon_true_in_the_rimzd_view_zellij() {
        // No view_name on these fixtures: the daemon view is recognised by the
        // host command markers alone, covering builds that omit tab names.
        let view = own_view(
            "terminal_0",
            vec![
                pane("terminal_0", "tab_0", Some("rimz-sidebar"), false),
                pane(
                    "terminal_1",
                    "tab_0",
                    Some("claude remote-control --spawn worktree"),
                    false,
                ),
                pane(
                    "terminal_2",
                    "tab_0",
                    Some("rimz codex app-server serve"),
                    false,
                ),
            ],
        )
        .expect("own pane present");
        assert!(view.own_view_is_daemon);
    }

    #[test]
    fn own_view_is_daemon_true_in_the_rimzd_view_tmux() {
        // tmux: a host pane is recognised by the window-name fallback even when
        // its command carries no marker.
        let view = own_view(
            "terminal_0",
            vec![
                pane_named("terminal_0", "rimzd", "rimz-sidebar", "rimzd"),
                pane_named("terminal_1", "rimzd", "claude", "rimzd"),
            ],
        )
        .expect("own pane present");
        assert!(view.own_view_is_daemon);
    }

    #[test]
    fn own_view_is_daemon_false_in_a_working_view() {
        let view = own_view(
            "terminal_0",
            vec![
                pane("terminal_0", "tab_1", Some("rimz-sidebar"), false),
                pane("terminal_1", "tab_1", Some("zsh"), false),
            ],
        )
        .expect("own pane present");
        assert!(!view.own_view_is_daemon);
    }
}
