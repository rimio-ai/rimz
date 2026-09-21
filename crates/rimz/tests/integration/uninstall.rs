//! Integration coverage for `rimz uninstall`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::common::Env;

struct UninstallFixture {
    env: Env,
    cargo_home: PathBuf,
    system_bin: PathBuf,
}

impl UninstallFixture {
    fn new() -> Self {
        let env = Env::new();
        let cargo_home = env.home_root.join("cargo");
        let system_bin = env.home_root.join("system-bin");
        for dir in [&cargo_home, &system_bin] {
            fs::create_dir_all(dir).expect("mkdir uninstall fixture dir");
        }
        Self {
            env,
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
        cmd.env("CARGO_HOME", &self.cargo_home)
            .env("RIMZ_SYSTEM_BIN_DIR", &self.system_bin);
    }

    /// The directory holding one category of RimZ files inside the home, or
    /// the runtime tree.
    fn root(&self, kind: RootKind) -> PathBuf {
        let home = self.env.rimz_home();
        match kind {
            RootKind::State => home.join("ws"),
            RootKind::Runtime => self.env.runtime_root.join("rimz"),
            RootKind::Cache => home.join("cache/providers"),
            RootKind::Config => home.join("trust"),
        }
    }

    fn seed_roots(&self) {
        for kind in RootKind::ALL {
            let root = self.root(kind);
            fs::create_dir_all(&root).expect("mkdir root");
            fs::write(root.join("marker"), kind.label()).expect("write marker");
        }
        for name in LIBRARY {
            let dir = self.env.agents_home().join(name);
            fs::create_dir_all(&dir).expect("mkdir library");
            fs::write(dir.join("marker"), name).expect("write library marker");
        }
    }

    fn assert_present(&self, kind: RootKind) {
        assert!(
            self.root(kind).join("marker").exists(),
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
        for name in LIBRARY {
            assert_eq!(
                fs::read_to_string(self.env.agents_home().join(name).join("marker")).unwrap(),
                name
            );
        }
    }
}

const LIBRARY: [&str; 6] = [
    "agents",
    "subagents",
    "teams",
    "traits",
    "skills",
    "accounts",
];

#[derive(Clone, Copy)]
enum RootKind {
    State,
    Runtime,
    Cache,
    Config,
}

impl RootKind {
    const ALL: [Self; 4] = [Self::State, Self::Runtime, Self::Cache, Self::Config];

    fn label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Runtime => "runtime",
            Self::Cache => "cache",
            Self::Config => "config",
        }
    }
}

#[test]
fn bare_uninstall_removes_runtime_and_cache_and_keeps_state_config() {
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Uninstalling RimZ..."), "{stderr}");
    assert_eq!(stderr.matches("Skill links: none").count(), 2, "{stderr}");
    fixture.assert_absent(RootKind::Runtime);
    fixture.assert_absent(RootKind::Cache);
    fixture.assert_present(RootKind::State);
    fixture.assert_present(RootKind::Config);
}

#[test]
fn uninstall_all_removes_user_roots_but_keeps_agent_library() {
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    for name in LIBRARY {
        assert!(
            stderr.contains(&format!(
                "kept {}",
                config_only.env.agents_home().join(name).display()
            )),
            "{stderr}"
        );
    }
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

#[cfg(unix)]
#[test]
fn uninstall_previews_and_removes_only_owned_skill_links_in_every_mode() {
    use std::os::unix::fs::symlink;

    for flags in [vec![], vec!["--state"], vec!["--config"], vec!["--all"]] {
        let fixture = UninstallFixture::new();
        fixture.seed_roots();
        let library_home = fixture.env.home_root.join("custom-library");
        let library = library_home.join("skills");
        fs::create_dir_all(library.join("shared")).unwrap();
        fs::write(library.join("shared/SKILL.md"), "shared skill").unwrap();
        let account_home = fixture.env.rimz_home().join("accounts/claude/work");
        fs::write(
            fixture.env.rimz_home().join("config.toml"),
            format!("[accounts.claude.work]\nhome = {:?}\n", account_home),
        )
        .unwrap();
        let roots = [
            fixture.env.home_root.join(".agents/skills"),
            fixture.env.home_root.join(".claude/skills"),
            account_home.join("skills"),
        ];
        let foreign = fixture.env.home_root.join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        for root in &roots {
            fs::create_dir_all(root.join("mine")).unwrap();
            fs::write(root.join("mine/SKILL.md"), "my skill").unwrap();
            symlink(library.join("shared"), root.join("shared")).unwrap();
            symlink(library.join("gone"), root.join("stale")).unwrap();
            symlink(&foreign, root.join("foreign")).unwrap();
        }

        let preview = fixture
            .rimz()
            .env("RIMZ_AGENTS_HOME", &library_home)
            .args(["uninstall", "--keep-binary"])
            .args(&flags)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&preview.stderr);
        assert!(!preview.status.success(), "{stderr}");
        assert!(stderr.contains("pass --yes"), "{stderr}");
        assert!(stderr.contains("Skill links:\n"), "{stderr}");
        for root in &roots {
            assert!(
                stderr.contains(&format!("  {}\n", root.display())),
                "{stderr}"
            );
            assert_eq!(
                fs::read_link(root.join("shared")).unwrap(),
                library.join("shared")
            );
            assert!(fs::symlink_metadata(root.join("stale")).is_ok());
        }

        let output = fixture
            .rimz()
            .env("RIMZ_AGENTS_HOME", &library_home)
            .args(["uninstall", "--yes", "--keep-binary"])
            .args(&flags)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(
            stderr.matches("Skill links: removed 2 from ").count(),
            roots.len(),
            "{stderr}"
        );
        for root in &roots {
            assert!(
                stderr.contains(&format!("Skill links: removed 2 from {}", root.display())),
                "{stderr}"
            );
            assert!(fs::symlink_metadata(root.join("shared")).is_err());
            assert!(fs::symlink_metadata(root.join("stale")).is_err());
            assert_eq!(
                fs::read_to_string(root.join("mine/SKILL.md")).unwrap(),
                "my skill"
            );
            assert_eq!(fs::read_link(root.join("foreign")).unwrap(), foreign);
        }
        assert!(library.join("shared/SKILL.md").is_file());
    }
}

#[test]
fn uninstall_removes_managed_hooks() {
    let fixture = UninstallFixture::new();
    fixture.env.install_agent_hooks("claude");
    assert!(fixture.env.agent_hooks_installed("claude"));
    let added = fixture
        .rimz()
        .args(["accounts", "add", "claude", "work"])
        .output()
        .expect("spawn accounts add");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let accounts = fixture.env.rimz_home().join("accounts");
    let work_home = accounts.join("claude/work");
    let work_settings = work_home.join("settings.json");
    let hooked = fs::read_to_string(&work_settings).expect("work settings");
    assert!(hooked.contains("rimz"), "{hooked}");
    fs::write(work_home.join(".credentials.json"), b"{}").expect("seed credentials");

    let output = fixture
        .rimz()
        .args(["uninstall", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(!fixture.env.agent_hooks_installed("claude"));
    assert!(
        stderr.contains("Hooks: removed claude, claude@work"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("kept {}", accounts.display())),
        "{stderr}"
    );
    assert!(
        stderr.contains("Accounts: kept; their credentials and history stay on disk"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("clear by hand: rm -rf {}", work_home.display())),
        "{stderr}"
    );
    assert!(work_home.join(".credentials.json").is_file());
    let unhooked = fs::read_to_string(&work_settings).expect("work settings kept");
    assert!(!unhooked.contains("rimz"), "{unhooked}");
}

#[test]
fn uninstall_unhooks_native_homes_when_the_accounts_config_is_refused() {
    let fixture = UninstallFixture::new();
    fixture.env.install_agent_hooks("claude");
    let config = fixture.env.rimz_home();
    fs::create_dir_all(&config).expect("config root");
    fs::write(
        config.join("config.toml"),
        "[accounts.claude.a]\nhome = \"/srv/shared\"\n[accounts.claude.b]\nhome = \"/srv/shared\"\n",
    )
    .expect("seed refused accounts config");

    let root = fixture.env.home_root.join(".agents/skills");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(
        fixture.env.agents_home().join("skills/gone"),
        root.join("gone"),
    )
    .unwrap();

    let output = fixture
        .rimz()
        .args(["uninstall", "--yes", "--keep-binary"])
        .output()
        .expect("spawn uninstall");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(!fixture.env.agent_hooks_installed("claude"), "{stderr}");
    assert!(stderr.contains("Hooks: removed claude"), "{stderr}");
    assert!(
        stderr.contains(&format!("Skill links: removed 1 from {}", root.display())),
        "{stderr}"
    );
    assert!(fs::symlink_metadata(root.join("gone")).is_err());
    assert!(
        stderr.contains("read provider accounts: `accounts.claude.b.home`"),
        "{stderr}"
    );
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
