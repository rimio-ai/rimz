//! `rimz config` — inspect and edit the per-machine config.

use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::{ConfigEditor, MachineConfig};

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

fn init(args: InitArgs) -> Result<()> {
    let editor = ConfigEditor::machine();
    if args.print {
        print_text(&render_all_templates(&editor))?;
        return Ok(());
    }
    let files = editor.files().ordered();
    if !args.force
        && let Some(existing) = files.iter().find(|file| file.path().exists())
    {
        bail!(
            "{} already exists; pass --force to replace the per-machine config set",
            existing.path().display()
        );
    }
    editor.write_defaults(args.force)?;
    for file in files {
        print_line(&format!("wrote {}", file.path().display()))?;
    }
    Ok(())
}

fn render_all_templates(editor: &ConfigEditor) -> String {
    let mut rendered = String::new();
    for file in editor.files().ordered() {
        let name = file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("machine config file has a UTF-8 name");
        rendered.push_str(&format!("# === {name} ===\n"));
        rendered.push_str(file.template());
    }
    rendered
}

fn print_path() -> Result<()> {
    print_line(
        &ConfigEditor::machine()
            .files()
            .core_path()
            .display()
            .to_string(),
    )
}

fn get(args: GetArgs) -> Result<()> {
    let selected = ConfigEditor::machine().get(args.key.as_deref())?;
    if args.json {
        let rendered = serde_json::to_string_pretty(&selected).context("rendering config JSON")?;
        return print_line(&rendered);
    }
    print_text(&render_value(&selected)?)
}

fn set(args: SetArgs) -> Result<()> {
    let transition = remote_control_transition(&args.key, &args.value);
    if let Some((host, true)) = transition {
        preflight_remote_control_toggle(host)?;
    }
    ConfigEditor::machine().set(&args.key, &args.value)?;
    if let Some((host, _)) = transition {
        let machine = MachineConfig::load().context("loading the updated per-machine config")?;
        rimz::remote_control::apply_runtime_toggle(host, &machine)
            .context("applying the remote-control toggle")?;
    }
    print_line(&format!("set {}", args.key))
}

fn remote_control_transition(
    key: &str,
    raw_value: &str,
) -> Option<(rimz::remote_control::RemoteControlHost, bool)> {
    let host = match key {
        "remote_control.claude" => rimz::remote_control::RemoteControlHost::Claude,
        "remote_control.codex" => rimz::remote_control::RemoteControlHost::Codex,
        _ => return None,
    };
    raw_value
        .parse::<toml::Value>()
        .ok()?
        .as_bool()
        .map(|enabled| (host, enabled))
}

fn preflight_remote_control_toggle(host: rimz::remote_control::RemoteControlHost) -> Result<()> {
    let config = match host {
        rimz::remote_control::RemoteControlHost::Claude => rimz::config::RemoteControlConfig {
            claude: true,
            codex: false,
        },
        rimz::remote_control::RemoteControlHost::Codex => rimz::config::RemoteControlConfig {
            claude: false,
            codex: true,
        },
    };
    let result = match host {
        rimz::remote_control::RemoteControlHost::Claude => {
            rimz::remote_control::preflight_claude(&config)
        }
        rimz::remote_control::RemoteControlHost::Codex => {
            rimz::remote_control::preflight_codex(&config)
        }
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) if err.is_uninstalled_host() => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn render_value(value: &toml::Value) -> Result<String> {
    let rendered = match value {
        toml::Value::String(value) => format!("{value}\n"),
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
    fn remote_control_set_values_map_to_live_transitions() {
        use rimz::remote_control::RemoteControlHost;

        assert_eq!(
            remote_control_transition("remote_control.claude", "true"),
            Some((RemoteControlHost::Claude, true))
        );
        assert_eq!(
            remote_control_transition("remote_control.codex", "false"),
            Some((RemoteControlHost::Codex, false))
        );
        assert_eq!(
            remote_control_transition("remote_control.codex", "\"true\""),
            None
        );
        assert_eq!(remote_control_transition("sidebar.enabled", "true"), None);
    }
}
