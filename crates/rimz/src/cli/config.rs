//! `rimz config` — inspect and edit the per-machine config.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::{GlyphRole, MachineConfig, validate_glyph_cells, validate_glyph_source};
use rimz::store::atomic::write_bytes_atomically;
use rimz::store::paths;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigSubcmd,
}

#[derive(Debug, Subcommand)]
enum ConfigSubcmd {
    /// Write the commented default config templates.
    Init(InitArgs),
    /// Print the resolved core per-machine config path.
    Path,
    /// Print the effective per-machine config, or one dotted key.
    Get(GetArgs),
    /// Set one dotted key while preserving TOML comments.
    Set(SetArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Replace an existing config file.
    #[arg(long)]
    force: bool,
    /// Print the template to stdout instead of writing it.
    #[arg(long)]
    print: bool,
}

#[derive(Debug, Args)]
struct GetArgs {
    /// Dotted config key, for example `theme.display.max_cols`.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::config_keys
    ))]
    key: Option<String>,
    /// Emit JSON instead of TOML/plain scalar output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// Dotted config key, for example `theme.display.max_cols`.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::config_keys
    ))]
    key: String,
    /// TOML value. Bare words are treated as strings.
    value: String,
}

pub fn run(args: ConfigArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        ConfigSubcmd::Init(args) => init(args),
        ConfigSubcmd::Path => print_path(),
        ConfigSubcmd::Get(args) => get(args),
        ConfigSubcmd::Set(args) => set(args),
    }
}

pub(crate) fn write_default_config(force: bool) -> Result<bool> {
    let files = [
        (MachineConfig::config_path(), MachineConfig::template_core()),
        (MachineConfig::theme_path(), MachineConfig::template_theme()),
        (
            MachineConfig::agents_path(),
            MachineConfig::template_agents(),
        ),
        (MachineConfig::loop_path(), MachineConfig::template_loop()),
    ];
    if !force && files.iter().any(|(path, _)| path.exists()) {
        return Ok(false);
    }
    for (path, template) in files {
        write_bytes_atomically(&path, template.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(true)
}

#[derive(Debug)]
pub(crate) struct MergeReport {
    pub(crate) files: Vec<FileMergeOutcome>,
}

#[derive(Debug)]
pub(crate) struct FileMergeOutcome {
    pub(crate) path: PathBuf,
    pub(crate) action: MergeAction,
    pub(crate) skipped: Vec<SkippedKey>,
}

#[derive(Debug)]
pub(crate) enum MergeAction {
    Wrote,
    Merged { kept: usize },
    LeftUnparseable { error: String },
}

#[derive(Debug)]
pub(crate) struct SkippedKey {
    pub(crate) key: String,
    pub(crate) reason: String,
}

pub(crate) fn merge_default_config() -> Result<MergeReport> {
    let files = [
        (MachineConfig::config_path(), MachineConfig::template_core()),
        (MachineConfig::theme_path(), MachineConfig::template_theme()),
        (
            MachineConfig::agents_path(),
            MachineConfig::template_agents(),
        ),
        (MachineConfig::loop_path(), MachineConfig::template_loop()),
    ];
    let mut outcomes = Vec::new();
    for (path, template) in files {
        outcomes.push(merge_one(&path, template)?);
    }
    Ok(MergeReport { files: outcomes })
}

fn merge_one(path: &Path, template: &str) -> Result<FileMergeOutcome> {
    let Some(old_text) = read_existing(path)? else {
        write_bytes_atomically(path, template.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(FileMergeOutcome {
            path: path.to_path_buf(),
            action: MergeAction::Wrote,
            skipped: Vec::new(),
        });
    };
    let old_doc = match old_text.parse::<DocumentMut>() {
        Ok(doc) => doc,
        Err(err) => {
            return Ok(FileMergeOutcome {
                path: path.to_path_buf(),
                action: MergeAction::LeftUnparseable {
                    error: one_line(&err.to_string()),
                },
                skipped: Vec::new(),
            });
        }
    };

    let mut new_doc = template
        .parse::<DocumentMut>()
        .context("parsing shipped config template")?;
    let kind = FileKind::for_path(path);
    let mut pending = Vec::new();
    let mut skipped = Vec::new();
    for found in collect_explicit_keys(kind, &old_doc) {
        match found {
            Found::Settable { logical, value } => {
                if template_has_same_value(&new_doc, &logical, &value) {
                    continue;
                }
                pending.push(PendingKey { logical, value });
            }
        }
    }
    let kept = apply_merge_keys(
        path,
        &mut new_doc,
        pending,
        &mut skipped,
        &paths::agents_home(),
    );

    let rendered = new_doc.to_string();
    write_bytes_atomically(path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(FileMergeOutcome {
        path: path.to_path_buf(),
        action: MergeAction::Merged { kept },
        skipped,
    })
}

fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn init(args: InitArgs) -> Result<()> {
    if args.print {
        print_text(&render_all_templates())?;
        return Ok(());
    }

    let files = [
        MachineConfig::config_path(),
        MachineConfig::theme_path(),
        MachineConfig::agents_path(),
        MachineConfig::loop_path(),
    ];
    if !args.force && files.iter().any(|path| path.exists()) {
        bail!(
            "{} already exists; pass --force to replace the per-machine config set",
            files
                .iter()
                .find(|path| path.exists())
                .expect("an existing path")
                .display()
        );
    }
    write_default_config(args.force)?;
    for path in files {
        print_line(&format!("wrote {}", path.display()))?;
    }
    Ok(())
}

fn print_path() -> Result<()> {
    print_line(&MachineConfig::config_path().display().to_string())
}

fn get(args: GetArgs) -> Result<()> {
    let config = MachineConfig::load().context("loading per-machine config")?;
    let root = config_value(&config)?;
    let selected = match args.key.as_deref() {
        Some(key) => {
            let parsed = parse_key(key)?;
            match value_at(&root, key) {
                Some(value) => value,
                None if is_known_get_key(&parsed)? => bail!("config key `{key}` is unset"),
                None => bail!("unknown config key `{key}`"),
            }
        }
        None => &root,
    };

    if args.json {
        let rendered = serde_json::to_string_pretty(selected).context("rendering config JSON")?;
        print_line(&rendered)?;
        return Ok(());
    }

    let rendered = render_value(selected)?;
    print_text(&rendered)
}

fn set(args: SetArgs) -> Result<()> {
    set_config_key(&args.key, &args.value)?;
    print_line(&format!("set {}", args.key))
}

pub(crate) fn set_config_key(key: &str, raw_value: &str) -> Result<()> {
    let requested_key = parse_key(key)?;
    let value = parse_set_value(&requested_key, raw_value);
    let key = normalize_set_key(&requested_key, &value)?;
    validate_set_key(&key)?;
    let (path, template) = file_for_key(&key);

    let text = read_config_or_template(&path, template)?;
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    apply_logical_key(&mut doc, &path, &key, value, &paths::agents_home())?;
    let rendered = doc.to_string();
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn apply_logical_key(
    doc: &mut DocumentMut,
    path: &Path,
    logical: &[String],
    value: Value,
    agents_home: &Path,
) -> Result<()> {
    validate_set_value(logical, &value)?;
    set_document_value(doc, &document_key_for_set(logical), value)?;
    reject_unknown_set_key(path, logical, doc)?;
    MachineConfig::parse_text(path, &doc.to_string(), agents_home)
        .map(|_| ())
        .with_context(|| format!("validating `{}`", logical.join(".")))
}

fn read_existing(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

#[derive(Clone, Copy, Debug)]
enum FileKind {
    Core,
    Theme,
    Agents,
    Loop,
}

impl FileKind {
    fn for_path(path: &Path) -> Self {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("theme.toml") => Self::Theme,
            Some("agents.toml") => Self::Agents,
            Some("loop.toml") => Self::Loop,
            _ => Self::Core,
        }
    }
}

#[derive(Debug)]
enum Found {
    Settable { logical: Vec<String>, value: Value },
}

struct PendingKey {
    logical: Vec<String>,
    value: Value,
}

fn collect_explicit_keys(kind: FileKind, doc: &DocumentMut) -> Vec<Found> {
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
) -> usize {
    let mut kept = 0;
    let mut pending = keys;
    while !pending.is_empty() {
        let mut progressed = false;
        let mut next = Vec::new();
        for PendingKey { logical, value } in pending {
            let mut trial = doc.clone();
            match apply_logical_key(&mut trial, path, &logical, value.clone(), agents_home) {
                Ok(()) => {
                    *doc = trial;
                    kept += 1;
                    progressed = true;
                }
                Err(err) => next.push((PendingKey { logical, value }, format!("{err:#}"))),
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

fn walk_table(kind: FileKind, doc_prefix: &[String], table: &Table, out: &mut Vec<Found>) {
    for (key, item) in table.iter() {
        let mut doc_path = doc_prefix.to_vec();
        doc_path.push(key.to_string());
        let logical = to_logical(kind, &doc_path);
        if is_context_meter_band(&logical) {
            if let Some(value) = item_to_value(item) {
                out.push(Found::Settable { logical, value });
            }
        } else if let Some(value) = item.as_value()
            && let Some(inline) = value.as_inline_table()
        {
            walk_inline_table(kind, &doc_path, inline, out);
        } else if let Some(child) = item.as_table() {
            walk_table(kind, &doc_path, child, out);
        } else if let Some(value) = item_to_value(item) {
            out.push(Found::Settable { logical, value });
        }
    }
}

fn walk_inline_table(
    kind: FileKind,
    doc_prefix: &[String],
    table: &InlineTable,
    out: &mut Vec<Found>,
) {
    for (key, value) in table.iter() {
        let mut doc_path = doc_prefix.to_vec();
        doc_path.push(key.to_string());
        let logical = to_logical(kind, &doc_path);
        if is_context_meter_band(&logical) {
            out.push(Found::Settable {
                logical,
                value: value.clone(),
            });
        } else if let Some(inline) = value.as_inline_table() {
            walk_inline_table(kind, &doc_path, inline, out);
        } else {
            out.push(Found::Settable {
                logical,
                value: value.clone(),
            });
        }
    }
}

fn to_logical(kind: FileKind, doc_path: &[String]) -> Vec<String> {
    match kind {
        FileKind::Theme if doc_path.first().is_some_and(|segment| segment == "colors") => {
            std::iter::once("theme".to_owned())
                .chain(doc_path.iter().cloned())
                .collect()
        }
        FileKind::Loop => std::iter::once("loop".to_owned())
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

fn render_all_templates() -> String {
    format!(
        "# === config.toml ===\n{}# === theme.toml ===\n{}# === agents.toml ===\n{}# === loop.toml ===\n{}",
        MachineConfig::template_core(),
        MachineConfig::template_theme(),
        MachineConfig::template_agents(),
        MachineConfig::template_loop()
    )
}

fn file_for_key(path: &[String]) -> (PathBuf, &'static str) {
    match path.first().map(String::as_str) {
        Some("theme") => (MachineConfig::theme_path(), MachineConfig::template_theme()),
        Some("agents") => (
            MachineConfig::agents_path(),
            MachineConfig::template_agents(),
        ),
        Some("loop") => (MachineConfig::loop_path(), MachineConfig::template_loop()),
        _ => (MachineConfig::config_path(), MachineConfig::template_core()),
    }
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
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

pub(super) fn config_value(config: &MachineConfig) -> Result<toml::Value> {
    let text = toml::to_string(config).context("serializing per-machine config")?;
    toml::from_str(&text).context("building per-machine config value")
}

fn value_at<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(root, |value, segment| match value {
        toml::Value::Table(table) => table.get(segment),
        _ => None,
    })
}

fn render_value(value: &toml::Value) -> Result<String> {
    let rendered = match value {
        toml::Value::String(value) => {
            let mut out = value.clone();
            out.push('\n');
            out
        }
        toml::Value::Integer(value) => format!("{value}\n"),
        toml::Value::Float(value) => format!("{value}\n"),
        toml::Value::Boolean(value) => format!("{value}\n"),
        toml::Value::Datetime(value) => format!("{value}\n"),
        toml::Value::Array(_) => format!("{value}\n"),
        toml::Value::Table(_) => {
            let mut out = toml::to_string_pretty(value).context("rendering TOML value")?;
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    };
    Ok(rendered)
}

fn parse_key(key: &str) -> Result<Vec<String>> {
    let segments: Vec<String> = key.split('.').map(str::to_owned).collect();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        bail!("config keys use non-empty dotted segments");
    }
    Ok(segments)
}

fn validate_set_key(path: &[String]) -> Result<()> {
    reject_reserved_set_key(path)?;
    let (config_path, template) = file_for_key(path);
    let doc_key = document_key_for_set(path);
    let mut doc = template
        .parse::<DocumentMut>()
        .context("parsing shipped config template")?;
    set_document_value(&mut doc, &doc_key, Value::from("__rimz_probe__"))?;
    reject_if_ignored(&config_path, path, &doc_key, &doc)
}

fn reject_unknown_set_key(path: &Path, logical: &[String], doc: &DocumentMut) -> Result<()> {
    reject_reserved_set_key(logical)?;
    reject_if_ignored(path, logical, &document_key_for_set(logical), doc)
}

fn reject_reserved_set_key(path: &[String]) -> Result<()> {
    let joined = path.join(".");
    if matches!(path, [root, child, ..] if root == "notifications" && child == "handler") {
        bail!(
            "config key `{joined}` is an array of tables; edit {}",
            MachineConfig::config_path().display()
        );
    }
    if is_context_meter_subfield(path) || is_disallowed_set_container(path) {
        bail!("unknown config key `{joined}`");
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
        Err(err) if err.to_string().contains("unknown field") => {
            bail!("unknown config key `{}`", logical.join("."));
        }
        Err(_) => return Ok(()),
    };
    let document_key = doc_key.join(".");
    if ignored_path_matches(&ignored, &document_key) {
        bail!("unknown config key `{}`", logical.join("."));
    }
    Ok(())
}

fn is_known_get_key(path: &[String]) -> Result<bool> {
    if is_context_meter_subfield(path) || is_unknown_get_shape(path) {
        return Ok(false);
    }
    let (config_path, template) = file_for_key(path);
    let doc_key = document_key_for_set(path);
    if doc_key.is_empty() {
        return Ok(true);
    }
    let mut doc = template
        .parse::<DocumentMut>()
        .context("parsing shipped config template")?;
    set_document_value(&mut doc, &doc_key, Value::from("__rimz_probe__"))?;
    let ignored = match MachineConfig::parse_text_unknown_keys(&config_path, &doc.to_string()) {
        Ok(ignored) => ignored,
        Err(err) if err.to_string().contains("unknown field") => return Ok(false),
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
                    && !ANIMATION_ROLES.contains(&role.as_str())
        )
        || matches!(
            path,
            [root, child, role, field]
                if root == "theme"
                    && child == "animations"
                    && !(ANIMATION_ROLES.contains(&role.as_str())
                        && ANIMATION_FIELDS.contains(&field.as_str()))
        )
        || matches!(path, [root, child, _, _, ..] if root == "theme" && child == "animations" && path.len() > 4)
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_theme_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_none()
        )
        || matches!(path, [root, child, set, _, _, ..] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set) && path.len() > 5)
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
                    && !(ANIMATION_ROLES.contains(&role.as_str())
                        && ANIMATION_FIELDS.contains(&field.as_str()))
        )
        || matches!(path, [root, child, _, _, ..] if root == "theme" && child == "animations" && path.len() > 4)
        || matches!(path, [root, child, set] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set))
        || matches!(path, [root, child, set, ..] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set) && path.len() < 5)
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_theme_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_none()
        )
        || matches!(path, [root, child, set, _, _, ..] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set) && path.len() > 5)
        || matches!(path, [root, tasks, _] if root == "loop" && tasks == "tasks")
        || matches!(path, [root, tasks, _, wake] if root == "loop" && tasks == "tasks" && wake == "wake")
}

fn is_theme_glyph_set(set: &str) -> bool {
    matches!(set, "unicode" | "nerd_font")
}

const CONTEXT_METER_BANDS: &[&str] = &["green", "yellow", "amber", "red"];
const ANIMATION_ROLES: &[&str] = &[
    "thinking",
    "working",
    "compacting",
    "delegating",
    "resolving",
    "idle",
    "success",
    "paused",
    "waiting",
    "failed",
];
const ANIMATION_FIELDS: &[&str] = &["frames", "color", "effect", "speed"];

fn parse_edit_value(raw: &str) -> Value {
    raw.parse::<Value>()
        .unwrap_or_else(|_| Value::from(raw.to_owned()))
}

fn parse_set_value(path: &[String], raw: &str) -> Value {
    if is_harness_smart_compact_edit(path)
        || is_harness_rtk_edit(path)
        || is_daily_budget_edit(path)
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
    if is_daily_budget_edit(path) {
        let Some(raw) = value.as_str() else {
            bail!("{} must be a string ending in `/day`", path.join("."));
        };
        raw.parse::<rimz::config::DayCap>()
            .map_err(anyhow::Error::msg)?;
    }
    if is_harness_smart_compact_edit(path) {
        let Some(threshold) = value.as_str() else {
            bail!("harness.smart_compact must be a string");
        };
        if let Err(err) = rimz::message::AutoCompact::parse(threshold) {
            bail!("{err}");
        }
    }
    if is_harness_rtk_edit(path) {
        let Some(mode) = value.as_str() else {
            bail!("harness.rtk must be a string");
        };
        if !matches!(mode, "auto" | "on" | "off") {
            bail!("harness.rtk must be one of auto, on, or off");
        }
    }
    if matches!(
        path,
        [root, leaf] if root == "theme" && leaf == "scheme"
    ) {
        let Some(scheme) = value.as_str() else {
            bail!("theme.scheme must be a string");
        };
        if let Err(err) = rimz::sidebar_pane::render::scheme::validate_explicit_scheme(scheme) {
            bail!("{err}");
        }
    }
    if matches!(
        path,
        [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set"
    ) {
        let Some(source) = value.as_str() else {
            bail!("theme.glyphs.set must be a string");
        };
        if let Err(err) = validate_glyph_source(source) {
            bail!("{err}");
        }
    }
    if let [root, child, set, namespace, role] = path
        && root == "theme"
        && child == "glyphs"
        && is_theme_glyph_set(set)
        && GlyphRole::from_namespaced(namespace, role).is_some()
    {
        let Some(glyph) = value.as_str() else {
            bail!("theme.glyphs.{set}.{namespace}.{role} must be a string");
        };
        if let Err(err) = validate_glyph_cells(glyph) {
            bail!("sidebar glyph `{namespace}.{role}` {err}");
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

fn is_sidebar_glyph_string_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "glyphs")
        || matches!(path, [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set")
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_theme_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_some()
        )
}

fn normalize_set_key(path: &[String], value: &Value) -> Result<Vec<String>> {
    if matches!(path, [root] if root == "theme") {
        if !value.is_str() {
            bail!("theme shorthand sets a scheme string");
        }
        return Ok(["theme", "scheme"].into_iter().map(str::to_owned).collect());
    }
    if matches!(path, [root, child] if root == "theme" && child == "glyphs") {
        if !value.is_str() {
            bail!("theme.glyphs shorthand sets a glyph set string");
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
            .with_context(|| format!("`{segment}` is not a table"))?;
    }
    let leaf = path.last().expect("validated key has a leaf");
    table[leaf] = value_to_item(value);
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

#[expect(clippy::print_stdout, reason = "config command stdout")]
fn print_line(line: &str) -> Result<()> {
    println!("{line}");
    Ok(())
}

#[expect(clippy::print_stdout, reason = "config command stdout")]
fn print_text(text: &str) -> Result<()> {
    print!("{text}");
    std::io::stdout().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests;
