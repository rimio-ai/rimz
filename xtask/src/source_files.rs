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

pub(crate) fn is_test_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
        || path
            .components()
            .any(|component| component.as_os_str() == OsStr::new("tests"))
}

pub(crate) fn inline_test_marker_line(source: &str) -> Option<u64> {
    let mut offset = 0;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        if line.trim() == "#[cfg(test)]" && trailing_test_region(source, offset) {
            return Some(index as u64 + 1);
        }
        offset += line.len();
    }
    None
}

pub(crate) fn split_file_loc(sloc: f64, inline_test_marker: Option<u64>) -> (f64, f64) {
    let Some(marker) = inline_test_marker else {
        return (sloc, 0.0);
    };
    let code_sloc = marker.saturating_sub(1) as f64;
    (code_sloc, (sloc - code_sloc).max(0.0))
}

fn trailing_test_region(source: &str, marker_offset: usize) -> bool {
    let mut offset = marker_offset;
    loop {
        let Some(after_cfg) = consume_cfg_test_line(source, offset) else {
            return false;
        };
        offset = skip_blank_and_attribute_lines(source, after_cfg);
        let Some(after_module) = consume_mod_item(source, offset) else {
            return false;
        };
        offset = skip_trivia(source, after_module);
        if offset == source.len() {
            return true;
        }
        if !source[offset..].starts_with("#[cfg(test)]") {
            return false;
        }
    }
}

fn consume_cfg_test_line(source: &str, offset: usize) -> Option<usize> {
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |end| offset + end + 1);
    (source[offset..line_end].trim() == "#[cfg(test)]").then_some(line_end)
}

fn skip_blank_and_attribute_lines(source: &str, mut offset: usize) -> usize {
    loop {
        let line_end = source[offset..]
            .find('\n')
            .map_or(source.len(), |end| offset + end + 1);
        let trimmed = source[offset..line_end].trim();
        if trimmed.is_empty() || (trimmed.starts_with("#[") && trimmed.ends_with(']')) {
            offset = line_end;
            if offset == source.len() {
                return offset;
            }
        } else {
            return offset;
        }
    }
}

fn consume_mod_item(source: &str, offset: usize) -> Option<usize> {
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |end| offset + end + 1);
    let line = source[offset..line_end].trim();
    let declaration = strip_visibility(line).strip_prefix("mod ")?;
    let delimiter = declaration.find(['{', ';'])?;
    let name = declaration[..delimiter].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        return None;
    }
    match declaration.as_bytes()[delimiter] {
        b';' => (declaration[delimiter + 1..].trim().is_empty()).then_some(line_end),
        b'{' => {
            let brace = source[offset..line_end].find('{')? + offset;
            matching_closing_brace(source, brace).map(|closing| closing + 1)
        }
        _ => None,
    }
}

fn strip_visibility(line: &str) -> &str {
    let line = line.strip_prefix("pub ").unwrap_or(line);
    if let Some(rest) = line.strip_prefix("pub(")
        && let Some(end) = rest.find(')')
    {
        return rest[end + 1..].trim_start();
    }
    line
}

fn skip_trivia(source: &str, mut offset: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if bytes
            .get(offset..)
            .is_some_and(|rest| rest.starts_with(b"//"))
        {
            offset = source[offset..]
                .find('\n')
                .map_or(source.len(), |end| offset + end + 1);
        } else if bytes
            .get(offset..)
            .is_some_and(|rest| rest.starts_with(b"/*"))
        {
            let Some(end) = source[offset + 2..].find("*/") else {
                return source.len();
            };
            offset += end + 4;
        } else {
            return offset;
        }
    }
}

fn matching_closing_brace(source: &str, opening: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0_u32;
    let mut block_comment_depth = 0_u32;
    let mut line_comment = false;
    let mut string = false;
    let mut string_escape = false;
    let mut raw_string_hashes = None;
    let mut character_end = None;
    let mut index = opening;
    while index < bytes.len() {
        let byte = bytes[index];
        if line_comment {
            line_comment = byte != b'\n';
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
            line_comment = true;
            index += 2;
        } else if bytes[index..].starts_with(b"/*") {
            block_comment_depth = 1;
            index += 2;
        } else if let Some((prefix_len, hashes)) = raw_string_start(&bytes[index..]) {
            raw_string_hashes = Some(hashes);
            index += prefix_len;
        } else if byte == b'"' {
            string = true;
            index += 1;
        } else if byte == b'\'' {
            character_end = character_literal_end(bytes, index);
            index += 1;
        } else if byte == b'{' {
            depth += 1;
            index += 1;
        } else if byte == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
            index += 1;
        } else {
            index += 1;
        }
    }
    None
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
