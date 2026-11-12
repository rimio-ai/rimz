//! The home-wide note store behind `rimz notes`: Markdown notes under
//! `<home>/notes/<kind>/<YYYYMMDD>/<slug>.md`, each opening with a YAML header
//! RimZ stamps from the caller's launch environment. RimZ writes a note once
//! and never deletes, moves, or renames it.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};
use serde::Serialize;

use crate::disk::{atomic, paths};
use crate::harness::launch::{
    ENV_AGENT_MODEL, ENV_AGENT_NAME, ENV_AGENT_PROFILE, ENV_AGENT_ROLE, ENV_TEAM,
};
use crate::harness::loop_task::LOOP_TASK_ENV;
use crate::workspace::{
    ENV_CHANNEL, ENV_PROJECT_ROOT, ENV_WORKTREE_PATH, WorkspaceErr, WorkspaceResolver,
};

const SLUG_MAX: usize = 48;
/// How far `list` reads into a note for its header before calling it unclosed.
const HEADER_MAX: u64 = 1024 * 1024;
const DIR_MODE: u32 = 0o700;
const NOTE_MODE: u32 = 0o600;
const WORKFLOW_RUN_ENV: &str = "RIMZ_WORKFLOW_RUN";

/// Caller header entries, `(key, value)` in the order they are written.
type MetaEntries = Vec<(String, String)>;

/// Every header key RimZ owns, in the order a note writes them. A caller's
/// `--meta` key never takes one of these names.
const RIMZ_KEYS: [&str; 13] = [
    "kind",
    "title",
    "written",
    "author",
    "profile",
    "team",
    "role",
    "model",
    "project",
    "worktree",
    "channel",
    "loop-task",
    "workflow-run",
];

/// The caller facts read from the launch environment, by header key.
const ENV_KEYS: [(&str, &str); 8] = [
    ("author", ENV_AGENT_NAME),
    ("profile", ENV_AGENT_PROFILE),
    ("team", ENV_TEAM),
    ("role", ENV_AGENT_ROLE),
    ("model", ENV_AGENT_MODEL),
    ("channel", ENV_CHANNEL),
    ("loop-task", LOOP_TASK_ENV),
    ("workflow-run", WORKFLOW_RUN_ENV),
];

#[derive(Debug, thiserror::Error)]
pub enum NotesErr {
    #[error("{what} `{value}` has no ASCII letter or digit to name a file by")]
    NoSlug { what: &'static str, value: String },
    #[error("the note body is empty; write the note on stdin or name it with --file")]
    EmptyBody,
    #[error("--meta `{arg}`: {reason}")]
    BadMeta { arg: String, reason: &'static str },
    #[error(
        "the note header is {bytes} bytes, past the {limit} bytes `rimz notes list` reads; \
         shorten the title or the --meta values",
        limit = HEADER_MAX
    )]
    HeaderTooLarge { bytes: usize },
    #[error("cannot write note {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: atomic::AtomicErr,
    },
    #[error("cannot read note store {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// One note to write. The body is the bytes as read and is never parsed.
#[derive(Debug, Default)]
pub struct AddRequest {
    pub kind: String,
    pub title: String,
    pub body: Vec<u8>,
    pub fence: bool,
    /// Caller header entries as given on the command line, `key=value`.
    pub meta: Vec<String>,
    /// The `--root` override, which names the project ahead of the room pin.
    pub root: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Added {
    /// Absolute path of the published note.
    pub path: PathBuf,
    /// `--meta` keys dropped because RimZ owns a header key of that name.
    pub dropped_meta: Vec<String>,
    /// Why the note has no `project` key, when resolving the project failed.
    pub unresolved_project: Option<WorkspaceErr>,
}

/// Which notes `list` keeps; every filter set must match.
#[derive(Debug, Default)]
pub struct ListFilter {
    pub kind: Option<String>,
    pub since: Option<Timestamp>,
    pub author: Option<String>,
    pub project: Option<PathBuf>,
}

/// One stored note's header, RimZ's keys beside the caller's.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Note {
    pub path: PathBuf,
    #[serde(flatten)]
    pub keys: BTreeMap<String, String>,
    pub meta: BTreeMap<String, String>,
    #[serde(skip)]
    pub written: Timestamp,
}

#[derive(Debug, Default)]
pub struct Listing {
    /// Matching notes by write time, then path.
    pub notes: Vec<Note>,
    /// `.md` files in the store that carry no readable RimZ header, with why.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Write one note for the calling process and return its absolute path.
pub fn add(request: &AddRequest) -> Result<Added, NotesErr> {
    let env = |key: &str| std::env::var(key).ok();
    // `--root` names the project ahead of the room pin, the pin ahead of the cwd.
    let pin = match request.root {
        Some(_) => None,
        None => env(ENV_PROJECT_ROOT).filter(|pin| !pin.is_empty()),
    };
    let (project, unresolved_project) = match pin {
        Some(pin) => (Some(pin), None),
        None => match WorkspaceResolver::resolve(".", request.root.clone()) {
            Ok(workspace) => (Some(workspace.project_root.display().to_string()), None),
            Err(error) => (None, Some(error)),
        },
    };
    let facts = caller_facts(env, project);
    let root = std::path::absolute(paths::notes_dir()).unwrap_or_else(|_| paths::notes_dir());
    let added = add_at(&root, request, &facts, &Zoned::now())?;
    Ok(Added {
        unresolved_project,
        ..added
    })
}

/// Read every note in the store that `filter` keeps.
pub fn list(filter: &ListFilter) -> Result<Listing, NotesErr> {
    list_at(&paths::notes_dir(), filter)
}

fn caller_facts(
    env: impl Fn(&str) -> Option<String>,
    project: Option<String>,
) -> Vec<(&'static str, String)> {
    let known = |key: &str| env(key).filter(|value| !value.is_empty());
    let mut facts: Vec<(&'static str, String)> = ENV_KEYS
        .iter()
        .filter_map(|(header, key)| Some((*header, known(key)?)))
        .collect();
    facts.extend(project.map(|project| ("project", project)));
    facts.extend(known(ENV_WORKTREE_PATH).map(|worktree| ("worktree", worktree)));
    facts
}

fn add_at(
    root: &Path,
    request: &AddRequest,
    facts: &[(&'static str, String)],
    now: &Zoned,
) -> Result<Added, NotesErr> {
    let kind = slug(&request.kind).ok_or_else(|| NotesErr::NoSlug {
        what: "kind",
        value: request.kind.clone(),
    })?;
    let title = slug(&request.title).ok_or_else(|| NotesErr::NoSlug {
        what: "title",
        value: request.title.clone(),
    })?;
    let (meta, dropped_meta) = parse_meta(&request.meta)?;
    // A body that is not UTF-8 holds some byte past ASCII, so it is never blank.
    if std::str::from_utf8(&request.body).is_ok_and(|text| text.trim().is_empty()) {
        return Err(NotesErr::EmptyBody);
    }
    let written = now.strftime("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string();
    let mut header = vec![
        ("kind", kind.clone()),
        ("title", request.title.clone()),
        ("written", written),
    ];
    for key in &RIMZ_KEYS[3..] {
        header.extend(
            facts
                .iter()
                .filter(|(fact, _)| fact == key)
                .map(|(fact, value)| (*fact, value.clone())),
        );
    }
    let header = render_header(&header, &meta);
    // `list` stops reading at HEADER_MAX, so a longer header would never list.
    if !u64::try_from(header.len()).is_ok_and(|len| len <= HEADER_MAX) {
        return Err(NotesErr::HeaderTooLarge {
            bytes: header.len(),
        });
    }
    let bytes = render(header, &request.body, request.fence);
    let levels = [
        root.to_owned(),
        root.join(&kind),
        root.join(&kind).join(now.strftime("%Y%m%d").to_string()),
    ];
    let path = claim(&levels, &title, &bytes, |levels| {
        levels
            .iter()
            .try_for_each(|level| atomic::create_dir_all_mode(level, DIR_MODE))
    })?;
    Ok(Added {
        path,
        dropped_meta,
        unresolved_project: None,
    })
}

/// The title or kind lowercased to ASCII, every other run of characters one
/// `-`, trimmed of `-` and cut to [`SLUG_MAX`]; `None` without a letter or digit.
fn slug(text: &str) -> Option<String> {
    let mut slug = String::new();
    for word in text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }
    slug.truncate(SLUG_MAX);
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

/// Caller entries in the order given, a repeated key keeping its last value,
/// and the keys dropped for naming a RimZ header key.
fn parse_meta(raw: &[String]) -> Result<(MetaEntries, Vec<String>), NotesErr> {
    let mut meta = MetaEntries::new();
    let mut dropped = Vec::new();
    for arg in raw {
        let bad = |reason| NotesErr::BadMeta {
            arg: arg.clone(),
            reason,
        };
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| bad("expected key=value"))?;
        let mut chars = key.chars();
        if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase())
            || !chars.all(|ch| matches!(ch, 'a'..='z' | '0'..='9' | '_' | '-'))
        {
            return Err(bad(concat!(
                "a key starts with a lowercase letter and holds only lowercase letters, ",
                "digits, `_` and `-`",
            )));
        }
        if value.contains(['\n', '\r']) {
            return Err(bad("a value is one line"));
        }
        if RIMZ_KEYS.contains(&key) {
            if !dropped.iter().any(|seen| seen == key) {
                dropped.push(key.to_owned());
            }
            continue;
        }
        match meta.iter_mut().find(|(seen, _)| seen == key) {
            Some(entry) => entry.1 = value.to_owned(),
            None => meta.push((key.to_owned(), value.to_owned())),
        }
    }
    Ok((meta, dropped))
}

/// The header block, its opening and closing `---` lines included.
fn render_header(header: &[(&'static str, String)], meta: &[(String, String)]) -> String {
    let mut text = String::from("---\n");
    // Keys go bare so a reader can match `^written:`. Of the keys the grammar
    // admits, YAML reads only a bare `null` as no string, so that one is quoted.
    // A JSON-quoted value is one YAML string scalar once the characters YAML
    // forbids in a double-quoted scalar, which JSON leaves as they are, are escaped.
    let entries = header
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .chain(
            meta.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
    for (key, value) in entries {
        let key = match key {
            "null" => "\"null\"",
            key => key,
        };
        let value: String = serde_json::json!(value)
            .to_string()
            .chars()
            .map(|ch| match ch {
                '\u{7f}'..='\u{9f}' | '\u{fffe}' | '\u{ffff}' => {
                    format!("\\u{:04X}", u32::from(ch))
                }
                ch => ch.to_string(),
            })
            .collect();
        text.push_str(&format!("{key}: {value}\n"));
    }
    text.push_str("---\n");
    text
}

fn render(header: String, body: &[u8], fence: bool) -> Vec<u8> {
    let mut bytes = header.into_bytes();
    if !fence {
        bytes.extend_from_slice(body);
        return bytes;
    }
    let longest = body
        .split(|byte| *byte != b'`')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0);
    let marker = "`".repeat((longest + 1).max(3));
    bytes.extend_from_slice(marker.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(body);
    if !body.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(marker.as_bytes());
    bytes.push(b'\n');
    bytes
}

/// Publish `bytes` at the first free `<title>.md`, `<title>-2.md`, ... in the
/// day folder, creating its levels first and once more if the day folder
/// vanishes mid-claim.
fn claim(
    levels: &[PathBuf],
    title: &str,
    bytes: &[u8],
    ensure: impl Fn(&[PathBuf]) -> atomic::Result<()>,
) -> Result<PathBuf, NotesErr> {
    // `levels` always ends in the day folder.
    let day = &levels[levels.len() - 1];
    let failed = |path: &Path| {
        let path = path.to_owned();
        move |source| NotesErr::Write { path, source }
    };
    ensure(levels).map_err(failed(day))?;
    let mut suffix = 1;
    let mut recreated = false;
    loop {
        let path = day.join(if suffix == 1 {
            format!("{title}.md")
        } else {
            format!("{title}-{suffix}.md")
        });
        match atomic::write_bytes_new_mode(&path, bytes, NOTE_MODE) {
            Ok(()) => return Ok(path),
            Err(atomic::AtomicErr::Exists { .. }) => suffix += 1,
            Err(atomic::AtomicErr::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound && !recreated =>
            {
                ensure(levels).map_err(failed(day))?;
                recreated = true;
            }
            Err(error) => return Err(failed(&path)(error)),
        }
    }
}

fn list_at(root: &Path, filter: &ListFilter) -> Result<Listing, NotesErr> {
    let kinds = match &filter.kind {
        Some(kind) => slug(kind)
            .map(|kind| vec![root.join(kind)])
            .ok_or_else(|| NotesErr::NoSlug {
                what: "kind",
                value: kind.clone(),
            })?,
        None => directories(root)?,
    };
    let author = filter
        .author
        .as_deref()
        .map(|author| author.strip_prefix('@').unwrap_or(author));
    let project = filter.project.as_deref().map(comparable);
    let mut listing = Listing::default();
    for kind in kinds {
        for day in directories(&kind)? {
            for path in files(&day)? {
                let note = match read_note(&path) {
                    Ok(note) => note,
                    Err(reason) => {
                        listing.skipped.push((path, reason));
                        continue;
                    }
                };
                let keeps = filter.since.is_none_or(|since| note.written >= since)
                    && author
                        .is_none_or(|author| note.keys.get("author").is_some_and(|a| a == author))
                    && project.as_ref().is_none_or(|project| {
                        note.keys
                            .get("project")
                            .is_some_and(|value| comparable(Path::new(value)) == *project)
                    });
                if keeps {
                    listing.notes.push(note);
                }
            }
        }
    }
    listing
        .notes
        .sort_by(|a, b| (a.written, &a.path).cmp(&(b.written, &b.path)));
    Ok(listing)
}

fn comparable(path: &Path) -> PathBuf {
    crate::utils::path::normalize_path_lexical(
        &std::path::absolute(path).unwrap_or_else(|_| path.to_owned()),
    )
}

/// Read the header of the note at `path`, never past its closing `---` line or
/// [`HEADER_MAX`] bytes, whichever comes first.
fn read_note(path: &Path) -> Result<Note, String> {
    let failed = |error: io::Error| error.to_string();
    let file = std::fs::File::open(path).map_err(failed)?;
    let mut reader = io::BufReader::new(file.take(HEADER_MAX));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).map_err(failed)?;
    if line != b"---\n" {
        return Err("has no RimZ header".into());
    }
    let mut header = Vec::new();
    loop {
        line.clear();
        reader.read_until(b'\n', &mut line).map_err(failed)?;
        if !line.ends_with(b"\n") {
            return Err("never closes its header with `---` in its first 1 MiB".into());
        }
        if line == b"---\n" {
            break;
        }
        header.append(&mut line);
    }
    let yaml = std::str::from_utf8(&header).map_err(|_| "header is not UTF-8")?;
    let header: BTreeMap<String, String> = serde_saphyr::from_str(yaml)
        .map_err(|error| format!("header is not a mapping of strings: {error}"))?;
    let (keys, meta): (BTreeMap<_, _>, BTreeMap<_, _>) = header
        .into_iter()
        .partition(|(key, _)| RIMZ_KEYS.contains(&key.as_str()));
    let written = keys
        .get("written")
        .ok_or("header has no `written` key")?
        .parse::<Timestamp>()
        .map_err(|error| format!("header `written` is no RFC 3339 instant: {error}"))?;
    Ok(Note {
        path: path.to_owned(),
        keys,
        meta,
        written,
    })
}

/// Subdirectories of `dir`, sorted; a missing `dir` holds none.
fn directories(dir: &Path) -> Result<Vec<PathBuf>, NotesErr> {
    entries(dir, |path| path.is_dir())
}

/// The `.md` files of a day folder, sorted; temp siblings and other files are
/// not notes.
fn files(dir: &Path) -> Result<Vec<PathBuf>, NotesErr> {
    entries(dir, |path| {
        path.extension().is_some_and(|extension| extension == "md") && path.is_file()
    })
}

fn entries(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>, NotesErr> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(NotesErr::Read {
                path: dir.to_owned(),
                source,
            });
        }
    };
    let mut paths = Vec::new();
    for entry in read {
        let path = entry
            .map_err(|source| NotesErr::Read {
                path: dir.to_owned(),
                source,
            })?
            .path();
        if keep(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests;
