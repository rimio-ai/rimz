//! `rimz config` — inspect and edit the per-machine config.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::MachineConfig;
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
    /// Write a commented default config template.
    Init(InitArgs),
    /// Print the resolved per-machine config path.
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
    /// Dotted config key, for example `sidebar.max_cols`.
    key: Option<String>,
    /// Emit JSON instead of TOML/plain scalar output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// Dotted config key, for example `sidebar.max_cols`.
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
    let path = MachineConfig::path();
    if path.exists() && !force {
        return Ok(false);
    }
    write_bytes_atomically(&path, MachineConfig::template().as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn init(args: InitArgs) -> Result<()> {
    if args.print {
        print_text(MachineConfig::template())?;
        return Ok(());
    }

    let path = MachineConfig::path();
    if path.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    write_default_config(args.force)?;
    print_line(&format!("wrote {}", path.display()))
}

fn print_path() -> Result<()> {
    print_line(&MachineConfig::path().display().to_string())
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
    let path = MachineConfig::path();
    let key = parse_key(&args.key)?;
    validate_set_key(&key)?;

    let text = read_config_or_template(&path)?;
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let value = parse_edit_value(&args.value);
    validate_set_value(&key, &value)?;
    set_document_value(&mut doc, &key, value)?;

    let rendered = doc.to_string();
    toml::from_str::<MachineConfig>(&rendered)
        .with_context(|| format!("validating `{}`", args.key))?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    print_line(&format!("set {}", args.key))
}

fn read_config_or_template(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(MachineConfig::template().to_owned())
        }
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
        || matches!(path, [root] if root == "tab")
        || matches!(path, [root, child] if root == "tab" && matches!(child.as_str(), "keywords" | "layouts"))
        || is_account_usage_limit_get_key(path)
        || is_sidebar_animation_get_key(path)
        || matches!(path, [root, child] if root == "sidebar" && child == "providers")
        || matches!(path, [root, child, _] if root == "sidebar" && child == "providers")
}

fn is_exact_or_dynamic_set_key(path: &[String]) -> bool {
    let joined = path.join(".");
    exact_set_keys().contains(&joined)
        || is_tab_key(path)
        || is_account_usage_limit_key(path)
        || is_provider_style_key(path)
        || is_sidebar_animation_set_key(path)
}

fn is_tab_key(path: &[String]) -> bool {
    matches!(path, [root, child, _] if root == "tab" && child == "layouts")
        || matches!(path, [root, child, _] if root == "tab" && child == "keywords")
        || matches!(
            path,
            [root, child, _, leaf]
                if root == "tab"
                    && child == "keywords"
                    && matches!(leaf.as_str(), "command" | "agent" | "mode" | "args")
        )
}

fn is_provider_style_key(path: &[String]) -> bool {
    path.len() == 4
        && path[0] == "sidebar"
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
    matches!(path, [root, child] if root == "sidebar" && child == "animations")
        || matches!(path, [root, child, role] if root == "sidebar" && child == "animations" && is_sidebar_animation_role(role))
}

fn is_sidebar_animation_set_key(path: &[String]) -> bool {
    matches!(
        path,
        [root, child, role, field]
            if root == "sidebar"
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

fn exact_set_keys() -> BTreeSet<String> {
    [
        "worktree.dir",
        "worktree.base",
        "accounts.oauth_usage",
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
        "sidebar.refresh_ms",
        "sidebar.max_provider_blocks",
        "sidebar.provider_tabs",
        "sidebar.provider_list",
        "sidebar.max_cols",
        "sidebar.card_density",
        "sidebar.context.yellow",
        "sidebar.context.amber",
        "sidebar.context.red",
        "sidebar.budget.yellow",
        "sidebar.budget.amber",
        "sidebar.budget.red",
        "sidebar.budget.pace.yellow",
        "sidebar.budget.pace.amber",
        "sidebar.budget.pace.red",
        "sidebar.attention.stalled_after_secs",
        "sidebar.trunk",
        "sidebar.theme.mode",
        "sidebar.theme.scheme",
        "sidebar.theme.good",
        "sidebar.theme.warn",
        "sidebar.theme.caution",
        "sidebar.theme.alarm",
        "sidebar.theme.accent",
        "sidebar.theme.cool",
        "sidebar.theme.meta",
        "sidebar.theme.soft",
        "sidebar.theme.dim",
        "sidebar.theme.faint",
        "sidebar.theme.rule",
        "sidebar.theme.selection",
        "sidebar.scrollbar",
        "sidebar.glow",
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
        "resume.on_rebirth",
        "resume.max",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn parse_edit_value(raw: &str) -> Value {
    raw.parse::<Value>()
        .unwrap_or_else(|_| Value::from(raw.to_owned()))
}

fn validate_set_value(path: &[String], value: &Value) -> Result<()> {
    if matches!(
        path,
        [root, child, leaf] if root == "sidebar" && child == "theme" && leaf == "scheme"
    ) {
        let Some(scheme) = value.as_str() else {
            bail!("sidebar.theme.scheme must be a string");
        };
        if scheme != "auto"
            && let Err(err) = rimz::sidebar_pane::render::scheme::validate_explicit_scheme(scheme)
        {
            bail!("{err}");
        }
    }
    Ok(())
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
            "sidebar.max_cols",
            "sidebar.budget.pace.red",
            "accounts.oauth_usage",
            "accounts.usage_limit_usd.codex",
            "tab.layouts.review",
            "tab.keywords.vim",
            "tab.keywords.codex-yolo.agent",
            "tab.keywords.codex-yolo.mode",
            "tab.keywords.codex-yolo.args",
            "tab.keywords.htop.command",
            "zellij.auto_layout",
            "sidebar.providers.claude.color",
            "sidebar.theme.mode",
            "sidebar.theme.scheme",
            "sidebar.theme.caution",
            "sidebar.animations.thinking.frames",
            "sidebar.animations.working.color",
            "sidebar.animations.idle.effect",
            "sidebar.animations.success.speed",
        ] {
            validate_set_key(&parse_key(key).unwrap()).unwrap_or_else(|err| panic!("{key}: {err}"));
        }

        for key in [
            "sidebar.nope",
            "accounts.nope",
            "accounts.usage_limit_usd",
            "accounts.usage_limit_usd.codex.extra",
            "tab.layouts.peer.shape",
            "tab.keywords.codex-yolo.flags",
            "sidebar.providers.claude.nope",
            "sidebar.animations",
            "sidebar.animations.nope.frames",
            "sidebar.animations.thinking.nope",
            "sidebar.animations.thinking.frames.extra",
        ] {
            assert!(validate_set_key(&parse_key(key).unwrap()).is_err(), "{key}");
        }

        for (key, known) in [
            ("sidebar.animations", true),
            ("sidebar.animations.thinking", true),
            ("sidebar.animations.thinking.frames", true),
            ("sidebar.animations.nope", false),
            ("accounts", true),
            ("accounts.usage_limit_usd", true),
            ("accounts.usage_limit_usd.codex", true),
        ] {
            assert_eq!(is_known_get_key(&parse_key(key).unwrap()), known, "{key}");
        }
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

    fn set_key_reaches_leaf(set_keys: &BTreeSet<String>, leaf: &str) -> bool {
        set_keys.contains(leaf)
            || set_keys.iter().any(|key| {
                leaf.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
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
