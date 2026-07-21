use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
}

#[test]
fn update_stops_when_homebrew_refresh_fails() {
    let fixture = HomebrewUpdateFixture::new(true);

    let output = fixture.run();

    assert!(
        !output.status.success(),
        "rimz update unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("`brew update` exited with exit status: 42"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.log(), "update\n");
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
            .env("RIMZ_TEST_FORMULA_PREFIX", &self.formula_prefix);
        if self.fail_update {
            command.env("RIMZ_TEST_BREW_UPDATE_FAIL", "1");
        }
        command.output().expect("run rimz update")
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.brew_log).expect("read brew log")
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
