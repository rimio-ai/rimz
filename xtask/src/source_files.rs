use std::ffi::OsStr;
use std::ops::Range;
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

pub(crate) fn is_test_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
        || path
            .components()
            .any(|component| component.as_os_str() == OsStr::new("tests"))
}

pub(crate) fn split_rust_sloc(source: &str, test_regions: &[Range<usize>]) -> (u64, u64) {
    let total = rust_sloc(source);
    let test_source = source
        .split_inclusive('\n')
        .enumerate()
        .filter(|(index, _)| {
            let line = index + 1;
            test_regions.iter().any(|region| region.contains(&line))
        })
        .map(|(_, line)| line)
        .collect::<String>();
    let tests = rust_sloc(&test_source);
    (total.saturating_sub(tests), tests)
}

pub(crate) fn rust_sloc(source: &str) -> u64 {
    let bytes = source.as_bytes();
    let mut code_lines = 0;
    let mut line_has_code = false;
    let mut block_comment_depth = 0_u32;
    let mut string_escape = false;
    let mut string = false;
    let mut raw_string_hashes = None;
    let mut character_end = None;
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            code_lines += u64::from(line_has_code);
            line_has_code = string || raw_string_hashes.is_some();
            string_escape = false;
            index += 1;
            continue;
        }
        if block_comment_depth > 0 {
            if bytes[index..].starts_with(b"/*") {
                block_comment_depth += 1;
                index += 2;
            } else if bytes[index..].starts_with(b"*/") {
                block_comment_depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(end) = character_end {
            if index == end {
                character_end = None;
            }
            index += 1;
            continue;
        }
        if let Some(hashes) = raw_string_hashes {
            if byte == b'"'
                && bytes
                    .get(index + 1..index + 1 + hashes)
                    .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
            {
                raw_string_hashes = None;
                index += hashes + 1;
            } else {
                index += 1;
            }
            continue;
        }
        if string {
            if string_escape {
                string_escape = false;
            } else if byte == b'\\' {
                string_escape = true;
            } else if byte == b'"' {
                string = false;
            }
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            block_comment_depth = 1;
            index += 2;
            continue;
        }
        if let Some((prefix_len, hashes)) = raw_string_start(&bytes[index..]) {
            line_has_code = true;
            raw_string_hashes = Some(hashes);
            index += prefix_len;
            continue;
        }
        if byte == b'\'' {
            line_has_code = true;
            character_end = character_literal_end(bytes, index);
            index += 1;
            continue;
        }
        if byte == b'"' {
            line_has_code = true;
            string = true;
            index += 1;
            continue;
        }
        if !byte.is_ascii_whitespace() {
            line_has_code = true;
        }
        index += 1;
    }

    code_lines + u64::from(line_has_code)
}

pub(crate) fn character_literal_end(bytes: &[u8], opening: usize) -> Option<usize> {
    let mut escaped = false;
    for (relative, byte) in bytes.get(opening + 1..)?.iter().copied().enumerate() {
        if byte == b'\n' {
            return None;
        }
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'\'' {
            return Some(opening + relative + 1);
        }
    }
    None
}

pub(crate) fn raw_string_start(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = usize::from(bytes.starts_with(b"br"));
    if index == 0 && !bytes.starts_with(b"r") {
        return None;
    }
    index += 1;
    let hash_start = index;
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'"')).then_some((index + 1, index - hash_start))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_sloc_splits_trailing_inline_tests() {
        let source = "fn live() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {}\n}\n";
        assert_eq!(rust_sloc(source), 6);
        let test_region = 2..7;
        assert_eq!(
            split_rust_sloc(source, std::slice::from_ref(&test_region)),
            (1, 5)
        );
    }

    #[test]
    fn rust_sloc_splits_multiple_inline_test_regions() {
        let source =
            "fn one() {}\n#[cfg(test)]\nmod first {}\nfn two() {}\n#[cfg(test)]\nmod second {}\n";
        assert_eq!(split_rust_sloc(source, &[2..4, 5..7]), (2, 4));
    }

    #[test]
    fn rust_sloc_ignores_comments_and_understands_nested_and_raw_literals() {
        let source = r####"
// comment
fn live() { /* comment
    /* nested */
*/ }
const URL: &str = "https://example.com/a//b";
const RAW: &str = r###"not /* a comment */
still code"###;
const QUOTE: char = '"';
// not code after the quote character

fn after_quote() {}
"####;
        assert_eq!(rust_sloc(source), 7);
    }
}
