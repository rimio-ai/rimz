//! `rimz setup` — first-run environment report and default config bootstrap.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use rimz::ids::MuxName;
use rimz::trust::TrustState;
use rimz::workspace::WorkspaceResolver;

use super::{GlobalFlags, config};
use crate::cli::render;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Apply the non-interactive default setup: write config only, no hooks or trust.
    #[arg(long)]
    yes: bool,
    /// Replace an existing per-machine config when writing.
    #[arg(long)]
    force: bool,
}

pub fn run(args: SetupArgs, globals: &GlobalFlags) -> Result<()> {
    let report = SetupReport::detect(globals);
    let interactive = std::io::stdin().is_terminal();

    if !interactive && !args.yes {
        print_report(&report)?;
        print_line("No terminal input is available; setup changed nothing.")?;
        print_line(
            "Run `rimz setup --yes` to write the default config, or run setup from a terminal.",
        )?;
        return Ok(());
    }

    if args.yes {
        print_report(&report)?;
        write_config(args.force)?;
        print_line("No hooks or trust grants were changed by --yes.")?;
        print_line("Run `rimz start` when ready.")?;
        return Ok(());
    }

    print_report(&report)?;
    if super::confirm("Write the default per-machine config now?")? {
        write_config(args.force)?;
    } else {
        print_line("Config unchanged.")?;
    }
    print_line("Run `rimz start` when ready.")?;
    Ok(())
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
                    hook_install: descriptor.capabilities.hook_install,
                    hooks_installed: agent.hooks_installed(),
                }
            })
            .collect();

        let config_path = rimz::config::MachineConfig::path();
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

fn write_config(force: bool) -> Result<()> {
    let path = rimz::config::MachineConfig::path();
    if config::write_default_config(force)? {
        print_line(&format!("Wrote {}", path.display()))?;
    } else {
        print_line(&format!(
            "{} already exists; pass --force to replace it.",
            path.display()
        ))?;
    }
    Ok(())
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
        let path_state = if agent.on_path {
            "on PATH"
        } else {
            "not on PATH"
        };
        let hook_state = if !agent.hook_install {
            "hook install unsupported"
        } else if agent.hooks_installed {
            "hooks installed"
        } else {
            "hooks not installed"
        };
        let style = if !agent.on_path {
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
