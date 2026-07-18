//! Agent binary and config-file location helpers.
//!
//! Launch, setup, install, and provider-header version probes share this
//! machine-local discovery layer.

use std::path::{Path, PathBuf};

use super::descriptor::AgentDescriptor;
use super::{AgentErr, Result, version};

pub(crate) fn probe_descriptor_version(
    descriptor: &AgentDescriptor,
    parse: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    probe_descriptor_version_with_locator(descriptor, parse, locate_binary)
}

fn probe_descriptor_version_with_locator(
    descriptor: &AgentDescriptor,
    parse: &dyn Fn(&str, &str) -> Option<String>,
    locate: impl FnOnce(&AgentDescriptor) -> Option<PathBuf>,
) -> Option<String> {
    let binary = locate(descriptor)?;
    version::probe_cli_version_with(binary, parse)
}

/// Resolve an agent's binary on this machine: `$PATH` first, then the
/// descriptor's [`extra_bin_dirs`](AgentDescriptor::extra_bin_dirs) joined under
/// `$HOME`. An installer that drops its binary in a private dir (OpenCode's
/// `~/.opencode/bin`) and edits a shell rc the running environment never sourced
/// leaves the agent off `$PATH` yet present; this finds it. Returns the absolute
/// path, or `None` when the binary is nowhere RimZ knows to look.
pub fn locate_binary(descriptor: &AgentDescriptor) -> Option<PathBuf> {
    for name in descriptor.bin_names {
        if let Ok(path) = which::which(name) {
            return Some(path);
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").filter(|value| !value.is_empty())?);
    binary_in_install_dirs(descriptor, &home)
}

/// The `$PATH`-miss branch of [`locate_binary`], split out so it tests without
/// touching process env: the first existing `<home>/<dir>/<kind>` file across
/// the descriptor's [`extra_bin_dirs`](AgentDescriptor::extra_bin_dirs).
fn binary_in_install_dirs(descriptor: &AgentDescriptor, home: &Path) -> Option<PathBuf> {
    descriptor.extra_bin_dirs.iter().find_map(|dir| {
        descriptor.bin_names.iter().find_map(|name| {
            let candidate = home.join(dir).join(name);
            candidate.is_file().then_some(candidate)
        })
    })
}

/// Resolve an agent's per-user config file path. An explicit `override_env`
/// value wins (so tests and tooling can point at a tempdir); otherwise the path
/// is `$HOME` joined with `rel`. Returns an `Install` error naming the agent
/// when `$HOME` is unset.
pub(crate) fn agent_config_path(
    agent: &'static str,
    override_env: &str,
    rel: &Path,
) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(override_env).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AgentErr::Install {
            agent,
            reason: format!("$HOME is not set; cannot resolve ~/{}", rel.display()),
        })?;
    Ok(home.join(rel))
}

/// Read an agent config file's current contents for install preview and
/// uninstall. A missing file reads as `None`; any other IO error propagates
/// with agent + path context so the user sees which adapter failed and where.
pub(crate) fn read_optional_file(agent: &'static str, path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent,
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::descriptor_by_kind;

    #[test]
    fn binary_resolves_from_a_known_install_dir_off_path() {
        let home = tempfile::tempdir().unwrap();
        let opencode = descriptor_by_kind("opencode").unwrap();
        // Off PATH and not yet installed: nowhere under HOME to find it.
        assert_eq!(binary_in_install_dirs(opencode, home.path()), None);
        // OpenCode's installer drops the binary here without editing PATH.
        let bin_dir = home.path().join(".opencode/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("opencode");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        assert_eq!(binary_in_install_dirs(opencode, home.path()), Some(bin));
        // An agent declaring no install dirs is never found this way.
        let claude = descriptor_by_kind("claude").unwrap();
        assert_eq!(binary_in_install_dirs(claude, home.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_uses_the_located_install_dir_binary() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let opencode = descriptor_by_kind("opencode").unwrap();
        let bin_dir = home.path().join(".opencode/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("opencode");
        std::fs::write(&bin, b"#!/bin/sh\nprintf 'opencode 1.17.7\\n'\n").unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        let version = probe_descriptor_version_with_locator(
            opencode,
            &|stdout, stderr| {
                let version = version::conventional_cli_version(stdout, stderr)?;
                Some(format!("selected:{version}"))
            },
            |descriptor| binary_in_install_dirs(descriptor, home.path()),
        );

        assert_eq!(version.as_deref(), Some("selected:1.17.7"));
    }
}
