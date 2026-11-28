//! The record of a run of a workflow script and the files beside it, at
//! `owned/workflow-runs/<id>/`: `record.json`, the only truth; `answers.<generation>.jsonl`, the
//! answer log; `journal.jsonl`, evidence nothing routes on; `files/<generation>/`, the sealed
//! copies of the scripts the run reads; `steps/<number>/`, a command step's directory; and
//! `results/`, the results too large for a line.
//!
//! The record holds the length of the log and of the journal, so a line past it does not count:
//! [`commit`] is the one writer of all three, under the workspace lock, and cuts a file back to the
//! length the record holds before it appends. The module owns its wire types and imports nothing
//! from `harness`: a block is stored by its name, and a call's object, a result, and a failure as
//! the JSON the harness types serialize to. Transitions belong to the harness.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::WorkflowRunPaths;
use crate::ids::WorkflowRunId;

/// The record's own shape version; [`load`] refuses a record of any other before decoding it.
pub const VERSION: u32 = 1;

/// A result whose JSON is longer than this is a file under the home, and its line names it.
pub const LINE_RESULT_CAP: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WorkflowRunStoreErr {
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error("workflow run I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workflow run JSON at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "workflow run at {path} has record version {version}, written by another RimZ version; \
         this RimZ reads only version {current}",
        current = VERSION
    )]
    Version { path: PathBuf, version: u64 },
    #[error("{path} holds {found} bytes, fewer than the {recorded} its run's record covers")]
    Short {
        path: PathBuf,
        recorded: u64,
        found: u64,
    },
    #[error("{link} links outside the workflow's directory, so the run does not copy it")]
    Link { link: PathBuf },
}

pub type Result<T> = std::result::Result<T, WorkflowRunStoreErr>;

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> WorkflowRunStoreErr + '_ {
    move |source| WorkflowRunStoreErr::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn json_err(path: &Path) -> impl FnOnce(serde_json::Error) -> WorkflowRunStoreErr + '_ {
    move |source| WorkflowRunStoreErr::Json {
        path: path.to_path_buf(),
        source,
    }
}

/// What a run runs: an installed workflow's name, a directory or file, or a script's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Name(String),
    Path(PathBuf),
    Script(String),
}

/// A process, by its id and its start token where the platform has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    /// How many launches the run may make, restarts included.
    pub launches: u32,
    /// How many calls the script may make.
    pub steps: u64,
    /// How many of the run's agent and team steps may hold a launch at once.
    pub parallel: u32,
    /// When the run fails `timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
}

/// Where the answer log is: its generation, and the bytes and lines the record covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogCursor {
    pub generation: u32,
    pub length: u64,
    pub lines: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub id: WorkflowRunId,
    pub origin: Origin,
    pub checkout: PathBuf,
    /// The run's `args`, a `null` kept apart from none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub args: Option<Value>,
    /// The version of the engine that started the run.
    pub engine: u32,
    pub bounds: Bounds,
    /// When the room's holds of a launch give up, for a step no `workflow` call sets its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub give_up: Option<Timestamp>,
    #[serde(default)]
    pub skip_capacity: bool,
    /// Every published start of an agent or team step, restarts included.
    pub launches: u32,
    /// The generation of the sealed copies.
    pub files: u32,
    pub log: LogCursor,
    /// The journal's length in bytes.
    pub journal: u64,
    /// How many of the log's lines the last script pass covered; none before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered: Option<usize>,
    pub open: BTreeMap<u64, OpenStep>,
    /// Work to stop, kept until a stop succeeds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed: Vec<Stop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
    pub created_at: Timestamp,
}

/// A field that is present, `null` included.
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// A call the log does not answer yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenStep {
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    pub block: String,
    /// The call's object as the script passed it.
    pub object: Value,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub summary: String,
    pub opened: Timestamp,
    /// How many times the step has been started.
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    pub work: Work,
}

/// What an open step waits on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Work {
    /// Nothing started: a first start, or a restart, is due.
    Due,
    /// A start the run's parallel cap or the room holds.
    Held {
        since: Timestamp,
        hold: Hold,
    },
    /// A start in progress by `starter`, which alone may start the work.
    Starting {
        starter: Process,
    },
    /// A command under its host, in `dir`, given `files`.
    Command {
        host: Process,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<Process>,
        dir: PathBuf,
        files: PathBuf,
    },
    /// A launch, by its launch ids, and an agent's handle.
    Launch {
        launches: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
    },
    Watch,
    /// A called workflow's body, with the `give_up` and `skip_capacity` the call set.
    Workflow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        give_up: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skip_capacity: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Hold {
    /// The run's own parallel cap.
    Parallel,
    /// The room, for this reason.
    Room { reason: String },
}

/// Work a run owes a stop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stop {
    Launches { launches: Vec<String> },
    Process { process: Process },
    Group { process: Process },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ended {
    pub ending: Ending,
    pub at: Timestamp,
    /// The steps open at the end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Ending {
    Done {
        result: Value,
    },
    Failed {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u64>,
        /// The failure that ended the run, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<Value>,
    },
    Canceled,
}

/// How a call settled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Settled {
    Result(Value),
    Failure(Value),
}

/// One line of the answer log.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswerLine {
    pub number: u64,
    pub block: String,
    pub digest: String,
    pub phase: Option<String>,
    pub summary: String,
    pub at: Timestamp,
    pub settled: Settled,
}

#[derive(Serialize, Deserialize)]
struct WireLine {
    number: u64,
    block: String,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    summary: String,
    at: Timestamp,
    #[serde(flatten)]
    outcome: WireOutcome,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireOutcome {
    Result(Value),
    ResultFile(PathBuf),
    Failure(Value),
}

/// One journal entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: Timestamp,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The script's stage from here, after `after` calls.
    Phase {
        after: u64,
        title: String,
    },
    Log {
        after: u64,
        text: String,
    },
    /// A step's work started, at an attempt.
    Start {
        step: u64,
        attempt: u32,
    },
    Stop {
        stop: Stop,
    },
    End {
        ending: Ending,
    },
}

/// What a sealed copy is: the script's file name, the directory a relative path it names is read
/// against, whether its origin was a directory, or the sentence that refused it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_dir: Option<PathBuf>,
    #[serde(default)]
    pub directory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// What a copy is sealed from.
pub enum SealFrom<'a> {
    Dir(&'a Path),
    File(&'a Path),
    Text(&'a str),
    Refused,
}

/// The sealed outcome under `dir` and the directory of its copy.
pub fn copy_dir(dir: &Path) -> PathBuf {
    dir.join("workflow")
}

/// The record at `paths`, if the run has one.
pub fn load(paths: &WorkflowRunPaths) -> Result<Option<Record>> {
    let bytes = match std::fs::read(&paths.record) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_err(&paths.record)(source)),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(json_err(&paths.record))?;
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version != u64::from(VERSION) {
        return Err(WorkflowRunStoreErr::Version {
            path: paths.record.clone(),
            version,
        });
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(json_err(&paths.record))
}

/// Publish a run's first record by one rename of a staged home `staged`, which holds its first
/// copy; refused when a home of that id exists.
pub fn create(
    staged: &Path,
    paths: &WorkflowRunPaths,
    record: &Record,
    _guard: &WorkspaceLock,
) -> Result<()> {
    crate::disk::atomic::write_temp_then_rename(&staged.join("record.json"), record)?;
    crate::disk::atomic::sync_tree(staged)?;
    Ok(crate::disk::atomic::publish_staged_no_replace(
        staged,
        &paths.home,
    )?)
}

/// The one writer of a run's record, log, and journal. `on_disk` is the record read under
/// `_guard`: the log of `next`'s generation and the journal are cut back to the lengths it holds
/// (refused when shorter), `answers` and `entries` are appended and synced, and `next` replaces
/// the record with the new lengths. A log of a generation other than `on_disk`'s was written
/// whole by [`write_log`], and `next` names its cursor.
pub fn commit(
    paths: &WorkflowRunPaths,
    on_disk: &Record,
    mut next: Record,
    answers: &[AnswerLine],
    entries: &[Entry],
    _guard: &WorkspaceLock,
) -> Result<Record> {
    if next.log.generation == on_disk.log.generation {
        let log = paths.answers(next.log.generation);
        let bytes = encode_lines(paths, next.log.generation, answers)?;
        next.log.length = append_fitted(&log, on_disk.log.length, &bytes)?;
        next.log.lines = on_disk.log.lines + answers.len();
    }
    let mut journal = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut journal, entry).map_err(json_err(&paths.journal))?;
        journal.push(b'\n');
    }
    next.journal = append_fitted(&paths.journal, on_disk.journal, &journal)?;
    crate::disk::atomic::write_temp_then_rename(&paths.record, &next)?;
    Ok(next)
}

/// Write a whole new log of `generation` holding `lines`, replacing any file of that generation
/// (one the record does not name yet), its large results included, and give its cursor.
pub fn write_log(
    paths: &WorkflowRunPaths,
    generation: u32,
    lines: &[AnswerLine],
    _guard: &WorkspaceLock,
) -> Result<LogCursor> {
    let prefix = format!("{generation}.");
    for entry in read_dir(&paths.results())? {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let path = entry.path();
            std::fs::remove_file(&path).map_err(io_err(&path))?;
        }
    }
    let bytes = encode_lines(paths, generation, lines)?;
    crate::disk::atomic::write_bytes_atomically(&paths.answers(generation), &bytes)?;
    Ok(LogCursor {
        generation,
        length: bytes.len() as u64,
        lines: lines.len(),
    })
}

/// Remove every log, large result, and files generation `record` does not name.
pub fn remove_stale(
    paths: &WorkflowRunPaths,
    record: &Record,
    _guard: &WorkspaceLock,
) -> Result<()> {
    for entry in read_dir(&paths.home)? {
        let name = entry.file_name();
        let Some(generation) = name
            .to_str()
            .and_then(|name| name.strip_prefix("answers."))
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if generation != record.log.generation {
            let path = entry.path();
            std::fs::remove_file(&path).map_err(io_err(&path))?;
        }
    }
    let generation = format!("{}.", record.log.generation);
    for entry in read_dir(&paths.results())? {
        if !entry.file_name().to_string_lossy().starts_with(&generation) {
            let path = entry.path();
            std::fs::remove_file(&path).map_err(io_err(&path))?;
        }
    }
    let files = paths.home.join("files");
    for entry in read_dir(&files)? {
        if entry.file_name().to_str() != Some(record.files.to_string().as_str()) {
            let path = entry.path();
            std::fs::remove_dir_all(&path).map_err(io_err(&path))?;
        }
    }
    Ok(())
}

fn read_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.collect::<io::Result<Vec<_>>>().map_err(io_err(dir)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(io_err(dir)(source)),
    }
}

/// The log's lines the record covers, in the order they arrived, each with its whole outcome.
pub fn answers(paths: &WorkflowRunPaths, record: &Record) -> Result<Vec<AnswerLine>> {
    let path = paths.answers(record.log.generation);
    let bytes = covered(&path, record.log.length)?;
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| {
            let wire: WireLine = serde_json::from_slice(line).map_err(json_err(&path))?;
            let settled = match wire.outcome {
                WireOutcome::Result(value) => Settled::Result(value),
                WireOutcome::Failure(value) => Settled::Failure(value),
                WireOutcome::ResultFile(file) => {
                    let bytes = std::fs::read(&file).map_err(io_err(&file))?;
                    Settled::Result(serde_json::from_slice(&bytes).map_err(json_err(&file))?)
                }
            };
            Ok(AnswerLine {
                number: wire.number,
                block: wire.block,
                digest: wire.digest,
                phase: wire.phase,
                summary: wire.summary,
                at: wire.at,
                settled,
            })
        })
        .collect()
}

/// The journal's entries the record covers.
pub fn journal(paths: &WorkflowRunPaths, record: &Record) -> Result<Vec<Entry>> {
    let bytes = covered(&paths.journal, record.journal)?;
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| serde_json::from_slice(line).map_err(json_err(&paths.journal)))
        .collect()
}

fn covered(path: &Path, length: u64) -> Result<Vec<u8>> {
    let mut bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io_err(path)(source)),
    };
    if (bytes.len() as u64) < length {
        return Err(WorkflowRunStoreErr::Short {
            path: path.to_path_buf(),
            recorded: length,
            found: bytes.len() as u64,
        });
    }
    // The length fits in memory: the file it covers was just read whole.
    bytes.truncate(length as usize);
    Ok(bytes)
}

fn encode_lines(
    paths: &WorkflowRunPaths,
    generation: u32,
    lines: &[AnswerLine],
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for line in lines {
        let outcome = match &line.settled {
            Settled::Failure(value) => WireOutcome::Failure(value.clone()),
            Settled::Result(value) => {
                let text = serde_json::to_vec(value).map_err(json_err(&paths.home))?;
                if text.len() <= LINE_RESULT_CAP {
                    WireOutcome::Result(value.clone())
                } else {
                    let dir = paths.results();
                    crate::disk::atomic::create_dir_all_mode(&dir, 0o700)?;
                    let file = dir.join(format!("{generation}.{}.json", line.number));
                    crate::disk::atomic::write_bytes_atomically(&file, &text)?;
                    WireOutcome::ResultFile(file)
                }
            }
        };
        let wire = WireLine {
            number: line.number,
            block: line.block.clone(),
            digest: line.digest.clone(),
            phase: line.phase.clone(),
            summary: line.summary.clone(),
            at: line.at,
            outcome,
        };
        serde_json::to_writer(&mut bytes, &wire).map_err(json_err(&paths.home))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Cut `path` back to `recorded` bytes, refusing a shorter file, append `bytes`, sync, and
/// give the new length.
fn append_fitted(path: &Path, recorded: u64, bytes: &[u8]) -> Result<u64> {
    let found = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(source) => return Err(io_err(path)(source)),
    };
    if found < recorded {
        return Err(WorkflowRunStoreErr::Short {
            path: path.to_path_buf(),
            recorded,
            found,
        });
    }
    if found > recorded {
        crate::disk::atomic::truncate_file(path, recorded)?;
    }
    if bytes.is_empty() {
        return Ok(recorded);
    }
    crate::disk::atomic::append_record_bytes(path, bytes)?;
    crate::disk::atomic::sync_file_data(path)?;
    Ok(recorded + bytes.len() as u64)
}

/// The sealed outcome under `dir`, if one was published.
pub fn sealed(dir: &Path) -> Result<Option<Sealed>> {
    let path = dir.join("read.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(json_err(&path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_err(&path)(source)),
    }
}

/// Seal `from` at `dir`, whole or not at all: `read.json` holds `outcome`, and `workflow/`
/// the copy, `workflow.js` for a file or text. A copy published first by another process stands,
/// and is what this answers.
pub fn seal(dir: &Path, outcome: &Sealed, from: SealFrom<'_>) -> Result<Sealed> {
    let parent = dir.parent().unwrap_or(dir);
    crate::disk::atomic::create_dir_all_mode(parent, 0o700)?;
    let leaf = dir.file_name().unwrap_or_default().to_string_lossy();
    let staged = parent.join(format!(
        ".{leaf}.{}.{}",
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));
    let result = stage(&staged, outcome, from).and_then(|()| {
        crate::disk::atomic::sync_tree(&staged)?;
        Ok(crate::disk::atomic::publish_staged_no_replace(
            &staged, dir,
        )?)
    });
    match result {
        Ok(()) => Ok(outcome.clone()),
        Err(WorkflowRunStoreErr::Atomic(crate::disk::atomic::AtomicErr::Exists { .. })) => {
            let _ = std::fs::remove_dir_all(&staged);
            sealed(dir)?.ok_or_else(|| io_err(dir)(io::ErrorKind::NotFound.into()))
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staged);
            Err(error)
        }
    }
}

fn stage(staged: &Path, outcome: &Sealed, from: SealFrom<'_>) -> Result<()> {
    std::fs::create_dir_all(staged).map_err(io_err(staged))?;
    let bytes = serde_json::to_vec_pretty(outcome).map_err(json_err(staged))?;
    let read = staged.join("read.json");
    std::fs::write(&read, bytes).map_err(io_err(&read))?;
    let copy = copy_dir(staged);
    let script = copy.join("workflow.js");
    match from {
        SealFrom::Refused => Ok(()),
        SealFrom::Text(text) => {
            std::fs::create_dir_all(&copy).map_err(io_err(&copy))?;
            std::fs::write(&script, text).map_err(io_err(&script))
        }
        SealFrom::File(file) => {
            std::fs::create_dir_all(&copy).map_err(io_err(&copy))?;
            std::fs::copy(file, &script).map_err(io_err(file))?;
            Ok(())
        }
        SealFrom::Dir(origin) => {
            let root = origin.canonicalize().map_err(io_err(origin))?;
            copy_tree(&root, &root, &copy, &mut Vec::new())
        }
    }
}

/// Copy `from` to `to`, following each link whose target resolves inside `root` and refusing
/// one that resolves outside it; a directory reached twice on one path is copied once.
fn copy_tree(root: &Path, from: &Path, to: &Path, seen: &mut Vec<PathBuf>) -> Result<()> {
    std::fs::create_dir_all(to).map_err(io_err(to))?;
    seen.push(from.to_path_buf());
    for entry in read_dir(from)? {
        let path = entry.path();
        let target = path.canonicalize().map_err(io_err(&path))?;
        if !target.starts_with(root) {
            return Err(WorkflowRunStoreErr::Link { link: path });
        }
        let into = to.join(entry.file_name());
        if target.is_dir() {
            if !seen.contains(&target) {
                copy_tree(root, &target, &into, seen)?;
            }
        } else {
            std::fs::copy(&target, &into).map_err(io_err(&target))?;
        }
    }
    seen.pop();
    Ok(())
}

#[cfg(test)]
mod tests;
