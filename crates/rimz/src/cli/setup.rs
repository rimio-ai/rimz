//! `rimz setup` — first-run environment report and default config bootstrap.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rimz::config::{ConfigEditor, MergeAction, MergeReport};
use rimz::ids::MuxName;
use rimz::trust::TrustState;
use rimz::workspace::{RootClass, WorkspaceResolver};

use super::{GlobalFlags, first_run, hooks};
use crate::cli::render;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Write or merge the config files and stop: no questions, no hooks, no trust.
    #[arg(long, alias = "yes")]
    config_only: bool,
}

pub fn run(args: SetupArgs, globals: &GlobalFlags) -> Result<()> {
    let report = SetupReport::detect(globals);
    let interactive = std::io::stdin().is_terminal();

    if !interactive && !args.config_only {
        print_report(&report)?;
        print_line("Setup changed nothing: there is no terminal to ask from.")?;
        print_line(
            "  rimz setup --config-only   write the default config (installs no hooks, grants no trust)",
        )?;
        let missing_hooks = report
            .agents
            .iter()
            .filter(|agent| agent.lacks_hooks())
            .count();
        if missing_hooks > 0 {
            let agents = if missing_hooks == 1 {
                "agent"
            } else {
                "agents"
            };
            print_line(&format!(
                "  rimz hooks install         install hooks for the {missing_hooks} {agents} without them"
            ))?;
        }
        return Ok(());
    }

    if args.config_only {
        print_report(&report)?;
        let editor = ConfigEditor::machine();
        retire_idle_compact_keys(&editor)?;
        render_merge_report(&editor.merge_defaults()?)?;
        report_remote_template()?;
        report_consensus_copy()?;
        print_line("Installed no hooks and granted no trust.")?;
        print_line("Run `rimz start` when ready.")?;
        return Ok(());
    }

    print_report(&report)?;
    let exists = ConfigEditor::machine()
        .files()
        .ordered()
        .iter()
        .any(|file| file.path().exists());
    if exists {
        if super::confirm_with_default("Keep your current config?", true)? {
            retire_idle_compact_keys(&ConfigEditor::machine())?;
            let merge = ConfigEditor::machine().merge_defaults()?;
            let left_unparseable = merge
                .files
                .iter()
                .any(|file| matches!(file.action, MergeAction::LeftUnparseable { .. }));
            render_merge_report(&merge)?;
            if left_unparseable {
                print_line("Fix the unparseable file(s), then rerun `rimz setup`.")?;
                return Ok(());
            }
        } else {
            write_fresh_config()?;
        }
    } else {
        write_fresh_config()?;
    }
    report_remote_template()?;
    report_consensus_copy()?;
    let hook_intro_rendered = hooks::ensure_detected_agent_hooks(interactive)?;
    let config = rimz::config::MachineConfig::load().context("loading per-machine config")?;
    first_run::run(&config, hook_intro_rendered)?;
    print_line("Run `rimz start` when ready.")?;
    Ok(())
}

/// First-run config bootstrap: write the default config set and remote.toml
/// when absent. Idempotent; returns whether anything was written.
pub(crate) fn ensure_default_config() -> Result<bool> {
    let wrote_core = ConfigEditor::machine().write_defaults(false)?;
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
    root_class: RootClass,
    trust: Option<TrustState>,
}

struct DetectedAgent {
    name: &'static str,
    /// Where the binary resolves — on `$PATH`, or in a known install dir an
    /// installer used without editing `$PATH`. `None` when nowhere known.
    binary: Option<PathBuf>,
    hook_install: bool,
    hook_install_blocked: bool,
    hooks_installed: bool,
    hook_upgrade_available: bool,
}

impl DetectedAgent {
    /// Whether a bare `rimz hooks install` would install this agent's hooks.
    fn lacks_hooks(&self) -> bool {
        self.binary.is_some()
            && self.hook_install
            && !self.hook_install_blocked
            && !self.hooks_installed
    }
}

impl SetupReport {
    fn detect(globals: &GlobalFlags) -> Self {
        let login_env = rimz::agents::ambient_env();
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
                    root_class: ws.root_class,
                    trust,
                })
            }
            Err(err) => Err(err.to_string()),
        };

        let agents = rimz::agents::all_definitions()
            .map(|agent| {
                let definition = agent.spec();
                DetectedAgent {
                    name: definition.kind,
                    binary: rimz::agents::locate_binary(definition),
                    hook_install: definition.has_wired_hook_install(),
                    hook_install_blocked: agent
                        .managed_integration()
                        .is_some_and(|integration| integration.install_blocker().is_some()),
                    hooks_installed: agent.hooks_installed(&login_env),
                    hook_upgrade_available: agent
                        .managed_integration()
                        .is_some_and(|integration| integration.upgrade_available(&login_env)),
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

fn write_fresh_config() -> Result<()> {
    let editor = ConfigEditor::machine();
    editor.write_defaults(true)?;
    for file in editor.files().ordered() {
        print_line(&format!("Wrote {}", file.path().display()))?;
    }
    Ok(())
}

fn retire_idle_compact_keys(editor: &ConfigEditor) -> Result<()> {
    let removed = match editor.retire_idle_compact_keys() {
        Ok(removed) => removed,
        // The merge report owns the existing malformed-file diagnostic.
        Err(rimz::config::ConfigEditErr::DocumentParse { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for key in removed {
        print_line(&format!("✓ removed {key} (no longer read)"))?;
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

/// Refresh the read-only copy of the built-in team consensus. It is generated
/// output, not a setting or a definition, so setup rewrites it whenever the
/// embedded text or the version header changed.
fn report_consensus_copy() -> Result<()> {
    let home = rimz::disk::paths::agents_home();
    if let Some(path) = rimz::harness::team_prompt::publish_consensus_copy(&home)
        .context("publishing the team consensus copy")?
    {
        print_line(&format!(
            "Wrote {} (read-only copy of the built-in team consensus)",
            path.display()
        ))?;
    }
    Ok(())
}

fn render_merge_report(report: &MergeReport) -> Result<()> {
    for file in &report.files {
        match file.action {
            MergeAction::Wrote => {
                print_line(&format!("Wrote {}", file.path.display()))?;
            }
            MergeAction::Merged { kept } => {
                print_line(&format!(
                    "Merged {} - kept {kept} setting(s)",
                    file.path.display()
                ))?;
            }
            MergeAction::LeftUnparseable { ref diagnosis } => {
                print_line(&format!(
                    "Left {} untouched - unparseable: {}; fix the file and rerun rimz setup",
                    file.path.display(),
                    diagnosis.summary(),
                ))?;
            }
        }
        for skipped in &file.skipped {
            let reason = format!("invalid: {}", render::one_line(&skipped.reason));
            print_line(&format!("  skipped {} ({reason})", skipped.key))?;
        }
    }
    Ok(())
}

fn print_report(report: &SetupReport) -> std::io::Result<()> {
    render_report(report, &mut render::out())
}

fn render_report(report: &SetupReport, out: &mut impl std::io::Write) -> std::io::Result<()> {
    writeln!(out, "RimZ setup")?;
    let mut kv = render::KeyVals::new().indent(2);
    match &report.mux {
        Ok(mux) => {
            let version = mux.version.as_deref().unwrap_or("version unknown");
            let prefix = format!("{} ", mux.name);
            let version = version.strip_prefix(prefix.as_str()).unwrap_or(version);
            kv.push(
                "multiplexer",
                render::cell(format!("{} {version}", mux.name)),
            );
        }
        Err(err) => kv.push(
            "multiplexer",
            render::cell(format!("unavailable ({err})")).fg(render::palette::alarm()),
        ),
    }
    match &report.workspace {
        Ok(workspace) => {
            let class = match workspace.root_class {
                RootClass::Repo => "git repository",
                RootClass::Marker => "project marker, no git repository",
                RootClass::Directory => "plain directory: no git repository or project marker",
            };
            kv.push(
                "project",
                render::cell(format!(
                    "{} ({class})",
                    render::home_relative_path(&workspace.project_root)
                ))
                .fg(render::palette::accent()),
            );
            if let Some(trust) = workspace.trust {
                let label = match trust {
                    TrustState::NoConfig => "no project config",
                    TrustState::Trusted => "trusted",
                    TrustState::Untrusted => "untrusted",
                    TrustState::Stale => "stale",
                };
                kv.push(
                    "project trust",
                    render::cell(label).fg(render::status::trust(trust)),
                );
            }
        }
        Err(err) => kv.push(
            "project root",
            render::cell(err).fg(render::palette::alarm()),
        ),
    }
    let (config_state, config_style) = if report.config_exists {
        ("present", render::palette::good())
    } else {
        ("missing", render::palette::warn())
    };
    kv.push(
        "config",
        render::cell(format!(
            "{} ({config_state})",
            render::home_relative_path(&report.config_path)
        ))
        .fg(config_style),
    );
    let mut installed = Vec::new();
    let mut not_installed = Vec::new();
    let mut not_on_path = Vec::new();
    for agent in &report.agents {
        if agent.binary.is_none() {
            not_on_path.push(agent.name.to_owned());
            continue;
        }
        if !agent.hook_install {
            continue;
        }
        if !agent.hooks_installed {
            not_installed.push(agent.name.to_owned());
            continue;
        }
        installed.push(if agent.hook_upgrade_available {
            format!("{} (upgrade available)", agent.name)
        } else {
            agent.name.to_owned()
        });
    }
    let mut hook_lines = Vec::new();
    for (mut names, label, style) in [
        (installed, "installed", render::palette::good()),
        (not_installed, "not installed", render::palette::warn()),
        (not_on_path, "not on PATH", render::palette::alarm()),
    ] {
        if names.is_empty() {
            continue;
        }
        names.sort_unstable();
        hook_lines.push(vec![
            render::cell(format!("{} {label}: {}", names.len(), names.join(", "))).fg(style),
        ]);
    }
    if hook_lines.is_empty() {
        hook_lines.push(vec![render::cell("no agents detected")]);
    }
    kv.push_lines("hooks", hook_lines);
    kv.render(out)
}

fn print_line(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    writeln!(render::out(), "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SetupReport {
        let home = PathBuf::from(std::env::var_os("HOME").expect("test harness supplies HOME"));
        SetupReport {
            mux: Ok(DetectedMux {
                name: MuxName::Tmux,
                version: Some("3.7c".into()),
            }),
            workspace: Ok(DetectedWorkspace {
                project_root: home.join("proj"),
                root_class: RootClass::Repo,
                trust: Some(TrustState::NoConfig),
            }),
            agents: Vec::new(),
            config_path: home.join(".rimz/config.toml"),
            config_exists: false,
        }
    }

    fn agent(name: &'static str) -> DetectedAgent {
        DetectedAgent {
            name,
            binary: Some(PathBuf::from("/opt/agent")),
            hook_install: true,
            hook_install_blocked: false,
            hooks_installed: false,
            hook_upgrade_available: false,
        }
    }

    fn rendered(report: &SetupReport) -> String {
        let mut out = Vec::new();
        render_report(report, &mut out).unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    }

    #[test]
    fn setup_report_folds_hooks_and_abbreviates_paths() {
        let mut report = report();
        report.agents = vec![
            DetectedAgent {
                hooks_installed: true,
                ..agent("pi")
            },
            agent("antigravity"),
            DetectedAgent {
                binary: None,
                ..agent("kiro")
            },
            DetectedAgent {
                hooks_installed: true,
                hook_upgrade_available: true,
                ..agent("opencode")
            },
            DetectedAgent {
                hook_install: false,
                ..agent("discovery-only")
            },
        ];
        assert_eq!(
            rendered(&report),
            concat!(
                "RimZ setup\n",
                "  multiplexer:   tmux 3.7c\n",
                "  project:       ~/proj (git repository)\n",
                "  project trust: no project config\n",
                "  config:        ~/.rimz/config.toml (missing)\n",
                "  hooks:         2 installed: opencode (upgrade available), pi\n",
                "                 1 not installed: antigravity\n",
                "                 1 not on PATH: kiro\n",
            )
        );
    }

    #[test]
    fn setup_report_lists_install_blocked_agents_as_not_installed() {
        let mut report = report();
        report.agents = vec![DetectedAgent {
            hook_install_blocked: true,
            ..agent("kiro")
        }];
        assert!(rendered(&report).contains("1 not installed: kiro\n"));
    }

    #[test]
    fn setup_hook_hint_counts_only_installable_missing_hooks() {
        let blocked = DetectedAgent {
            hook_install_blocked: true,
            ..agent("kiro")
        };
        assert!(
            !blocked.lacks_hooks(),
            "blocked hooks must not produce an install hint"
        );
        let mut report = report();
        report.agents = vec![blocked, agent("claude")];
        assert_eq!(
            report
                .agents
                .iter()
                .filter(|agent| agent.lacks_hooks())
                .count(),
            1
        );
        report.agents.pop();
        assert_eq!(
            report
                .agents
                .iter()
                .filter(|agent| agent.lacks_hooks())
                .count(),
            0
        );
    }

    #[test]
    fn setup_report_does_not_repeat_mux_name_in_version() {
        for (name, version) in [(MuxName::Tmux, "3.7c"), (MuxName::Zellij, "0.44.0")] {
            for detected in [version.to_owned(), format!("{name} {version}")] {
                let mut report = report();
                report.mux = Ok(DetectedMux {
                    name,
                    version: Some(detected),
                });
                let output = rendered(&report);
                assert!(
                    output.contains(&format!("multiplexer:   {name} {version}\n")),
                    "{output}"
                );
            }
        }
    }

    #[test]
    fn setup_report_speaks_root_classes_and_trust_states() {
        for (class, label) in [
            (RootClass::Repo, "git repository"),
            (RootClass::Marker, "project marker, no git repository"),
            (
                RootClass::Directory,
                "plain directory: no git repository or project marker",
            ),
        ] {
            let mut report = report();
            report.workspace.as_mut().unwrap().root_class = class;
            assert!(rendered(&report).contains(&format!("~/proj ({label})\n")));
        }
        for (trust, label) in [
            (TrustState::NoConfig, "no project config"),
            (TrustState::Trusted, "trusted"),
            (TrustState::Untrusted, "untrusted"),
            (TrustState::Stale, "stale"),
        ] {
            let mut report = report();
            report.workspace.as_mut().unwrap().trust = Some(trust);
            assert!(rendered(&report).contains(&format!("project trust: {label}\n")));
        }
    }

    #[test]
    fn setup_report_omits_empty_hook_groups_and_absent_trust() {
        let mut report = report();
        report.workspace.as_mut().unwrap().trust = None;
        report.config_exists = true;
        report.agents = vec![DetectedAgent {
            hooks_installed: true,
            ..agent("claude")
        }];
        let output = rendered(&report);
        assert!(output.contains("1 installed: claude\n"), "{output}");
        assert!(!output.contains("not installed:"));
        assert!(!output.contains("not on PATH:"));
        assert!(!output.contains("project trust:"));
        assert!(output.contains("~/.rimz/config.toml (present)"));
        report.agents = vec![DetectedAgent {
            hook_install: false,
            ..agent("hookless")
        }];
        assert!(rendered(&report).contains("no agents detected\n"));
    }

    #[test]
    fn setup_report_keeps_probe_errors_visible() {
        let mut report = report();
        report.mux = Err("no multiplexer found".into());
        report.workspace = Err("root unavailable".into());
        let output = rendered(&report);
        assert!(
            output.contains("multiplexer:  unavailable (no multiplexer found)\n"),
            "{output}"
        );
        assert!(
            output.contains("project root: root unavailable\n"),
            "{output}"
        );
    }
}
