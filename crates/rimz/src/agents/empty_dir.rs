//! Trust-gated, room-owned working directories for projectless agents.

use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use super::{AgentState, FolderTrust, FolderTrustRow, RoomLoginSet};
use crate::disk::paths::StatePaths;
use crate::ids::AgentKind;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Applies,
    Grant(super::FolderTrustGap),
    Fallback(FolderTrust),
}

#[derive(Debug, thiserror::Error)]
pub enum EmptyDirErr {
    #[error("empty-dir contains `{}`; remove it before launching", .0.display())]
    Dirty(PathBuf),
    #[error("agent name `{0}` is already in use")]
    NameInUse(String),
    #[error("invalid agent name `{0}`")]
    InvalidName(String),
    #[error("not an empty-dir agent directory: `{}`", .0.display())]
    InvalidDirectory(PathBuf),
    #[error("`{}` holds files from an earlier launch; remove it or pick another name", .0.display())]
    Leftover(PathBuf),
    #[error("cannot access `{}`: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct Prepared {
    pub cwd: PathBuf,
    pub name: String,
}

#[derive(Debug)]
pub enum Decision {
    Applied(Prepared),
    Granted {
        key: PathBuf,
        path: PathBuf,
        prepared: Result<Prepared, EmptyDirErr>,
    },
    Fallback(String),
    Skip,
}

/// Decide a fresh launch's cwd only after its account and placement are known.
pub fn decide(
    paths: &StatePaths,
    agents: &[AgentState],
    requested: Option<&str>,
    kind: &AgentKind,
    login: &super::ProviderLogin,
    enabled: bool,
) -> Result<Decision, EmptyDirErr> {
    if !enabled {
        return Ok(Decision::Skip);
    }
    check(paths)?;
    let grant = match settle_trust(
        &paths.empty_dir_root(),
        kind,
        &login.env(&super::ambient_env()),
        super::grant_folder_trust,
    ) {
        Ok(grant) => grant,
        Err(cause) => {
            return Ok(Decision::Fallback(format!(
                "empty-dir ignored: {kind} ({}) {cause}",
                login.name()
            )));
        }
    };
    let prepared = prepare(paths, &agents.iter().collect::<Vec<_>>(), requested);
    match grant {
        Some(gap) => Ok(Decision::Granted {
            key: gap.key,
            path: gap.path,
            prepared,
        }),
        None => prepared.map(Decision::Applied),
    }
}

pub fn verdict(kind: &AgentKind, login_env: &BTreeMap<String, String>) -> Verdict {
    verdict_at(&crate::disk::paths::empty_dir_root(), kind, login_env)
}

fn verdict_at(root: &Path, kind: &AgentKind, login_env: &BTreeMap<String, String>) -> Verdict {
    match trust_at(root, kind, login_env) {
        None | Some(FolderTrust::Decided) => Verdict::Applies,
        Some(FolderTrust::Undecided(gap))
            if gap.grant.is_ok() && gap.key == resolved_root(root) =>
        {
            Verdict::Grant(gap)
        }
        Some(trust) => Verdict::Fallback(trust),
    }
}

fn settle_trust(
    root: &Path,
    kind: &AgentKind,
    env: &BTreeMap<String, String>,
    mut write: impl FnMut(&super::FolderTrustGap) -> Result<(), super::FolderTrustErr>,
) -> Result<Option<super::FolderTrustGap>, String> {
    const GRANT_ATTEMPTS: usize = 3;
    let mut attempts = 0;
    loop {
        let gap = match verdict_at(root, kind, env) {
            Verdict::Applies | Verdict::Fallback(FolderTrust::Decided) => return Ok(None),
            Verdict::Fallback(FolderTrust::Restricted { .. }) => {
                return Err("saved the empty folder as untrusted (see rimz doctor)".into());
            }
            Verdict::Fallback(FolderTrust::Undecided(gap)) => {
                let cause = match gap.grant {
                    Ok(_) => format!(
                        "needs a grant that would also trust `{}`",
                        gap.key.display()
                    ),
                    Err(reason) => format!("cannot write its folder trust: {reason}"),
                };
                return Err(format!("{cause} (see rimz doctor)"));
            }
            Verdict::Grant(gap) => gap,
        };
        if attempts == GRANT_ATTEMPTS {
            return Err(format!(
                "could not grant folder trust: `{}` changed during all {GRANT_ATTEMPTS} attempts",
                gap.path.display()
            ));
        }
        attempts += 1;
        match write(&gap) {
            Ok(()) => return Ok(Some(gap)),
            Err(super::FolderTrustErr::Changed { .. }) => {}
            Err(error) => {
                return Err(format!("could not grant folder trust: {error}"));
            }
        }
    }
}

/// Machine-wide folder-trust rows for detected providers and every declared login.
pub fn rows(logins: &RoomLoginSet) -> Vec<FolderTrustRow> {
    let root = crate::disk::paths::empty_dir_root();
    super::all_definitions()
        .filter(|adapter| super::locate_binary(adapter.spec()).is_some())
        .flat_map(|adapter| {
            let kind = adapter.spec().kind;
            let root = &root;
            logins.declared(kind).into_iter().filter_map(move |login| {
                let trust = trust_at(root, login.kind(), &logins.env(&login))?;
                Some(FolderTrustRow {
                    kind,
                    login: login.name().clone(),
                    trust,
                })
            })
        })
        .collect()
}

fn trust_at(
    root: &Path,
    kind: &AgentKind,
    login_env: &BTreeMap<String, String>,
) -> Option<FolderTrust> {
    let adapter = super::find_definition(kind.as_str())?;
    let root = resolved_root(root);
    // A grant can precede directory creation. Probe Git at the nearest existing ancestor.
    let repo_root = root
        .ancestors()
        .find(|path| path.is_dir())
        .and_then(|path| {
            let output = crate::proc::git_command(path)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        });
    adapter.folder_trust(&root, repo_root.as_deref(), login_env)
}

fn resolved_root(root: &Path) -> PathBuf {
    root.ancestors()
        .find_map(|ancestor| {
            let resolved = ancestor.canonicalize().ok()?;
            let rest = root.strip_prefix(ancestor).ok()?;
            Some(if rest.as_os_str().is_empty() {
                resolved
            } else {
                resolved.join(rest)
            })
        })
        .unwrap_or_else(|| root.into())
}

/// Check both instruction-bearing parent levels; the agent's own contents are its own.
pub fn check(paths: &StatePaths) -> Result<(), EmptyDirErr> {
    check_level(&paths.empty_dir_root())?;
    check_level(&paths.empty_room_dir())
}

/// Check the machine-wide parent without requiring a room.
pub fn check_root() -> Result<(), EmptyDirErr> {
    check_level(&crate::disk::paths::empty_dir_root())
}

fn check_level(path: &Path) -> Result<(), EmptyDirErr> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(EmptyDirErr::Io {
                path: path.into(),
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(EmptyDirErr::Dirty(path.into()));
    }
    for entry in fs::read_dir(path).map_err(|source| EmptyDirErr::Io {
        path: path.into(),
        source,
    })? {
        let entry = entry.map_err(|source| EmptyDirErr::Io {
            path: path.into(),
            source,
        })?;
        let entry_path = entry.path();
        let kind = entry.file_type().map_err(|source| EmptyDirErr::Io {
            path: entry_path.clone(),
            source,
        })?;
        if !kind.is_dir() || entry.file_name().as_encoded_bytes().starts_with(b".") {
            return Err(EmptyDirErr::Dirty(entry_path));
        }
    }
    Ok(())
}

/// Prepare a fresh cwd from retained room rows. Use a requested name as explicit at the mint, otherwise use the returned candidate as soft.
fn prepare(
    paths: &StatePaths,
    agents: &[&AgentState],
    requested: Option<&str>,
) -> Result<Prepared, EmptyDirErr> {
    prepare_parents(paths)?;
    let room = paths.empty_room_dir();
    let resolved_room = room
        .canonicalize()
        .map_err(|source| EmptyDirErr::Io { path: room, source })?;
    let taken = |name: &str| {
        agents.iter().any(|agent| {
            agent.name.as_deref() == Some(name)
                || agent.agent_id.as_str().starts_with(name)
                || agent
                    .worktree_path
                    .as_deref()
                    .map(Path::new)
                    .is_some_and(|path| {
                        path.file_name() == Some(std::ffi::OsStr::new(name))
                            && path
                                .parent()
                                .and_then(|parent| parent.canonicalize().ok())
                                .as_deref()
                                == Some(resolved_room.as_path())
                    })
        })
    };
    let name = match requested {
        Some(name) if !super::petname::valid_agent_name(name) => {
            return Err(EmptyDirErr::InvalidName(name.into()));
        }
        Some(name) if taken(name) => return Err(EmptyDirErr::NameInUse(name.into())),
        Some(name) => name.to_owned(),
        None => super::petname::mint(taken),
    };
    let cwd = paths.empty_agent_dir(&name);
    if requested.is_none() && cwd.exists() {
        fs::remove_dir_all(&cwd).map_err(|source| EmptyDirErr::Io {
            path: cwd.clone(),
            source,
        })?;
    }
    match fs::DirBuilder::new().mode(0o700).create(&cwd) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && reusable(&cwd)? => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(EmptyDirErr::Leftover(cwd));
        }
        Err(source) => return Err(EmptyDirErr::Io { path: cwd, source }),
    }
    Ok(Prepared { cwd, name })
}

/// An explicit name's retry may take over the empty directory its failed launch left behind.
fn reusable(cwd: &Path) -> Result<bool, EmptyDirErr> {
    let io_err = |source| EmptyDirErr::Io {
        path: cwd.to_path_buf(),
        source,
    };
    if !fs::symlink_metadata(cwd).map_err(io_err)?.is_dir() {
        return Ok(false);
    }
    Ok(fs::read_dir(cwd).map_err(io_err)?.next().is_none())
}

/// Recreate a recorded cwd at the same path without clearing any surviving agent files.
pub fn prepare_reentry(paths: &StatePaths, cwd: &Path) -> Result<(), EmptyDirErr> {
    if !cwd
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(super::petname::valid_agent_name)
    {
        return Err(EmptyDirErr::InvalidDirectory(cwd.into()));
    }
    prepare_parents(paths)?;
    let room = paths.empty_room_dir();
    let resolved_room = room
        .canonicalize()
        .map_err(|source| EmptyDirErr::Io { path: room, source })?;
    if cwd
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .as_deref()
        != Some(resolved_room.as_path())
    {
        return Err(EmptyDirErr::InvalidDirectory(cwd.into()));
    }
    match fs::DirBuilder::new().mode(0o700).create(cwd) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => check(paths),
        Err(source) => Err(EmptyDirErr::Io {
            path: cwd.into(),
            source,
        }),
    }
}

fn prepare_parents(paths: &StatePaths) -> Result<(), EmptyDirErr> {
    check(paths)?;
    let room = paths.empty_room_dir();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&room)
        .map_err(|source| EmptyDirErr::Io { path: room, source })?;
    check(paths)
}

#[cfg(test)]
mod tests;
