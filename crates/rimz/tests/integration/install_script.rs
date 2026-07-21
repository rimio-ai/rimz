use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn macos_latest_install_delegates_to_homebrew_and_falls_back_on_failure() {
    let temp = TempDir::new().expect("tempdir");
    let brew_root = temp.path().join("homebrew");
    let bin_dir = brew_root.join("bin");
    let formula_prefix = brew_root.join("opt/rimz");
    fs::create_dir_all(&bin_dir).expect("brew bin");

    write_executable(
        &bin_dir.join("uname"),
        "case \"$1\" in\n  -s) printf 'Darwin\\n' ;;\n  -m) printf 'arm64\\n' ;;\nesac\n",
    );
    write_executable(
        &bin_dir.join("brew"),
        r#"case "$1" in
    list)
        [ -f "$RIMZ_TEST_FORMULA_PREFIX/bin/rimz" ]
        ;;
    install|upgrade)
        printf '%s\n' "$*" > "$RIMZ_TEST_BREW_LOG"
        if [ "${RIMZ_TEST_BREW_FAIL:-0}" = 1 ]; then
            printf 'simulated brew failure\n' >&2
            exit 42
        fi
        mkdir -p "$RIMZ_TEST_FORMULA_PREFIX/bin"
        cp "$RIMZ_TEST_RIMZ_FIXTURE" "$RIMZ_TEST_FORMULA_PREFIX/bin/rimz"
        ln -sf "$RIMZ_TEST_FORMULA_PREFIX/bin/rimz" "$RIMZ_TEST_BREW_PREFIX/bin/rimz"
        ;;
    --prefix)
        if [ "${2:-}" = rimz ]; then
            printf '%s\n' "$RIMZ_TEST_FORMULA_PREFIX"
        else
            printf '%s\n' "$RIMZ_TEST_BREW_PREFIX"
        fi
        ;;
esac
"#,
    );
    write_executable(&bin_dir.join("curl"), "exit 43\n");
    let fixture = temp.path().join("rimz-fixture");
    write_executable(&fixture, "printf 'rimz 0.5.0\\n'\n");
    let brew_log = temp.path().join("brew.log");
    let install_script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");

    let run_installer = |brew_fails: bool| {
        let mut command = Command::new("/bin/sh");
        command
            .arg(&install_script)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .env("HOME", temp.path())
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("RIMZ_TEST_BREW_LOG", &brew_log)
            .env("RIMZ_TEST_BREW_PREFIX", &brew_root)
            .env("RIMZ_TEST_FORMULA_PREFIX", &formula_prefix)
            .env("RIMZ_TEST_RIMZ_FIXTURE", &fixture);
        if brew_fails {
            command.env("RIMZ_TEST_BREW_FAIL", "1");
        }
        command.output().expect("run installer")
    };

    let output = run_installer(false);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("utf-8 installer output");
    assert!(stderr.contains("Installing RimZ with Homebrew"));
    assert!(stderr.contains("rimz 0.5.0"));
    assert!(!stderr.contains("Downloading RimZ"));
    assert!(!stderr.contains("Add RimZ to your PATH"));
    assert_eq!(
        fs::read_to_string(&brew_log).expect("brew invocation log"),
        "install rimio-ai/rimz/rimz\n"
    );

    let output = run_installer(false);
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&brew_log).expect("brew upgrade log"),
        "upgrade rimio-ai/rimz/rimz\n"
    );

    fs::remove_file(brew_root.join("bin/rimz")).expect("remove linked rimz");
    fs::remove_dir_all(&formula_prefix).expect("remove formula");
    let output = run_installer(true);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf-8 fallback output");
    assert!(stderr.contains("simulated brew failure"));
    assert!(stderr.contains("Homebrew install failed; falling back to the release download"));
    assert!(stderr.contains("Downloading RimZ latest for aarch64-apple-darwin"));
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}")).expect("write executable");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}
