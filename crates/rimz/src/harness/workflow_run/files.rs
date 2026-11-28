//! The sealed copies a run reads: the run's own script, sealed once per files generation at the
//! start or a resume, and one per `workflow` call, published by the reader the first time a pass
//! meets the call, with the refusal in its place when it cannot be read, so every later pass of
//! that generation reads the same body or the same refusal.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Result, anyhow};

use super::Room;
use crate::harness::block::WorkflowSource;
use crate::harness::workflow_script::{Called, Calling, Reader};
use crate::store::workflow_run::{self as store, Origin, SealFrom, Sealed};

/// The script of the run's own copy, and the directory its `file` reads.
pub(super) struct Root {
    pub text: String,
    pub dir: Option<PathBuf>,
}

/// The directory of files generation `files` under `home`.
pub(super) fn generation(home: &Path, files: u32) -> PathBuf {
    home.join("files").join(files.to_string())
}

/// The copy a command step made by the script `parent` names (the run's own for none) is given.
pub(super) fn copy_of(home: &Path, files: u32, parent: Option<u64>) -> PathBuf {
    let key = parent.map_or_else(|| "root".to_owned(), |number| number.to_string());
    store::copy_dir(&generation(home, files).join(key))
}

/// Seal the run's own script from `origin` into the generation directory `at`.
pub(super) fn seal_root(
    at: &Path,
    origin: &Origin,
    checkout: &Path,
    room: &dyn Room,
) -> Result<()> {
    let dir = at.join("root");
    let (from, origin_dir, directory) = match origin {
        Origin::Script(text) => (Some(Source::Text(text.clone())), None, false),
        Origin::Name(name) => {
            let found = room
                .installed(name)
                .ok_or_else(|| anyhow!("no installed workflow is named `{name}`"))?;
            (Some(Source::Dir(found.clone())), Some(found), true)
        }
        Origin::Path(path) => {
            let path = checkout.join(path);
            if path.is_dir() {
                (Some(Source::Dir(path.clone())), Some(path), true)
            } else if path.is_file() {
                let parent = path.parent().map(Path::to_path_buf);
                (Some(Source::File(path)), parent, false)
            } else {
                return Err(anyhow!(
                    "{} is no workflow file or directory",
                    path.display()
                ));
            }
        }
    };
    let outcome = Sealed {
        name: "workflow.js".to_owned(),
        origin_dir,
        directory,
        refusal: None,
    };
    seal(&dir, &outcome, from.as_ref())?;
    Ok(())
}

/// The run's own script in the generation directory `at`.
pub(super) fn root(at: &Path) -> Result<Root> {
    let dir = at.join("root");
    let sealed = store::sealed(&dir)?.ok_or_else(|| {
        anyhow!(
            "the run's copy of its script at {} is missing",
            dir.display()
        )
    })?;
    let copy = store::copy_dir(&dir);
    let text = std::fs::read_to_string(copy.join("workflow.js"))
        .map_err(|error| anyhow!("reading {}: {error}", copy.join("workflow.js").display()))?;
    Ok(Root {
        text,
        dir: sealed.directory.then_some(copy),
    })
}

/// Refuse a script that does not load, by one pass that follows no `workflow` call.
pub(super) fn check_loads(at: &Path) -> Result<()> {
    use crate::harness::workflow_script::{Caps, Log, Pass, replay};
    let root = root(at)?;
    let log = Log::Answers(Vec::new());
    replay(&Pass {
        name: "workflow.js",
        text: &root.text,
        args: None,
        log: &log,
        caps: Caps::PASS,
        dir: root.dir.as_deref(),
        reader: None,
        covered: None,
        steps: None,
    })
    .map(drop)
    .map_err(|error| anyhow!("the workflow's script does not load: {error}"))
}

/// The reader a pass over the generation directory `at` hands its script engine, and the error
/// that, once set, discards the pass: a copy that could not be read for a reason a later pass may
/// not meet.
pub(super) fn reader(
    at: &Path,
    room: &Rc<dyn Room>,
) -> (Rc<Reader>, Rc<RefCell<Option<anyhow::Error>>>) {
    let at = at.to_path_buf();
    let room = Rc::clone(room);
    let broken = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&broken);
    let reader = move |calling: &Calling<'_>| {
        let dir = at.join(calling.number.to_string());
        let sealed = match store::sealed(&dir) {
            Ok(Some(sealed)) => Ok(sealed),
            Ok(None) => seal_call(&at, &dir, &*room, calling),
            Err(error) => Err(error.into()),
        };
        let sealed = match sealed {
            Ok(sealed) => sealed,
            Err(error) => {
                let reason = format!("{error:#}");
                *slot.borrow_mut() = Some(error);
                return Err(reason);
            }
        };
        if let Some(refusal) = sealed.refusal {
            return Err(refusal);
        }
        let copy = store::copy_dir(&dir);
        let script = copy.join("workflow.js");
        // The copy is sealed, so what it holds is the call's answer on every pass.
        let text = match std::fs::read_to_string(&script) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let origin = sealed
                    .origin_dir
                    .as_deref()
                    .unwrap_or(Path::new(calling.value));
                return Err(format!("{} holds no `workflow.js`", origin.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(format!(
                    "the script of `{}` is not UTF-8 text",
                    calling.value
                ));
            }
            Err(error) => {
                let reason = format!("reading {}: {error}", script.display());
                *slot.borrow_mut() = Some(anyhow!(reason.clone()));
                return Err(reason);
            }
        };
        Ok(Called {
            name: format!("workflow-{}.js", calling.number),
            text,
            dir: sealed.directory.then_some(copy),
        })
    };
    (Rc::new(reader), broken)
}

enum Source {
    Dir(PathBuf),
    File(PathBuf),
    Text(String),
}

/// Read the script `calling` names and publish its copy, or its refusal, at `dir`.
fn seal_call(at: &Path, dir: &Path, room: &dyn Room, calling: &Calling<'_>) -> Result<Sealed> {
    let refused = |refusal: String| Sealed {
        name: "workflow.js".to_owned(),
        origin_dir: None,
        directory: false,
        refusal: Some(refusal),
    };
    let calling_dir = match calling.parent {
        Some(parent) => store::sealed(&at.join(parent.to_string()))?,
        None => store::sealed(&at.join("root"))?,
    }
    .and_then(|sealed| sealed.origin_dir);
    let value = calling.value;
    let (outcome, from) = match calling.source {
        WorkflowSource::Script => (
            Sealed {
                name: "workflow.js".to_owned(),
                origin_dir: None,
                directory: false,
                refusal: None,
            },
            Some(Source::Text(value.to_owned())),
        ),
        WorkflowSource::Name => match room.installed(value) {
            Some(found) => (
                Sealed {
                    name: "workflow.js".to_owned(),
                    origin_dir: Some(found.clone()),
                    directory: true,
                    refusal: None,
                },
                Some(Source::Dir(found)),
            ),
            None => (
                refused(format!("no installed workflow is named `{value}`")),
                None,
            ),
        },
        WorkflowSource::Path | WorkflowSource::Run => {
            let path = Path::new(value);
            let path = match (path.is_absolute(), &calling_dir) {
                (true, _) => Some(path.to_path_buf()),
                (false, Some(base)) => Some(base.join(path)),
                (false, None) => None,
            };
            match path {
                None => (
                    refused(format!(
                        "`{value}` is a relative path, and the script that calls it has no directory to read it against"
                    )),
                    None,
                ),
                Some(path) if path.is_dir() => (
                    Sealed {
                        name: "workflow.js".to_owned(),
                        origin_dir: Some(path.clone()),
                        directory: true,
                        refusal: None,
                    },
                    Some(Source::Dir(path)),
                ),
                Some(path) if path.is_file() => (
                    Sealed {
                        name: "workflow.js".to_owned(),
                        origin_dir: path.parent().map(Path::to_path_buf),
                        directory: false,
                        refusal: None,
                    },
                    Some(Source::File(path)),
                ),
                Some(path) => (
                    refused(format!(
                        "{} is no workflow file or directory",
                        path.display()
                    )),
                    None,
                ),
            }
        }
    };
    // A link out of the workflow, or a source gone or closed to the run, is the call's refusal;
    // any other error is one a later pass may not meet.
    let refusal = match seal(dir, &outcome, from.as_ref()) {
        Err(error @ store::WorkflowRunStoreErr::Link { .. }) => error.to_string(),
        Err(store::WorkflowRunStoreErr::Io { path, source })
            if !path.starts_with(at)
                && matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
        {
            format!("reading {}: {source}", path.display())
        }
        sealed => return Ok(sealed?),
    };
    Ok(seal(dir, &refused(refusal), None)?)
}

fn seal(dir: &Path, outcome: &Sealed, from: Option<&Source>) -> store::Result<Sealed> {
    let from = match from {
        Some(Source::Dir(path)) => SealFrom::Dir(path),
        Some(Source::File(path)) => SealFrom::File(path),
        Some(Source::Text(text)) => SealFrom::Text(text),
        None => SealFrom::Refused,
    };
    store::seal(dir, outcome, from)
}
