//! Hermetic process namespace for live Zellij integration tests.

use std::path::Path;
use std::process::Command;

use portable_pty::CommandBuilder;
use rimz::testkit::sandbox::TestSandbox;

use super::{CommandTimeoutExt, ScrubSessionEnvExt};

/// Owns the private HOME, XDG, and temporary-file surface shared by one
/// Zellij test server and all of its clients.
pub struct ZellijNamespace {
    sandbox: TestSandbox,
}

impl ZellijNamespace {
    pub fn new() -> Self {
        let sandbox = TestSandbox::zellij()
            .and_then(|sandbox| sandbox.arm(Path::new(env!("CARGO_BIN_EXE_rimz-test-reaper"))))
            .expect("arm Zellij namespace reaper");
        let config_dir = sandbox.home_root().join(".config/zellij");
        std::fs::create_dir_all(&config_dir).expect("zellij config dir");
        std::fs::write(
            config_dir.join("config.kdl"),
            "// Hermetic test config: stock behavior, no first-run wizard or tips UI.\nshow_startup_tips false\nshow_release_notes false\n",
        )
        .expect("zellij config.kdl");
        Self { sandbox }
    }

    pub fn path(&self) -> &Path {
        self.sandbox.home_root()
    }

    /// Build a short-lived control command scoped to this namespace.
    pub fn command(&self) -> Command {
        Self::command_at(self.path())
    }

    /// Build a control command from a path retained by a managed client or
    /// helper while the owning namespace remains alive.
    pub(crate) fn command_at(path: &Path) -> Command {
        let mut command = Command::new("zellij");
        command.scrub_session_env();
        Self::pin_command_at(path, &mut command);
        command
    }

    /// Scope a long-lived PTY child to the same namespace as control calls.
    pub fn pin_pty(&self, command: &mut CommandBuilder) {
        Self::pin_pty_at(self.path(), command);
    }

    /// Reapply an owned namespace path when a self-healing client respawns.
    pub(crate) fn pin_pty_at(path: &Path, command: &mut CommandBuilder) {
        command.scrub_session_env();
        command.env("XDG_RUNTIME_DIR", path);
        command.env("XDG_STATE_HOME", path);
        command.env("XDG_CONFIG_HOME", path);
        command.env("XDG_CACHE_HOME", path);
        command.env("HOME", path);
        command.env("TMPDIR", path);
        command.env("ZELLIJ_CONFIG_DIR", path.join(".config/zellij"));
    }

    /// Best-effort teardown for an owned session.
    pub fn delete_session(&self, name: &str) {
        let _ = self
            .command()
            .args(["delete-session", name, "--force"])
            .bounded_output();
    }

    fn pin_command_at(path: &Path, command: &mut Command) {
        command
            .env("XDG_RUNTIME_DIR", path)
            .env("XDG_STATE_HOME", path)
            .env("XDG_CONFIG_HOME", path)
            .env("XDG_CACHE_HOME", path)
            .env("HOME", path)
            .env("TMPDIR", path)
            .env("ZELLIJ_CONFIG_DIR", path.join(".config/zellij"));
    }
}

impl Default for ZellijNamespace {
    fn default() -> Self {
        Self::new()
    }
}
