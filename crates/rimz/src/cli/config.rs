//! `rimz config` — inspect and edit the per-machine config.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::{GlyphRole, MachineConfig, validate_glyph_cells};
use rimz::ledger::atomic::write_bytes_atomically;
use rimz::ledger::paths;
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

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
    key: Option<String>,
    /// Emit JSON instead of TOML/plain scalar output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// Dotted config key, for example `theme.display.max_cols`.
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
}

#[derive(Debug)]
pub(crate) struct SkippedKey {
    pub(crate) key: String,
    pub(crate) reason: SkipReason,
}

#[derive(Debug)]
pub(crate) enum SkipReason {
    Unknown,
    Invalid(String),
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
    let Ok(old_doc) = old_text.parse::<DocumentMut>() else {
        write_bytes_atomically(path, template.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(FileMergeOutcome {
            path: path.to_path_buf(),
            action: MergeAction::Wrote,
            skipped: vec![SkippedKey {
                key: "<file>".to_owned(),
                reason: SkipReason::Invalid("unparseable; rewritten from template".to_owned()),
            }],
        });
    };

    let mut new_doc = template
        .parse::<DocumentMut>()
        .context("parsing shipped config template")?;
    let kind = FileKind::for_path(path);
    let mut pending = Vec::new();
    let mut skipped = Vec::new();
    for found in collect_explicit_keys(kind, &old_doc) {
        match found {
            Found::Unknown(key) => skipped.push(SkippedKey {
                key,
                reason: SkipReason::Unknown,
            }),
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
                None if is_known_get_key(&parsed) => bail!("config key `{key}` is unset"),
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
    let requested_key = parse_key(&args.key)?;
    let value = parse_set_value(&requested_key, &args.value);
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
    print_line(&format!("set {}", args.key))
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
    Unknown(String),
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
                reason: SkipReason::Invalid(err),
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
        if is_known_merge_key(&logical) {
            match item.clone().into_value() {
                Ok(value) => out.push(Found::Settable { logical, value }),
                Err(_) => out.push(Found::Unknown(doc_path.join("."))),
            }
        } else if let Some(child) = item.as_table() {
            walk_table(kind, &doc_path, child, out);
        } else {
            out.push(Found::Unknown(doc_path.join(".")));
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

fn is_known_merge_key(logical: &[String]) -> bool {
    validate_set_key(logical).is_ok()
        || matches!(
            logical,
            [root, leaf]
                if root == "sentry" && matches!(leaf.as_str(), "dsn" | "environment")
        )
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

fn config_value(config: &MachineConfig) -> Result<toml::Value> {
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
    let joined = path.join(".");
    if matches!(path, [root, child, ..] if root == "notifications" && child == "handler") {
        bail!(
            "config key `{joined}` is an array of tables; edit {}",
            MachineConfig::config_path().display()
        );
    }
    if is_exact_or_dynamic_set_key(path) {
        return Ok(());
    }
    bail!("unknown config key `{joined}`")
}

fn is_known_get_key(path: &[String]) -> bool {
    if is_exact_or_dynamic_set_key(path) {
        return true;
    }
    let joined = path.join(".");
    let prefix = format!("{joined}.");
    exact_set_keys().iter().any(|key| key.starts_with(&prefix))
        || key_patterns()
            .iter()
            .any(|pattern| pattern.get && pattern_matches_prefix(path, pattern))
}

fn is_exact_or_dynamic_set_key(path: &[String]) -> bool {
    let joined = path.join(".");
    exact_set_keys().contains(&joined)
        || key_patterns()
            .iter()
            .any(|pattern| pattern.set && path_matches_pattern(path, pattern))
}

fn is_sidebar_glyph_set_key(path: &[String]) -> bool {
    key_patterns()
        .iter()
        .any(|pattern| pattern.name == "theme glyph" && path_matches_pattern(path, pattern))
}

fn is_theme_glyph_set(set: &str) -> bool {
    GLYPH_SETS.contains(&set)
}

#[derive(Clone, Copy)]
struct KeyPattern {
    name: &'static str,
    segments: &'static [KeySegment],
    set: bool,
    get: bool,
}

#[derive(Clone, Copy)]
enum KeySegment {
    Lit(&'static str),
    Any,
    OneOf(&'static [&'static str]),
    Pred(fn(&[String], usize) -> bool),
}

const AGENT_PROFILE_FIELDS: &[&str] = &[
    "agent",
    "mode",
    "model",
    "effort",
    "args",
    "system-prompt-file",
];
const AGENT_TEAM_FIELDS: &[&str] = &["roles", "layout"];
const LOOP_TASK_FIELDS: &[&str] = &[
    "spec",
    "prompt",
    "prompt-file",
    "check",
    "on",
    "root",
    "worktree",
    "mode",
    "effort",
    "system-prompt-file",
    "timeout",
    "at",
    "days",
    "every",
    "cron",
    "deadline",
    "once",
    "bind",
];
const LOOP_BIND_FIELDS: &[&str] = &["kind", "session", "handle"];
const PROVIDER_STYLE_FIELDS: &[&str] = &["product_name", "ascii_art", "color"];
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
const CONTEXT_METER_BANDS: &[&str] = &["green", "yellow", "amber", "red"];
const GLYPH_SETS: &[&str] = &["unicode", "nerd_font"];
const GLYPH_NAMESPACES: &[&str] = &[
    "status", "cockpit", "tokens", "meter", "clock", "worktree", "card", "process", "keys",
    "chrome",
];
const COLOR_TABLES: &[&str] = &["primary", "normal", "bright", "selection"];

fn key_patterns() -> &'static [KeyPattern] {
    &[
        KeyPattern {
            name: "agents team",
            segments: &[
                KeySegment::Lit("agents"),
                KeySegment::Lit("teams"),
                KeySegment::Any,
                KeySegment::OneOf(AGENT_TEAM_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "agents command",
            segments: &[
                KeySegment::Lit("agents"),
                KeySegment::Lit("commands"),
                KeySegment::Any,
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "agents profile",
            segments: &[
                KeySegment::Lit("agents"),
                KeySegment::Lit("profiles"),
                KeySegment::Any,
                KeySegment::OneOf(AGENT_PROFILE_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "loop tasks",
            segments: &[KeySegment::Lit("loop"), KeySegment::Lit("tasks")],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "loop task",
            segments: &[
                KeySegment::Lit("loop"),
                KeySegment::Lit("tasks"),
                KeySegment::Any,
                KeySegment::OneOf(LOOP_TASK_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "loop bind",
            segments: &[
                KeySegment::Lit("loop"),
                KeySegment::Lit("tasks"),
                KeySegment::Any,
                KeySegment::Lit("bind"),
                KeySegment::OneOf(LOOP_BIND_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "account usage limit",
            segments: &[
                KeySegment::Lit("accounts"),
                KeySegment::Lit("usage_limit_usd"),
                KeySegment::Any,
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "theme provider",
            segments: &[
                KeySegment::Lit("theme"),
                KeySegment::Lit("providers"),
                KeySegment::Any,
                KeySegment::OneOf(PROVIDER_STYLE_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "theme animation",
            segments: &[
                KeySegment::Lit("theme"),
                KeySegment::Lit("animations"),
                KeySegment::OneOf(ANIMATION_ROLES),
                KeySegment::OneOf(ANIMATION_FIELDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "theme context meter",
            segments: &[
                KeySegment::Lit("theme"),
                KeySegment::Lit("display"),
                KeySegment::Lit("context_meter"),
                KeySegment::OneOf(CONTEXT_METER_BANDS),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "theme glyph",
            segments: &[
                KeySegment::Lit("theme"),
                KeySegment::Lit("glyphs"),
                KeySegment::OneOf(GLYPH_SETS),
                KeySegment::OneOf(GLYPH_NAMESPACES),
                KeySegment::Pred(is_glyph_role_segment),
            ],
            set: true,
            get: true,
        },
        KeyPattern {
            name: "theme colors",
            segments: &[
                KeySegment::Lit("theme"),
                KeySegment::Lit("colors"),
                KeySegment::OneOf(COLOR_TABLES),
                KeySegment::Pred(is_theme_color_leaf_segment),
            ],
            set: true,
            get: true,
        },
    ]
}

fn path_matches_pattern(path: &[String], pattern: &KeyPattern) -> bool {
    path.len() == pattern.segments.len()
        && pattern
            .segments
            .iter()
            .enumerate()
            .all(|(idx, segment)| segment_matches(path, idx, *segment))
}

fn pattern_matches_prefix(path: &[String], pattern: &KeyPattern) -> bool {
    path.len() < pattern.segments.len()
        && pattern
            .segments
            .iter()
            .take(path.len())
            .enumerate()
            .all(|(idx, segment)| segment_matches(path, idx, *segment))
}

fn segment_matches(path: &[String], idx: usize, segment: KeySegment) -> bool {
    let Some(value) = path.get(idx).map(String::as_str) else {
        return false;
    };
    match segment {
        KeySegment::Lit(expected) => value == expected,
        KeySegment::Any => !value.is_empty(),
        KeySegment::OneOf(values) => values.contains(&value),
        KeySegment::Pred(pred) => pred(path, idx),
    }
}

fn is_glyph_role_segment(path: &[String], idx: usize) -> bool {
    let (Some(namespace), Some(role)) = (
        idx.checked_sub(1).and_then(|idx| path.get(idx)),
        path.get(idx),
    ) else {
        return false;
    };
    GlyphRole::from_namespaced(namespace, role).is_some()
}

fn is_theme_color_leaf_segment(path: &[String], idx: usize) -> bool {
    let (Some(table), Some(leaf)) = (
        idx.checked_sub(1).and_then(|idx| path.get(idx)),
        path.get(idx),
    ) else {
        return false;
    };
    match table.as_str() {
        "primary" => matches!(leaf.as_str(), "background" | "foreground"),
        "normal" | "bright" => matches!(
            leaf.as_str(),
            "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"
        ),
        "selection" => matches!(leaf.as_str(), "background" | "text"),
        _ => false,
    }
}

fn exact_set_keys() -> &'static BTreeSet<String> {
    static KEYS: OnceLock<BTreeSet<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let root = config_value(&schema_config())
            .expect("static config key schema must serialize to a TOML value");
        let mut leaves = BTreeSet::new();
        collect_leaf_paths("", &root, &mut leaves);
        leaves
    })
}

fn collect_leaf_paths(prefix: &str, value: &toml::Value, out: &mut BTreeSet<String>) {
    if is_atomic_schema_key(prefix) {
        out.insert(prefix.to_owned());
        return;
    }
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let next = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_leaf_paths(&next, value, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned());
        }
    }
}

fn is_atomic_schema_key(prefix: &str) -> bool {
    let Some(band) = prefix.strip_prefix("theme.display.context_meter.") else {
        return false;
    };
    !band.contains('.') && CONTEXT_METER_BANDS.contains(&band)
}

fn schema_config() -> MachineConfig {
    // Static schema fixture: parse failures are programmer errors in the
    // checked-in config key description.
    toml::from_str(SCHEMA_CONFIG).expect("static config key schema must parse")
}

const SCHEMA_CONFIG: &str = r##"
timezone = "UTC"

[accounts.usage_limit_usd]
schema = 50.0

[remote_control]
claude = true
codex = true

[notifications]
enabled = true
triggers = ["waiting", "failed", "paused", "success"]
desktop = "auto"
sound = "bell"
suppress_focused = true
debounce_ms = 5000
coalesce_ms = 1000
remind_secs = 60
title = "Rimz: {{agent}} {{kind}}"
body = "{{task}}"
command = "ntfy publish rimz"

[sidebar]
spend_window = "24h"
trunk = "main"
focus_key = "Alt+p"
afk_after_secs = 900

[zellij]
mouse_mode = true
mouse_click_through = true
advanced_mouse_actions = true
mouse_hover_effects = false
focus_follows_mouse = false
pane_frames = true
on_force_close = "detach"
scroll_buffer_size = 100000
show_startup_tips = false
show_release_notes = false
copy_clipboard = "system"
copy_on_select = true
support_kitty_keyboard_protocol = true
osc8_hyperlinks = true
auto_layout = true
session_serialization = false

[tmux]
mouse = true
focus_events = true
history_limit = 100000
allow_passthrough = true
set_clipboard = "on"
extended_keys = true
extended_keys_format = "csi-u"
escape_time_ms = 0
renumber_windows = true
aggressive_resize = true
pane_border_status = "top"
pane_border_lines = "heavy"

[resume]
on_rebirth = true
max = 128
auto_continue = true
auto_continue_backoff_secs = [180, 300]
auto_continue_max_retries = 13
auto_continue_text = "continue"

[harness]
smart_compact = "70%"
rtk = "auto"

[theme]
style = "modern"
mode = "truecolor"
scheme = "TokyoNight Night"
good = "green"
warn = "yellow"
caution = "#e0915c"
alarm = "red"
accent = "cyan"
cool = "blue"
meta = "magenta"
body = "#a6a19a"
muted = "#767168"
faint = "#45423d"
rule = "#343230"
selection = "bright_blue"
selection_bg = "#2a2723"

[theme.display]
refresh_ms = 100
max_cols = 72
scrollbar = "auto"
card_density = "auto"
provider_tabs = "auto"
provider_list = ["claude"]
max_provider_blocks = 3

[theme.display.context_meter]
green = { percent = 40, tokens = 100000 }
yellow = { percent = 60, tokens = 160000 }
amber = { percent = 75, tokens = 258000 }
red = { percent = 90, tokens = 420000 }

[theme.display.budget_bar]
yellow = 50
amber = 25
red = 10

[theme.display.budget_bar.burn_rate]
yellow = 100
amber = 150
red = 200

[theme.colors.primary]
background = "#101010"
foreground = "#f0f0f0"

[theme.colors.normal]
black = "#000000"
red = "#aa0000"
green = "#00aa00"
yellow = "#aa5500"
blue = "#0000aa"
magenta = "#aa00aa"
cyan = "#00aaaa"
white = "#aaaaaa"

[theme.colors.bright]
black = "#555555"
red = "#ff5555"
green = "#55ff55"
yellow = "#ffff55"
blue = "#5555ff"
magenta = "#ff55ff"
cyan = "#55ffff"
white = "#ffffff"

[theme.colors.selection]
background = "#223344"
text = "#ddeeff"

[theme.pets]
enabled = true
pet = "dewey"
glyphs = "sextant"
voice = false

[theme.animations]
unread = "blink"

[theme.animations.thinking]
frames = "ab"
color = "accent"
effect = "breathe"
speed = "fast"

[theme.glyphs]
set = "unicode"

[theme.providers.claude]
product_name = "Claude"
ascii_art = "C"
color = "#d97757"

[agents]
placement = "tab"

[agents.worktree]
dir = "../{repo}-worktrees"
base = "fresh"

[agents.attention]
stalled_after_secs = 1800
inactive_after_secs = 3600

[agents.profiles.schema]
agent = "codex"
mode = "auto"
model = "gpt-5.5"
effort = "low"
system-prompt-file = "~/.config/rimz/prompts/schema.md"
args = "--search none"

[agents.commands]
schema = "nvim -p"

[agents.teams.schema]
layout = "coder"

[[agents.teams.schema.roles]]
role = "coder"
profile = "schema"

[loop.tasks.schema]
spec = "codex"
prompt = "check CI"
prompt-file = "~/.config/rimz/prompts/ci.md"
check = "cargo test"
on = "fail"
root = "/tmp"
worktree = "main"
mode = "auto"
effort = "low"
system-prompt-file = "~/.config/rimz/prompts/loop.md"
timeout = "30m"
at = "09:30"
days = "weekdays"
every = "15m"
cron = "*/15 * * * *"
deadline = "2026-07-01T12:00:00Z"
once = true

[loop.tasks.schema.bind]
kind = "claude"
session = "sess-abc123"
handle = "@planner"
"##;

fn parse_edit_value(raw: &str) -> Value {
    raw.parse::<Value>()
        .unwrap_or_else(|_| Value::from(raw.to_owned()))
}

fn parse_set_value(path: &[String], raw: &str) -> Value {
    if is_harness_smart_compact_edit(path)
        || is_harness_rtk_edit(path)
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
        if let Err(err) = rimz::sidebar_pane::render::glyph_set::validate_glyph_source(source) {
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

fn is_sidebar_glyph_string_edit(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "glyphs")
        || matches!(path, [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set")
        || is_sidebar_glyph_set_key(path)
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
