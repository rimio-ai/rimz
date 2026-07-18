//! Live regression for process reaping across isolated Zellij domains.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rimz::workspace::WorkspaceResolver;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, ScrubSessionEnvExt, cargo_bin};

use super::support::SPAWN_TIMEOUT;

#[test]
fn reload_spares_same_workspace_sidebars_in_another_environment_domain() {
    require_zellij!();

    let project = TempDir::new().expect("shared project tempdir");
    let workspace = WorkspaceResolver::resolve(project.path(), None).expect("resolve workspace");
    let domain_a = ZellijDomain::new(project.path());
    let domain_b = ZellijDomain::new(project.path());

    domain_b.start();
    let foreign_sidebars = wait_for_sidebars(
        &workspace.workspace_id,
        &workspace.session_name,
        &domain_b.state_home,
    );
    assert!(
        !foreign_sidebars.is_empty(),
        "foreign domain must have a live sidebar before the regression action",
    );

    domain_a.start();
    let output = domain_a
        .rimz()
        .arg("reload")
        .env("RIMZ_TEST_SKIP_STATS_RELOAD", "1")
        .bounded_output_within(SPAWN_TIMEOUT)
        .expect("reload first environment domain");
    assert!(
        output.status.success(),
        "reload failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let dead = foreign_sidebars
        .iter()
        .copied()
        .filter(|pid| !rimz::proc::process_is_live(*pid, None))
        .collect::<Vec<_>>();
    assert!(
        dead.is_empty(),
        "reload in one domain killed foreign sidebar processes {dead:?}",
    );
}

fn wait_for_sidebars(
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    state_home: &Path,
) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pids = rimz::proc::list_processes()
            .into_iter()
            .filter(|process| {
                process.cmdline.contains("sidebar")
                    && process.cmdline.contains("serve")
                    && process.cmdline.contains(workspace_id.as_str())
                    && process.cmdline.contains(session_name)
            })
            .filter(|process| {
                rimz::proc::environ(process.pid).is_some_and(|environment| {
                    environment.iter().any(|(key, value)| {
                        key == "XDG_STATE_HOME" && Path::new(value) == state_home
                    })
                })
            })
            .map(|process| process.pid)
            .collect::<Vec<_>>();
        if !pids.is_empty() || Instant::now() >= deadline {
            return pids;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct ZellijDomain {
    _root: TempDir,
    project: PathBuf,
    home: PathBuf,
    state_home: PathBuf,
    runtime_home: PathBuf,
    config_home: PathBuf,
    cache_home: PathBuf,
    data_home: PathBuf,
    tmpdir: PathBuf,
    tmux_tmpdir: PathBuf,
    session_name: String,
}

impl ZellijDomain {
    fn new(project: &Path) -> Self {
        let root = tempfile::Builder::new()
            .prefix("rzd")
            .tempdir_in("/tmp")
            .expect("domain tempdir");
        let home = root.path().join("home");
        let state_home = root.path().join("state");
        let runtime_home = root.path().join("runtime");
        let config_home = root.path().join("config");
        let cache_home = root.path().join("cache");
        let data_home = root.path().join("data");
        let tmpdir = root.path().join("tmp");
        let tmux_tmpdir = root.path().join("tmux");
        for path in [
            &home,
            &state_home,
            &runtime_home,
            &config_home,
            &cache_home,
            &data_home,
            &tmpdir,
            &tmux_tmpdir,
        ] {
            std::fs::create_dir_all(path)
                .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
        }
        let zellij_config = config_home.join("zellij");
        std::fs::create_dir_all(&zellij_config).expect("create Zellij config directory");
        std::fs::write(
            zellij_config.join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\n",
        )
        .expect("write Zellij config");
        let session_name = WorkspaceResolver::resolve(project, None)
            .expect("resolve domain workspace")
            .session_name;
        Self {
            _root: root,
            project: project.to_path_buf(),
            home,
            state_home,
            runtime_home,
            config_home,
            cache_home,
            data_home,
            tmpdir,
            tmux_tmpdir,
            session_name,
        }
    }

    fn start(&self) {
        let output = self
            .rimz()
            .args(["--mux", "zellij", "start", "--no-attach"])
            .bounded_output_within(SPAWN_TIMEOUT)
            .expect("start isolated Zellij room");
        assert!(
            output.status.success(),
            "Zellij room start failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn rimz(&self) -> Command {
        let mut command = Command::new(cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")));
        self.apply_env(&mut command);
        command.current_dir(&self.project);
        command
    }

    fn zellij(&self) -> Command {
        let mut command = Command::new("zellij");
        self.apply_env(&mut command);
        command
    }

    fn apply_env(&self, command: &mut Command) {
        command
            .scrub_session_env()
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_CACHE_HOME", &self.cache_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("ZELLIJ_CONFIG_DIR", self.config_home.join("zellij"))
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env("TMPDIR", &self.tmpdir)
            .env("SHELL", "/bin/sh")
            .env_remove("RUST_LOG");
    }
}

impl Drop for ZellijDomain {
    fn drop(&mut self) {
        let _ = self
            .zellij()
            .args(["delete-session", &self.session_name, "--force"])
            .bounded_output_within(Duration::from_secs(5));
    }
}
