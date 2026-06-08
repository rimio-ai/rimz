//! `rimz setup` — first-run environment report and default config bootstrap.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use rimz::ids::MuxName;
use rimz::trust::TrustState;
use rimz::workspace::WorkspaceResolver;

use super::{GlobalFlags, config};

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
        print_report(&report);
        print_line("No terminal input is available; setup changed nothing.");
        print_line(
            "Run `rimz setup --yes` to write the default config, or run setup from a terminal.",
        );
        return Ok(());
    }

    if args.yes {
        print_report(&report);
        write_config(args.force)?;
        print_line("No hooks or trust grants were changed by --yes.");
        print_line("Run `rimz start` when ready.");
        return Ok(());
    }

    print_report(&report);
    if super::confirm("Write the default per-machine config now?")? {
        write_config(args.force)?;
    } else {
        print_line("Config unchanged.");
    }
    print_line("Run `rimz start` when ready.");
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
        print_line(&format!("Wrote {}", path.display()));
    } else {
        print_line(&format!(
            "{} already exists; pass --force to replace it.",
            path.display()
        ));
    }
    Ok(())
}

#[expect(clippy::print_stdout, reason = "setup is a user-facing report")]
fn print_report(report: &SetupReport) {
    println!("Rimz setup");
    match &report.mux {
        Ok(mux) => {
            let version = mux.version.as_deref().unwrap_or("version unknown");
            println!("  multiplexer   : {} ({version})", mux.name);
        }
        Err(err) => println!("  multiplexer   : unavailable ({err})"),
    }
    match &report.workspace {
        Ok(workspace) => {
            println!("  project root  : {}", workspace.project_root.display());
            println!("  root class    : {}", workspace.root_class);
            if let Some(trust) = workspace.trust {
                println!("  trust         : {}", trust.as_str());
            }
        }
        Err(err) => println!("  workspace     : could not resolve ({err})"),
    }
    let config_state = if report.config_exists {
        "present"
    } else {
        "missing"
    };
    println!(
        "  config        : {} ({config_state})",
        report.config_path.display()
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
        println!("  agent {:<7}: {path_state}; {hook_state}", agent.name);
    }
}

#[expect(clippy::print_stdout, reason = "setup is a user-facing report")]
fn print_line(line: &str) {
    println!("{line}");
}
