//! Wrap recognized cargo commands through `rtk` so Rimz-launched agents see
//! compressed output. Gated by `RIMZ_RTK` from `[harness] rtk`; absent means a
//! human run and leaves cargo untouched.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Once;

/// Cargo subcommands rtk recognizes and compresses.
const RTK_SUBCOMMANDS: &[&str] = &["build", "check", "test", "nextest", "clippy"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Off,
    Auto,
    On,
}

fn mode() -> Mode {
    mode_from_env(std::env::var("RIMZ_RTK").ok().as_deref())
}

fn mode_from_env(raw: Option<&str>) -> Mode {
    match raw {
        Some("auto") => Mode::Auto,
        Some("on") => Mode::On,
        _ => Mode::Off,
    }
}

/// True when `cargo <args>` should run as `rtk cargo <args>`.
pub(crate) fn wrap_cargo<S: AsRef<OsStr>>(program: &str, args: &[S]) -> bool {
    if program != "cargo" {
        return false;
    }
    let Some(subcommand) = args.first() else {
        return false;
    };
    if !is_rtk_subcommand(subcommand.as_ref()) {
        return false;
    }
    decide(mode(), rtk_on_path)
}

fn is_rtk_subcommand(subcommand: &OsStr) -> bool {
    RTK_SUBCOMMANDS
        .iter()
        .any(|name| OsStr::new(name) == subcommand)
}

fn decide(mode: Mode, rtk_present: impl Fn() -> bool) -> bool {
    match mode {
        Mode::Off => false,
        Mode::Auto => rtk_present(),
        Mode::On if rtk_present() => true,
        Mode::On => {
            warn_forced_but_missing();
            false
        }
    }
}

fn rtk_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join("rtk")))
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
    reason = "xtask surfaces the rtk misconfig to the operator's stderr"
)]
fn warn_forced_but_missing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "xtask: [harness] rtk = \"on\" but `rtk` is not on PATH; running cargo without compression"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_tracks_mode_and_rtk_presence() {
        assert!(!decide(Mode::Off, || true));
        assert!(!decide(Mode::Off, || false));
        assert!(decide(Mode::Auto, || true));
        assert!(!decide(Mode::Auto, || false));
        assert!(decide(Mode::On, || true));
        assert!(!decide(Mode::On, || false));
    }

    #[test]
    fn mode_from_env_accepts_known_values_only() {
        assert_eq!(mode_from_env(Some("auto")), Mode::Auto);
        assert_eq!(mode_from_env(Some("on")), Mode::On);
        assert_eq!(mode_from_env(Some("off")), Mode::Off);
        assert_eq!(mode_from_env(Some("always")), Mode::Off);
        assert_eq!(mode_from_env(None), Mode::Off);
    }

    #[test]
    fn subcommand_filter_matches_rtk_recognized_commands() {
        for subcommand in ["build", "check", "test", "nextest", "clippy"] {
            assert!(is_rtk_subcommand(OsStr::new(subcommand)), "{subcommand}");
        }
        for subcommand in ["fmt", "llvm-cov", "deny", "machete", "zigbuild"] {
            assert!(!is_rtk_subcommand(OsStr::new(subcommand)), "{subcommand}");
        }
    }
}
