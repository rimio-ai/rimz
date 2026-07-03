use serde::{Deserialize, Serialize};

/// Remote-control auto-launch policy, per agent. Off unless explicitly enabled
/// — Rimz never links your account or starts a remote-control host without
/// opt-in, so the absence of this section reads as "do nothing". Each agent has
/// its own toggle because each links a different account and is detected
/// independently — Claude on PATH, Codex by its managed standalone install.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteControlConfig {
    /// Auto-launch `claude remote-control` (the worktree spawn mode) in the
    /// managed background view when Claude is on PATH and a workspace starts.
    pub claude: bool,
    /// Ensure the per-user Codex app-server daemon by spawning `codex
    /// remote-control start` detached on workspace start — a per-user singleton
    /// (one control socket), not a pane. `remote-control start` boots its daemon
    /// from the managed standalone install (a `codex` on PATH alone won't do).
    /// When that install is absent, `rimz start` skips the Codex host and still
    /// brings the room up; `rimz doctor` flags it with the install fix. The
    /// daemon it brings up is the one Codex enrichment re-uses over the control
    /// socket.
    pub codex: bool,
}

impl RemoteControlConfig {
    pub fn enabled_for(&self, kind: &str) -> bool {
        match kind {
            "claude" => self.claude,
            "codex" => self.codex,
            _ => false,
        }
    }
}
