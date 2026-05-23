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
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId};

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
    fn ensure_session(&self, name: &str) -> Result<()>;
    fn attach_command(&self, name: &str) -> CommandSpec;
    fn detach(&self, name: &str) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<String>>;
    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>>;
    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()>;
    fn focus_pane(&self, pane: &PaneId) -> Result<()>;
    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture>;
    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn open_sidebar(&self, session_name: &str, width: u16) -> Result<()>;
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
