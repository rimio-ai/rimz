//! Write-side registration of a staged team's room-state archive and checkout link.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::scratch::{self, TEAM_LINK};
use crate::disk::{atomic, lock::WorkspaceLock};
use crate::{Store, config::Team};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Fresh,
    Resume,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterErr {
    #[error(
        "cannot link team notes at {0}: move the existing file or directory aside, then relaunch or resume the team"
    )]
    Occupied(PathBuf),
    #[error(
        "cannot link team notes at {0}: .rimz is a symlink; seed .rimz through .worktreeinclude instead of .worktreelink, or replace the link with a directory"
    )]
    SharedRimz(PathBuf),
    #[error(
        "cannot link team notes at {0}: this is the RimZ home; launch the team from a project checkout"
    )]
    RimzHome(PathBuf),
    #[error(
        "cannot register team notes at {path}: {source}; fix the path and relaunch or resume the team"
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error("cannot register team notes: {0}; fix the path and relaunch or resume the team")]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Worktree(#[from] crate::worktree::WorktreeErr),
}

pub fn register(
    store: &Store,
    checkout: &Path,
    name: &str,
    team: &Team,
    launch: Launch,
) -> Result<Option<PathBuf>, RegisterErr> {
    let mut patterns = team.scratch_patterns();
    if !team.staged() {
        crate::worktree::exclude_team_scratch(checkout, &patterns);
        return Ok(None);
    }
    let checkout = checkout.canonicalize().map_err(|source| RegisterErr::Io {
        path: checkout.to_owned(),
        source,
    })?;
    let _lock = WorkspaceLock::acquire(&store.runtime_paths().board_lock(&checkout))?;
    let link = checkout.join(TEAM_LINK);
    let rimz = checkout.join(".rimz");
    match fs::symlink_metadata(&rimz) {
        Ok(metadata) if metadata.is_symlink() => return Err(RegisterErr::SharedRimz(rimz)),
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(RegisterErr::Io { path: rimz, source }),
    }
    if crate::disk::paths::holds_rimz_home(&checkout, store.paths().home()) {
        return Err(RegisterErr::RimzHome(rimz));
    }
    match fs::symlink_metadata(&link) {
        Ok(metadata) if !metadata.is_symlink() => return Err(RegisterErr::Occupied(link)),
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(RegisterErr::Io { path: link, source }),
    }
    let existing = scratch::run_dir(store.paths(), &checkout)
        .filter(|run| scratch::run_team(run).as_deref() == Some(name));
    let done = if existing.is_some() && launch == Launch::Fresh {
        let board = scratch::board_path(&checkout);
        match fs::read_to_string(&board) {
            Ok(text) => scratch::parse_board_stage(&text)
                .is_some_and(|stage| stage.name == crate::config::DONE_STAGE),
            Err(source) if source.kind() == io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(RegisterErr::Io {
                    path: board,
                    source,
                });
            }
        }
    } else {
        false
    };
    let reuse = existing.filter(|_| !done);
    let run = match reuse {
        Some(run) => run,
        None => {
            let restored = if launch == Launch::Resume {
                latest_run(store, &checkout, name)?
            } else {
                None
            };
            let run = match restored {
                Some(run) => run
                    .canonicalize()
                    .map_err(|source| RegisterErr::Io { path: run, source })?,
                None => create_run(store, &checkout, name)?,
            };
            atomic::symlink_atomically(&run, &link)?;
            atomic::sync_dir(&checkout)?;
            run
        }
    };
    patterns.push(format!("/{TEAM_LINK}"));
    let root = crate::worktree::discover_checkout(&checkout).unwrap_or_else(|error| {
        tracing::warn!(%error, "resolving team notes Git exclude root");
        None
    });
    if let Some(root) = root
        && let Ok(prefix) = checkout.strip_prefix(root)
        && !prefix.as_os_str().is_empty()
    {
        let mut pattern = String::from("/");
        for ch in prefix.to_string_lossy().chars() {
            if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        pattern.push('/');
        pattern.push_str(TEAM_LINK);
        patterns.push(pattern);
    }
    crate::worktree::exclude_team_scratch(&checkout, &patterns);
    Ok(Some(run))
}

pub fn is_open(run: &Path) -> bool {
    scratch::board_is_open(&run.join(super::board::BOARD_FILE))
}

pub(crate) fn migrate(paths: &crate::StatePaths) -> Result<(), RegisterErr> {
    use crate::store::notes::{self, Kind, Manifest};

    let _lock = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let folders = notes::enumerate(&paths.team_notes_dir).map_err(|source| RegisterErr::Io {
        path: paths.team_notes_dir.clone(),
        source,
    })?;
    for run in folders.runs {
        if folders.dates.iter().any(|date| run.starts_with(date)) {
            continue;
        }
        let Ok(manifest) = Manifest::read(&run) else {
            continue;
        };
        if !matches!(manifest.kind, Kind::Team { .. }) {
            continue;
        }
        let old = run.canonicalize().map_err(|source| RegisterErr::Io {
            path: run.clone(),
            source,
        })?;
        let link = manifest.checkout.join(TEAM_LINK);
        let relink = fs::symlink_metadata(&link).is_ok_and(|metadata| metadata.is_symlink())
            && link.canonicalize().is_ok_and(|target| target == old);
        let moved = notes::migrate_legacy(paths, &run)
            .map_err(|source| RegisterErr::Io { path: run, source })?;
        if relink && let Some(moved) = moved {
            let target = moved.canonicalize().map_err(|source| RegisterErr::Io {
                path: moved,
                source,
            })?;
            atomic::symlink_atomically(&target, &link)?;
            atomic::sync_dir(&manifest.checkout)?;
        }
    }
    Ok(())
}

fn latest_run(store: &Store, checkout: &Path, team: &str) -> Result<Option<PathBuf>, RegisterErr> {
    let archive = &store.paths().team_notes_dir;
    let folders = crate::store::notes::enumerate(archive).map_err(|source| RegisterErr::Io {
        path: archive.clone(),
        source,
    })?;
    let mut latest = None;
    for path in folders.runs {
        if let Ok(manifest) = crate::store::notes::Manifest::read(&path)
            && manifest.checkout == checkout
            && matches!(manifest.kind, crate::store::notes::Kind::Team { team: ref owner, .. } if owner == team)
        {
            latest = latest.max(Some((manifest.created_at, path)));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn create_run(store: &Store, checkout: &Path, name: &str) -> Result<PathBuf, RegisterErr> {
    use crate::store::notes::{self, Kind, Worktree};

    let worktree =
        crate::worktree::read_marker_from_checkout_metadata(checkout)?.map(|marker| Worktree {
            name: marker.name,
            branch: marker.branch,
            base_branch: marker.base_branch,
            from_pr: marker.from_pr,
            repo_root: marker.repo_root,
        });
    let relative = notes::allocate(
        store.paths(),
        Kind::Team {
            team: name.to_owned(),
            worktree,
        },
        checkout,
        jiff::Timestamp::now(),
    )
    .map_err(|source| RegisterErr::Io {
        path: store.paths().team_notes_dir.clone(),
        source,
    })?;
    let run = store
        .paths()
        .notes_run(crate::disk::paths::Class::TeamNotes, &relative);
    run.canonicalize()
        .map_err(|source| RegisterErr::Io { path: run, source })
}

#[cfg(test)]
mod tests;
