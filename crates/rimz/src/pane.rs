//! Shared pane identity and runtime owner primitives.
//!
//! Pane references are live-view routing metadata shared by agent rollups, mux
//! snapshots, and sidebar projections. They stay outside the
//! store snapshot modules so live-presence types do not depend on durable
//! read/write layers.

pub mod bandwidth;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId, PaneId, ViewKind};

/// Pane title/name that marks RimZ's own sidebar renderer — the one chrome
/// classification key. The renderer sets it (terminal title escape), the
/// Zellij layout names its pane with it, and the tmux/Zellij/store
/// classifiers all match against it.
pub const SIDEBAR_CHROME_TITLE: &str = "rimz-sidebar";

/// Runtime owner class for records that should appear in live views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOwnerKind {
    Agent,
    Daemon,
    Script,
}

impl RuntimeOwnerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Daemon => "daemon",
            Self::Script => "script",
        }
    }
}

/// Process identity for read-time runtime projection.
///
/// Durable records remain on disk after the owner exits. Runtime views include
/// them only while this process identity is still alive. `process_start` is
/// the Linux `/proc/<pid>/stat` start-time token when available; it defeats PID
/// reuse without becoming a cross-platform requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeOwner {
    pub kind: RuntimeOwnerKind,
    pub subject_id: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<String>,
}

impl RuntimeOwner {
    pub fn new(
        kind: RuntimeOwnerKind,
        subject_id: impl Into<String>,
        pid: u32,
        process_start: Option<String>,
    ) -> Self {
        Self {
            kind,
            subject_id: subject_id.into(),
            pid,
            process_start,
        }
    }
}

/// A pane-local process hint for an agent CLI running under another real uid.
/// This is display metadata only: it never mutates [`PaneRef::command`], so
/// agent binding and idle synthesis continue to read only mux-reported process
/// identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ElevatedAgent {
    pub kind: AgentKind,
    pub uid: u32,
}

/// Lean pane location for routing humans to the right pane — never used for
/// correctness-critical state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane_id: PaneId,
    pub session_name: String,
    /// The view (tab/window) holding the pane, by the backend's *internal* id
    /// (Zellij `tab_15`, tmux `@3`). An opaque grouping key, never the view's
    /// on-screen label: a Zellij tab *named* "Tab #15" and the internal id
    /// `tab_15` are routinely different tabs — see
    /// docs/internals/multiplexers.md → Pane and view IDs.
    #[serde(default)]
    pub view_id: Option<String>,
    #[serde(default)]
    pub view_kind: Option<ViewKind>,
    /// View name as reported by the multiplexer (tmux window name, Zellij tab
    /// name). Advisory UI metadata — used to recognise RimZ-launched background
    /// views such as the remote-control host. Never a correctness signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_name: Option<String>,
    /// Pane title/name as reported by the multiplexer. Advisory classification
    /// metadata for sidebar chrome and managed background panes; agent binding
    /// never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether this pane is a floating overlay rather than part of the tiled
    /// room. Floating panes stay addressable but are not rendered as room rows.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_floating: bool,
    /// Foreground command as reported by the multiplexer, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Full `/proc` cmdline matched to [`Self::command`] when the mux reports
    /// only a program basename. Display-only; never an identity key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cmdline: Option<String>,
    /// Birth argv used to launch the pane, if the backend reports it. Advisory
    /// identity/classification metadata, never liveness; display prefers
    /// [`Self::command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_command: Option<String>,
    /// Current working directory as reported by the multiplexer, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Best-effort live pane process id. This is advisory routing metadata,
    /// not correctness state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_pid: Option<u32>,
    /// Used to detect reused pane IDs across mux restarts.
    #[serde(default)]
    pub pane_process_start: Option<Timestamp>,
    /// Agent kind whose in-pane CLI process is live under this pane's root
    /// process. Producer-derived process-tree truth; process rows use it to
    /// disambiguate shared runtimes, while wired rows use it for binding and
    /// liveness without changing the mux-reported command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_agent_kind: Option<AgentKind>,
    /// Start time of [`Self::hosted_agent_kind`]'s in-pane CLI process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_agent_process_start: Option<Timestamp>,
    /// Session id parsed from a resumed agent command such as
    /// `codex resume <session-id>`. Exact rebirth binding reads this before any
    /// cwd or process-start heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_session_id: Option<AgentSessionId>,
    /// Best-effort marker for an agent descendant running through an elevation
    /// wrapper as another real uid. It stays separate from `command` so the row
    /// can be relabelled without ever binding as a local agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated_agent: Option<ElevatedAgent>,
    /// Producer wall-clock millisecond when this pane id first appeared in a
    /// repaired frame. `None` means an older producer/cache wrote the frame, so
    /// newborn-specific guards stay disabled for wire compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at_ms: Option<u64>,
}

impl PaneRef {
    /// A minimal reference carrying just the normalized pane id — the ambient
    /// stamp hooks and script asks record. Live mux truth (command, cwd,
    /// process start) joins at the pane fold.
    pub fn from_id(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            session_name: String::new(),
            view_id: None,
            view_kind: None,
            view_name: None,
            title: None,
            is_floating: false,
            command: None,
            foreground_cmdline: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    /// Whether this pane is RimZ's own sidebar chrome. Worktree liveness checks
    /// ignore it because the sidebar inherits its view's cwd without being a
    /// user pane working in that tree.
    pub fn is_rimz_sidebar(&self) -> bool {
        self.command
            .as_deref()
            .is_some_and(crate::store::snapshot::command_is_sidebar_chrome)
    }
}

/// Pick the session's sidebar pane for a caller: the sidebar sharing the first
/// matching preferred view (in order), else any sidebar in the listing.
pub fn select_sidebar_pane<'a>(
    panes: &'a [PaneRef],
    preferred_views: &[Option<String>],
) -> Option<&'a PaneRef> {
    for view in preferred_views.iter().flatten() {
        if let Some(sidebar) = panes
            .iter()
            .filter(|pane| pane.is_rimz_sidebar())
            .find(|pane| pane.view_id.as_ref() == Some(view))
        {
            return Some(sidebar);
        }
    }
    panes.iter().find(|pane| pane.is_rimz_sidebar())
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(raw: &str, view: &str, command: Option<&str>) -> PaneRef {
        PaneRef {
            view_id: Some(view.to_owned()),
            command: command.map(ToOwned::to_owned),
            cwd: Some("/repo-worktrees/demo".to_owned()),
            ..PaneRef::from_id(PaneId::from_parts(crate::ids::MuxName::Zellij, raw))
        }
    }

    #[test]
    fn pane_ref_classifies_rimz_sidebar_from_command() {
        assert!(pane("terminal_1", "tab_1", Some(SIDEBAR_CHROME_TITLE)).is_rimz_sidebar());
        assert!(!pane("terminal_1", "tab_1", Some("codex")).is_rimz_sidebar());
        assert!(!pane("terminal_1", "tab_1", Some("zsh")).is_rimz_sidebar());
        assert!(!pane("terminal_1", "tab_1", None).is_rimz_sidebar());
    }

    #[test]
    fn sidebar_selection_respects_preferred_view_order() {
        let panes = vec![
            pane("terminal_1", "tab_1", Some(SIDEBAR_CHROME_TITLE)),
            pane("terminal_2", "tab_2", Some(SIDEBAR_CHROME_TITLE)),
        ];

        let selected = select_sidebar_pane(
            &panes,
            &[Some("tab_2".to_owned()), Some("tab_1".to_owned())],
        )
        .expect("sidebar");

        assert_eq!(selected.pane_id.as_str(), "zellij:terminal_2");
    }

    #[test]
    fn sidebar_selection_falls_back_to_any_sidebar() {
        let panes = vec![
            pane("terminal_1", "tab_1", Some("zsh")),
            pane("terminal_2", "tab_2", Some(SIDEBAR_CHROME_TITLE)),
        ];

        let selected =
            select_sidebar_pane(&panes, &[Some("missing".to_owned())]).expect("fallback sidebar");

        assert_eq!(selected.pane_id.as_str(), "zellij:terminal_2");
    }

    #[test]
    fn sidebar_selection_ignores_non_sidebar_panes() {
        let panes = vec![
            pane("terminal_1", "tab_1", Some("zsh")),
            pane("terminal_2", "tab_2", Some("codex")),
        ];

        assert!(select_sidebar_pane(&panes, &[Some("tab_1".to_owned())]).is_none());
        assert!(select_sidebar_pane(&[], &[]).is_none());
    }
}
