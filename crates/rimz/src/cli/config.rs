//! `rimz config` — inspect and edit the per-machine config.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::{GlyphRole, MachineConfig, validate_glyph_cells};
use rimz::ledger::atomic::write_bytes_atomically;
use toml_edit::{DocumentMut, Item, Table, Value};

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
    let kept = apply_merge_keys(path, &mut new_doc, pending, &mut skipped);

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
    apply_logical_key(&mut doc, &path, &key, value)?;
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
) -> Result<()> {
    validate_set_value(logical, &value)?;
    set_document_value(doc, &document_key_for_set(logical), value)?;
    MachineConfig::parse_text(path, &doc.to_string())
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
}

impl FileKind {
    fn for_path(path: &Path) -> Self {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("theme.toml") => Self::Theme,
            Some("agents.toml") => Self::Agents,
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
) -> usize {
    let mut kept = 0;
    let mut pending = keys;
    while !pending.is_empty() {
        let mut progressed = false;
        let mut next = Vec::new();
        for PendingKey { logical, value } in pending {
            let mut trial = doc.clone();
            match apply_logical_key(&mut trial, path, &logical, value.clone()) {
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
    toml::from_str::<toml::Table>(&format!("x = {}", value.to_string().trim()))
        .ok()?
        .remove("x")
}

fn render_all_templates() -> String {
    format!(
        "# === config.toml ===\n{}# === theme.toml ===\n{}# === agents.toml ===\n{}",
        MachineConfig::template_core(),
        MachineConfig::template_theme(),
        MachineConfig::template_agents()
    )
}

fn file_for_key(path: &[String]) -> (PathBuf, &'static str) {
    match path.first().map(String::as_str) {
        Some("theme") => (MachineConfig::theme_path(), MachineConfig::template_theme()),
        Some("agents") => (
            MachineConfig::agents_path(),
            MachineConfig::template_agents(),
        ),
        _ => (MachineConfig::config_path(), MachineConfig::template_core()),
    }
}

fn document_key_for_set(path: &[String]) -> Vec<String> {
    if matches!(path, [root, child, ..] if root == "theme" && child == "colors") {
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
        || matches!(path, [root] if root == "agents")
        || matches!(path, [root] if root == "accounts")
        || matches!(path, [root, child] if root == "agents" && matches!(child.as_str(), "profiles" | "commands" | "teams" | "worktree" | "loop" | "attention" | "pets"))
        || is_account_usage_limit_get_key(path)
        || is_sidebar_animation_get_key(path)
        || is_sidebar_glyph_get_key(path)
        || is_theme_colors_get_key(path)
        || matches!(path, [root, child] if root == "theme" && child == "providers")
        || matches!(path, [root, child, _] if root == "theme" && child == "providers")
}

fn is_exact_or_dynamic_set_key(path: &[String]) -> bool {
    let joined = path.join(".");
    exact_set_keys().contains(&joined)
        || is_agents_key(path)
        || is_account_usage_limit_key(path)
        || is_provider_style_key(path)
        || is_sidebar_animation_set_key(path)
        || is_sidebar_glyph_set_key(path)
        || is_theme_colors_set_key(path)
}

fn is_agents_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, _, leaf]
            if root == "agents" && child == "teams" && matches!(leaf.as_str(), "roles" | "layout")
    ) || matches!(path, [root, child, _] if root == "agents" && child == "commands")
        || matches!(
            path,
            [root, child, _, leaf]
                if root == "agents"
                    && child == "profiles"
                    && matches!(leaf.as_str(), "agent" | "mode" | "model" | "effort" | "args" | "system-prompt-file")
        )
        || matches!(
            path,
            [root, loop_, tasks, _, leaf]
                if root == "agents"
                    && loop_ == "loop"
                    && tasks == "tasks"
                    && matches!(
                        leaf.as_str(),
                        "spec"
                            | "prompt"
                            | "prompt-file"
                            | "root"
                            | "worktree"
                            | "mode"
                            | "effort"
                            | "system-prompt-file"
                            | "timeout"
                            | "at"
                            | "days"
                            | "every"
                            | "cron"
                            | "once"
                    )
        )
}

fn is_provider_style_key(path: &[String]) -> bool {
    path.len() == 4
        && path[0] == "theme"
        && path[1] == "providers"
        && matches!(path[3].as_str(), "product_name" | "ascii_art" | "color")
}

fn is_account_usage_limit_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, provider] if root == "accounts" && child == "usage_limit_usd" && !provider.is_empty()
    )
}

fn is_account_usage_limit_get_key(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "accounts" && child == "usage_limit_usd")
        || is_account_usage_limit_key(path)
}

fn is_sidebar_animation_get_key(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "animations")
        || matches!(path, [root, child, role] if root == "theme" && child == "animations" && is_sidebar_animation_role(role))
}

fn is_sidebar_animation_set_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, role, field]
            if root == "theme"
                && child == "animations"
                && is_sidebar_animation_role(role)
                && matches!(field.as_str(), "frames" | "color" | "effect" | "speed")
    )
}

fn is_sidebar_animation_role(role: &str) -> bool {
    matches!(
        role,
        "thinking"
            | "working"
            | "compacting"
            | "delegating"
            | "resolving"
            | "idle"
            | "success"
            | "paused"
            | "waiting"
            | "failed"
    )
}

fn is_sidebar_glyph_get_key(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "glyphs")
        || matches!(path, [root, child, leaf] if root == "theme" && child == "glyphs" && leaf == "set")
        || matches!(path, [root, child, set] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set))
        || matches!(path, [root, child, set, namespace] if root == "theme" && child == "glyphs" && is_theme_glyph_set(set) && is_sidebar_glyph_namespace(namespace))
        || matches!(
            path,
            [root, child, set, namespace, role]
                if root == "theme"
                    && child == "glyphs"
                    && is_theme_glyph_set(set)
                    && GlyphRole::from_namespaced(namespace, role).is_some()
        )
}

fn is_sidebar_glyph_set_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, set, namespace, role]
            if root == "theme"
                && child == "glyphs"
                && is_theme_glyph_set(set)
                && GlyphRole::from_namespaced(namespace, role).is_some()
    )
}

fn is_theme_glyph_set(set: &str) -> bool {
    matches!(set, "unicode" | "nerd_font")
}

fn is_sidebar_glyph_namespace(namespace: &str) -> bool {
    matches!(
        namespace,
        "status"
            | "cockpit"
            | "tokens"
            | "meter"
            | "clock"
            | "worktree"
            | "card"
            | "process"
            | "keys"
            | "chrome"
    )
}

fn is_theme_colors_get_key(path: &[String]) -> bool {
    matches!(path, [root, child] if root == "theme" && child == "colors")
        || matches!(path, [root, child, table] if root == "theme" && child == "colors" && is_theme_colors_table(table))
        || is_theme_colors_set_key(path)
}

fn is_theme_colors_set_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, table, leaf]
            if root == "theme"
                && child == "colors"
                && match table.as_str() {
                    "primary" => matches!(leaf.as_str(), "background" | "foreground"),
                    "normal" | "bright" => matches!(leaf.as_str(), "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"),
                    "selection" => matches!(leaf.as_str(), "background" | "text"),
                    _ => false,
                }
    )
}

fn is_theme_colors_table(table: &str) -> bool {
    matches!(table, "primary" | "normal" | "bright" | "selection")
}

fn exact_set_keys() -> BTreeSet<String> {
    [
        "agents.worktree.dir",
        "agents.worktree.base",
        "agents.placement",
        "harness.smart_compact",
        "resume.on_rebirth",
        "resume.max",
        "resume.auto_continue",
        "resume.auto_continue_overloaded",
        "resume.auto_continue_overloaded_backoff_secs",
        "resume.auto_continue_overloaded_max_retries",
        "resume.auto_continue_text",
        "remote_control.claude",
        "remote_control.codex",
        "notifications.enabled",
        "notifications.triggers",
        "notifications.desktop",
        "notifications.sound",
        "notifications.suppress_focused",
        "notifications.debounce_ms",
        "notifications.coalesce_ms",
        "notifications.remind_secs",
        "notifications.command",
        "theme.style",
        "theme.display.refresh_ms",
        "theme.display.max_provider_blocks",
        "theme.display.provider_tabs",
        "theme.display.provider_list",
        "theme.display.max_cols",
        "theme.display.scrollbar",
        "theme.display.glow",
        "theme.display.card_density",
        "theme.display.context_meter.green",
        "theme.display.context_meter.yellow",
        "theme.display.context_meter.amber",
        "theme.display.context_meter.red",
        "theme.display.budget_bar.yellow",
        "theme.display.budget_bar.amber",
        "theme.display.budget_bar.red",
        "theme.display.budget_bar.burn_rate.yellow",
        "theme.display.budget_bar.burn_rate.amber",
        "theme.display.budget_bar.burn_rate.red",
        "sidebar.focus_key",
        "sidebar.spend_window",
        "sidebar.spend_timezone",
        "agents.attention.stalled_after_secs",
        "agents.attention.inactive_after_secs",
        "agents.pets.enabled",
        "agents.pets.pet",
        "agents.pets.size",
        "agents.pets.glyphs",
        "agents.pets.voice",
        "agents.loop.tasks",
        "theme.animations.unread",
        "theme.glyphs.set",
        "theme.colors.primary.background",
        "theme.colors.primary.foreground",
        "theme.colors.normal.black",
        "theme.colors.normal.red",
        "theme.colors.normal.green",
        "theme.colors.normal.yellow",
        "theme.colors.normal.blue",
        "theme.colors.normal.magenta",
        "theme.colors.normal.cyan",
        "theme.colors.normal.white",
        "theme.colors.bright.black",
        "theme.colors.bright.red",
        "theme.colors.bright.green",
        "theme.colors.bright.yellow",
        "theme.colors.bright.blue",
        "theme.colors.bright.magenta",
        "theme.colors.bright.cyan",
        "theme.colors.bright.white",
        "theme.colors.selection.background",
        "theme.colors.selection.text",
        "sidebar.trunk",
        "theme.mode",
        "theme.scheme",
        "theme.good",
        "theme.warn",
        "theme.caution",
        "theme.alarm",
        "theme.accent",
        "theme.cool",
        "theme.meta",
        "theme.body",
        "theme.muted",
        "theme.faint",
        "theme.rule",
        "theme.selection",
        "theme.selection_bg",
        "zellij.mouse_mode",
        "zellij.mouse_click_through",
        "zellij.advanced_mouse_actions",
        "zellij.mouse_hover_effects",
        "zellij.focus_follows_mouse",
        "zellij.pane_frames",
        "zellij.on_force_close",
        "zellij.scroll_buffer_size",
        "zellij.show_startup_tips",
        "zellij.show_release_notes",
        "zellij.copy_clipboard",
        "zellij.copy_on_select",
        "zellij.support_kitty_keyboard_protocol",
        "zellij.osc8_hyperlinks",
        "zellij.auto_layout",
        "zellij.session_serialization",
        "tmux.mouse",
        "tmux.focus_events",
        "tmux.history_limit",
        "tmux.allow_passthrough",
        "tmux.set_clipboard",
        "tmux.extended_keys",
        "tmux.extended_keys_format",
        "tmux.escape_time_ms",
        "tmux.renumber_windows",
        "tmux.aggressive_resize",
        "tmux.pane_border_status",
        "tmux.pane_border_lines",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn parse_edit_value(raw: &str) -> Value {
    raw.parse::<Value>()
        .unwrap_or_else(|_| Value::from(raw.to_owned()))
}

fn parse_set_value(path: &[String], raw: &str) -> Value {
    if is_harness_smart_compact_edit(path)
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
    table[leaf] = Item::Value(value);
    Ok(())
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
mod tests {
    use super::*;

    #[test]
    fn validates_config_key_read_and_write_surfaces() {
        for key in [
            "theme.display.max_cols",
            "theme.display.budget_bar.burn_rate.red",
            "accounts.usage_limit_usd.codex",
            "agents.teams.review.roles",
            "agents.teams.review.layout",
            "agents.commands.vim",
            "agents.profiles.codex-slim.agent",
            "agents.profiles.codex-slim.mode",
            "agents.profiles.codex-slim.model",
            "agents.profiles.codex-slim.effort",
            "agents.profiles.codex-slim.args",
            "agents.profiles.codex-slim.system-prompt-file",
            "zellij.auto_layout",
            "theme.providers.claude.color",
            "agents.pets.enabled",
            "agents.pets.pet",
            "agents.pets.size",
            "agents.pets.glyphs",
            "agents.pets.voice",
            "theme.mode",
            "theme.scheme",
            "theme.caution",
            "sidebar.focus_key",
            "sidebar.spend_window",
            "sidebar.spend_timezone",
            "theme.animations.thinking.frames",
            "theme.animations.working.color",
            "theme.animations.idle.effect",
            "theme.animations.success.speed",
            "theme.animations.unread",
            "theme.glyphs.set",
            "theme.glyphs.unicode.status.working",
            "theme.glyphs.unicode.tokens.total",
            "theme.glyphs.unicode.keys.focus",
            "theme.glyphs.unicode.chrome.box_vertical",
            "theme.glyphs.nerd_font.clock.over",
            "resume.auto_continue",
            "resume.auto_continue_text",
            "harness.smart_compact",
        ] {
            validate_set_key(&parse_key(key).unwrap()).unwrap_or_else(|err| panic!("{key}: {err}"));
        }

        for key in [
            "sidebar.nope",
            "accounts.nope",
            "accounts.usage_limit_usd",
            "accounts.usage_limit_usd.codex.extra",
            "agents.teams.peer.shape",
            "agents.profiles.codex-slim.flags",
            "agents.commands.vim.command",
            "theme.providers.claude.nope",
            "theme.animations",
            "theme.animations.nope.frames",
            "theme.animations.thinking.nope",
            "theme.animations.thinking.frames.extra",
            "theme.glyphs.nope",
            "theme.glyphs.unicode.tokens.nope",
            "theme.glyphs.unicode.tokens.total.extra",
        ] {
            assert!(validate_set_key(&parse_key(key).unwrap()).is_err(), "{key}");
        }

        for (key, known) in [
            ("theme.animations", true),
            ("theme.animations.thinking", true),
            ("theme.animations.thinking.frames", true),
            ("theme.animations.unread", true),
            ("theme.animations.nope", false),
            ("theme.glyphs", true),
            ("theme.glyphs.unicode.tokens", true),
            ("theme.glyphs.unicode.keys", true),
            ("theme.glyphs.unicode.tokens.total", true),
            ("theme.glyphs.unicode.keys.focus", true),
            ("theme.glyphs.unicode.tokens.nope", false),
            ("accounts", true),
            ("accounts.usage_limit_usd", true),
            ("accounts.usage_limit_usd.codex", true),
        ] {
            assert_eq!(is_known_get_key(&parse_key(key).unwrap()), known, "{key}");
        }
    }

    #[test]
    fn collect_explicit_keys_maps_theme_colors_and_reports_unknowns() {
        let doc = r##"
[colors.primary]
background = "#000000"
nope = "surprise"
"##
        .parse::<DocumentMut>()
        .expect("parse theme snippet");

        let expected_background = parse_key("theme.colors.primary.background").expect("key");
        let found = collect_explicit_keys(FileKind::Theme, &doc);
        let mut saw_background = false;
        let mut saw_unknown = false;
        for item in found {
            match item {
                Found::Settable { logical, value } if logical == expected_background => {
                    assert_eq!(value.as_str(), Some("#000000"));
                    saw_background = true;
                }
                Found::Unknown(key) if key == "colors.primary.nope" => {
                    saw_unknown = true;
                }
                other => panic!("unexpected key: {other:?}"),
            }
        }
        assert!(saw_background, "background override should be settable");
        assert!(saw_unknown, "unknown color leaf should be reported");
    }

    #[test]
    fn merge_key_oracle_accepts_sentry_and_rejects_bogus_keys() {
        assert!(is_known_merge_key(&parse_key("sentry.dsn").expect("key")));
        assert!(is_known_merge_key(
            &parse_key("sentry.environment").expect("key")
        ));
        assert!(is_known_merge_key(
            &parse_key("notifications.enabled").expect("key")
        ));
        assert!(!is_known_merge_key(
            &parse_key("notifications.nope").expect("key")
        ));
    }

    #[test]
    fn set_keys_cover_serialized_default_leaves() {
        let root = config_value(&MachineConfig::default()).expect("default config serializes");
        let mut leaves = BTreeSet::new();
        collect_leaf_paths("", &root, &mut leaves);
        let set_keys = exact_set_keys();

        for leaf in leaves {
            assert!(
                set_key_reaches_leaf(&set_keys, &leaf),
                "serialized default leaf `{leaf}` is not reachable by config set"
            );
        }
    }

    #[test]
    fn bare_words_become_strings() {
        assert_eq!(parse_edit_value("always").as_str(), Some("always"));
        assert_eq!(parse_edit_value("80").as_integer(), Some(80));
        assert_eq!(parse_edit_value("false").as_bool(), Some(false));
    }

    #[test]
    fn theme_scheme_values_are_parsed_as_strings() {
        let key = parse_key("theme.scheme").expect("key");
        assert_eq!(parse_set_value(&key, "0x96f").as_str(), Some("0x96f"));
        assert_eq!(
            parse_set_value(&key, "\"Catppuccin Mocha\"").as_str(),
            Some("Catppuccin Mocha")
        );

        let shorthand = parse_key("theme").expect("key");
        assert_eq!(parse_set_value(&shorthand, "0x96f").as_str(), Some("0x96f"));

        let numeric = parse_key("theme.display.max_cols").expect("key");
        assert_eq!(parse_set_value(&numeric, "80").as_integer(), Some(80));
    }

    #[test]
    fn glyph_values_are_parsed_as_strings() {
        let set = parse_key("theme.glyphs.set").expect("key");
        assert_eq!(
            parse_set_value(&set, "nerd_font").as_str(),
            Some("nerd_font")
        );

        let shorthand = parse_key("theme.glyphs").expect("key");
        assert_eq!(
            parse_set_value(&shorthand, "nerd_font").as_str(),
            Some("nerd_font")
        );

        let leaf = parse_key("theme.glyphs.unicode.process.cpu").expect("key");
        assert_eq!(parse_set_value(&leaf, "1").as_str(), Some("1"));
    }

    #[test]
    fn harness_smart_compact_values_are_parsed_as_strings() {
        let key = parse_key("harness.smart_compact").expect("key");

        assert_eq!(parse_set_value(&key, "70%").as_str(), Some("70%"));
        assert_eq!(parse_set_value(&key, "120000").as_str(), Some("120000"));
    }

    #[test]
    fn harness_smart_compact_validation_rejects_bad_values() {
        let key = parse_key("harness.smart_compact").expect("key");

        validate_set_value(&key, &Value::from("70%")).expect("percent threshold");
        validate_set_value(&key, &Value::from("120000")).expect("token threshold");

        let err = validate_set_value(&key, &Value::from("abc"))
            .expect_err("invalid smart-compact threshold")
            .to_string();
        assert!(
            err.contains("invalid auto-compact threshold `abc`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn theme_scheme_validation_accepts_bundled_names_and_rejects_auto() {
        let key = parse_key("theme.scheme").expect("key");

        validate_set_value(&key, &Value::from("Afterglow")).expect("bundled theme");
        validate_set_value(&key, &Value::from("0x96f")).expect("numeric-looking bundled theme");

        let err = validate_set_value(&key, &Value::from("auto"))
            .expect_err("auto is no longer a selectable scheme")
            .to_string();
        assert!(
            err.contains("unknown sidebar theme scheme `auto`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn glyph_validation_accepts_sets_and_rejects_bad_values() {
        let set = parse_key("theme.glyphs.set").expect("key");
        validate_set_value(&set, &Value::from("unicode")).expect("unicode");
        validate_set_value(&set, &Value::from("nerd_font")).expect("nerd_font");

        let err = validate_set_value(&set, &Value::from("auto"))
            .expect_err("unknown glyph set")
            .to_string();
        assert!(
            err.contains("unknown theme glyph set `auto`"),
            "unexpected error: {err}"
        );

        let leaf = parse_key("theme.glyphs.unicode.tokens.total").expect("key");
        validate_set_value(&leaf, &Value::from("◇")).expect("single-cell glyph");
        validate_set_value(&leaf, &Value::from("\u{efa0} ")).expect("double-width glyph");
        let err = validate_set_value(&leaf, &Value::from("abc"))
            .expect_err("over-wide glyph")
            .to_string();
        assert!(
            err.contains("must occupy one or two terminal cells"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sidebar_theme_set_key_is_scheme_shorthand() {
        let key = parse_key("theme").expect("key");
        assert_eq!(
            normalize_set_key(&key, &Value::from("Afterglow")).expect("normalize"),
            parse_key("theme.scheme").expect("scheme key")
        );

        let err = normalize_set_key(&key, &Value::from(256))
            .expect_err("shorthand only accepts a scheme string")
            .to_string();
        assert!(
            err.contains("theme shorthand sets a scheme string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sidebar_glyphs_set_key_is_set_shorthand() {
        let key = parse_key("theme.glyphs").expect("key");
        assert_eq!(
            normalize_set_key(&key, &Value::from("nerd_font")).expect("normalize"),
            parse_key("theme.glyphs.set").expect("glyph set key")
        );

        let err = normalize_set_key(&key, &Value::from(256))
            .expect_err("shorthand only accepts a set string")
            .to_string();
        assert!(
            err.contains("theme.glyphs shorthand sets a glyph set string"),
            "unexpected error: {err}"
        );
    }

    fn set_key_reaches_leaf(set_keys: &BTreeSet<String>, leaf: &str) -> bool {
        set_keys.contains(leaf)
            || set_keys.iter().any(|key| {
                leaf.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
            || parse_key(leaf)
                .ok()
                .is_some_and(|key| validate_set_key(&key).is_ok())
    }

    fn collect_leaf_paths(prefix: &str, value: &toml::Value, out: &mut BTreeSet<String>) {
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
}
