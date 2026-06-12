use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::source_files::tracked_text_files;

/// Verify every relative markdown link target — and any `#fragment` it carries
/// — resolves in the working tree. A broken relative link or a `#anchor` with no
/// matching heading fails CI. External (`http(s)`/`mailto:`) targets are out of
/// scope; this gate stays offline and deterministic.
pub(crate) fn docs_links(root: &Path) -> Result<()> {
    let files: Vec<PathBuf> = tracked_text_files(root)?
        .into_iter()
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some("md"))
        // CLAUDE.md is a symlink to AGENTS.md; checking the real file covers it.
        .filter(|path| {
            !fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
        })
        .collect();

    let mut headings: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut violations = Vec::new();

    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let dir = path.parent().unwrap_or(root);
        for (idx, line) in text.lines().enumerate() {
            for target in link_targets_in_line(line) {
                let (rel, fragment) = match target.split_once('#') {
                    Some((rel, frag)) => (rel, Some(frag)),
                    None => (target, None),
                };
                let target_file = if rel.is_empty() {
                    path.clone()
                } else {
                    normalize_lexical(&dir.join(rel))
                };
                if !target_file.exists() {
                    violations.push(format!(
                        "{}:{}: broken link `{target}`",
                        path.display(),
                        idx + 1
                    ));
                    continue;
                }
                let Some(fragment) = fragment.filter(|frag| !frag.is_empty()) else {
                    continue;
                };
                if target_file.extension().and_then(OsStr::to_str) != Some("md") {
                    continue;
                }
                let slugs = headings.entry(target_file.clone()).or_insert_with(|| {
                    fs::read_to_string(&target_file)
                        .map(|text| heading_slugs(&text))
                        .unwrap_or_default()
                });
                if !slugs.contains(&fragment.to_lowercase()) {
                    violations.push(format!(
                        "{}:{}: broken anchor `{target}`",
                        path.display(),
                        idx + 1
                    ));
                }
            }
        }
    }

    if violations.is_empty() {
        return Ok(());
    }
    violations.sort();
    bail!(
        "documentation links must resolve to a file and #anchor:\n{}",
        violations.join("\n")
    );
}

/// Inline markdown link targets on one line, with `http(s)`/`mailto:` links and
/// the optional `"title"` suffix dropped. Prose is one logical line per
/// paragraph, so a link never spans a newline.
fn link_targets_in_line(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut targets = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            if let Some(rel) = line[start..].find(')') {
                let target = line[start..start + rel]
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if !target.is_empty()
                    && !target.starts_with("http://")
                    && !target.starts_with("https://")
                    && !target.starts_with("mailto:")
                {
                    targets.push(target);
                }
                i = start + rel + 1;
                continue;
            }
        }
        i += 1;
    }
    targets
}

/// GitHub-style heading anchors for one markdown document, fenced code skipped.
/// Duplicate headings get the `-1`..`-6` disambiguation suffixes GitHub appends.
fn heading_slugs(text: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if !(1..=6).contains(&hashes) {
            continue;
        }
        let title = trimmed[hashes..]
            .trim_start()
            .trim_end_matches('#')
            .trim_end();
        let base = slugify(title);
        if base.is_empty() {
            continue;
        }
        for suffix in 1..=6 {
            slugs.insert(format!("{base}-{suffix}"));
        }
        slugs.insert(base);
    }
    slugs
}

/// GitHub heading slug: link text only, lowered, emphasis/code marks dropped,
/// spaces hyphenated, other punctuation removed.
fn slugify(title: &str) -> String {
    let lowered = strip_md_links(title).to_lowercase();
    let mut slug = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if matches!(ch, '`' | '*' | '_' | '~') {
            continue;
        }
        if ch == ' ' {
            slug.push('-');
        } else if ch.is_alphanumeric() || ch == '-' {
            slug.push(ch);
        }
    }
    slug
}

/// Replace `[text](target)` with its visible `text`, so a heading carrying a
/// link slugs from what the reader sees.
fn strip_md_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let (before, tail) = rest.split_at(open);
        out.push_str(before);
        if let Some(close) = tail.find(']')
            && tail[close + 1..].starts_with('(')
            && let Some(paren) = tail[close + 2..].find(')')
        {
            out.push_str(&tail[1..close]);
            rest = &tail[close + 2 + paren + 1..];
            continue;
        }
        out.push('[');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// Lexically resolve `.`/`..` without touching the filesystem — no symlink
/// surprises, and it works for paths that may not exist.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests;
