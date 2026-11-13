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
use crate::utils::filter::{self, Op, TermError, Word, glob};
use crate::utils::time::parse_compact_duration;
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
/// `--meta` key never takes one of these names; `list`'s filter names each.
pub const RIMZ_KEYS: [&str; 13] = [
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
    #[error("empty filter: write <field><op><value>, such as kind=handoff; see `--filter help`")]
    EmptyFilter,
    #[error(transparent)]
    Term(#[from] TermError),
    #[error("filter term `{term}`: {reason}")]
    BadTerm { term: String, reason: String },
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

/// Which notes `list` keeps and how many of the newest it shows.
#[derive(Debug, Default)]
pub struct ListRequest {
    /// `--filter` queries; every term of every query must match.
    pub filters: Vec<String>,
    /// The newest matches to keep; 0 keeps every match.
    pub limit: usize,
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
    /// The newest matches the limit keeps, by write time, then path.
    pub notes: Vec<Note>,
    /// Older matches the limit left out.
    pub hidden: usize,
    /// Filter keys that are no RimZ key and that no note read carries, once
    /// each in query order; any one of them empties the listing.
    pub unknown_keys: Vec<String>,
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

/// Read the newest notes in the store that `request` keeps.
pub fn list(request: &ListRequest) -> Result<Listing, NotesErr> {
    list_at(&paths::notes_dir(), request, Timestamp::now())
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

fn list_at(root: &Path, request: &ListRequest, now: Timestamp) -> Result<Listing, NotesErr> {
    let conditions = parse_query(&request.filters)?;
    let mut listing = Listing::default();
    let mut read = Vec::new();
    for kind in directories(root)? {
        for day in directories(&kind)? {
            for path in files(&day)? {
                match read_note(&path) {
                    Ok(note) => read.push(note),
                    Err(reason) => listing.skipped.push((path, reason)),
                }
            }
        }
    }
    for condition in &conditions {
        let Key::Meta(key) = &condition.key else {
            continue;
        };
        if !listing.unknown_keys.contains(key)
            && !read.iter().any(|note| note.meta.contains_key(key))
        {
            listing.unknown_keys.push(key.clone());
        }
    }
    if !listing.unknown_keys.is_empty() {
        return Ok(listing);
    }
    listing.notes = read
        .into_iter()
        .filter(|note| {
            conditions
                .iter()
                .all(|condition| condition.matches(note, now))
        })
        .collect();
    listing
        .notes
        .sort_by(|a, b| (a.written, &a.path).cmp(&(b.written, &b.path)));
    if request.limit > 0 {
        listing.hidden = listing.notes.len().saturating_sub(request.limit);
        listing.notes.drain(..listing.hidden);
    }
    Ok(listing)
}

/// One `--filter` term over a note's header.
struct Condition {
    key: Key,
    op: Op,
    values: Vec<Value>,
}

/// The header key a term names: `age`, derived from `written`, and the RimZ
/// keys ignore ASCII case; any other name is a `--meta` key as written.
enum Key {
    Age,
    Rimz(&'static str),
    Meta(String),
}

enum Value {
    None,
    Any,
    Text(String),
    Seconds(i64),
    Instant(Timestamp),
}

fn parse_query(filters: &[String]) -> Result<Vec<Condition>, NotesErr> {
    let mut conditions = Vec::new();
    for filter in filters {
        let mut rest = filter.as_str();
        if rest.trim().is_empty() {
            return Err(NotesErr::EmptyFilter);
        }
        while let Some(term) = filter::next_term(&mut rest) {
            conditions.push(Condition::parse(term?)?);
        }
    }
    Ok(conditions)
}

impl Condition {
    fn parse(term: filter::Term<'_>) -> Result<Self, NotesErr> {
        let bad = |reason: String| NotesErr::BadTerm {
            term: term.text.into(),
            reason,
        };
        let lower = term.field.to_ascii_lowercase();
        let key = match RIMZ_KEYS.iter().find(|key| **key == lower) {
            _ if lower == "age" => Key::Age,
            Some(key) => Key::Rimz(key),
            None => Key::Meta(term.field.into()),
        };
        if term.op.ordered() && !key.ordered() {
            return Err(bad(format!("{} is not ordered; use = or !=", key.name())));
        }
        let values = term
            .values
            .iter()
            .map(|word| key.value(word).map_err(&bad))
            .collect::<Result<Vec<_>, _>>()?;
        if term.op.ordered()
            && !matches!(values.as_slice(), [Value::Seconds(_) | Value::Instant(_)])
        {
            return Err(bad("a comparison takes one value".into()));
        }
        Ok(Self {
            key,
            op: term.op,
            values,
        })
    }

    fn matches(&self, note: &Note, now: Timestamp) -> bool {
        let age = || now.duration_since(note.written).as_secs();
        if self.op.ordered() {
            return match self.values.as_slice() {
                [Value::Seconds(want)] => self.op.compares(age(), *want),
                [Value::Instant(want)] => self.op.compares(note.written, *want),
                _ => false,
            };
        }
        self.op.holds(&self.values, |value| match value {
            Value::None => !self.key.present(note),
            Value::Any => self.key.present(note),
            Value::Seconds(want) => age() == *want,
            Value::Instant(want) => note.written == *want,
            Value::Text(want) => self
                .key
                .text(note)
                .is_some_and(|have| self.key.holds(want, have)),
        })
    }
}

impl Key {
    fn name(&self) -> &str {
        match self {
            Self::Age => "age",
            Self::Rimz(key) => key,
            Self::Meta(key) => key,
        }
    }

    fn ordered(&self) -> bool {
        matches!(self, Self::Age | Self::Rimz("written"))
    }

    fn present(&self, note: &Note) -> bool {
        matches!(self, Self::Age) || self.text(note).is_some()
    }

    fn text<'n>(&self, note: &'n Note) -> Option<&'n str> {
        match self {
            Self::Age => None,
            Self::Rimz(key) => note.keys.get(*key),
            Self::Meta(key) => note.meta.get(key),
        }
        .map(String::as_str)
    }

    /// A value as this key compares it, or why it cannot be one.
    fn value(&self, word: &Word) -> Result<Value, String> {
        let raw = match word {
            Word::Any => return Ok(Value::Any),
            Word::None => return Ok(Value::None),
            Word::Text(raw) => raw.as_str(),
        };
        let wildcard = raw.contains('*');
        match self {
            Self::Age => parse_compact_duration(raw)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .map(Value::Seconds)
                .ok_or_else(|| format!("`{raw}` is not a duration such as 30m or 2d")),
            Self::Rimz("written") => raw.parse().map(Value::Instant).map_err(|_| {
                format!("`{raw}` is not an RFC 3339 instant such as 2026-11-12T09:00:00Z")
            }),
            Self::Rimz("kind") if !wildcard => slug(raw)
                .map(Value::Text)
                .ok_or_else(|| format!("`{raw}` has no ASCII letter or digit to name a kind by")),
            Self::Rimz("project") if !wildcard => Ok(Value::Text(
                comparable(Path::new(raw)).display().to_string(),
            )),
            Self::Rimz("author") => Ok(Value::Text(raw.strip_prefix('@').unwrap_or(raw).into())),
            Self::Rimz("title") => Ok(Value::Text(raw.to_lowercase())),
            _ => Ok(Value::Text(raw.into())),
        }
    }

    /// Whether the header text `have` matches the parsed value `want`.
    fn holds(&self, want: &str, have: &str) -> bool {
        let wildcard = want.contains('*');
        match self {
            Self::Rimz("kind") if !wildcard => slug(have).is_some_and(|have| have == want),
            Self::Rimz("project") if !wildcard => {
                comparable(Path::new(have)).display().to_string() == want
            }
            Self::Rimz("title") => glob(want, &have.to_lowercase()),
            _ => glob(want, have),
        }
    }
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
