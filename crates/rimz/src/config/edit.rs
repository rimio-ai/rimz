//! Comment-preserving editing for the per-machine config set.
//!
//! The reader accepts TOML 1.1, so an inline table may span lines and hold
//! comments in its entries' key prefixes, value suffixes, and `trailing()`.
//! A write whose path crosses an inline table converts it to a standard
//! table and carries that decor across.

use std::path::{Path, PathBuf};

use toml_edit::{
    Array, ArrayOfTables, Decor, DocumentMut, InlineTable, Item, KeyMut, RawString, Table,
    TableLike, Value,
};

use crate::disk::atomic::write_bytes_atomically;

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
        source: crate::disk::atomic::AtomicErr,
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

    fn new(files: MachineConfigFiles) -> Self {
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
        let root =
            toml::Value::try_from(&config).map_err(|source| ConfigEditErr::Serialize { source })?;
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

    pub fn set(&self, key: &str, raw_value: &str) -> Result<String> {
        let requested_key = parse_key(key)?;
        let value = parse_set_value(&requested_key, raw_value);
        let key = normalize_set_key(&requested_key, &value)?;
        validate_set_key(&self.files, &key)?;
        let file = file_for_key(&self.files, &key);
        let existing = read_existing(file.path())?;
        let text = existing.as_deref().unwrap_or(file.template());
        let mut doc = parse_document(file.path(), text)?;
        let doc_key = document_key_for_set(&key);
        let previous = existing
            .as_ref()
            .and_then(|_| item_at(&doc, &doc_key))
            .and_then(item_to_value);
        if item_at(&doc, &doc_key).is_none()
            && let Some(uncommented) = uncomment_template_default(text, &doc_key)
            && let Ok(uncommented) = uncommented.parse::<DocumentMut>()
        {
            doc = uncommented;
        }
        apply_logical_key(&mut doc, file.path(), &key, value, self.files.agents_home())?;
        // A successful edit stores the addressed key, including table-valued settings.
        let value = item_at(&doc, &doc_key)
            .and_then(item_to_value)
            .expect("successful config edit stores the key");
        let key = key.join(".");
        let defaults = toml::Value::try_from(MachineConfig::default())
            .map_err(|source| ConfigEditErr::Serialize { source })?;
        let default = value_at(&defaults, &key);
        let old = previous
            .as_ref()
            .and_then(as_toml_value)
            .or_else(|| default.cloned());
        let was = if old == as_toml_value(&value) {
            "unchanged".to_owned()
        } else if let Some(previous) = previous {
            format!("was {}", receipt_value(previous))
        } else if let Some(default) = default {
            format!("was {default}, the default")
        } else {
            "was unset".to_owned()
        };
        let receipt = format!("set {key} = {}  ({was})", receipt_value(value));
        write(file.path(), doc.to_string().as_bytes())?;
        Ok(receipt)
    }

    /// Declare `[accounts.<kind>.<name>]`, or set the given fields of a
    /// declared one. An empty label removes that field. An account with no
    /// fields is written as an empty table so its defaults stay derived.
    /// An inline table on the account's path becomes a standard table.
    pub fn upsert_named_account(
        &self,
        kind: &crate::ids::AgentKind,
        name: &crate::ids::LoginName,
        home: Option<&Path>,
        history: Option<super::AccountHistory>,
        label: Option<&str>,
    ) -> Result<()> {
        let text = self.named_account_text(kind, name, home, history, label)?;
        write(self.files.core_path(), text.as_bytes())
    }

    /// Check the update [`Self::upsert_named_account`] would make to a
    /// declared account, without writing it.
    pub fn check_named_account_update(
        &self,
        kind: &crate::ids::AgentKind,
        name: &crate::ids::LoginName,
        history: Option<super::AccountHistory>,
        label: Option<&str>,
    ) -> Result<()> {
        self.named_account_text(kind, name, None, history, label)
            .map(|_| ())
    }

    fn named_account_text(
        &self,
        kind: &crate::ids::AgentKind,
        name: &crate::ids::LoginName,
        home: Option<&Path>,
        history: Option<super::AccountHistory>,
        label: Option<&str>,
    ) -> Result<String> {
        let file = self.files.file(MachineConfigFileKind::Core);
        let text = read_config_or_template(file.path(), file.template())?;
        let mut doc = parse_document(file.path(), &text)?;
        let key = vec!["accounts".to_owned(), kind.to_string(), name.to_string()];
        let fields = [
            ("home", home.map(|home| home.to_string_lossy().into_owned())),
            ("history", history.map(|mode| mode.as_str().to_owned())),
            (
                "label",
                label.filter(|label| !label.is_empty()).map(str::to_owned),
            ),
        ];
        if fields.iter().all(|(_, value)| value.is_none()) {
            if item_at(&doc, &key).is_none() {
                set_document_value(&mut doc, &key, Value::InlineTable(InlineTable::new()))?;
            } else {
                table_at_mut(&mut doc, &key)?;
            }
        }
        for (field, value) in fields {
            let Some(value) = value else { continue };
            apply_logical_key(
                &mut doc,
                file.path(),
                &[key.as_slice(), &[field.to_owned()]].concat(),
                Value::from(value),
                self.files.agents_home(),
            )?;
        }
        if label == Some("") {
            table_at_mut(&mut doc, &key)?.remove("label");
            validate_merged_text(file.path(), &doc.to_string(), self.files.agents_home())?;
        }
        Ok(doc.to_string())
    }

    /// Select an account for new rooms; `default` clears the machine override.
    pub fn use_account(
        &self,
        kind: &crate::ids::AgentKind,
        name: &crate::ids::LoginName,
    ) -> Result<()> {
        if !name.is_default() {
            return self
                .set(&format!("accounts.use.{kind}"), name.as_str())
                .map(|_| ());
        }
        let file = self.files.file(MachineConfigFileKind::Core);
        let Some(text) = read_existing(file.path())? else {
            return Ok(());
        };
        let mut doc = parse_document(file.path(), &text)?;
        if clear_account_use(&mut doc, kind, None) {
            write(file.path(), doc.to_string().as_bytes())?;
        }
        Ok(())
    }

    /// Drop `[accounts.<kind>.<name>]`, and the kind table with it once it
    /// holds no accounts. Reports whether the entry was there.
    pub fn remove_named_account(
        &self,
        kind: &crate::ids::AgentKind,
        name: &crate::ids::LoginName,
    ) -> Result<bool> {
        let file = self.files.file(MachineConfigFileKind::Core);
        let Some(text) = read_existing(file.path())? else {
            return Ok(false);
        };
        let mut doc = parse_document(file.path(), &text)?;
        let kinds = vec!["accounts".to_owned(), kind.to_string()];
        let table = table_at_mut(&mut doc, &kinds)?;
        if table.remove(name.as_str()).is_none() {
            return Ok(false);
        }
        if table.is_empty() {
            table_at_mut(&mut doc, &kinds[..1])?.remove(kind.as_str());
        }
        clear_account_use(&mut doc, kind, Some(name));
        write(file.path(), doc.to_string().as_bytes())?;
        Ok(true)
    }

    /// Write the shipped template for each file. Without `force` a file that
    /// already exists is left alone, so a bootstrap fills the gaps a user left
    /// rather than skipping the whole set. Reports whether anything was written.
    pub fn write_defaults(&self, force: bool) -> Result<bool> {
        let mut wrote = false;
        for file in self.files.ordered() {
            if !force
                && crate::disk::entry_exists(file.path()).map_err(|source| ConfigEditErr::Read {
                    path: file.path().to_path_buf(),
                    source,
                })?
            {
                continue;
            }
            write(file.path(), file.template().as_bytes())?;
            wrote = true;
        }
        Ok(wrote)
    }

    pub fn retire_idle_compact_keys(&self) -> Result<Vec<String>> {
        let file = self.files.file(MachineConfigFileKind::Core);
        let Some(text) = read_existing(file.path())? else {
            return Ok(Vec::new());
        };
        let mut doc = parse_document(file.path(), &text)?;
        let Some(harness) = doc.get_mut("harness").and_then(Item::as_table_like_mut) else {
            return Ok(Vec::new());
        };
        let mut removed = Vec::new();
        if harness.remove("idle_compact_after").is_some() {
            removed.push("harness.idle_compact_after".to_owned());
        }
        if let Some(mode) = harness
            .get("idle_compact")
            .and_then(Item::as_str)
            .filter(|mode| mode.parse::<super::IdleCompact>().is_err())
        {
            removed.push(format!("harness.idle_compact = \"{mode}\""));
            harness.remove("idle_compact");
        }
        if !removed.is_empty() {
            write(file.path(), doc.to_string().as_bytes())?;
        }
        Ok(removed)
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
    MachineConfig::parse_text_for_edit(path, text, agents_home)
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
) -> Result<()> {
    validate_set_value(logical, &value)?;
    let value_display = value.to_string().trim().to_owned();
    let pre_image = doc.to_string();
    set_document_value(doc, &document_key_for_set(logical), value)?;
    reject_if_ignored(path, logical, &document_key_for_set(logical), doc)?;
    match MachineConfig::parse_text_for_edit(path, &doc.to_string(), agents_home) {
        Ok(_) => Ok(()),
        Err(source) => match MachineConfig::parse_text_for_edit(path, &pre_image, agents_home) {
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
    // Template headers change traversal order after the first merge; append blocks in stable key order.
    found.sort_by(|left, right| left.logical.cmp(&right.logical));
    found
}

fn apply_merge_keys(
    path: &Path,
    doc: &mut DocumentMut,
    keys: Vec<PendingKey>,
    skipped: &mut Vec<SkippedKey>,
    agents_home: &Path,
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
            match apply_logical_key(&mut trial, path, &logical, value.clone(), agents_home) {
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
        let empty = item.as_table_like().is_some_and(|table| table.is_empty());
        if is_whole_struct_table(&logical) || is_empty_named_account(&logical, empty) {
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
        let empty = value.as_inline_table().is_some_and(InlineTable::is_empty);
        if is_whole_struct_table(&logical) || is_empty_named_account(&logical, empty) {
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

/// Context-meter bands and LSP servers have several required fields, so no individual leaf validates alone.
fn is_whole_struct_table(path: &[String]) -> bool {
    matches!(
        path,
        [root, display, meter, band]
            if root == "theme"
                && display == "display"
                && meter == "context_meter"
                && CONTEXT_METER_BANDS.contains(&band.as_str())
    ) || matches!(path, [root, servers, _] if root == "lsp" && servers == "servers")
}

/// An empty `[accounts.<kind>.<name>]` declares the account at its default home, so the table itself is the setting.
fn is_empty_named_account(path: &[String], empty: bool) -> bool {
    empty && matches!(path, [root, kind, _] if root == "accounts" && is_named_account_kind(kind))
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

fn parse_document(path: &Path, text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>()
        .map_err(|source| ConfigEditErr::DocumentParse {
            path: path.to_path_buf(),
            diagnosis: Box::new(ConfigFileDiagnosis::from_toml_edit(path, text, &source)),
        })
}

/// The table at `path`, created as an empty table where the document has none.
fn table_at_mut<'a>(doc: &'a mut DocumentMut, path: &[String]) -> Result<&'a mut Table> {
    convert_inline_tables(doc, path)?;
    let position = if !path.is_empty() && item_at(doc, path).is_none() {
        place_new_table(doc, path).position
    } else {
        0
    };
    placed_table_at_mut(doc, path, position)
}

/// Convert only existing containers on the path, before table-placement decisions.
fn convert_inline_tables(doc: &mut DocumentMut, path: &[String]) -> Result<()> {
    let mut table = doc.as_table_mut();
    // The renderer starts root-level tables at zero, even without a stored position.
    let mut position = table.position().unwrap_or(0);
    for segment in path {
        let Some((mut key, item)) = table.get_key_value_mut(segment) else {
            return Ok(());
        };
        if let Item::Value(Value::InlineTable(inline)) = item {
            let above = decor_text(key.leaf_decor().prefix()).trim_start_matches('\n');
            let mut block = inline_into_block(std::mem::take(inline), above);
            key.leaf_decor_mut().clear();
            block.set_position(Some(position));
            *item = Item::Table(block);
        }
        table = item
            .as_table_mut()
            .ok_or_else(|| ConfigEditErr::DocumentShape {
                segment: segment.clone(),
            })?;
        position = table.position().unwrap_or(position);
    }
    Ok(())
}

/// `inline` as a standard table that keeps every comment: `above`, the
/// lines over its key, over the header, and each line's comments over or
/// beside that line, where a dotted entry such as `a.b = 1` is a line of
/// its own.
fn inline_into_block(inline: InlineTable, above: &str) -> Table {
    let mut decor = Decor::default();
    // An unset prefix lets the renderer omit the blank line before the file's first table.
    if !above.trim().is_empty() {
        decor.set_prefix(format!("\n{above}"));
    }
    let mut header_suffix = decor_text(inline.decor().suffix()).to_owned();
    let mut lines: Vec<(String, String)> = Vec::new();
    // The parser keeps a line's leading text on its last key, so a dotted line keeps it inside.
    for (path, value) in inline.get_values() {
        let mut over = String::new();
        let leading = path.last().and_then(|key| key.leaf_decor().prefix());
        for (same_line, comment) in decor_comments(leading) {
            match lines.last_mut() {
                // A comment after the previous line's comma annotates that line.
                Some((_, beside)) if same_line => push_comment(beside, true, comment),
                _ => {
                    over.push_str(comment);
                    over.push('\n');
                }
            }
        }
        let mut beside = String::new();
        for (same_line, comment) in decor_comments(value.decor().suffix()) {
            push_comment(&mut beside, same_line, comment);
        }
        lines.push((over, beside));
    }
    let trailing = decor_comments(Some(inline.trailing()));
    if let Some((_, beside)) = lines.last_mut() {
        trailing.for_each(|(same_line, comment)| push_comment(beside, same_line, comment));
    } else {
        trailing.for_each(|(_, comment)| push_comment(&mut header_suffix, false, comment));
    }
    decor.set_suffix(header_suffix);
    // `into_table` keeps each entry a value and resets decor on top-level entries only.
    let mut block = inline.into_table();
    let entries = block
        .iter_mut()
        .filter_map(|(key, item)| Some((key, item.as_value_mut()?)));
    for ((mut key, value), (over, beside)) in lines_mut(entries).into_iter().zip(lines) {
        key.leaf_decor_mut().clear();
        key.leaf_decor_mut().set_prefix(over);
        value.decor_mut().clear();
        value.decor_mut().set_suffix(beside);
    }
    *block.decor_mut() = decor;
    block
}

/// The last key and the value of each line `entries` render, in the
/// renderer's order: a dotted entry renders a line per value inside it.
fn lines_mut<'t>(
    entries: impl Iterator<Item = (KeyMut<'t>, &'t mut Value)>,
) -> Vec<(KeyMut<'t>, &'t mut Value)> {
    let mut lines = Vec::new();
    for (key, value) in entries {
        // A guard would hold a borrow of `value` into the arm that moves it.
        match (
            value.as_inline_table().is_some_and(InlineTable::is_dotted),
            value,
        ) {
            (true, Value::InlineTable(dotted)) => lines.extend(lines_mut(dotted.iter_mut())),
            (_, value) => lines.push((key, value)),
        }
    }
    lines
}

fn decor_text(raw: Option<&RawString>) -> &str {
    raw.and_then(RawString::as_str).unwrap_or_default()
}

/// The comment lines in decor text, each flagged when it sits on the line
/// the text starts on.
fn decor_comments(raw: Option<&RawString>) -> impl Iterator<Item = (bool, &str)> {
    decor_text(raw)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim_start();
            line.starts_with('#').then_some((index == 0, line))
        })
}

/// Append `comment` to a decor suffix: beside the line when it shared one
/// and nothing is there yet, otherwise on a line of its own.
fn push_comment(suffix: &mut String, same_line: bool, comment: &str) {
    suffix.push(if same_line && suffix.is_empty() {
        ' '
    } else {
        '\n'
    });
    suffix.push_str(comment);
}

/// [`table_at_mut`], giving each table it creates `position`.
fn placed_table_at_mut<'a>(
    doc: &'a mut DocumentMut,
    path: &[String],
    position: isize,
) -> Result<&'a mut Table> {
    let mut table = doc.as_table_mut();
    for segment in path {
        let item = table.entry(segment).or_insert(Item::None);
        if item.is_none() {
            let mut parent = Table::new();
            parent.set_implicit(true);
            parent.set_position(Some(position));
            *item = Item::Table(parent);
        }
        table = item
            .as_table_mut()
            .ok_or_else(|| ConfigEditErr::DocumentShape {
                segment: segment.clone(),
            })?;
    }
    Ok(table)
}

fn last_table_position(table: &Table) -> isize {
    table
        .iter()
        .flat_map(|(_, item)| {
            item.as_table()
                .into_iter()
                .chain(item.as_array_of_tables().into_iter().flatten())
        })
        .map(|child| {
            child
                .position()
                .unwrap_or(0)
                .max(last_table_position(child))
        })
        .max()
        .unwrap_or(0)
}

/// Where a new table under `path` is rendered, and the comments above its header.
struct NewTablePlace {
    position: isize,
    /// `Some` when the table goes inside the file rather than at its end.
    decor: Option<Decor>,
}

/// Place a newly rendered `[accounts.*]` header right after the last `accounts`
/// table, so `[accounts.use]` lands beside the templated `[accounts]` block rather
/// than at the end of the file. Later tables shift down one. The comments a section
/// carries after its last table are stored as the next header's prefix; the part up to
/// its last blank line stays with the section, above the new header, and the
/// next header keeps only the comments directly above it. Other sections, an
/// `accounts` section the document lacks, or one that ends the file, append.
fn place_new_table(doc: &mut DocumentMut, path: &[String]) -> NewTablePlace {
    let end = last_table_position(doc.as_table()) + 1;
    let section_last = path
        .first()
        .filter(|section| *section == "accounts")
        .and_then(|section| doc.as_table().get(section))
        .and_then(Item::as_table)
        .and_then(|section| {
            let mut last = section.position();
            for_each_table(section, &mut |table| last = last.max(table.position()));
            last
        });
    let Some(after) = section_last.filter(|last| last + 1 < end) else {
        return NewTablePlace {
            position: end,
            decor: None,
        };
    };
    let position = after + 1;
    let mut next = None;
    for_each_table_mut(doc.as_table_mut(), &mut |table| {
        if let Some(at) = table.position().filter(|at| *at >= position) {
            table.set_position(Some(at + 1));
            if renders_header(table) && next.is_none_or(|next| at < next) {
                next = Some(at);
            }
        }
    });
    let mut decor = Decor::default();
    decor.set_prefix("\n");
    let Some(next) = next.map(|at| at + 1) else {
        return NewTablePlace {
            position,
            decor: Some(decor),
        };
    };
    for_each_table_mut(doc.as_table_mut(), &mut |table| {
        if table.position() != Some(next) || !renders_header(table) {
            return;
        }
        let prefix = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .to_owned();
        let lines: Vec<&str> = prefix.split_inclusive('\n').collect();
        let split = lines
            .iter()
            .rposition(|line| line.trim().is_empty())
            .map_or(0, |blank| blank + 1);
        let (section_comments, heading) = lines.split_at(split);
        let section_comments = section_comments.concat();
        if !section_comments.is_empty() {
            decor.set_prefix(section_comments);
        }
        table
            .decor_mut()
            .set_prefix(format!("\n{}", heading.concat()));
    });
    NewTablePlace {
        position,
        decor: Some(decor),
    }
}

/// Whether `table` renders a `[header]` line of its own.
fn renders_header(table: &Table) -> bool {
    !table.is_dotted() && (!table.is_implicit() || table.iter().any(|(_, item)| item.is_value()))
}

/// Visit every table under `table`, depth first, array-of-tables blocks included.
fn for_each_table(table: &Table, visit: &mut impl FnMut(&Table)) {
    for (_, item) in table.iter() {
        let children = item
            .as_table()
            .into_iter()
            .chain(item.as_array_of_tables().into_iter().flatten());
        for child in children {
            visit(child);
            for_each_table(child, visit);
        }
    }
}

/// [`for_each_table`], mutably.
fn for_each_table_mut(table: &mut Table, visit: &mut impl FnMut(&mut Table)) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => {
                visit(child);
                for_each_table_mut(child, visit);
            }
            Item::ArrayOfTables(blocks) => {
                for child in blocks.iter_mut() {
                    visit(child);
                    for_each_table_mut(child, visit);
                }
            }
            _ => {}
        }
    }
}

fn item_at<'a>(doc: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    let (leaf, parents) = path.split_last()?;
    let mut table: &dyn TableLike = doc.as_table();
    for segment in parents {
        table = table.get(segment)?.as_table_like()?;
    }
    table.get(leaf)
}

fn as_toml_value(value: &Value) -> Option<toml::Value> {
    // A bare TOML value is not a document; wrap and reparse so equality ignores
    // toml_edit formatting/decor and compares the semantic value.
    toml::from_str::<toml::Table>(&format!("x = {}", receipt_value(value.clone())))
        .ok()?
        .remove("x")
}

fn receipt_value(mut value: Value) -> String {
    fn clean(value: &mut Value) {
        value.decor_mut().clear();
        match value {
            Value::String(string) if string.display_repr().contains(['\n', '\r']) => {
                // JSON quoting escapes line breaks; TOML additionally requires escaped DEL.
                let quoted = serde_json::Value::String(string.value().clone())
                    .to_string()
                    .replace('\u{7f}', "\\u007f");
                *value = quoted
                    .parse()
                    .expect("JSON with escaped DEL is a TOML string");
            }
            Value::Array(array) => {
                for value in array.iter_mut() {
                    clean(value);
                }
                array.fmt();
            }
            Value::InlineTable(table) => {
                for (_, value) in table.iter_mut() {
                    clean(value);
                }
                table.fmt();
                table.set_trailing("");
                table.set_trailing_comma(false);
            }
            _ => {}
        }
    }
    clean(&mut value);
    value.to_string()
}

fn file_for_key(files: &MachineConfigFiles, path: &[String]) -> MachineConfigFile {
    let kind = match path.first().map(String::as_str) {
        Some("theme") => MachineConfigFileKind::Theme,
        Some("agents" | "subagents") => MachineConfigFileKind::Core,
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
    if let [root, table, rest @ ..] = path {
        let tree = match (root.as_str(), table.as_str()) {
            ("agents", "profiles") => Some("agents"),
            ("agents", "teams") => Some("teams"),
            ("subagents", "profiles") => Some("subagents"),
            _ => None,
        };
        if let Some(tree) = tree {
            let name = rest.first().map_or("<name>", String::as_str);
            return Err(ConfigEditErr::InvalidKey(format!(
                "edit {}",
                files
                    .agents_home()
                    .join(tree)
                    .join(format!("{name}.md"))
                    .display()
            )));
        }
    }
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
        Err(_) => return Ok(true),
    };
    let document_key = doc_key.join(".");
    Ok(!ignored_path_matches(&ignored, &document_key))
}

fn clear_account_use(
    doc: &mut DocumentMut,
    kind: &crate::ids::AgentKind,
    matching: Option<&crate::ids::LoginName>,
) -> bool {
    let Some(accounts) = doc.get_mut("accounts").and_then(Item::as_table_like_mut) else {
        return false;
    };
    let Some(selection) = accounts.get_mut("use").and_then(Item::as_table_like_mut) else {
        return false;
    };
    if matching.is_some_and(|name| {
        selection.get(kind.as_str()).and_then(Item::as_str) != Some(name.as_str())
    }) {
        return false;
    }
    let removed = selection.remove(kind.as_str()).is_some();
    if selection.is_empty() {
        accounts.remove("use");
    }
    removed
}

fn is_unknown_get_shape(path: &[String]) -> bool {
    matches!(path, [root, child, ..] if (root == "agents" && matches!(child.as_str(), "profiles" | "teams")) || (root == "subagents" && child == "profiles"))
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "usage_limit_usd" && path.len() > 3)
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "budget" && path.len() > 3)
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "use" && path.len() > 3)
        || matches!(path, [root, child, _, field] if root == "accounts" && is_named_account_kind(child) && !matches!(field.as_str(), "home" | "history" | "label" | "budget"))
        || matches!(path, [root, child, _, _, _, ..] if root == "accounts" && is_named_account_kind(child))
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

/// A kind whose `[accounts.<kind>]` table holds named accounts rather than a
/// per-kind scalar.
fn is_named_account_kind(segment: &str) -> bool {
    super::AccountsConfig::default()
        .named(&crate::ids::AgentKind::new_unchecked(segment))
        .is_some()
}

fn is_disallowed_set_container(path: &[String]) -> bool {
    matches!(path, [root] if root == "loop")
        || matches!(path, [root, child] if root == "accounts" && child == "usage_limit_usd")
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "usage_limit_usd" && path.len() > 3)
        || matches!(path, [root, child] if root == "accounts" && child == "budget")
        || matches!(path, [root, child] if root == "accounts" && child == "use")
        || matches!(path, [root, child, _, ..] if root == "accounts" && child == "use" && path.len() > 3)
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
        || matches!(path, [root, tasks, _, wait] if root == "loop" && tasks == "tasks" && wait == "wait")
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
    if matches!(path, [root, profiles, _, field] if matches!(root.as_str(), "agents" | "subagents") && profiles == "profiles" && field == "auto-compact")
        || matches!(path, [root, child, _] if root == "accounts" && child == "use")
        || matches!(path, [root, kind, _, field] if root == "accounts" && is_named_account_kind(kind) && field == "label")
        || matches!(path, [root, _, _] if root == "models")
        || is_harness_smart_compact_edit(path)
        || is_harness_auto_compact_edit(path)
        || is_harness_flip_compact_edit(path)
        || is_harness_compact_instruction_edit(path)
        || is_harness_idle_compact_edit(path)
        || is_harness_keep_warm_min_ttl_edit(path)
        || is_harness_prompt_cache_ttl_edit(path)
        || is_harness_cache_keepalive_max_edit(path)
        || is_daily_budget_edit(path)
        || is_account_budget_edit(path)
        || is_turn_budget_edit(path)
        || is_auto_redeem_min_gain_edit(path)
        || is_gc_duration_edit(path)
        || matches!(path, [root, leaf] if root == "gc" && matches!(leaf.as_str(), "transcript_retention" | "team_notes_retention"))
        || matches!(path, [root, leaf] if root == "gc" && leaf == "tmp_floor_min")
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
    if matches!(path, [root, kind, _, field] if root == "accounts" && is_named_account_kind(kind) && field == "label")
    {
        let Some(label) = value.as_str() else {
            invalid_value!("{} must be a string", path.join("."));
        };
        if let Err(error) = super::NamedAccount::validate_label(label) {
            invalid_value!("{} {error}", path.join("."));
        }
    }
    if matches!(path, [root, leaf] if root == "gc" && matches!(leaf.as_str(), "transcript_retention" | "team_notes_retention"))
    {
        super::HistoryRetention::parse(value.as_str().unwrap_or_default(), &path.join("."))
            .map_err(ConfigEditErr::InvalidValue)?;
    }
    if is_auto_redeem_min_gain_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("resume.auto_redeem_min_gain must be a duration string");
        };
        if let Err(err) = super::parse_auto_redeem_min_gain(raw) {
            invalid_value!("resume.auto_redeem_min_gain {err}");
        }
    }
    if is_gc_duration_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("{} must be a duration string", path.join("."));
        };
        if let Err(err) = super::parse_older_than(raw) {
            invalid_value!("{} {err}; use a duration such as 8h or 3d", path.join("."));
        }
    }
    if matches!(path, [root, leaf] if root == "gc" && leaf == "tmp_floor_min") {
        let Some(raw) = value.as_str() else {
            invalid_value!("gc.tmp_floor_min must be a byte-size string");
        };
        if let Err(err) = crate::utils::size::parse_byte_size(raw) {
            invalid_value!("gc.tmp_floor_min {err}");
        }
    }
    if matches!(path, [root, leaf] if root == "gc" && leaf == "tmp_floor_percent")
        && value
            .as_integer()
            .is_none_or(|value| !(0..=100).contains(&value))
    {
        invalid_value!("gc.tmp_floor_percent must be 0-100");
    }
    if is_daily_budget_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("{} must be a string ending in `/day`", path.join("."));
        };
        raw.parse::<super::DayCap>()
            .map_err(|err| ConfigEditErr::InvalidValue(err.to_string()))?;
    }
    if is_account_budget_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!(
                "{} must be a string ending in `/day` or `/month`",
                path.join(".")
            );
        };
        raw.parse::<super::AccountCap>()
            .map_err(|err| ConfigEditErr::InvalidValue(err.to_string()))?;
    }
    if is_turn_budget_edit(path) {
        let Some(raw) = value.as_str() else {
            invalid_value!("harness.turn_budget must be a plain dollar amount string");
        };
        raw.parse::<super::harness::TurnCap>()
            .map_err(|err| ConfigEditErr::InvalidValue(err.to_string()))?;
    }
    if is_harness_auto_compact_edit(path)
        && !value.as_str().is_some_and(|raw| {
            raw == "off" || crate::store::message::AutoCompact::parse_native_window(raw).is_ok()
        })
    {
        invalid_value!(
            "harness.auto_compact must be off or a token count from 100k through 1M, such as 272k"
        );
    }
    if is_harness_smart_compact_edit(path)
        && !value.as_str().is_some_and(|raw| {
            raw == "off" || crate::store::message::AutoCompact::parse(raw).is_ok()
        })
    {
        invalid_value!(
            "harness.smart_compact must be off, a token count such as 180k, or a percentage such as 70%"
        );
    }
    if is_harness_flip_compact_edit(path)
        && !value.as_str().is_some_and(|raw| {
            raw == "off" || crate::store::message::AutoCompact::parse(raw).is_ok()
        })
    {
        invalid_value!(
            "harness.flip_compact must be off, a token count such as 180k, or a percentage such as 70%"
        );
    }
    if is_harness_compact_instruction_edit(path) && !value.is_str() {
        invalid_value!("harness.compact_instruction must be a string");
    }
    if is_harness_idle_compact_edit(path) {
        let Some(mode) = value.as_str() else {
            invalid_value!("harness.idle_compact must be a string");
        };
        if mode.parse::<super::IdleCompact>().is_err() {
            invalid_value!(
                "harness.idle_compact must be off or a positive token floor in digits or k, such as 100k or 1000k"
            );
        }
    }
    if is_harness_keep_warm_min_ttl_edit(path)
        && value
            .as_str()
            .is_none_or(|floor| floor.parse::<super::KeepWarm>().is_err())
    {
        invalid_value!("harness.keep_warm_min_ttl must be off or a duration such as 15m");
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

fn is_harness_auto_compact_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "auto_compact")
}

fn is_harness_compact_instruction_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "compact_instruction")
}

fn is_harness_idle_compact_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "idle_compact")
}

fn is_harness_keep_warm_min_ttl_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "keep_warm_min_ttl")
}

fn is_harness_prompt_cache_ttl_edit(path: &[String]) -> bool {
    matches!(path, [root, table, _] | [root, table, _, _] if root == "harness" && table == "prompt_cache_ttl")
}

fn is_harness_cache_keepalive_max_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "cache_keepalive_max")
}

fn is_harness_flip_compact_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "flip_compact")
}

fn is_daily_budget_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "budget")
}

fn is_account_budget_edit(path: &[String]) -> bool {
    matches!(path, [root, child, _] if root == "accounts" && child == "budget")
        || matches!(path, [root, kind, _, field] if root == "accounts" && is_named_account_kind(kind) && field == "budget")
}

fn is_turn_budget_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "harness" && child == "turn_budget")
}

fn is_auto_redeem_min_gain_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "resume" && child == "auto_redeem_min_gain")
}

fn is_gc_duration_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "gc" && matches!(child.as_str(), "older_than" | "tmp_grace"))
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
    let (leaf, parents) = path.split_last().expect("validated key has a leaf");
    convert_inline_tables(doc, parents)?;
    let mut item = value_to_item(value);
    let old = item_at(doc, path);
    let old_position = old.and_then(Item::as_table).and_then(Table::position);
    let parent_starts_header = item.is_value()
        && !parents.is_empty()
        && item_at(doc, parents).is_none_or(|parent| {
            parent
                .as_table()
                .is_some_and(|table| !table.is_dotted() && !renders_header(table))
        });
    let creates_table =
        (parent_starts_header && old_position.is_none()) || (old.is_none() && !item.is_value());
    let changes_form = old.is_some_and(|old| old.is_value() != item.is_value());
    // A replaced block keeps the comments above its header; under a header-less parent,
    // the parent's first header takes them and the block's place. A new table or any other
    // first header goes beside its section (see `place_new_table`); one appended at the end
    // takes the template's trailing comments, so they stay with its last section, then a
    // blank line.
    let mut decor = match old {
        Some(Item::Table(block)) => Some(block.decor().clone()),
        Some(Item::ArrayOfTables(blocks)) => blocks.get(0).map(|block| block.decor().clone()),
        _ => None,
    };
    let mut position = old_position.unwrap_or_else(|| last_table_position(doc.as_table()) + 1);
    if creates_table {
        let place = place_new_table(doc, path);
        position = place.position;
        decor = Some(place.decor.unwrap_or_else(|| {
            let mut appended = Decor::default();
            appended.set_prefix(format!("{}\n", doc.trailing().as_str().unwrap_or_default()));
            doc.set_trailing("");
            appended
        }));
    }
    let table = placed_table_at_mut(doc, parents, position)?;
    match &mut item {
        Item::Table(block) => {
            block.set_position(Some(position));
            if let Some(decor) = decor {
                *block.decor_mut() = decor;
            }
        }
        Item::ArrayOfTables(blocks) => {
            for (index, block) in blocks.iter_mut().enumerate() {
                block.set_position(Some(position));
                if index == 0
                    && let Some(decor) = &decor
                {
                    *block.decor_mut() = decor.clone();
                }
            }
        }
        _ if parent_starts_header => {
            table.set_position(Some(position));
            if let Some(decor) = decor {
                *table.decor_mut() = decor;
            }
        }
        _ => {}
    }
    if let Some(old) = table.get(leaf).and_then(Item::as_value)
        && let Item::Value(new) = &mut item
    {
        *new.decor_mut() = old.decor().clone();
    }
    table[leaf] = item;
    // A key keeps the spacing of the form it was parsed in (`b= 1` from `[a.b]`, `[a.b ]`
    // from `b = 1`), so one that changes form takes its new form's default. The comment
    // lines above a `b = 1` line stay, and render above the new header.
    if changes_form && let Some(mut key) = table.key_mut(leaf) {
        let comments = key
            .leaf_decor()
            .prefix()
            .filter(|prefix| prefix.as_str().is_some_and(|text| text.contains('\n')))
            .cloned();
        let decor = key.leaf_decor_mut();
        decor.clear();
        if let Some(comments) = comments {
            decor.set_prefix(comments);
        }
    }
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
