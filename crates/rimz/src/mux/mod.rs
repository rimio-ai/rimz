//! Multiplexer abstraction.
//!
//! Everything correctness-critical (ledger, bridge, hooks, schemas) sits
//! above this trait and is identical across backends. Raw pane IDs live
//! only inside the adapter — see [`crate::ids::PaneId`] for the normalized
//! form that travels everywhere else.

mod selection;
pub mod tmux;
pub mod zellij;

pub use selection::auto_detect_backend;
pub use tmux::TmuxBackend;
pub use zellij::ZellijBackend;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, WorkspaceId};

#[derive(Debug, thiserror::Error)]
pub enum MuxErr {
    #[error("multiplexer command `{program}` not found on PATH")]
    NotInstalled { program: String },
    #[error("no multiplexer found: install zellij or tmux")]
    NoMuxFound,
    #[error("multiplexer command failed: {program} {args}: {stderr}")]
    Command {
        program: String,
        args: String,
        stderr: String,
    },
    #[error("pane id `{pane_id}` belongs to `{actual}`, but `{expected}` backend was selected")]
    PaneBackendMismatch {
        pane_id: PaneId,
        expected: MuxName,
        actual: MuxName,
    },
    #[error("could not parse mux output from `{program}`: {reason}")]
    Output { program: String, reason: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, MuxErr>;

/// A built-up command we can run or hand back to `exec(3)`.
#[derive(Clone, Debug, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(&self.env);
        command
    }

    pub fn run(&self) -> Result<Output> {
        let output = self.to_command().output().map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: self.program.clone(),
            },
            _ => MuxErr::Io(err),
        })?;
        if !output.status.success() {
            return Err(MuxErr::Command {
                program: self.program.clone(),
                args: self.args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(output)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneCapture {
    pub pane_id: PaneId,
    pub raw_text: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PaneListOptions {
    pub session_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub session_name: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SidebarPaneOptions {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub width_percent: u16,
    pub rimz_bin: PathBuf,
    pub replace_existing: bool,
}

/// Tally of one in-place sidebar recovery pass ([`MuxBackend::recover_sidebars`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views (Zellij tabs / tmux windows) that had lost their sidebar and got
    /// one re-added in place this pass.
    pub recovered: usize,
    /// Views that needed a sidebar but whose in-place add failed — logged and
    /// skipped, never retried.
    pub failed: usize,
}

/// Group `(view_id, is_sidebar)` pane classifications and return the view ids
/// that hold at least one working (non-sidebar) pane but no sidebar pane — the
/// views whose sidebar was lost and should gain one in place. First-seen order,
/// each view once. Shared by both backends so the "which views lost a sidebar"
/// rule lives in exactly one place.
pub(crate) fn views_missing_sidebar(classified: &[(String, bool)]) -> Vec<String> {
    use std::collections::HashMap;

    let mut order: Vec<String> = Vec::new();
    // view -> (has_working_pane, has_sidebar_pane)
    let mut state: HashMap<String, (bool, bool)> = HashMap::new();
    for (view, is_sidebar) in classified {
        let entry = state.entry(view.clone()).or_insert_with(|| {
            order.push(view.clone());
            (false, false)
        });
        if *is_sidebar {
            entry.1 = true;
        } else {
            entry.0 = true;
        }
    }
    order
        .into_iter()
        .filter(|view| {
            let (has_work, has_sidebar) = state[view];
            has_work && !has_sidebar
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct SplitPaneOptions {
    pub target_pane_id: Option<PaneId>,
    pub cwd: Option<String>,
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
}

/// Backend-neutral mux operations. Every Zellij/tmux command lives behind
/// one of these methods.
pub trait MuxBackend: Send + Sync {
    fn name(&self) -> MuxName;
    fn ensure_session(&self, opts: &SessionOptions) -> Result<()>;
    fn attach_command(&self, name: &str) -> CommandSpec;
    fn detach(&self, name: &str) -> Result<()>;
    /// Force-remove a session by name. A missing session is success — the goal
    /// state is "no session by that name", so callers can retire a stale or
    /// renamed session idempotently.
    fn kill_session(&self, name: &str) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<String>>;
    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>>;
    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()>;
    fn focus_pane(&self, pane: &PaneId) -> Result<()>;
    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture>;
    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn open_sidebar(&self, opts: &SidebarPaneOptions) -> Result<()>;
    /// Re-add a sidebar to every view (Zellij tab / tmux window) that holds
    /// working panes but lost its sidebar, in place and without disturbing
    /// existing panes. One best-effort pass: a view whose add fails is logged
    /// and skipped — never retried, never a session rebirth. Unlike
    /// [`Self::open_sidebar`], this never deletes or recreates the session.
    fn recover_sidebars(&self, opts: &SidebarPaneOptions) -> Result<SidebarRecovery>;
    /// Best-effort wakeup; sockets are the channel of record per the docs.
    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()>;
    fn version(&self) -> Result<String>;
}

/// Construct a boxed backend for the named multiplexer.
pub fn backend_for(mux: MuxName) -> Box<dyn MuxBackend> {
    match mux {
        MuxName::Zellij => Box::new(ZellijBackend),
        MuxName::Tmux => Box::new(TmuxBackend::new()),
    }
}

pub(crate) fn ensure_pane_backend(pane: &PaneId, expected: MuxName) -> Result<()> {
    let actual = pane.mux();
    if actual != expected {
        return Err(MuxErr::PaneBackendMismatch {
            pane_id: pane.clone(),
            expected,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_missing_sidebar_flags_only_worked_views_without_one() {
        let classified = vec![
            // tab 12: a working pane, no sidebar -> needs recovery.
            ("12".to_owned(), false),
            // tab 15: sidebar + working pane -> healthy.
            ("15".to_owned(), true),
            ("15".to_owned(), false),
            // tab 16: sidebar only (no working pane) -> nothing to serve, skip.
            ("16".to_owned(), true),
            // tab 17: two working panes, no sidebar -> needs recovery, listed once.
            ("17".to_owned(), false),
            ("17".to_owned(), false),
        ];
        assert_eq!(
            views_missing_sidebar(&classified),
            vec!["12".to_owned(), "17".to_owned()],
            "only views with work but no sidebar, in first-seen order, deduped"
        );
    }

    #[test]
    fn views_missing_sidebar_is_empty_when_every_view_is_healthy() {
        let classified = vec![
            ("a".to_owned(), true),
            ("a".to_owned(), false),
            ("b".to_owned(), false),
            ("b".to_owned(), true),
        ];
        assert!(views_missing_sidebar(&classified).is_empty());
    }

    #[test]
    fn pane_backend_mismatch_is_rejected_before_running_mux_command() {
        let pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let err = ensure_pane_backend(&pane, MuxName::Tmux).unwrap_err();
        assert!(matches!(
            err,
            MuxErr::PaneBackendMismatch {
                expected: MuxName::Tmux,
                actual: MuxName::Zellij,
                ..
            }
        ));
    }
}
