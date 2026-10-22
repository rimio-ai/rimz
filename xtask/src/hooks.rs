//! Git hook activation and staged Rust formatting for the tracked `.githooks/`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::runner::run;

/// Activate the repo's tracked git hooks by setting the local `core.hooksPath`
/// to `.githooks/`, so the committed `pre-commit` gate (staged fmt, invariants,
/// conform, docs-links) runs on every commit.
/// Idempotent — re-running rewrites the same value. The hook script routes git's
/// call back through `cargo xtask`, so the gate definition stays single-sourced.
pub(crate) fn install(root: &Path) -> Result<()> {
    run(root, "git", ["config", "core.hooksPath", ".githooks"])
}

/// The pre-commit formatting step: judge each staged Rust file's index copy through stdin
/// rustfmt, and format only the refused files' working copies, never the index.
pub(crate) fn fmt_staged(root: &Path) -> Result<()> {
    let paths = git_output(
        root,
        [
            "diff",
            "--cached",
            "-z",
            "--name-only",
            "--diff-filter=ACMR",
        ],
    )?;
    let mut refused = Vec::new();
    for name in paths
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let path = Path::new(OsStr::from_bytes(name));
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        let mut index_path = OsString::from(":");
        index_path.push(path);
        let source = git_output(
            root,
            [OsStr::new("cat-file"), OsStr::new("blob"), &index_path],
        )
        .with_context(|| format!("reading index copy of {path:?}"))?;
        let problem = match rustfmt(root, &source) {
            Ok(formatted) if formatted == source => continue,
            Ok(_) => "index copy not formatted; ".to_owned(),
            Err(error) => format!("index copy could not be formatted: {error:#}; "),
        };
        let outcome = match format_working_copy(root, path) {
            Ok(true) => "working copy formatted; review the diff and re-stage".to_owned(),
            Ok(false) => "working copy already formatted; re-stage it".to_owned(),
            Err(error) => format!("working copy not formatted: {error:#}"),
        };
        refused.push(format!("{path:?}: {problem}{outcome}"));
    }
    if !refused.is_empty() {
        bail!("{}", refused.join("\n"));
    }
    Ok(())
}

fn format_working_copy(root: &Path, path: &Path) -> Result<bool> {
    let path = root.join(path);
    let source = fs::read(&path).context("reading working copy")?;
    let formatted = rustfmt(root, &source)?;
    if formatted == source {
        return Ok(false);
    }
    fs::write(&path, formatted).context("writing working copy")?;
    Ok(true)
}

fn git_output<I, S>(root: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn rustfmt(root: &Path, source: &[u8]) -> Result<Vec<u8>> {
    // Stdin input keeps out-of-line child modules outside this file's scope.
    let mut child = Command::new("rustfmt")
        .args(["--emit", "stdout"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting rustfmt")?;
    let mut stdin = child.stdin.take().context("opening rustfmt stdin")?;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(source));
        let output = child.wait_with_output().context("waiting for rustfmt");
        let written = writer
            .join()
            .map_err(|_| anyhow!("rustfmt stdin writer panicked"))?;
        let output = output?;
        if !output.status.success() {
            bail!(
                "rustfmt failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        written.context("writing rustfmt stdin")?;
        Ok(output.stdout)
    })
}

#[cfg(test)]
mod tests;
