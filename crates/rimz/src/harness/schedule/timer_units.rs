//! Where `rimz loop timer install` writes the operating system's timer, and whether it is
//! installed: a file check, so a booking asks it without starting a process. Whether an
//! installed timer is active asks `systemctl` or `launchctl`, and stays the CLI's.

use std::path::PathBuf;

use crate::disk::paths::{config_home, env_path};

const SYSTEMD_SERVICE: &str = "rimz-loop.service";
pub const SYSTEMD_TIMER: &str = "rimz-loop.timer";
pub const LAUNCHD_LABEL: &str = "ai.rimz.loop";

pub struct SystemdPaths {
    pub service: PathBuf,
    pub timer: PathBuf,
}

pub fn systemd_paths() -> SystemdPaths {
    let root = config_home().join("systemd/user");
    SystemdPaths {
        service: root.join(SYSTEMD_SERVICE),
        timer: root.join(SYSTEMD_TIMER),
    }
}

pub fn launchd_path() -> PathBuf {
    env_path("HOME")
        .unwrap_or_else(std::env::temp_dir)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// Whether this platform's timer is installed: launchd's plist on macOS, systemd's timer unit
/// elsewhere, which `rimz loop timer install` writes after its service.
pub fn installed() -> bool {
    if cfg!(target_os = "macos") {
        launchd_path().exists()
    } else {
        systemd_paths().timer.exists()
    }
}
