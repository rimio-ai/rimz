//! Route recognized cargo compile commands through `sccache` when available.
//! Local runs opt in by installing `sccache`; CI supplies a GHA-backed cache.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Once;

/// Cargo subcommands that compile Rust code and can use `RUSTC_WRAPPER`.
const SCCACHE_SUBCOMMANDS: &[&str] = &[
    "bench", "build", "check", "clippy", "llvm-cov", "nextest", "test",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Off,
    Auto,
    On,
}

fn mode() -> Mode {
    mode_from_env(std::env::var("RIMZ_SCCACHE").ok().as_deref())
}

fn mode_from_env(raw: Option<&str>) -> Mode {
    match raw {
        Some("off") => Mode::Off,
        Some("on") => Mode::On,
        _ => Mode::Auto,
    }
}

/// True when `cargo <args>` should compile through `RUSTC_WRAPPER=sccache`.
pub(crate) fn should_wrap<S: AsRef<OsStr>>(program: &str, args: &[S]) -> bool {
    if program != "cargo" {
        return false;
    }
    let Some(subcommand) = args.first() else {
        return false;
    };
    if !is_sccache_subcommand(subcommand.as_ref()) {
        return false;
    }
    decide(mode(), sccache_on_path)
}

fn is_sccache_subcommand(subcommand: &OsStr) -> bool {
    SCCACHE_SUBCOMMANDS
        .iter()
        .any(|name| OsStr::new(name) == subcommand)
}

fn decide(mode: Mode, sccache_present: impl Fn() -> bool) -> bool {
    match mode {
        Mode::Off => false,
        Mode::Auto => sccache_present(),
        Mode::On if sccache_present() => true,
        Mode::On => {
            warn_forced_but_missing();
            false
        }
    }
}

fn sccache_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join("sccache")))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[expect(
    clippy::print_stderr,
    reason = "xtask surfaces the sccache misconfig to the operator's stderr"
)]
fn warn_forced_but_missing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "xtask: RIMZ_SCCACHE=on but `sccache` is not on PATH; running cargo without sccache"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_tracks_mode_and_sccache_presence() {
        assert!(!decide(Mode::Off, || true));
        assert!(!decide(Mode::Off, || false));
        assert!(decide(Mode::Auto, || true));
        assert!(!decide(Mode::Auto, || false));
        assert!(decide(Mode::On, || true));
        assert!(!decide(Mode::On, || false));
    }

    #[test]
    fn mode_from_env_defaults_to_auto() {
        assert_eq!(mode_from_env(Some("off")), Mode::Off);
        assert_eq!(mode_from_env(Some("on")), Mode::On);
        assert_eq!(mode_from_env(Some("auto")), Mode::Auto);
        assert_eq!(mode_from_env(Some("always")), Mode::Auto);
        assert_eq!(mode_from_env(None), Mode::Auto);
    }

    #[test]
    fn subcommand_filter_matches_compile_commands() {
        for subcommand in [
            "bench", "build", "check", "clippy", "llvm-cov", "nextest", "test",
        ] {
            assert!(
                is_sccache_subcommand(OsStr::new(subcommand)),
                "{subcommand}"
            );
        }
        for subcommand in ["deny", "fmt", "machete", "semver-checks", "vet"] {
            assert!(
                !is_sccache_subcommand(OsStr::new(subcommand)),
                "{subcommand}"
            );
        }
    }
}
