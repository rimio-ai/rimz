//! What RimZ's MCP tools say: the server's instructions, each tool's
//! description, and the guides `guide` loads, as Markdown files compiled in
//! from `tools/`. A file at the same relative path under `mcps/` in the RimZ
//! home replaces the built-in one for every server and launch that resolves
//! the wording after it is written. A replacement never switches a tool off:
//! one that cannot be read, is empty, or breaks a limit a provider keeps is
//! reported and the built-in text is served, and a file at a path RimZ has no
//! wording file for is reported and ignored.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::catalog::{Capability, Purpose, TOOLS, Tool};

/// Every built-in wording file, by its path relative to `tools/`.
const BUILT_IN: [(&str, &str); 16] = [
    ("instructions.md", include_str!("tools/instructions.md")),
    ("guide.md", include_str!("tools/guide.md")),
    ("tasks.md", include_str!("tools/tasks.md")),
    ("join.md", include_str!("tools/join.md")),
    ("stop.md", include_str!("tools/stop.md")),
    (
        "rimz-lsp/instructions.md",
        include_str!("tools/rimz-lsp/instructions.md"),
    ),
    ("rimz-lsp/lsp.md", include_str!("tools/rimz-lsp/lsp.md")),
    (
        "rimz-wait/instructions.md",
        include_str!("tools/rimz-wait/instructions.md"),
    ),
    ("rimz-wait/wait.md", include_str!("tools/rimz-wait/wait.md")),
    (
        "rimz-wait/guides/conditions.md",
        include_str!("tools/rimz-wait/guides/conditions.md"),
    ),
    (
        "rimz-control/instructions.md",
        include_str!("tools/rimz-control/instructions.md"),
    ),
    (
        "rimz-control/self_stop.md",
        include_str!("tools/rimz-control/self_stop.md"),
    ),
    (
        "rimz-control/self_compact.md",
        include_str!("tools/rimz-control/self_compact.md"),
    ),
    (
        "rimz-control/self_tune.md",
        include_str!("tools/rimz-control/self_tune.md"),
    ),
    (
        "rimz-notes/instructions.md",
        include_str!("tools/rimz-notes/instructions.md"),
    ),
    (
        "rimz-notes/notes.md",
        include_str!("tools/rimz-notes/notes.md"),
    ),
];

/// The shared part of the instructions, after the group sentences.
const SHARED: &str = "instructions.md";

/// Codex shows a deferred tool only the instructions' first line; Codex's
/// docs give 250 and 512 characters for what it keeps, and the smaller holds
/// for the line generated with every group served.
#[cfg(test)]
const FIRST_LINE_LIMIT: usize = 250;

/// Claude Code cuts the instructions and each description at 4,096
/// characters, and may count a tool's argument descriptions with it.
const TEXT_LIMIT: usize = 4096;

/// One wording file's text, and the path of the user's file it came from;
/// `None` for the built-in text.
pub struct Text {
    text: Cow<'static, str>,
    path: Option<PathBuf>,
}

impl Text {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// A replacement file that is not served, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordingWarning {
    /// The group whose folder holds the replaced file; `None` for a shared
    /// file and for a file RimZ has no wording file for.
    group: Option<Capability>,
    message: String,
}

impl std::fmt::Display for WordingWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The resolved text of every wording file, and the warnings resolving it
/// raised.
pub struct Wording {
    files: BTreeMap<&'static str, Text>,
    warnings: Vec<WordingWarning>,
}

impl Wording {
    /// The built-in text of every file.
    pub fn built_in() -> Self {
        let files = BUILT_IN
            .iter()
            .map(|(path, text)| {
                let text = Text {
                    text: Cow::Borrowed(text.trim()),
                    path: None,
                };
                (*path, text)
            })
            .collect();
        Self {
            files,
            warnings: Vec::new(),
        }
    }

    /// The built-in text, with each file under `dir` that replaces one and
    /// keeps every limit in its place.
    pub fn read(dir: &Path) -> Self {
        let mut wording = Self::built_in();
        let mut found = Vec::new();
        let depth = BUILT_IN
            .iter()
            .map(|(path, _)| path.matches('/').count())
            .max()
            .unwrap_or_default();
        if let Err(err) = markdown_files(dir, dir, depth, &mut found, &mut wording.warnings)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            wording.warnings.push(WordingWarning {
                group: None,
                message: format!(
                    "{} cannot be read, so the built-in text is served: {err}",
                    dir.display()
                ),
            });
        }
        found.sort();
        for (relative, path) in found {
            let Some(&(known, _)) = BUILT_IN.iter().find(|(known, _)| *known == relative) else {
                wording.warnings.push(misplaced(dir, &relative, &path));
                continue;
            };
            let text = match std::fs::read(&path) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map(|text| text.trim().to_owned())
                    .map_err(|_| "it is not UTF-8".to_owned()),
                Err(err) => Err(format!("it cannot be read: {err}")),
            };
            match text.and_then(|text| match breach(known, &text) {
                Some(reason) => Err(reason),
                None => Ok(text),
            }) {
                Ok(text) => {
                    let text = Text {
                        text: Cow::Owned(text),
                        path: Some(path),
                    };
                    wording.files.insert(known, text);
                }
                Err(reason) => wording.reject(known, path, &reason),
            }
        }
        if let Some(reason) = wording.instructions_breach() {
            for group in Capability::ALL {
                wording.revert(&group_sentence(group), &reason);
            }
            wording.revert(SHARED, &reason);
        }
        wording
    }

    /// The wording under the `mcps/` folder of this process's RimZ home.
    pub fn read_home() -> Self {
        Self::read(&crate::disk::paths::mcps_dir_under(
            &crate::disk::paths::rimz_home(),
        ))
    }

    /// Every warning resolving the wording raised.
    pub fn warnings(&self) -> &[WordingWarning] {
        &self.warnings
    }

    /// The warnings that concern a launch serving `groups`: those of the
    /// groups' own files, of the shared files, and of every misplaced file.
    pub fn warnings_for<'a>(
        &'a self,
        groups: &'a [Capability],
    ) -> impl Iterator<Item = &'a WordingWarning> {
        self.warnings
            .iter()
            .filter(|warning| warning.group.is_none_or(|group| groups.contains(&group)))
    }

    /// `tool`'s description.
    pub fn description(&self, tool: &Tool) -> &Text {
        self.file(&tool.wording_path())
    }

    /// The server's instructions for a launch serving `groups`: a first
    /// line naming the served tools by purpose, then the sentence of each
    /// group, in group order, a line each, then the shared text.
    pub fn instructions(&self, groups: &[Capability]) -> String {
        let sentences: Vec<&str> = Capability::ALL
            .into_iter()
            .filter(|group| groups.contains(group))
            .map(|group| self.file(&group_sentence(group)).text())
            .collect();
        format!(
            "{}\n{}\n{}",
            first_line(groups),
            sentences.join("\n"),
            self.file(SHARED).text()
        )
    }

    /// The user's files among the parts of `groups`' instructions.
    pub fn instruction_paths<'a>(
        &'a self,
        groups: &'a [Capability],
    ) -> impl Iterator<Item = &'a Path> {
        Capability::ALL
            .into_iter()
            .filter(|group| groups.contains(group))
            .map(group_sentence)
            .chain([SHARED.to_owned()])
            .filter_map(|path| self.file(&path).path())
    }

    /// The text of the guide `name` (`<group>/<guide>`) when one of
    /// `groups` has it; else every guide name `groups` have.
    pub fn guide(&self, groups: &[Capability], name: &str) -> Result<&str, Vec<String>> {
        let served = guide_names(groups);
        let Some((group, guide)) = name
            .split_once('/')
            .filter(|_| served.iter().any(|known| known == name))
        else {
            return Err(served);
        };
        Ok(self.file(&format!("{group}/guides/{guide}.md")).text())
    }

    /// Each limit a provider keeps that the wording breaks, as
    /// `<path>: <reason>`: the check the built-in text is tested against,
    /// made of the same rules `read` holds a replacement to.
    #[cfg(test)]
    pub(crate) fn breaches(&self) -> Vec<String> {
        let mut breaches: Vec<String> = self
            .files
            .iter()
            .filter_map(|(path, text)| Some(format!("{path}: {}", breach(path, text.text())?)))
            .collect();
        if let Some(reason) = self.first_line_breach() {
            breaches.push(format!("the group instructions: {reason}"));
        }
        if let Some(reason) = self.instructions_breach() {
            breaches.push(format!("{SHARED}: {reason}"));
        }
        breaches
    }

    /// Why the instructions' first line with every group served breaks its
    /// limit.
    #[cfg(test)]
    fn first_line_breach(&self) -> Option<String> {
        let instructions = self.instructions(&Capability::ALL);
        let length = instructions
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        (length > FIRST_LINE_LIMIT).then(|| {
            format!(
                "the instructions' first line, with every group served, would hold {length} \
                 characters, over the {FIRST_LINE_LIMIT} Codex shows"
            )
        })
    }

    /// Why the instructions with every group break their limit.
    fn instructions_breach(&self) -> Option<String> {
        let length = self.instructions(&Capability::ALL).chars().count();
        (length > TEXT_LIMIT).then(|| {
            format!(
                "the instructions with every group would hold {length} characters, over the \
                 {TEXT_LIMIT} Claude keeps"
            )
        })
    }

    /// Serve the built-in text of `path` in place of the user's file, when
    /// it holds one, and warn why.
    fn revert(&mut self, path: &str, reason: &str) {
        let Some((known, text)) = Self::built_in().files.remove_entry(path) else {
            return;
        };
        let Some(user) = self.files.insert(known, text).and_then(|user| user.path) else {
            return;
        };
        self.reject(known, user, reason);
    }

    fn reject(&mut self, known: &str, path: PathBuf, reason: &str) {
        let group = known
            .split_once('/')
            .and_then(|(group, _)| Capability::from_name(group));
        self.warnings.push(WordingWarning {
            group,
            message: format!(
                "{} is not used, so the built-in text is served: {reason}",
                path.display()
            ),
        });
    }

    /// The file at `path`, which every caller takes from `BUILT_IN`'s paths.
    fn file(&self, path: &str) -> &Text {
        // `built_in` fills every path of `BUILT_IN`, and a replacement only
        // overwrites one, so a path from it is always present.
        self.files
            .get(path)
            .expect("every wording path is built in")
    }
}

/// Why `text` cannot be the wording file `path` on its own: it is empty, a
/// group's sentence spans lines, or a tool's description with its argument
/// descriptions is over the limit.
fn breach(path: &str, text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some("it is empty".to_owned());
    }
    if let Some(tool) = TOOLS.iter().find(|tool| tool.wording_path() == path) {
        let length = text.chars().count() + tool.argument_text_len();
        return (length > TEXT_LIMIT).then(|| {
            format!(
                "`{}`'s description and its arguments' descriptions would hold {length} \
                 characters, over the {TEXT_LIMIT} Claude keeps",
                tool.name
            )
        });
    }
    let sentence = Capability::ALL
        .into_iter()
        .any(|group| group_sentence(group) == path);
    (sentence && text.contains('\n')).then(|| "a group's instructions are one line".to_owned())
}

/// Collect every `.md` file under `dir` with its path relative to `root`,
/// `/`-separated, entering folders, symlinked ones included, at most `depth`
/// deep, which also ends a symlink loop. A folder below `root` that cannot be
/// read is skipped with a warning naming it; `dir` that cannot be read is the
/// error.
fn markdown_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    found: &mut Vec<(String, PathBuf)>,
    warnings: &mut Vec<WordingWarning>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(err) => {
                warnings.push(unreadable_folder(root, dir, &err));
                continue;
            }
        };
        if path.is_dir() {
            if depth == 0 {
                continue;
            }
            if let Err(err) = markdown_files(root, &path, depth - 1, found, warnings) {
                warnings.push(unreadable_folder(root, &path, &err));
            }
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative: Vec<_> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        found.push((relative.join("/"), path));
    }
    Ok(())
}

/// The warning for `dir`, a folder under `root` that cannot be read, kept
/// with the group whose folder it is in.
fn unreadable_folder(root: &Path, dir: &Path, err: &std::io::Error) -> WordingWarning {
    let group = dir
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|first| Capability::from_name(&first.as_os_str().to_string_lossy()));
    WordingWarning {
        group,
        message: format!(
            "{} cannot be read, so no file in it is used: {err}",
            dir.display()
        ),
    }
}

/// The warning for a file at `relative`, a path RimZ has no wording file
/// at, naming the one built-in path with the same file name when there is one.
fn misplaced(dir: &Path, relative: &str, path: &Path) -> WordingWarning {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let mut same_name = BUILT_IN
        .iter()
        .map(|(known, _)| *known)
        .filter(|known| known.rsplit('/').next() == Some(name));
    let belongs = match (same_name.next(), same_name.next()) {
        (Some(known), None) => format!("; it belongs at {}", dir.join(known).display()),
        _ => String::new(),
    };
    WordingWarning {
        group: None,
        message: format!(
            "{} is ignored: RimZ has no wording file {relative}{belongs}",
            path.display()
        ),
    }
}

/// Whether `group` has a guide among the built-in files; a replacement
/// cannot add one.
pub fn has_guides(group: Capability) -> bool {
    !guide_names(&[group]).is_empty()
}

/// The names `guide` takes for `groups`' guides, `<group>/<guide>`.
fn guide_names(groups: &[Capability]) -> Vec<String> {
    BUILT_IN
        .iter()
        .filter_map(|(path, _)| {
            let (group, file) = path.split_once("/guides/")?;
            let group = Capability::from_name(group).filter(|group| groups.contains(group))?;
            Some(format!("{}/{}", group.name(), file.strip_suffix(".md")?))
        })
        .collect()
}

/// The instructions' first line for a launch serving `groups`: each served
/// tool's name, grouped by what the tools are for.
fn first_line(groups: &[Capability]) -> String {
    let tools: Vec<&Tool> = if groups.is_empty() {
        Vec::new()
    } else {
        super::catalog::tools_for(groups).collect()
    };
    let mut purposes: Vec<Purpose> = tools.iter().map(|tool| tool.purpose).collect();
    purposes.sort();
    purposes.dedup();
    let parts: Vec<String> = purposes
        .into_iter()
        .map(|purpose| {
            let names: Vec<String> = tools
                .iter()
                .filter(|tool| tool.purpose == purpose)
                .map(|tool| format!("`{}`", tool.name))
                .collect();
            let names = match names.as_slice() {
                [one] => one.clone(),
                [first, second] => format!("{first} and {second}"),
                [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
                [] => String::new(),
            };
            format!("{names} {}", purpose.phrase())
        })
        .collect();
    format!("RimZ tools: {}.", parts.join("; "))
}

/// The path of `group`'s one-line instructions.
fn group_sentence(group: Capability) -> String {
    format!("{}/instructions.md", group.name())
}

#[cfg(test)]
#[path = "wording_tests.rs"]
mod tests;
