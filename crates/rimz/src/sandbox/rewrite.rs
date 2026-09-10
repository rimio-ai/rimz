//! Immutable, content-addressed copies carrying provider user-only invocation markers.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::agents::ManualSkill;

use super::SandboxErr;

struct Entry {
    path: PathBuf,
    permissions: fs::Permissions,
    bytes: Option<Vec<u8>>,
}

pub(super) fn materialize(
    skills_dir: &Path,
    source: &Path,
    kind: ManualSkill,
) -> Result<PathBuf, SandboxErr> {
    super::validate_path(skills_dir)?;
    let mut entries = Vec::new();
    collect(source, Path::new(""), &mut Vec::new(), &mut entries)
        .map_err(|error| io_error(source, error))?;
    let target = skills_dir.join(digest(&entries, kind));
    crate::disk::paths::ensure_private_runtime_dir(skills_dir)?;
    if target.is_dir() {
        return Ok(target);
    }
    let temp = skills_dir.join(format!(".{}", uuid::Uuid::now_v7()));
    crate::disk::paths::ensure_private_runtime_dir(&temp)?;
    let result = (|| {
        for entry in &entries {
            let path = temp.join(&entry.path);
            match &entry.bytes {
                Some(bytes) => fs::write(&path, bytes),
                None => fs::create_dir(&path),
            }
            .map_err(|error| io_error(&path, error))?;
        }
        rewrite(&temp, source, kind)?;
        for entry in entries.iter().rev() {
            let path = temp.join(&entry.path);
            let mut permissions = entry.permissions.clone();
            if entry.bytes.is_none() {
                permissions.set_mode(permissions.mode() | 0o700);
            }
            fs::set_permissions(&path, permissions).map_err(|error| io_error(&path, error))?;
        }
        match fs::rename(&temp, &target) {
            Ok(()) => Ok(target.clone()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                ) && target.is_dir() =>
            {
                Ok(target.clone())
            }
            Err(error) => Err(io_error(&target, error)),
        }
    })();
    if temp.exists() {
        fs::remove_dir_all(&temp).map_err(|error| io_error(&temp, error))?;
    }
    result
}

fn io_error(path: &Path, source: io::Error) -> SandboxErr {
    SandboxErr::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn collect(
    source: &Path,
    relative: &Path,
    ancestors: &mut Vec<PathBuf>,
    entries: &mut Vec<Entry>,
) -> io::Result<()> {
    let canonical = source.canonicalize()?;
    if ancestors.contains(&canonical) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "skill contains a directory symlink cycle",
        ));
    }
    ancestors.push(canonical);
    let mut children = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "skipping broken skill symlink");
                continue;
            }
            Err(error) => return Err(error),
        };
        let relative = relative.join(child.file_name());
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "skill contains a non-file entry",
            ));
        }
        entries.push(Entry {
            path: relative.clone(),
            permissions: metadata.permissions(),
            bytes: if metadata.is_file() {
                Some(fs::read(&path)?)
            } else {
                None
            },
        });
        if metadata.is_dir() {
            collect(&path, &relative, ancestors, entries)?;
        }
    }
    ancestors.pop();
    Ok(())
}

fn digest(entries: &[Entry], kind: ManualSkill) -> String {
    let mut hash = Sha256::new();
    hash.update(match kind {
        ManualSkill::Unsupported => b"unsupported-v1".as_slice(),
        ManualSkill::Frontmatter => b"frontmatter-v2".as_slice(),
        ManualSkill::OpenAiPolicy => b"openai-policy-v2".as_slice(),
    });
    for entry in entries {
        let path = entry.path.as_os_str().as_encoded_bytes();
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path);
        hash.update(entry.permissions.mode().to_le_bytes());
        hash.update([u8::from(entry.bytes.is_some())]);
        if let Some(bytes) = &entry.bytes {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
    }
    hex::encode(hash.finalize())
}

fn rewrite(root: &Path, source: &Path, kind: ManualSkill) -> Result<(), SandboxErr> {
    let metadata = match kind {
        ManualSkill::Frontmatter => "SKILL.md",
        ManualSkill::OpenAiPolicy => "agents/openai.yaml",
        ManualSkill::Unsupported => return Ok(()),
    };
    let path = root.join(metadata);
    let text = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error(&path, error)),
    };
    let edited = match kind {
        ManualSkill::Frontmatter => {
            let Some(text) = text else {
                return Ok(());
            };
            frontmatter_user_only(&text)
        }
        ManualSkill::OpenAiPolicy => openai_policy_user_only(text.as_deref()),
        ManualSkill::Unsupported => return Ok(()),
    }
    .map_err(|reason| SandboxErr::SkillMetadata {
        path: source.join(metadata),
        reason,
    })?;
    if kind == ManualSkill::OpenAiPolicy {
        let agents = root.join("agents");
        fs::create_dir_all(&agents).map_err(|error| io_error(&agents, error))?;
    }
    fs::write(&path, edited).map_err(|error| io_error(&path, error))
}

fn mapping_key(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_matches([' ', '\t', '\r', '\n']);
    let (key, value) = if line.starts_with(['\'', '"']) {
        let end = quoted_end(line)?;
        let key = &line[1..end - 1];
        let value = line[end..]
            .trim_start_matches([' ', '\t'])
            .strip_prefix(':')?;
        (key, value)
    } else {
        let (key, value) = line.split_once(':')?;
        (key.trim_matches([' ', '\t']), value)
    };
    if key.contains('\\') || (!value.is_empty() && !value.starts_with([' ', '\t', '\r', '\n'])) {
        return None;
    }
    Some((key, value.trim_matches([' ', '\t', '\r', '\n'])))
}

fn quoted_end(text: &str) -> Option<usize> {
    let quote = text.as_bytes()[0];
    let mut bytes = text.bytes().enumerate().skip(1).peekable();
    while let Some((index, byte)) = bytes.next() {
        if quote == b'"' && byte == b'\\' {
            bytes.next();
            continue;
        }
        if byte != quote {
            continue;
        }
        if quote == b'\'' && bytes.peek().is_some_and(|(_, next)| *next == quote) {
            bytes.next();
            continue;
        }
        return Some(index + 1);
    }
    None
}

fn validate_lines(text: &str) -> Result<(), &'static str> {
    let mut block_depth = None;
    for line in text.lines().filter(|line| meaningful(line)) {
        let depth = indentation(line).len();
        if block_depth.is_some_and(|block| depth > block) {
            continue;
        }
        block_depth = None;
        let mut node = line.trim();
        let mut node_depth = depth;
        let mut scalar_depth = depth;
        while let Some(item) = node
            .strip_prefix('-')
            .filter(|item| item.starts_with(char::is_whitespace))
        {
            scalar_depth = node_depth;
            node_depth += node.len() - item.trim_start().len();
            node = item.trim_start();
        }
        if node.starts_with(['&', '*', '!', '{', '[', '?']) {
            return Err(
                "skill metadata must use untagged block mappings without anchors or aliases",
            );
        }
        if let Some((_, value)) = mapping_key(node) {
            node = value;
        } else {
            node_depth = scalar_depth;
        }
        if node.starts_with(['&', '*', '!', '{', '[']) {
            return Err(
                "skill metadata must use untagged block mappings without anchors or aliases",
            );
        }
        if node.starts_with(['\'', '"']) {
            let Some(end) = quoted_end(node) else {
                return Err("skill metadata quoted scalars must fit on one line");
            };
            let rest = node[end..].trim_start();
            if !rest.is_empty() && !rest.starts_with('#') {
                return Err("skill metadata quoted scalars must fit on one line");
            }
        }
        if node.starts_with(['|', '>']) {
            block_depth = Some(node_depth);
        }
    }
    Ok(())
}

fn meaningful(line: &str) -> bool {
    !line.trim().is_empty() && !line.trim_start().starts_with('#')
}

fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn frontmatter_user_only(text: &str) -> Result<String, &'static str> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end() != "---" {
        return Ok(format!("---\ndisable-model-invocation: true\n---\n{text}"));
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim_end() == "---" {
            let block = &text[first.len()..offset];
            let indent = block
                .lines()
                .find(|line| meaningful(line))
                .map_or("", indentation);
            let block = set_block_key(block, "disable-model-invocation", "true", indent)?;
            return Ok(format!("{first}{block}{}", &text[offset..]));
        }
        offset += line.len();
    }
    Err("SKILL.md frontmatter has no closing delimiter")
}

fn set_block_key(text: &str, key: &str, value: &str, indent: &str) -> Result<String, &'static str> {
    validate_lines(text)?;
    if indent.contains('\t') {
        return Err("skill metadata indentation must use spaces");
    }
    let mut output = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        if !meaningful(line) {
            output.push_str(line);
            continue;
        }
        let depth = indentation(line).len();
        if depth == indent.len() {
            let Some((name, _)) = mapping_key(line) else {
                return Err("skill metadata must use a block mapping");
            };
            if line.trim_start().starts_with(['{', '[', '-', '?']) {
                return Err("skill metadata must use a block mapping");
            }
            skipping = name == key;
        } else if depth < indent.len() {
            return Err("inconsistent skill metadata indentation");
        }
        if !skipping {
            output.push_str(line);
        }
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&format!("{indent}{key}: {value}\n"));
    Ok(output)
}

fn openai_policy_user_only(text: Option<&str>) -> Result<String, &'static str> {
    let text = text.unwrap_or("");
    validate_lines(text)?;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let root_indent = lines
        .iter()
        .find(|line| meaningful(line))
        .map_or("", |line| indentation(line));
    let mut policy = None;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate() {
        if !meaningful(line) || indentation(line).len() > root_indent.len() {
            continue;
        }
        if indentation(line) != root_indent || root_indent.contains('\t') {
            return Err("inconsistent skill metadata indentation");
        }
        let Some((key, value)) = mapping_key(line) else {
            return Err("openai.yaml must use a single block mapping");
        };
        if line.trim_start().starts_with(['{', '[', '-', '?']) {
            return Err("openai.yaml must use a single block mapping");
        }
        if policy.is_some() && end == lines.len() {
            end = index;
        }
        if key != "policy" {
            continue;
        }
        if policy.is_some() || (!value.is_empty() && !value.starts_with('#')) {
            return Err("rewrite policy as a single block mapping");
        }
        policy = Some(index);
    }
    let Some(start) = policy else {
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return Ok(format!(
            "{text}{separator}{root_indent}policy:\n{root_indent}  allow_implicit_invocation: false\n"
        ));
    };
    let block = &lines[start + 1..end];
    let default_indent = format!("{root_indent}  ");
    let indent = block
        .iter()
        .find(|line| meaningful(line))
        .map_or(default_indent.as_str(), |line| indentation(line));
    if indent.len() <= root_indent.len() || indent.contains('\t') {
        return Err("policy must contain indented mapping keys");
    }
    let edited = set_block_key(
        &block.concat(),
        "allow_implicit_invocation",
        "false",
        indent,
    )?;
    let mut output = lines[..=start].concat();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&edited);
    output.push_str(&lines[end..].concat());
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_marker_preserves_body_and_nested_keys() {
        for header in [
            "",
            "disable-model-invocation: false\n",
            "'disable-model-invocation': false\n",
        ] {
            let text = format!(
                "---\nname: demo\n{header}metadata:\n  disable-model-invocation: false\n---\nBody\r\n"
            );
            assert_eq!(
                frontmatter_user_only(&text).unwrap(),
                "---\nname: demo\nmetadata:\n  disable-model-invocation: false\ndisable-model-invocation: true\n---\nBody\r\n"
            );
        }
        assert_eq!(
            frontmatter_user_only("Body").unwrap(),
            "---\ndisable-model-invocation: true\n---\nBody"
        );
        assert!(frontmatter_user_only("---\nname: incomplete").is_err());
        assert_eq!(
            frontmatter_user_only(
                "---\n  name: demo\n  disable-model-invocation: false\n---\nBody"
            )
            .unwrap(),
            "---\n  name: demo\n  disable-model-invocation: true\n---\nBody"
        );
    }

    #[test]
    fn openai_marker_changes_only_policy() {
        assert_eq!(
            openai_policy_user_only(None).unwrap(),
            "policy:\n  allow_implicit_invocation: false\n"
        );
        assert_eq!(
            openai_policy_user_only(Some("interface:\n  display_name: Demo")).unwrap(),
            "interface:\n  display_name: Demo\npolicy:\n  allow_implicit_invocation: false\n"
        );
        for policy in [
            "",
            "  allow_implicit_invocation: true\n",
            "  'allow_implicit_invocation': true\n",
        ] {
            let text = format!(
                "interface:\n  allow_implicit_invocation: true\npolicy:\n{policy}  extra: keep\nnext: value\n"
            );
            assert_eq!(
                openai_policy_user_only(Some(&text)).unwrap(),
                "interface:\n  allow_implicit_invocation: true\npolicy:\n  extra: keep\n  allow_implicit_invocation: false\nnext: value\n"
            );
        }
        assert_eq!(
            openai_policy_user_only(Some("policy:")).unwrap(),
            "policy:\n  allow_implicit_invocation: false\n"
        );
        assert_eq!(
            openai_policy_user_only(Some("  policy:\n    allow_implicit_invocation: true\n"))
                .unwrap(),
            "  policy:\n    allow_implicit_invocation: false\n"
        );
    }

    #[test]
    fn openai_marker_refuses_ambiguous_yaml() {
        for text in [
            "policy: {allow_implicit_invocation: true}",
            "policy: *alias",
            "policy: |\n  text\n",
            "policy: true",
            "policy:\n  - item\n",
            "policy:\npolicy:\n",
            "{policy: {allow_implicit_invocation: true}}",
            "---\npolicy:\n",
            "\"pol\\u0069cy\": {allow_implicit_invocation: true}",
        ] {
            assert!(openai_policy_user_only(Some(text)).is_err(), "{text}");
        }
    }

    #[test]
    fn markers_refuse_multiline_quoted_scalars_with_fake_keys() {
        for quote in ['\'', '"'] {
            let text = format!(
                "interface:\n  short_description: {quote}foo\npolicy:\n  allow_implicit_invocation: true\nend: bar{quote}\n"
            );
            assert!(openai_policy_user_only(Some(&text)).is_err());
            let text = format!(
                "---\ndescription: {quote}foo\ndisable-model-invocation: false\nend: bar{quote}\n---\nBody"
            );
            assert!(frontmatter_user_only(&text).is_err());
            let text = format!(
                "interface:\n  descriptions:\n    - {quote}foo\npolicy:\n  allow_implicit_invocation: true\nend: bar{quote}\n"
            );
            assert!(openai_policy_user_only(Some(&text)).is_err());
            let text = text.replace("- ", "-\t");
            assert!(openai_policy_user_only(Some(&text)).is_err());
            let text = format!(
                "entries:\n  - description: |\n      block text\n    quoted: {quote}foo\npolicy:\n  allow_implicit_invocation: true\nend: bar{quote}\n"
            );
            assert!(openai_policy_user_only(Some(&text)).is_err());
            let text = format!(
                "entries:\n  - - |\n        block text\n    - {quote}foo\npolicy:\n  allow_implicit_invocation: true\nend: bar{quote}\n"
            );
            assert!(openai_policy_user_only(Some(&text)).is_err());
        }
    }

    #[test]
    fn markers_preserve_quoted_keys_with_colons_and_spaces() {
        let fields = "policy\u{a0}:\n  allow_implicit_invocation: true\n";
        assert_eq!(
            openai_policy_user_only(Some(fields)).unwrap(),
            format!("{fields}policy:\n  allow_implicit_invocation: false\n")
        );
        let fields = "disable-model-invocation\u{a0}: false\n";
        assert_eq!(
            frontmatter_user_only(&format!("---\n{fields}---\nBody")).unwrap(),
            format!("---\n{fields}disable-model-invocation: true\n---\nBody")
        );
        for quote in ['\'', '"'] {
            let fields = format!(
                "{quote}disable-model-invocation:extra{quote}: keep\n{quote}disable-model-invocation {quote}: keep\n"
            );
            assert_eq!(
                frontmatter_user_only(&format!("---\n{fields}---\nBody")).unwrap(),
                format!("---\n{fields}disable-model-invocation: true\n---\nBody")
            );
            let fields = format!(
                "{quote}policy:extra{quote}: keep\npolicy:\n  {quote}allow_implicit_invocation:extra{quote}: keep\n"
            );
            assert_eq!(
                openai_policy_user_only(Some(&fields)).unwrap(),
                format!("{fields}  allow_implicit_invocation: false\n")
            );
        }
    }

    #[test]
    fn markers_refuse_anchors_before_removing_alias_targets() {
        for block in [
            "disable-model-invocation: &flag false\ncustom: *flag\n",
            "disable-model-invocation:\n  nested: &flag false\ncustom: *flag\n",
        ] {
            assert!(frontmatter_user_only(&format!("---\n{block}---\n")).is_err());
        }
        for text in [
            "policy:\n  allow_implicit_invocation: &flag true\ncustom: *flag\n",
            "policy:\n  allow_implicit_invocation:\n    nested: &flag true\ncustom: *flag\n",
            "policy:\n  allow_implicit_invocation: !!bool &flag true\ncustom: *flag\n",
            "policy:\n  allow_implicit_invocation: {nested: &flag true}\ncustom: *flag\n",
        ] {
            assert!(openai_policy_user_only(Some(text)).is_err(), "{text}");
        }
    }

    #[test]
    fn markers_preserve_descriptions_without_interpreting_scalar_content() {
        for description in [
            "It's a user's tool\n",
            "'It''s a user''s tool' # keep\n",
            "\"It's a \\\"quoted\\\" tool\"\n",
            "|-\n  It's an unclosed \"quote\n  policy:\n    allow_implicit_invocation: true\n  &flag *alias\n",
            ">+\n  'unclosed quote\n  disable-model-invocation: false\n",
        ] {
            let fields = format!("name: demo\ndescription: {description}");
            assert_eq!(
                frontmatter_user_only(&format!("---\n{fields}---\nBody")).unwrap(),
                format!("---\n{fields}disable-model-invocation: true\n---\nBody")
            );
            assert_eq!(
                openai_policy_user_only(Some(&fields)).unwrap(),
                format!("{fields}policy:\n  allow_implicit_invocation: false\n")
            );
        }
        let text = "policy:\n  description: |\n    allow_implicit_invocation: true\n    \"unclosed quote\n  allow_implicit_invocation: true\n";
        assert_eq!(
            openai_policy_user_only(Some(text)).unwrap(),
            "policy:\n  description: |\n    allow_implicit_invocation: true\n    \"unclosed quote\n  allow_implicit_invocation: false\n"
        );
    }

    #[test]
    fn skill_digest_covers_content_kind_and_permissions() {
        let mut entries = vec![Entry {
            path: "SKILL.md".into(),
            permissions: fs::Permissions::from_mode(0o644),
            bytes: Some(b"body".to_vec()),
        }];
        let original = digest(&entries, ManualSkill::Frontmatter);
        assert_eq!(original, digest(&entries, ManualSkill::Frontmatter));
        assert_ne!(original, digest(&entries, ManualSkill::OpenAiPolicy));
        entries[0].bytes = Some(b"changed".to_vec());
        assert_ne!(original, digest(&entries, ManualSkill::Frontmatter));
        entries[0].bytes = Some(b"body".to_vec());
        entries[0].permissions = fs::Permissions::from_mode(0o755);
        assert_ne!(original, digest(&entries, ManualSkill::Frontmatter));
    }
}
