//! Agent binary and config-file location helpers.
//!
//! Launch, setup, install, and provider-header version probes share this
//! machine-local discovery layer.

use std::path::{Path, PathBuf};

use super::definition::{AgentSpec, BinIdentity};
use super::{AgentErr, Result, version};

pub(crate) fn probe_descriptor_version(
    definition: &AgentSpec,
    parse: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    probe_descriptor_version_with_locator(definition, parse, locate_binary)
}

fn probe_descriptor_version_with_locator(
    definition: &AgentSpec,
    parse: &dyn Fn(&str, &str) -> Option<String>,
    locate: impl FnOnce(&AgentSpec) -> Option<PathBuf>,
) -> Option<String> {
    let binary = locate(definition)?;
    version::probe_cli_version_with(binary, parse)
}

/// Resolve an agent's binary on this machine: `$PATH` first, then the
/// definition's [`extra_bin_dirs`](AgentSpec::extra_bin_dirs) joined under
/// `$HOME`. An installer that drops its binary in a private dir (OpenCode's
/// `~/.opencode/bin`) and edits a shell rc the running environment never sourced
/// leaves the agent off `$PATH` yet present; this finds it. A name another
/// provider's installer also ships — Cursor's `agent` versus Grok's install
/// alias — is confirmed by the definition's
/// [`bin_identity`](AgentSpec::bin_identity) before it is accepted, so a
/// colliding alias never resolves as this agent. Returns the absolute path, or
/// `None` when the binary is nowhere RimZ knows to look.
pub fn locate_binary(definition: &AgentSpec) -> Option<PathBuf> {
    for name in definition.bin_names {
        if let Some(identity) = definition.ambiguous_bin_identity(name) {
            let matched = first_matching_identity(
                which::which_all(name).ok().into_iter().flatten(),
                identity,
            );
            if matched.is_some() {
                return matched;
            }
        } else if let Ok(path) = which::which(name) {
            return Some(path);
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").filter(|value| !value.is_empty())?);
    binary_in_install_dirs(definition, &home)
}

/// The first candidate whose `--version` proves it is this provider. `$PATH` can
/// carry several binaries under an ambiguous name (Cursor's `agent` and Grok's
/// alias both), so every match is probed in order rather than trusting the
/// first-on-`$PATH` hit.
fn first_matching_identity(
    candidates: impl IntoIterator<Item = PathBuf>,
    identity: &BinIdentity,
) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find(|candidate| binary_has_identity(candidate, identity))
}

/// Confirm a candidate is genuinely this provider by running its `--version` and
/// applying the adapter's identity check. A spawn failure, timeout, nonzero
/// exit, or unrecognized banner reads as "not this provider", so a colliding
/// alias is skipped rather than impersonating the agent.
fn binary_has_identity(candidate: &Path, identity: &BinIdentity) -> bool {
    version::probe_cli_version_with(candidate, |stdout, stderr| {
        (identity.verify)(stdout, stderr).then(String::new)
    })
    .is_some()
}

/// The `$PATH`-miss branch of [`locate_binary`], split out so it tests without
/// touching process env: the first existing `<home>/<dir>/<kind>` file across
/// the definition's [`extra_bin_dirs`](AgentSpec::extra_bin_dirs). A candidate
/// matched by an ambiguous name is identity-checked, mirroring the `$PATH` path.
fn binary_in_install_dirs(definition: &AgentSpec, home: &Path) -> Option<PathBuf> {
    definition.extra_bin_dirs.iter().find_map(|dir| {
        definition.bin_names.iter().find_map(|name| {
            let candidate = home.join(dir).join(name);
            if !candidate.is_file() {
                return None;
            }
            match definition.ambiguous_bin_identity(name) {
                Some(identity) => binary_has_identity(&candidate, identity).then_some(candidate),
                None => Some(candidate),
            }
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
    use crate::agents::spec_by_kind;

    #[test]
    fn binary_resolves_from_a_known_install_dir_off_path() {
        let home = tempfile::tempdir().unwrap();
        let opencode = spec_by_kind("opencode").unwrap();
        // Off PATH and not yet installed: nowhere under HOME to find it.
        assert_eq!(binary_in_install_dirs(opencode, home.path()), None);
        // OpenCode's installer drops the binary here without editing PATH.
        let bin_dir = home.path().join(".opencode/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("opencode");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        assert_eq!(binary_in_install_dirs(opencode, home.path()), Some(bin));
        // An agent declaring no install dirs is never found this way.
        let claude = spec_by_kind("claude").unwrap();
        assert_eq!(binary_in_install_dirs(claude, home.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_uses_the_located_install_dir_binary() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let opencode = spec_by_kind("opencode").unwrap();
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
            |definition| binary_in_install_dirs(definition, home.path()),
        );

        assert_eq!(version.as_deref(), Some("selected:1.17.7"));
    }

    #[cfg(unix)]
    #[test]
    fn ambiguous_name_resolves_by_provider_identity_not_first_on_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, banner: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\nprintf '{banner}\\n'\n")).unwrap();
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
            path
        };
        let grok_alias = make("grok-agent", "grok 0.2.106 (bde89716f6) [stable]");
        let cursor_agent = make("cursor-agent-real", "2026.07.17-3e2a980");

        let cursor = spec_by_kind("cursor").unwrap();
        let identity = cursor
            .ambiguous_bin_identity("agent")
            .expect("cursor's `agent` name is ambiguous");

        assert!(!binary_has_identity(&grok_alias, identity));
        assert!(binary_has_identity(&cursor_agent, identity));
        assert_eq!(
            first_matching_identity([grok_alias.clone(), cursor_agent.clone()], identity),
            Some(cursor_agent),
        );
        assert_eq!(first_matching_identity([grok_alias], identity), None);
        assert!(cursor.ambiguous_bin_identity("cursor-agent").is_none());
    }
}
