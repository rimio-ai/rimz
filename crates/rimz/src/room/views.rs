//! The room's fixed views, shown by name: the dashboard and the numbered
//! terminals. A view's name is its whole identity, so nothing is recorded:
//! a closed terminal is simply absent, and the next show makes it again.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use super::RoomContext;
use crate::daemon_view::ReopenOutcome;
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::mux::{LayoutColumn, LayoutPanes, PaneCmd, TabOptions, ViewShown};

/// How long a terminal's creation waits on the session's sidebar reconcile
/// lock, the bound the dashboard reopen holds it to as well.
const CREATE_LOCK_BOUND: Duration = Duration::from_secs(60);

/// What showing a fixed view did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedView {
    /// The view is shown: found, created, or reopened.
    Shown,
    /// No client is focused on a terminal pane to show it to (Zellij).
    NoClient,
}

impl RoomContext {
    /// Show the dashboard, reopening it first when no view bears its name.
    pub fn show_dashboard(&self) -> Result<FixedView> {
        let name = crate::pane::VIEW_NAME;
        if let Some(shown) = self.show_named(name)? {
            return Ok(shown);
        }
        let sidebar = self.sidebar_options(&self.workspace.project_root);
        match crate::reload::reopen_dashboard(self.backend(), &sidebar, &self.machine_config) {
            ReopenOutcome::Reopened | ReopenOutcome::None => {}
            ReopenOutcome::Deferred => return Ok(FixedView::NoClient),
            ReopenOutcome::Busy => return Err(self.busy()),
            ReopenOutcome::Failed => bail!(
                "reopening the dashboard of session {} failed; run `rimz sidebar repair`",
                self.session_name()
            ),
        }
        self.show_named(name)?.ok_or_else(|| {
            anyhow!(
                "the dashboard of session {} is still closed after its reopen; run `rimz sidebar repair`",
                self.session_name()
            )
        })
    }

    /// Show terminal `n` (1..=9), the tab named `term-N`, opening it at the
    /// project root when no view bears the name. Creation holds the session's
    /// sidebar reconcile lock and asks again inside it, so concurrent calls
    /// leave one tab.
    pub fn show_terminal(&self, n: u8) -> Result<FixedView> {
        let name = crate::pane::terminal_view_name(n);
        if let Some(shown) = self.show_named(&name)? {
            return Ok(shown);
        }
        let lock = self
            .runtime
            .sidebar_reconcile_lock(self.mux_name(), self.session_name());
        let _guard = match WorkspaceLock::acquire_with_timeout(&lock, CREATE_LOCK_BOUND) {
            Ok(guard) => guard,
            Err(LockErr::Timeout { .. }) => return Err(self.busy()),
            Err(error) => return Err(error).context("locking the session to open a terminal"),
        };
        if let Some(shown) = self.show_named(&name)? {
            return Ok(shown);
        }
        let cwd = &self.workspace.project_root;
        self.backend
            .open_tab(&TabOptions {
                env: Default::default(),
                title: name.clone(),
                panes: LayoutPanes {
                    columns: vec![LayoutColumn {
                        panes: vec![PaneCmd {
                            argv: vec![crate::proc::user_shell_program()],
                            name: None,
                        }],
                        stacked: false,
                    }],
                    focused_pane: 0,
                },
                focus: true,
                dock_sidebar: true,
                after: None,
                sidebar: self.sidebar_options(cwd),
            })
            .with_context(|| format!("opening {name} in session {}", self.session_name()))?;
        Ok(FixedView::Shown)
    }

    fn show_named(&self, name: &str) -> Result<Option<FixedView>> {
        let shown = self
            .backend
            .show_view(self.session_name(), name)
            .with_context(|| format!("showing {name} in session {}", self.session_name()))?;
        Ok(match shown {
            ViewShown::Shown => Some(FixedView::Shown),
            ViewShown::NoClient => Some(FixedView::NoClient),
            ViewShown::Absent => None,
        })
    }

    fn busy(&self) -> anyhow::Error {
        anyhow!(
            "another repair holds session {}; press again",
            self.session_name()
        )
    }
}
