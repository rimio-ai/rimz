//! Integration coverage for `rimz uninstall`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::common::Env;

struct UninstallFixture {
    env: Env,
    data_root: PathBuf,
    cache_root: PathBuf,
    cargo_home: PathBuf,
    system_bin: PathBuf,
}

impl UninstallFixture {
    fn new() -> Self {
        let env = Env::new();
        let data_root = env.home_root.join("data");
        let cache_root = env.home_root.join("cache");
        let cargo_home = env.home_root.join("cargo");
        let system_bin = env.home_root.join("system-bin");
        for dir in [&data_root, &cache_root, &cargo_home, &system_bin] {
            fs::create_dir_all(dir).expect("mkdir uninstall fixture dir");
        }
        Self {
            env,
            data_root,
            cache_root,
            cargo_home,
            system_bin,
        }
    }

    fn rimz(&self) -> Command {
        let mut cmd = self.env.rimz();
        self.apply_env(&mut cmd);
        cmd
    }

    fn rimz_at(&self, rimz_bin: &Path) -> Command {
        let mut cmd = self.env.rimz_at(rimz_bin);
        self.apply_env(&mut cmd);
        cmd
    }

    fn apply_env(&self, cmd: &mut Command) {
        cmd.env("XDG_DATA_HOME", &self.data_root)
            .env("XDG_CACHE_HOME", &self.cache_root)
            .env("CARGO_HOME", &self.cargo_home)
            .env("RIMZ_SYSTEM_BIN_DIR", &self.system_bin);
    }

    fn root(&self, kind: RootKind) -> PathBuf {
        match kind {
            RootKind::State => self.env.state_root().join("rimz"),
            RootKind::Runtime => self.env.runtime_root.join("rimz"),
            RootKind::Data => self.data_root.join("rimz"),
            RootKind::Cache => self.cache_root.join("rimz"),
            RootKind::Config => self.env.config_root().join("rimz"),
        }
    }

    fn seed_roots(&self) {
        for kind in RootKind::ALL {
            let root = self.root(kind);
            fs::create_dir_all(&root).expect("mkdir root");
            fs::write(root.join("marker"), kind.label()).expect("write marker");
        }
    }

    fn assert_present(&self, kind: RootKind) {
        assert!(
            self.root(kind).exists(),
            "{} root should remain",
            kind.label()
        );
    }

    fn assert_absent(&self, kind: RootKind) {
        assert!(
            !self.root(kind).exists(),
            "{} root should be removed",
            kind.label()
        );
    }
}

#[derive(Clone, Copy)]
enum RootKind {
    State,
    Runtime,
    Data,
    Cache,
    Config,
}

impl RootKind {
    const ALL: [Self; 5] = [
        Self::State,
        Self::Runtime,
        Self::Data,
        Self::Cache,
        Self::Config,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Runtime => "runtime",
            Self::Data => "data",
            Self::Cache => "cache",
            Self::Config => "config",
        }
    }
}

#[test]
fn bare_uninstall_removes_runtime_cache_data_and_keeps_state_config() {
    let fixture = UninstallFixture::new();
    fixture.seed_roots();

    let output = fixture
        .rimz()
        .args(["uninstall", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "uninstall writes no stdout");
    fixture.assert_absent(RootKind::Runtime);
    fixture.assert_absent(RootKind::Cache);
    fixture.assert_absent(RootKind::Data);
    fixture.assert_present(RootKind::State);
    fixture.assert_present(RootKind::Config);
}

#[test]
fn uninstall_all_removes_all_user_roots() {
    let fixture = UninstallFixture::new();
    fixture.seed_roots();

    let output = fixture
        .rimz()
        .args(["uninstall", "--all", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for kind in RootKind::ALL {
        fixture.assert_absent(kind);
    }
}

#[test]
fn uninstall_state_and_config_flags_extend_default_scope_independently() {
    let state_only = UninstallFixture::new();
    state_only.seed_roots();
    let output = state_only
        .rimz()
        .args(["uninstall", "--state", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall --state");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    state_only.assert_absent(RootKind::State);
    state_only.assert_present(RootKind::Config);

    let config_only = UninstallFixture::new();
    config_only.seed_roots();
    let output = config_only
        .rimz()
        .args(["uninstall", "--config", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall --config");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    config_only.assert_present(RootKind::State);
    config_only.assert_absent(RootKind::Config);
}

#[test]
fn uninstall_requires_yes_without_tty_and_removes_nothing() {
    let fixture = UninstallFixture::new();
    fixture.seed_roots();

    let output = fixture
        .rimz()
        .args(["uninstall", "--keep-binary"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn uninstall");

    assert!(!output.status.success(), "uninstall should require --yes");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pass --yes"), "stderr:\n{stderr}");
    for kind in RootKind::ALL {
        fixture.assert_present(kind);
    }
}

#[test]
fn uninstall_removes_managed_hooks() {
    let fixture = UninstallFixture::new();
    fixture.env.install_agent_hooks("claude");
    assert!(fixture.env.agent_hooks_installed("claude"));

    let output = fixture
        .rimz()
        .args(["uninstall", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.env.agent_hooks_installed("claude"));
}

#[cfg(unix)]
#[test]
fn uninstall_removes_current_cargo_and_system_binaries() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = UninstallFixture::new();
    let runner_dir = tempfile::tempdir().expect("runner dir");
    let current = runner_dir.path().join("rimz");
    fs::copy(fixture.env.rimz_bin(), &current).expect("copy current rimz");
    let mut perms = fs::metadata(&current)
        .expect("current metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&current, perms).expect("chmod current");

    let cargo_bin = fixture.cargo_home.join("bin");
    fs::create_dir_all(&cargo_bin).expect("mkdir cargo bin");
    let cargo_copy = cargo_bin.join("rimz");
    hard_link_or_copy(&current, &cargo_copy).expect("link cargo rimz");
    let system_copy = fixture.system_bin.join("rimz");
    hard_link_or_copy(&current, &system_copy).expect("link system rimz");

    let output = fixture
        .rimz_at(&current)
        .args(["uninstall", "--yes"])
        .output()
        .expect("spawn copied uninstall");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!current.exists(), "running copy should be removed");
    assert!(!cargo_copy.exists(), "cargo bin copy should be removed");
    assert!(!system_copy.exists(), "system bin copy should be removed");
}

#[cfg(unix)]
fn hard_link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::hard_link(from, to).or_else(|_| fs::copy(from, to).map(|_| ()))
}

#[test]
fn uninstall_previews_project_local_rimz_dirs_and_leaves_them() {
    let fixture = UninstallFixture::new();
    fixture.env.record(&fixture.env.project_root);
    fixture
        .env
        .write_config(&fixture.env.project_root, "trusted = true\n");
    let project_rimz = fixture.env.project_root.join(".rimz");

    let output = fixture
        .rimz()
        .args(["uninstall", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&project_rimz.display().to_string()),
        "preview should name project .rimz dir:\n{stderr}"
    );
    assert!(project_rimz.exists(), "project .rimz dir should survive");
}
