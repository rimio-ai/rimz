//! Bubblewrap availability and mount-namespace admission probe.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{SandboxDiagnostic, SandboxErr};

const PROBE_ARGV: &[&str] = &[
    "--bind",
    "/",
    "/",
    "--dev-bind",
    "/dev",
    "/dev",
    "--die-with-parent",
    "--",
    "/usr/bin/true",
];

pub(super) fn probe() -> Result<PathBuf, SandboxErr> {
    let path = which::which("bwrap").map_err(|_| SandboxErr::MissingBwrap)?;
    probe_at(&path)?;
    Ok(path)
}

fn probe_at(path: &std::path::Path) -> Result<(), SandboxErr> {
    let output = Command::new(path)
        .args(PROBE_ARGV)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| SandboxErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(SandboxErr::ProbeFailed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(1200)
                .collect(),
        });
    }
    Ok(())
}

pub(super) fn diagnose() -> SandboxDiagnostic {
    let path = match which::which("bwrap") {
        Ok(path) => path,
        Err(_) => {
            return SandboxDiagnostic {
                path: None,
                version: None,
                error: Some(SandboxErr::MissingBwrap.to_string()),
            };
        }
    };
    let version = Command::new(&path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let error = probe_at(&path).err().map(|err| err.to_string());
    SandboxDiagnostic {
        path: Some(path),
        version,
        error,
    }
}
