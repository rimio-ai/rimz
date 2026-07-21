use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub(crate) fn tracked_text_files(root: &Path) -> Result<Vec<PathBuf>> {
    let files: Vec<_> = git_tracked_files(root)?
        .into_iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(OsStr::to_str),
                Some("rs" | "toml" | "md" | "json")
            )
        })
        .collect();
    if files.is_empty() {
        return walk_text_files(root);
    }
    Ok(files)
}

pub(crate) fn tracked_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(git_tracked_files(root)?
        .into_iter()
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some("rs"))
        .collect())
}

fn git_tracked_files(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(root)
        .output()
        .context("running `git ls-files`")?;
    if !output.status.success() {
        bail!("git ls-files failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|path| root.join(path))
        .filter(|path| path.is_file())
        .collect())
}

fn walk_text_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_text_files_inner(root, root, &mut files)?;
    Ok(files)
}

fn walk_text_files_inner(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.starts_with(root.join(".git")) || path.starts_with(root.join("target")) {
            continue;
        }
        if path.is_dir() {
            walk_text_files_inner(root, &path, files)?;
        } else if matches!(
            path.extension().and_then(OsStr::to_str),
            Some("rs" | "toml" | "md" | "json")
        ) {
            files.push(path);
        }
    }
    Ok(())
}
