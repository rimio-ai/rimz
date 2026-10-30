use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rimz::packs::{InstallSource, Record, transaction::Transaction};

use crate::common::git::{git, git_missing, git_stdout};
use crate::common::{Env, path_with_front};

#[test]
fn update_refreshes_homebrew_before_upgrading_the_tapped_formula() {
    let fixture = HomebrewUpdateFixture::new(false);

    let output = fixture.run();

    assert!(
        output.status.success(),
        "rimz update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.log(), "update\nupgrade rimio-ai/rimz/rimz\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Updating RimZ with Homebrew…\n"
    );
}

#[test]
fn update_stops_when_homebrew_refresh_fails() {
    if git_missing() {
        return;
    }
    let fixture = HomebrewUpdateFixture::new(true);
    let repo = fixture.install_pack("following", None);
    fs::write(repo.join("README.md"), "new commit").unwrap();
    publish(&repo);
    let record_path = fixture.env.agents_home().join("packs/installed.json");
    let before = fs::read(&record_path).unwrap();
    let checkouts = fixture.env.agents_home().join("packs/.checkouts");
    let before_mtime = fs::metadata(&checkouts).unwrap().modified().unwrap();

    let output = fixture.run();

    assert!(
        !output.status.success(),
        "rimz update unexpectedly succeeded: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("`brew update` exited with exit status: 42"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.log(), "update\n");
    assert_eq!(fs::read(record_path).unwrap(), before);
    assert_eq!(
        fs::metadata(checkouts).unwrap().modified().unwrap(),
        before_mtime
    );
}

#[test]
fn update_moves_following_packs_and_skips_pins_even_when_the_binary_is_current() {
    if git_missing() {
        return;
    }
    let fixture = HomebrewUpdateFixture::new(false);
    let following = fixture.install_pack("following", None);
    let pinned = fixture.install_pack("pinned", Some("main"));
    let before = fixture.record();
    for repo in [&following, &pinned] {
        fs::write(repo.join("README.md"), "new commit").unwrap();
        publish(repo);
    }
    let output = fixture.run();
    assert!(output.status.success(), "{output:?}");
    let after = fixture.record();
    assert_eq!(
        after.sources[0].commit,
        Some(git_stdout(&following, &["rev-parse", "HEAD"]))
    );
    assert_ne!(after.sources[0].commit, before.sources[0].commit);
    assert_eq!(after.sources[1], before.sources[1]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Skipped pinned: pinned at main"),
        "{output:?}"
    );
}

#[test]
fn update_runs_reload_then_pack_updates_through_the_active_binary() {
    if git_missing() {
        return;
    }
    let mut fixture = HomebrewUpdateFixture::new(false);
    fixture.install_pack("following", None);
    fixture.formula_prefix = fixture.env.home_root.join("active");
    fs::create_dir_all(fixture.formula_prefix.join("bin")).unwrap();
    write_executable(
        &fixture.formula_prefix.join("bin/rimz"),
        r#"printf '%s\n' "$*" >> "$RIMZ_TEST_ACTIVE_LOG"
[ "$1" != reload ] || exit 0
exec "$RIMZ_TEST_REAL_BIN" "$@"
"#,
    );
    let output = fixture.run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.env.home_root.join("active.log")).unwrap(),
        "reload\npacks update\n"
    );
}

#[test]
fn update_holds_widening_packs_without_a_terminal_and_names_the_retry() {
    if git_missing() {
        return;
    }
    let fixture = HomebrewUpdateFixture::new(false);
    let repo = fixture.install_pack("following", None);
    let record_path = fixture.env.agents_home().join("packs/installed.json");
    let before = fs::read(&record_path).unwrap();
    let profile = repo.join("agents/following-worker.md");
    let text = fs::read_to_string(&profile).unwrap();
    fs::write(profile, text.replace("mode: ask", "mode: yolo")).unwrap();
    publish(&repo);
    let output = fixture.run();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "RimZ itself is updated or current, but pack updates did not complete; retry with `rimz packs update`"
        ),
        "{output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Held following"),
        "{output:?}"
    );
    assert_eq!(fs::read(record_path).unwrap(), before);
}

fn publish(repo: &Path) {
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "fixture update"]);
}

struct HomebrewUpdateFixture {
    env: Env,
    rimz: PathBuf,
    brew_bin: PathBuf,
    brew_log: PathBuf,
    formula_prefix: PathBuf,
    fail_update: bool,
}

impl HomebrewUpdateFixture {
    fn new(fail_update: bool) -> Self {
        let env = Env::new();
        let rimz = env.home_root.join("Cellar/rimz/0.4/bin/rimz");
        fs::create_dir_all(rimz.parent().expect("Cellar bin parent")).expect("create Cellar bin");
        fs::copy(env.rimz_bin(), &rimz).expect("copy rimz into Cellar");

        let brew_bin = env.home_root.join("brew-bin");
        fs::create_dir_all(&brew_bin).expect("create brew bin");
        write_executable(
            &brew_bin.join("brew"),
            r#"case "$1" in
    update)
        printf '%s\n' "$*" >> "$RIMZ_TEST_BREW_LOG"
        if [ "${RIMZ_TEST_BREW_UPDATE_FAIL:-0}" = 1 ]; then
            exit 42
        fi
        ;;
    upgrade)
        printf '%s\n' "$*" >> "$RIMZ_TEST_BREW_LOG"
        ;;
    --prefix)
        [ "${2:-}" = rimz ] || exit 43
        printf '%s\n' "$RIMZ_TEST_FORMULA_PREFIX"
        ;;
    *)
        exit 44
        ;;
esac
"#,
        );

        let brew_log = env.home_root.join("brew.log");
        let formula_prefix = rimz
            .parent()
            .and_then(Path::parent)
            .expect("formula prefix")
            .to_path_buf();

        Self {
            env,
            rimz,
            brew_bin,
            brew_log,
            formula_prefix,
            fail_update,
        }
    }

    fn run(&self) -> std::process::Output {
        let mut command = self.env.rimz_at(&self.rimz);
        command
            .arg("update")
            .env("PATH", path_with_front(&self.brew_bin))
            .env("RIMZ_TEST_BREW_LOG", &self.brew_log)
            .env("RIMZ_TEST_FORMULA_PREFIX", &self.formula_prefix)
            .env(
                "RIMZ_TEST_ACTIVE_LOG",
                self.env.home_root.join("active.log"),
            )
            .env("RIMZ_TEST_REAL_BIN", self.env.rimz_bin());
        if self.fail_update {
            command.env("RIMZ_TEST_BREW_UPDATE_FAIL", "1");
        }
        command.output().expect("run rimz update")
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.brew_log).expect("read brew log")
    }

    fn record(&self) -> Record {
        Record::read(&self.env.agents_home()).unwrap()
    }

    fn install_pack(&self, name: &str, reference: Option<&str>) -> PathBuf {
        let repo = self.env.home_root.join("sources").join(name);
        fs::create_dir_all(repo.join("agents")).unwrap();
        fs::create_dir_all(self.env.agents_home().join("agents")).unwrap();
        fs::write(
            self.env.agents_home().join("agents/claude.md"),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
        fs::write(
            repo.join("rimz-packs.toml"),
            format!("[[pack]]\nname = {name:?}\ndescription = 'Test'\npath = '.'\n"),
        )
        .unwrap();
        fs::write(
            repo.join("agents").join(format!("{name}-worker.md")),
            "---\ndescription: Worker\nagent: claude\nmode: ask\ntools: [Read]\n---\nWork.",
        )
        .unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Pack Test"]);
        git(&repo, &["config", "user.email", "packs@example.test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        publish(&repo);
        let url = url::Url::from_directory_path(&repo).unwrap();
        let InstallSource::Git(mut source) = url.as_str().parse().unwrap() else {
            panic!("file URL must be a Git source");
        };
        source.reference = reference.map(str::to_owned);
        Transaction::stage(&self.env.agents_home(), &source)
            .unwrap()
            .commit()
            .unwrap();
        repo
    }
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}")).expect("write executable");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}
