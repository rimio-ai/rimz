//! Comment-preserving editing for the per-machine config set.

use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use crate::store::atomic::write_bytes_atomically;

use super::{
    AnimationRole, ConfigFileDiagnosis, GlyphRole, MachineConfig, MachineConfigFile,
    MachineConfigFileKind, MachineConfigFiles, is_named_glyph_set, validate_glyph_cells,
    validate_glyph_source,
};

type Result<T> = std::result::Result<T, ConfigEditErr>;

macro_rules! invalid_value {
    ($($arg:tt)*) => {
        return Err(ConfigEditErr::InvalidValue(format!($($arg)*)))
    };
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigEditErr {
    #[error("loading per-machine config: {source}")]
    Load {
        #[source]
        source: Box<super::ConfigErr>,
    },
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot edit {path} — the file has a TOML error")]
    DocumentParse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
    },
    #[error("parsing shipped config template: {source}")]
    TemplateParse {
        #[source]
        source: toml_edit::TomlError,
    },
    #[error("serializing per-machine config: {source}")]
    Serialize {
        #[source]
        source: toml::ser::Error,
    },
    #[error("invalid value {value} for `{key}`: {message}")]
    Validate {
        key: String,
        value: String,
        message: String,
    },
    #[error("cannot set `{key}`: the existing config is invalid")]
    ExistingInvalid {
        key: String,
        #[source]
        source: Box<super::ConfigErr>,
    },
    #[error("validating merged {path}: {source}")]
    ValidateMerged {
        path: PathBuf,
        #[source]
        source: Box<super::ConfigErr>,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: crate::store::atomic::AtomicErr,
    },
    #[error("config key `{key}` is unset")]
    UnsetKey { key: String },
    #[error("unknown config key `{key}`")]
    UnknownKey { key: String },
    #[error("{0}")]
    InvalidKey(String),
    #[error("{0}")]
    InvalidValue(String),
    #[error("`{segment}` is not a table")]
    DocumentShape { segment: String },
}

#[derive(Clone, Debug)]
pub struct ConfigEditor {
    files: MachineConfigFiles,
}

impl ConfigEditor {
    pub fn machine() -> Self {
        Self::new(MachineConfigFiles::machine())
    }

    pub fn new(files: MachineConfigFiles) -> Self {
        Self { files }
    }

    pub fn files(&self) -> &MachineConfigFiles {
        &self.files
    }

    pub fn get(&self, key: Option<&str>) -> Result<toml::Value> {
        let config = MachineConfig::load_from(self.files.core_path(), self.files.agents_home())
            .map_err(|source| ConfigEditErr::Load {
                source: Box::new(source),
            })?;
        let root = config
            .to_toml_value()
            .map_err(|source| ConfigEditErr::Serialize { source })?;
        let Some(key) = key else {
            return Ok(root);
        };
        let parsed = parse_key(key)?;
        match value_at(&root, key) {
            Some(value) => Ok(value.clone()),
            None if is_known_get_key(&self.files, &parsed)? => Err(ConfigEditErr::UnsetKey {
                key: key.to_owned(),
            }),
            None => Err(ConfigEditErr::UnknownKey {
                key: key.to_owned(),
            }),
        }
    }

    pub fn set(&self, key: &str, raw_value: &str) -> Result<()> {
        let requested_key = parse_key(key)?;
        let value = parse_set_value(&requested_key, raw_value);
        let key = normalize_set_key(&requested_key, &value)?;
        validate_set_key(&self.files, &key)?;
        let file = file_for_key(&self.files, &key);
        let text = read_config_or_template(file.path(), file.template())?;
        let mut doc =
            text.parse::<DocumentMut>()
                .map_err(|source| ConfigEditErr::DocumentParse {
                    path: file.path().to_path_buf(),
                    diagnosis: Box::new(ConfigFileDiagnosis::from_toml_edit(
                        file.path(),
                        &text,
                        &source,
                    )),
                })?;
        let doc_key = document_key_for_set(&key);
        if item_at(&doc, &doc_key).is_none()
            && let Some(uncommented) = uncomment_template_default(&text, &doc_key)
            && let Ok(uncommented) = uncommented.parse::<DocumentMut>()
        {
            doc = uncommented;
        }
        apply_logical_key(
            &mut doc,
            file.path(),
            &key,
            value,
            self.files.agents_home(),
            self.files.core_path(),
        )?;
        write(file.path(), doc.to_string().as_bytes())
    }

    pub fn write_defaults(&self, force: bool) -> Result<bool> {
        let files = self.files.ordered();
        if !force && files.iter().any(|file| file.path().exists()) {
            return Ok(false);
        }
        for file in files {
            write(file.path(), file.template().as_bytes())?;
        }
        Ok(true)
    }

    pub fn merge_defaults(&self) -> Result<MergeReport> {
        let mut outcomes = Vec::new();
        for kind in MachineConfigFileKind::ALL {
            outcomes.push(self.merge_one(kind)?);
        }
        Ok(MergeReport { files: outcomes })
    }

    fn merge_one(&self, kind: MachineConfigFileKind) -> Result<FileMergeOutcome> {
        let file = self.files.file(kind);
        let path = file.path();
        let Some(old_text) = read_existing(path)? else {
            validate_merged_text(path, file.template(), self.files.agents_home())?;
            write(path, file.template().as_bytes())?;
            return Ok(FileMergeOutcome {
                path: path.to_path_buf(),
                action: MergeAction::Wrote,
                skipped: Vec::new(),
            });
        };
        let old_doc = match old_text.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(error) => {
                return Ok(FileMergeOutcome {
                    path: path.to_path_buf(),
                    action: MergeAction::LeftUnparseable {
                        diagnosis: ConfigFileDiagnosis::from_toml_edit(path, &old_text, &error),
                    },
                    skipped: Vec::new(),
                });
            }
        };
        let mut new_doc = file
            .template()
            .parse::<DocumentMut>()
            .map_err(|source| ConfigEditErr::TemplateParse { source })?;
        let pending = collect_explicit_keys(kind, &old_doc)
            .into_iter()
            .filter(|pending| !template_has_same_value(&new_doc, &pending.logical, &pending.value))
            .collect();
        let mut skipped = Vec::new();
        let kept = apply_merge_keys(
            path,
            &mut new_doc,
            pending,
            &mut skipped,
            self.files.agents_home(),
            self.files.core_path(),
        );
        let rendered = new_doc.to_string();
        validate_merged_text(path, &rendered, self.files.agents_home())?;
        write(path, rendered.as_bytes())?;
        Ok(FileMergeOutcome {
            path: path.to_path_buf(),
            action: MergeAction::Merged { kept },
            skipped,
        })
    }
}

#[derive(Debug)]
pub struct MergeReport {
    pub files: Vec<FileMergeOutcome>,
}

#[derive(Debug)]
pub struct FileMergeOutcome {
    pub path: PathBuf,
    pub action: MergeAction,
    pub skipped: Vec<SkippedKey>,
}

#[derive(Debug)]
pub enum MergeAction {
    Wrote,
    Merged { kept: usize },
    LeftUnparseable { diagnosis: ConfigFileDiagnosis },
}

#[derive(Debug)]
pub struct SkippedKey {
    pub key: String,
    pub reason: String,
}

fn validate_merged_text(path: &Path, text: &str, agents_home: &Path) -> Result<()> {
    MachineConfig::parse_text(path, text, agents_home)
        .map(|_| ())
        .map_err(|source| ConfigEditErr::ValidateMerged {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
}

fn apply_logical_key(
    doc: &mut DocumentMut,
    path: &Path,
    logical: &[String],
    value: Value,
    agents_home: &Path,
    core_path: &Path,
) -> Result<()> {
    validate_set_value(logical, &value)?;
    let value_display = value.to_string().trim().to_owned();
    let pre_image = doc.to_string();
    set_document_value(doc, &document_key_for_set(logical), value)?;
    reject_unknown_set_key(path, logical, doc, core_path)?;
    match MachineConfig::parse_text(path, &doc.to_string(), agents_home) {
        Ok(_) => Ok(()),
        Err(source) => match MachineConfig::parse_text(path, &pre_image, agents_home) {
            Ok(_) => Err(ConfigEditErr::Validate {
                key: logical.join("."),
                value: value_display,
                message: source.validation_message(),
            }),
            Err(source) => Err(ConfigEditErr::ExistingInvalid {
                key: logical.join("."),
                source: Box::new(source),
            }),
        },
    }
}

fn read_existing(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigEditErr::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_atomically(path, bytes).map_err(|source| ConfigEditErr::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug)]
struct PendingKey {
    logical: Vec<String>,
    value: Value,
}

fn collect_explicit_keys(kind: MachineConfigFileKind, doc: &DocumentMut) -> Vec<PendingKey> {
    let mut found = Vec::new();
    walk_table(kind, &[], doc.as_table(), &mut found);
    found
}

fn apply_merge_keys(
    path: &Path,
    doc: &mut DocumentMut,
    keys: Vec<PendingKey>,
    skipped: &mut Vec<SkippedKey>,
    agents_home: &Path,
    core_path: &Path,
) -> usize {
    let mut kept = 0;
    let mut pending = keys;
    while !pending.is_empty() {
        let mut progressed = false;
        let mut next = Vec::new();
        for PendingKey { logical, value } in pending {
            let doc_key = document_key_for_set(&logical);
            let mut trial = if item_at(doc, &doc_key).is_none() {
                uncomment_template_default(&doc.to_string(), &doc_key)
                    .and_then(|text| text.parse::<DocumentMut>().ok())
                    .unwrap_or_else(|| doc.clone())
            } else {
                doc.clone()
            };
            match apply_logical_key(
                &mut trial,
                path,
                &logical,
                value.clone(),
                agents_home,
                core_path,
            ) {
                Ok(()) => {
                    *doc = trial;
                    kept += 1;
                    progressed = true;
                }
                Err(err) => next.push((PendingKey { logical, value }, err.to_string())),
            }
        }
        if !progressed {
            skipped.extend(next.into_iter().map(|(key, err)| SkippedKey {
                key: key.logical.join("."),
                reason: err,
            }));
            break;
        }
        pending = next.into_iter().map(|(key, _)| key).collect();
    }
    kept
}

fn uncomment_template_default(text: &str, doc_key: &[String]) -> Option<String> {
    let (leaf, parents) = doc_key.split_last()?;
    let mut section = Vec::new();
    let mut inside_commented_table = false;
    let mut rendered = String::with_capacity(text.len());
    let mut matched = false;

    for line in text.split_inclusive('\n') {
        let (content, ending) = if let Some(content) = line.strip_suffix("\r\n") {
            (content, "\r\n")
        } else if let Some(content) = line.strip_suffix('\n') {
            (content, "\n")
        } else {
            (line, "")
        };
        let trimmed = content.trim_start();
        let uncommented = trimmed
            .strip_prefix('#')
            .map(|line| line.trim_start_matches('#'))
            .map(|line| line.strip_prefix(' ').unwrap_or(line));

        if let Some(header) = template_table_header(trimmed) {
            section = header.split('.').map(str::trim).collect();
            inside_commented_table = false;
        } else if uncommented.and_then(template_table_header).is_some() {
            inside_commented_table = true;
        }

        let target = uncommented.filter(|line| {
            !matched
                && !inside_commented_table
                && section
                    .iter()
                    .copied()
                    .eq(parents.iter().map(String::as_str))
                && line
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == leaf)
        });

        if let Some(uncommented) = target {
            rendered.push_str(&content[..content.len() - trimmed.len()]);
            rendered.push_str(uncommented);
            matched = true;
        } else {
            rendered.push_str(content);
        }
        rendered.push_str(ending);
    }

    matched.then_some(rendered)
}

fn template_table_header(line: &str) -> Option<&str> {
    let line = line
        .split_once('#')
        .map_or(line, |(before, _)| before)
        .trim();
    if !line.starts_with('[') || !line.ends_with(']') {
        return None;
    }
    let header = line.trim_start_matches('[').trim_end_matches(']');
    (!header.is_empty()).then_some(header)
}

fn walk_table(
    kind: MachineConfigFileKind,
    doc_prefix: &[String],
    table: &Table,
    out: &mut Vec<PendingKey>,
) {
    for (key, item) in table.iter() {
        let mut doc_path = doc_prefix.to_vec();
        doc_path.push(key.to_string());
        let logical = to_logical(kind, &doc_path);
        if is_context_meter_band(&logical) {
            if let Some(value) = item_to_value(item) {
                out.push(PendingKey { logical, value });
            }
        } else if let Some(value) = item.as_value()
            && let Some(inline) = value.as_inline_table()
        {
            walk_inline_table(kind, &doc_path, inline, out);
        } else if let Some(child) = item.as_table() {
            walk_table(kind, &doc_path, child, out);
        } else if let Some(value) = item_to_value(item) {
            out.push(PendingKey { logical, value });
        }
    }
}

fn walk_inline_table(
    kind: MachineConfigFileKind,
    doc_prefix: &[String],
    table: &InlineTable,
    out: &mut Vec<PendingKey>,
) {
    for (key, value) in table.iter() {
        let mut doc_path = doc_prefix.to_vec();
        doc_path.push(key.to_string());
        let logical = to_logical(kind, &doc_path);
        if is_context_meter_band(&logical) {
            out.push(PendingKey {
                logical,
                value: value.clone(),
            });
        } else if let Some(inline) = value.as_inline_table() {
            walk_inline_table(kind, &doc_path, inline, out);
        } else {
            out.push(PendingKey {
                logical,
                value: value.clone(),
            });
        }
    }
}

fn to_logical(kind: MachineConfigFileKind, doc_path: &[String]) -> Vec<String> {
    match kind {
        MachineConfigFileKind::Theme
            if doc_path.first().is_some_and(|segment| segment == "colors") =>
        {
            std::iter::once("theme".to_owned())
                .chain(doc_path.iter().cloned())
                .collect()
        }
        MachineConfigFileKind::Loop => std::iter::once("loop".to_owned())
            .chain(doc_path.iter().cloned())
            .collect(),
        _ => doc_path.to_vec(),
    }
}

fn is_context_meter_band(path: &[String]) -> bool {
    matches!(
        path,
        [root, display, meter, band]
            if root == "theme"
                && display == "display"
                && meter == "context_meter"
                && CONTEXT_METER_BANDS.contains(&band.as_str())
    )
}

fn item_to_value(item: &Item) -> Option<Value> {
    match item {
        Item::Value(value) => Some(value.clone()),
        Item::Table(table) => Some(Value::InlineTable(table_to_inline(table))),
        Item::ArrayOfTables(tables) => {
            let mut array = Array::new();
            for table in tables {
                array.push(Value::InlineTable(table_to_inline(table)));
            }
            Some(Value::Array(array))
        }
        Item::None => None,
    }
}

fn table_to_inline(table: &Table) -> InlineTable {
    let mut inline = InlineTable::new();
    for (key, item) in table.iter() {
        if let Some(value) = item_to_value(item) {
            inline.insert(key, value);
        }
    }
    inline
}

fn template_has_same_value(doc: &DocumentMut, logical: &[String], value: &Value) -> bool {
    let Some(existing) = item_at(doc, &document_key_for_set(logical))
        .cloned()
        .and_then(|item| item.into_value().ok())
    else {
        return false;
    };
    as_toml_value(&existing) == as_toml_value(value)
}

fn item_at<'a>(doc: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    let (leaf, parents) = path.split_last()?;
    let mut table = doc.as_table();
    for segment in parents {
        table = table.get(segment)?.as_table()?;
    }
    table.get(leaf)
}

fn as_toml_value(value: &Value) -> Option<toml::Value> {
    // A bare TOML value is not a document; wrap and reparse so equality ignores
    // toml_edit formatting/decor and compares the semantic value.
    toml::from_str::<toml::Table>(&format!("x = {}", value.to_string().trim()))
        .ok()?
        .remove("x")
}

fn file_for_key(files: &MachineConfigFiles, path: &[String]) -> MachineConfigFile {
    let kind = match path.first().map(String::as_str) {
        Some("theme") => MachineConfigFileKind::Theme,
        Some("agents") => MachineConfigFileKind::Agents,
        Some("loop") => MachineConfigFileKind::Loop,
        _ => MachineConfigFileKind::Core,
    };
    files.file(kind)
}

fn document_key_for_set(path: &[String]) -> Vec<String> {
    if matches!(path, [root, child, ..] if root == "theme" && child == "colors")
        || matches!(path, [root, ..] if root == "loop")
    {
        path[1..].to_vec()
    } else {
        path.to_vec()
    }
}

fn read_config_or_template(path: &Path, template: &str) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(template.to_owned()),
        Err(source) => Err(ConfigEditErr::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn value_at<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(root, |value, segment| match value {
        toml::Value::Table(table) => table.get(segment),
        _ => None,
    })
}

fn parse_key(key: &str) -> Result<Vec<String>> {
    let segments: Vec<String> = key.split('.').map(str::to_owned).collect();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(ConfigEditErr::InvalidKey(
            "config keys use non-empty dotted segments".to_owned(),
        ));
    }
    Ok(segments)
}

fn validate_set_key(files: &MachineConfigFiles, path: &[String]) -> Result<()> {
    reject_reserved_set_key(path, files.core_path())?;
    let file = file_for_key(files, path);
    let doc_key = document_key_for_set(path);
    let mut doc = file
        .template()
        .parse::<DocumentMut>()
        .map_err(|source| ConfigEditErr::TemplateParse { source })?;
    set_document_value(&mut doc, &doc_key, Value::from("__rimz_probe__"))?;
    reject_if_ignored(file.path(), path, &doc_key, &doc)
}

fn reject_unknown_set_key(
    path: &Path,
    logical: &[String],
    doc: &DocumentMut,
    core_path: &Path,
) -> Result<()> {
    reject_reserved_set_key(logical, core_path)?;
    reject_if_ignored(path, logical, &document_key_for_set(logical), doc)
}

fn reject_reserved_set_key(path: &[String], core_path: &Path) -> Result<()> {
    let joined = path.join(".");
    if matches!(path, [root, child, ..] if root == "notifications" && child == "handler") {
        return Err(ConfigEditErr::InvalidKey(format!(
            "config key `{joined}` is an array of tables; edit {}",
            core_path.display()
        )));
    }
    if is_context_meter_subfield(path) || is_disallowed_set_container(path) {
        return Err(ConfigEditErr::UnknownKey { key: joined });
    }
    Ok(())
}

fn reject_if_ignored(
    path: &Path,
    logical: &[String],
    doc_key: &[String],
    doc: &DocumentMut,
) -> Result<()> {
    let ignored = match MachineConfig::parse_text_unknown_keys(path, &doc.to_string()) {
        Ok(ignored) => ignored,
        Err(err) if err.validation_message().contains("unknown field") => {
            return Err(ConfigEditErr::UnknownKey {
                key: logical.join("."),
            });
        }
        Err(_) => return Ok(()),
    };
    let document_key = doc_key.join(".");
    if ignored_path_matches(&ignored, &document_key) {
        return Err(ConfigEditErr::UnknownKey {
            key: logical.join("."),
        });
    }
    Ok(())
}

fn is_known_get_key(files: &MachineConfigFiles, path: &[String]) -> Result<bool> {
    if is_context_meter_subfield(path) || is_unknown_get_shape(path) {
        return Ok(false);
    }
    let file = file_for_key(files, path);
    let doc_key = document_key_for_set(path);
    if doc_key.is_empty() {
        return Ok(true);
    }
    let mut doc = file
        .template()
        .parse::<DocumentMut>()
        .map_err(|source| ConfigEditErr::TemplateParse { source })?;
    set_document_value(&mut doc, &doc_key, Value::from("__rimz_probe__"))?;
    let ignored = match MachineConfig::parse_text_unknown_keys(file.path(), &doc.to_string()) {
        Ok(ignored) => ignored,
        Err(err) if err.validation_message().contains("unknown field") => return Ok(false),
        Err(_) => return Ok(true),
    };
    let document_key = doc_key.join(".");
    Ok(!ignored_path_matches(&ignored, &document_key))
}

fn is_unknown_get_shape(path: &[String]) -> bool {
    matches!(path, [root, child, _, ..] if root == "accounts" && child == "usage_limit_usd" && path.len() > 3)
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "budget" && path.len() > 3)
        || matches!(path, [root, child, _, ..] if root == "agents" && child == "commands" && path.len() > 3)
        || matches!(
            path,
            [root, child, role]
                if root == "theme"
                    && child == "animations"
                    && role != "unread"
                    && !is_animation_role(role)
        )
        || matches!(
            path,
            [root, child, role, field]
                if root == "theme"
                    && child == "animations"
                    && !(is_animation_role(role)
                        && ANIMATION_FIELDS.contains(&field.as_str()))
        )
        || matches!(path, [root, child, _, _, ..] if root == "theme" && child == "animations" && path.len() > 4)
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_named_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_none()
        )
        || matches!(path, [root, child, set, _, _, ..] if root == "theme" && child == "glyphs" && is_named_glyph_set(set) && path.len() > 5)
}

fn ignored_path_matches(ignored: &[String], document_key: &str) -> bool {
    ignored
        .iter()
        .any(|ignored| document_key == ignored || document_key.starts_with(&format!("{ignored}.")))
}

fn is_context_meter_subfield(path: &[String]) -> bool {
    matches!(
        path,
        [root, display, meter, band, ..]
            if root == "theme"
                && display == "display"
                && meter == "context_meter"
                && CONTEXT_METER_BANDS.contains(&band.as_str())
                && path.len() > 4
    )
}

fn is_disallowed_set_container(path: &[String]) -> bool {
    matches!(path, [root] if root == "loop")
        || matches!(path, [root, child] if root == "accounts" && child == "usage_limit_usd")
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "usage_limit_usd" && path.len() > 3)
        || matches!(path, [root, child] if root == "accounts" && child == "budget")
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "budget" && path.len() > 3)
        || matches!(path, [root, child] if root == "agents" && matches!(child.as_str(), "profiles" | "teams" | "commands"))
        || matches!(path, [root, child, _, ..] if root == "agents" && child == "commands" && path.len() > 3)
        || matches!(path, [root, child, _] if root == "agents" && matches!(child.as_str(), "profiles" | "teams"))
        || matches!(path, [root, child] if root == "theme" && matches!(child.as_str(), "display" | "colors" | "providers" | "animations"))
        || matches!(path, [root, display, child] if root == "theme" && display == "display" && matches!(child.as_str(), "context_meter" | "budget_bar" | "highlight_steps"))
        || matches!(path, [root, display, budget, child] if root == "theme" && display == "display" && budget == "budget_bar" && child == "burn_rate")
        || matches!(path, [root, child, _] if root == "theme" && matches!(child.as_str(), "colors" | "providers"))
        || matches!(path, [root, child, role] if root == "theme" && child == "animations" && role != "unread")
        || matches!(
            path,
            [root, child, role, field]
                if root == "theme"
                    && child == "animations"
                    && !(is_animation_role(role)
                        && ANIMATION_FIELDS.contains(&field.as_str()))
        )
        || matches!(path, [root, child, _, _, ..] if root == "theme" && child == "animations" && path.len() > 4)
        || matches!(path, [root, child, set] if root == "theme" && child == "glyphs" && is_named_glyph_set(set))
        || matches!(path, [root, child, set, ..] if root == "theme" && child == "glyphs" && is_named_glyph_set(set) && path.len() < 5)
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_named_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_none()
        )
        || matches!(path, [root, child, set, _, _, ..] if root == "theme" && child == "glyphs" && is_named_glyph_set(set) && path.len() > 5)
        || matches!(path, [root, tasks, _] if root == "loop" && tasks == "tasks")
        || matches!(path, [root, tasks, _, wake] if root == "loop" && tasks == "tasks" && wake == "wake")
}

fn is_animation_role(role: &str) -> bool {
    AnimationRole::ALL
        .iter()
        .any(|candidate| candidate.config_key() == role)
}

const CONTEXT_METER_BANDS: &[&str] = &["green", "yellow", "amber", "red"];
const ANIMATION_FIELDS: &[&str] = &["frames", "color", "effect", "speed"];

fn parse_edit_value(raw: &str) -> Value {
    raw.parse::<Value>()
        .unwrap_or_else(|_| Value::from(raw.to_owned()))
}

fn parse_set_value(path: &[String], raw: &str) -> Value {
    if is_harness_smart_compact_edit(path)
        || is_harness_rtk_edit(path)
        || is_daily_budget_edit(path)
        || is_auto_redeem_min_gain_edit(path)
        || is_sidebar_theme_scheme_edit(path)
        || is_sidebar_glyph_string_edit(path)
    {
        return parse_string_edit_value(raw);
    }
    parse_edit_value(raw)
}

fn parse_string_edit_value(raw: &str) -> Value {
    match raw.parse::<Value>() {
        Ok(value) if value.is_str() => value,
        _ => Value::from(raw.to_owned()),
    }
}

fn validate_set_value(path: &[String], value: &Value) -> Result<()> {
    if is_auto_redeem_min_gain_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("resume.auto_redeem_min_gain must be a duration string");
        };
        if let Err(err) = super::parse_auto_redeem_min_gain(raw) {
            invalid_value!("resume.auto_redeem_min_gain {err}");
        }
    }
    if is_daily_budget_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("{} must be a string ending in `/day`", path.join("."));
        };
        raw.parse::<super::DayCap>()
            .map_err(|err| ConfigEditErr::InvalidValue(err.to_string()))?;
    }
    if is_harness_smart_compact_edit(path) {
        let Some(threshold) = value.as_str() else {
            invalid_value!("harness.smart_compact must be a string");
        };
        if let Err(err) = crate::message::AutoCompact::parse(threshold) {
            invalid_value!("{err}");
        }
    }
    if is_harness_rtk_edit(path) {
        let Some(mode) = value.as_str() else {
            invalid_value!("harness.rtk must be a string");
        };
        if !matches!(mode, "auto" | "on" | "off") {
            invalid_value!("harness.rtk must be one of auto, on, or off");
        }
    }
    if matches!(
        path,
        [root, leaf] if root == "theme" && leaf == "scheme"
    ) {
        let Some(scheme) = value.as_str() else {
            invalid_value!("theme.scheme must be a string");
        };
        if let Err(err) = super::scheme::validate_explicit_scheme(scheme) {
            invalid_value!("{err}");
        }
    }
    if matches!(
        path,
        [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set"
    ) {
        let Some(source) = value.as_str() else {
            invalid_value!("theme.glyphs.set must be a string");
        };
        if let Err(err) = validate_glyph_source(source) {
            invalid_value!("{err}");
        }
    }
    if let [root, child, set, namespace, role] = path
        && root == "theme"
        && child == "glyphs"
        && is_named_glyph_set(set)
        && GlyphRole::from_namespaced(namespace, role).is_some()
    {
        let Some(glyph) = value.as_str() else {
            invalid_value!("theme.glyphs.{set}.{namespace}.{role} must be a string");
        };
        if let Err(err) = validate_glyph_cells(glyph) {
            invalid_value!("sidebar glyph `{namespace}.{role}` {err}");
        }
    }
    Ok(())
}

fn is_sidebar_theme_scheme_edit(path: &[String]) -> bool {
    matches!(path, [root] if root == "theme")
        || matches!(path, [root, leaf] if root == "theme" && leaf == "scheme")
}

fn is_harness_smart_compact_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "smart_compact")
}

fn is_harness_rtk_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "rtk")
}

fn is_daily_budget_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "budget")
        || matches!(path, [root, child, _] if root == "accounts" && child == "budget")
}

fn is_auto_redeem_min_gain_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "resume" && child == "auto_redeem_min_gain")
}

fn is_sidebar_glyph_string_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "glyphs")
        || matches!(path, [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set")
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_named_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_some()
        )
}

fn normalize_set_key(path: &[String], value: &Value) -> Result<Vec<String>> {
    if matches!(path, [root] if root == "theme") {
        if !value.is_str() {
            invalid_value!("theme shorthand sets a scheme string");
        }
        return Ok(["theme", "scheme"].into_iter().map(str::to_owned).collect());
    }
    if matches!(path, [root, child] if root == "theme" && child == "glyphs") {
        if !value.is_str() {
            invalid_value!("theme.glyphs shorthand sets a glyph set string");
        }
        return Ok(["theme", "glyphs", "set"]
            .into_iter()
            .map(str::to_owned)
            .collect());
    }
    Ok(path.to_vec())
}

fn set_document_value(doc: &mut DocumentMut, path: &[String], value: Value) -> Result<()> {
    let mut table = doc.as_table_mut();
    for segment in &path[..path.len() - 1] {
        let item = table
            .entry(segment)
            .or_insert_with(|| Item::Table(Table::new()));
        if item.is_none() {
            *item = Item::Table(Table::new());
        }
        table = item
            .as_table_mut()
            .ok_or_else(|| ConfigEditErr::DocumentShape {
                segment: segment.clone(),
            })?;
    }
    let leaf = path.last().expect("validated key has a leaf");
    let mut item = value_to_item(value);
    if let Some(old) = table.get(leaf).and_then(Item::as_value)
        && let Item::Value(new) = &mut item
    {
        *new.decor_mut() = old.decor().clone();
    }
    table[leaf] = item;
    Ok(())
}

/// Re-emit structured values as TOML table blocks so multi-field config stays
/// readable: an inline table expands to `[header]` tables and an array of
/// inline tables to array-of-tables blocks, recursing through nested inline
/// tables. Scalars and scalar arrays keep their inline form.
fn value_to_item(value: Value) -> Item {
    match value {
        Value::InlineTable(inline) => Item::Table(expand_inline_table(inline)),
        Value::Array(array) if !array.is_empty() && array.iter().all(Value::is_inline_table) => {
            let mut tables = ArrayOfTables::new();
            for element in array {
                if let Value::InlineTable(inline) = element {
                    tables.push(expand_inline_table(inline));
                }
            }
            Item::ArrayOfTables(tables)
        }
        other => Item::Value(other),
    }
}

/// Convert an inline table to a standard table, recursively re-emitting any
/// nested inline tables or inline-table arrays as their block forms.
fn expand_inline_table(inline: InlineTable) -> Table {
    let mut table = inline.into_table();
    for (_, item) in table.iter_mut() {
        // `InlineTable::into_table` leaves every child as a value; nested
        // inline tables are values here and expand through `value_to_item`.
        let value = std::mem::replace(item, Item::None)
            .into_value()
            .expect("inline-table child is a value");
        *item = value_to_item(value);
    }
    table
}

#[cfg(test)]
mod tests;
