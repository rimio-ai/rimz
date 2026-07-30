use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::source_files;

use super::modules::path_in_scope;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Source {
    pub(super) path: PathBuf,
    #[serde(skip)]
    pub(super) text: String,
}

pub(super) fn scope_sources(root: &Path, scope: &Path, at: Option<&str>) -> Result<Vec<Source>> {
    let sources = if let Some(revision) = at {
        revision_sources(root, scope, revision)?
    } else {
        let mut files = source_files::tracked_rust_files(root)?;
        files.retain(|path| {
            path.strip_prefix(root)
                .is_ok_and(|path| path_in_scope(path, scope))
        });
        files
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(root)
                    .with_context(|| format!("making {} root-relative", path.display()))?
                    .to_path_buf();
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                Ok(Source {
                    path: relative,
                    text,
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    if sources.is_empty() {
        bail!("no tracked Rust files under `{}`", scope.display());
    }
    Ok(sources)
}

fn revision_sources(root: &Path, scope: &Path, revision: &str) -> Result<Vec<Source>> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", revision, "--"])
        .arg(scope)
        .current_dir(root)
        .output()
        .with_context(|| format!("listing Rust sources at `{revision}`"))?;
    if !output.status.success() {
        bail!(
            "git ls-tree `{revision}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let paths = String::from_utf8(output.stdout)
        .context("git ls-tree returned non-UTF-8 paths")?
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting git cat-file --batch")?;
    {
        let stdin = child.stdin.as_mut().context("opening git cat-file stdin")?;
        for path in &paths {
            writeln!(stdin, "{revision}:{}", path.display())
                .context("writing git cat-file request")?;
        }
    }
    drop(child.stdin.take());
    let mut stdout = BufReader::new(child.stdout.take().context("opening git cat-file stdout")?);
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let mut header = String::new();
        stdout
            .read_line(&mut header)
            .context("reading git cat-file header")?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.last() == Some(&"missing") {
            bail!("git object `{revision}:{}` is missing", path.display());
        }
        let size = fields
            .get(2)
            .context("malformed git cat-file header")?
            .parse::<usize>()
            .context("invalid git cat-file object size")?;
        let mut bytes = vec![0; size];
        stdout
            .read_exact(&mut bytes)
            .context("reading git cat-file object")?;
        let mut newline = [0];
        stdout
            .read_exact(&mut newline)
            .context("reading git cat-file separator")?;
        let text = String::from_utf8(bytes)
            .with_context(|| format!("{} at `{revision}` is not UTF-8", path.display()))?;
        sources.push(Source { path, text });
    }
    let status = child.wait().context("waiting for git cat-file")?;
    if !status.success() {
        bail!("git cat-file --batch failed");
    }
    Ok(sources)
}

pub(super) fn working_tree_rust_sources(root: &Path) -> Result<Vec<Source>> {
    let mut sources = Vec::new();
    walk(root, root, &mut sources)?;
    Ok(sources)
}

fn walk(root: &Path, directory: &Path, sources: &mut Vec<Source>) -> Result<()> {
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|name| name.to_str());
            if matches!(name, Some(".git" | "target")) {
                continue;
            }
            walk(root, &path, sources)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            sources.push(Source {
                path: path
                    .strip_prefix(root)
                    .with_context(|| format!("making {} root-relative", path.display()))?
                    .to_path_buf(),
                text,
            });
        }
    }
    Ok(())
}
