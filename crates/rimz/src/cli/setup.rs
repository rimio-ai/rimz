//! `rimz setup` — first-run environment report and default config bootstrap.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rimz::ids::MuxName;
use rimz::trust::TrustState;
use rimz::workspace::WorkspaceResolver;

use super::{GlobalFlags, config, first_run, room};
use crate::cli::render;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Apply non-interactive setup: refresh config only, no hooks or trust.
    #[arg(long)]
    yes: bool,
}

pub fn run(args: SetupArgs, globals: &GlobalFlags) -> Result<()> {
    let report = SetupReport::detect(globals);
    let interactive = std::io::stdin().is_terminal();

    if !interactive && !args.yes {
        print_report(&report)?;
        print_line("No terminal input is available; setup changed nothing.")?;
        print_line("Run `rimz setup --yes` to refresh config, or run setup from a terminal.")?;
        return Ok(());
    }

    if args.yes {
        print_report(&report)?;
        render_merge_report(&config::merge_default_config()?)?;
        report_remote_template()?;
        print_line("No hooks or trust grants were changed by --yes.")?;
        print_line("Run `rimz start` when ready.")?;
        return Ok(());
    }

    print_report(&report)?;
    let paths = default_config_paths();
    let exists = paths.iter().any(|path| path.exists());
    if exists {
        if super::confirm_with_default("Keep your current config?", true)? {
            render_merge_report(&config::merge_default_config()?)?;
        } else {
            write_fresh_config()?;
        }
    } else {
        write_fresh_config()?;
    }
    report_remote_template()?;
    let hook_intro_rendered = room::ensure_detected_agent_hooks()?;
    let config = rimz::config::MachineConfig::load().context("loading per-machine config")?;
    let defaults = first_run::Defaults::from_config(&config);
    first_run::run(defaults, hook_intro_rendered)?;
    print_line("Run `rimz start` when ready.")?;
    Ok(())
}

/// First-run config bootstrap: write the default config set and remote.toml
/// when absent. Idempotent; returns whether anything was written.
pub(crate) fn ensure_default_config() -> Result<bool> {
    let wrote_core = config::write_default_config(false)?;
    let wrote_remote = rimz::remote::aliases::RemoteAliases::ensure_template()?;
    Ok(wrote_core || wrote_remote)
}

struct SetupReport {
    mux: std::result::Result<DetectedMux, String>,
    workspace: std::result::Result<DetectedWorkspace, String>,
    agents: Vec<DetectedAgent>,
    config_path: PathBuf,
    config_exists: bool,
}

struct DetectedMux {
    name: MuxName,
    version: Option<String>,
}

struct DetectedWorkspace {
    project_root: PathBuf,
    root_class: &'static str,
    trust: Option<TrustState>,
}

struct DetectedAgent {
    name: &'static str,
    on_path: bool,
    /// Where the binary resolves — on `$PATH`, or in a known install dir an
    /// installer used without editing `$PATH`. `None` when nowhere known.
    binary: Option<PathBuf>,
    hook_install: bool,
    hooks_installed: bool,
}

impl SetupReport {
    fn detect(globals: &GlobalFlags) -> Self {
        let mux = match rimz::mux::auto_detect_backend(globals.mux) {
            Ok(name) => {
                let backend = rimz::mux::backend_for(name);
                let version = backend.version().ok().filter(|value| !value.is_empty());
                Ok(DetectedMux { name, version })
            }
            Err(err) => Err(err.to_string()),
        };

        let workspace = match WorkspaceResolver::resolve(".", globals.root.clone()) {
            Ok(ws) => {
                let trust = rimz::trust::status(&ws.project_root)
                    .ok()
                    .map(|report| report.state);
                Ok(DetectedWorkspace {
                    project_root: ws.project_root,
                    root_class: ws.root_class.label(),
                    trust,
                })
            }
            Err(err) => Err(err.to_string()),
        };

        let agents = rimz::agents::ADAPTERS
            .iter()
            .map(|agent| {
                let descriptor = agent.descriptor();
                DetectedAgent {
                    name: descriptor.kind,
                    on_path: which::which(descriptor.kind).is_ok(),
                    binary: rimz::agents::locate_binary(descriptor),
                    hook_install: descriptor.capabilities.hook_install,
                    hooks_installed: agent.hooks_installed(),
                }
            })
            .collect();

        let config_path = rimz::config::MachineConfig::config_path();
        let config_exists = config_path.exists();
        Self {
            mux,
            workspace,
            agents,
            config_path,
            config_exists,
        }
    }
}

fn default_config_paths() -> [PathBuf; 3] {
    [
        rimz::config::MachineConfig::config_path(),
        rimz::config::MachineConfig::theme_path(),
        rimz::config::MachineConfig::agents_path(),
    ]
}

fn write_fresh_config() -> Result<()> {
    config::write_default_config(true)?;
    for path in default_config_paths() {
        print_line(&format!("Wrote {}", path.display()))?;
    }
    Ok(())
}

fn report_remote_template() -> Result<()> {
    if rimz::remote::aliases::RemoteAliases::ensure_template()? {
        print_line(&format!(
            "Wrote {}",
            rimz::remote::aliases::RemoteAliases::config_path().display()
        ))?;
    }
    Ok(())
}

fn render_merge_report(report: &config::MergeReport) -> Result<()> {
    for file in &report.files {
        match file.action {
            config::MergeAction::Wrote => {
                print_line(&format!("Wrote {}", file.path.display()))?;
            }
            config::MergeAction::Merged { kept } => {
                print_line(&format!(
                    "Merged {} - kept {kept} setting(s)",
                    file.path.display()
                ))?;
            }
        }
        for skipped in &file.skipped {
            let reason = format!("invalid: {}", one_line(&skipped.reason));
            print_line(&format!("  skipped {} ({reason})", skipped.key))?;
        }
    }
    Ok(())
}

fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn print_report(report: &SetupReport) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = render::out();
    writeln!(out, "Rimz setup")?;
    let mut kv = render::KeyVals::new().indent(2);
    match &report.mux {
        Ok(mux) => {
            let version = mux.version.as_deref().unwrap_or("version unknown");
            kv.push(
                "multiplexer",
                render::cell(format!("{} ({version})", mux.name)),
            );
        }
        Err(err) => kv.push(
            "multiplexer",
            render::cell(format!("unavailable ({err})")).fg(render::palette::ALARM),
        ),
    }
    match &report.workspace {
        Ok(workspace) => {
            kv.push(
                "project root",
                render::cell(workspace.project_root.display().to_string())
                    .fg(render::palette::ACCENT),
            );
            kv.push("root class", render::cell(workspace.root_class.to_string()));
            if let Some(trust) = workspace.trust {
                kv.push(
                    "trust",
                    render::cell(trust.as_str()).fg(render::status::trust(trust)),
                );
            }
        }
        Err(err) => kv.push(
            "workspace",
            render::cell(format!("could not resolve ({err})")).fg(render::palette::ALARM),
        ),
    }
    let (config_state, config_style) = if report.config_exists {
        ("present", render::palette::GOOD)
    } else {
        ("missing", render::palette::WARN)
    };
    kv.push(
        "config",
        render::cell(format!("{} ({config_state})", report.config_path.display())).fg(config_style),
    );
    for agent in &report.agents {
        let path_state = match (&agent.binary, agent.on_path) {
            (Some(_), true) => "on PATH".to_string(),
            (Some(path), false) => format!("found at {}", path.display()),
            (None, _) => "not found".to_string(),
        };
        let hook_state = if !agent.hook_install {
            "hook install unsupported"
        } else if agent.hooks_installed {
            "hooks installed"
        } else {
            "hooks not installed"
        };
        let style = if agent.binary.is_none() {
            render::palette::ALARM
        } else if agent.hook_install && !agent.hooks_installed {
            render::palette::WARN
        } else {
            render::palette::GOOD
        };
        kv.push(
            format!("agent {}", agent.name),
            render::cell(format!("{path_state}; {hook_state}")).fg(style),
        );
    }
    kv.render(&mut out)
}

fn print_line(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    writeln!(render::out(), "{line}")
}
