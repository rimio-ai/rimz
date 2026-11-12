//! `rimz setup` — the machine report, the default config bootstrap, and the
//! setup questions with the review screen that writes their answers.

use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use rimz::config::{ConfigEditor, MergeAction, MergeReport};
use rimz::ids::MuxName;

use super::first_run::{self, MuxSetup, Questions, ReviewExit};
use super::{GlobalFlags, hooks};
use crate::cli::render;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Write or merge the config files and stop: no questions, no hooks, no trust.
    #[arg(long, alias = "yes")]
    config_only: bool,
    /// Overwrite config.toml, theme.toml and loop.toml with the shipped templates
    /// after a confirmation, then ask every question as on a fresh machine.
    #[arg(long, conflicts_with = "config_only")]
    reset: bool,
}

pub fn run(args: SetupArgs, globals: &GlobalFlags) -> Result<()> {
    let interactive = std::io::stdin().is_terminal();
    if args.reset && !interactive {
        anyhow::bail!(
            "rimz setup --reset asks before it overwrites your config, and there is no terminal to ask from; nothing changed"
        );
    }
    let report = SetupReport::detect(globals);

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
        render_merge_report(&editor.merge_defaults()?, true)?;
        report_remote_template()?;
        report_consensus_copy()?;
        print_line("Installed no hooks and granted no trust.")?;
        print_line("Run `rimz start` when ready.")?;
        return Ok(());
    }

    print_report(&report)?;
    let mut configured = report.config_exists;
    let mut agents = report.agents;
    if args.reset {
        if !confirm_reset()? {
            print_line("Nothing changed.")?;
            return Ok(());
        }
        write_fresh_config()?;
        configured = false;
        // The wipe can change what the hooks need (`allow-routine-rimz`).
        agents = detect_agents();
    }
    let editor = ConfigEditor::machine();
    retire_idle_compact_keys(&editor)?;
    if render_merge_report(&editor.merge_defaults()?, false)? {
        print_line("Fix the unparseable file(s), then rerun `rimz setup`.")?;
        return Ok(());
    }
    report_remote_template()?;
    report_consensus_copy()?;

    let config = rimz::config::MachineConfig::load().context("loading per-machine config")?;
    let detected = report
        .mux
        .as_ref()
        .map_or(MuxName::Tmux, |detected| detected.name);
    let mut questions = Questions::new(
        &config,
        MuxSetup {
            detected,
            enclosing: rimz::mux::enclosing_mux(),
            ask: first_run::setup_asks_mux(globals.mux, rimz::mux::is_installed),
        },
    );
    if !configured && !walk_all(&mut questions, &agents, true)? {
        return print_quit();
    }
    if configured {
        writeln!(
            render::err(),
            "This machine is set up. Enter keeps every setting; a number changes one."
        )?;
    }
    loop {
        let row = hooks_row(&detect_agents());
        let exit = questions.review(
            row.as_deref(),
            &mut std::io::stdin().lock(),
            &mut render::err(),
        )?;
        match exit {
            ReviewExit::Write => break,
            ReviewExit::Quit => return print_quit(),
            ReviewExit::Hooks => {
                if !ask_hooks(Some("Reporting hooks"))? {
                    writeln!(render::err(), "  Every detected agent's hooks are current.")?;
                }
            }
            ReviewExit::WalkAll => {
                if !walk_all(&mut questions, &detect_agents(), false)? {
                    return print_quit();
                }
            }
        }
    }
    let mut err = render::err();
    writeln!(err)?;
    questions.apply(&mut err)?;
    writeln!(err, "Next:  cd into a project and run `rimz`")?;
    Ok(())
}

/// Ask hooks when some agent needs them, then every setting question, numbered
/// as one sequence. `opening` prints the count line a fresh machine opens on.
fn walk_all(questions: &mut Questions, agents: &[DetectedAgent], opening: bool) -> Result<bool> {
    let hooks_pending = agents.iter().any(DetectedAgent::needs_hooks);
    let total = usize::from(hooks_pending) + questions.count();
    if opening {
        let mut err = render::err();
        writeln!(err)?;
        writeln!(
            err,
            "{total} questions. Enter keeps the answer in [brackets]."
        )?;
        writeln!(
            err,
            "Hooks install when you say yes; your settings are written at the review."
        )?;
    }
    let mut first = 1;
    if hooks_pending {
        ask_hooks(Some(&format!("1 of {total} · Reporting hooks")))?;
        first = 2;
    }
    questions.walk(
        first,
        total,
        &mut std::io::stdin().lock(),
        &mut render::err(),
    )
}

fn ask_hooks(heading: Option<&str>) -> Result<bool> {
    writeln!(render::err())?;
    hooks::ensure_detected_agent_hooks(hooks::HookInstallMode::Attended, heading)
}

fn print_quit() -> Result<()> {
    writeln!(
        render::err(),
        "Quit: your settings are unchanged; hooks you installed stay."
    )?;
    Ok(())
}

/// The review's hooks row: the agents that need an install or refresh, else the
/// agents whose hooks are installed. `None` when no detected agent takes hooks.
fn hooks_row(agents: &[DetectedAgent]) -> Option<String> {
    let mut hookable: Vec<&DetectedAgent> = agents
        .iter()
        .filter(|agent| agent.binary.is_some() && agent.hook_install)
        .collect();
    hookable.sort_unstable_by_key(|agent| agent.name);
    let names = |agents: &[&DetectedAgent]| {
        agents
            .iter()
            .map(|agent| agent.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let needing: Vec<&DetectedAgent> = hookable
        .iter()
        .copied()
        .filter(|agent| agent.needs_hooks())
        .collect();
    if !needing.is_empty() {
        let verb = if needing.len() == 1 { "needs" } else { "need" };
        return Some(format!("{} {verb} an install or refresh", names(&needing)));
    }
    let installed: Vec<&DetectedAgent> = hookable
        .into_iter()
        .filter(|agent| agent.hooks_installed)
        .collect();
    if installed.is_empty() {
        return None;
    }
    let mut row = format!("{} installed", names(&installed));
    for agent in installed.iter().filter(|agent| agent.hooks_untrusted) {
        row.push_str(&format!(" · {}: trust pending", agent.name));
    }
    Some(row)
}

fn confirm_reset() -> Result<bool> {
    let editor = ConfigEditor::machine();
    let names = editor
        .files()
        .ordered()
        .iter()
        .filter_map(|file| file.path().file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let dir = rimz::config::MachineConfig::config_path();
    let dir = dir
        .parent()
        .map(render::home_relative_path)
        .unwrap_or_default();
    let mut err = render::err();
    writeln!(err, "--reset overwrites {} in {dir}/", names.join(", "))?;
    writeln!(err, "with the shipped templates and keeps no backup.")?;
    super::confirm_with_default(
        "Reset them? Your settings and loop tasks there are lost.",
        false,
    )
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
    agents: Vec<DetectedAgent>,
    config_path: PathBuf,
    config_exists: bool,
}

struct DetectedMux {
    name: MuxName,
    version: Option<String>,
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
    /// Installed hooks the agent ignores until the user trusts them in it.
    hooks_untrusted: bool,
}

impl DetectedAgent {
    /// Whether a bare `rimz hooks install` would install this agent's hooks.
    fn lacks_hooks(&self) -> bool {
        self.binary.is_some()
            && self.hook_install
            && !self.hook_install_blocked
            && !self.hooks_installed
    }

    /// Whether the hooks question would offer this agent an install or refresh.
    fn needs_hooks(&self) -> bool {
        self.lacks_hooks()
            || (self.binary.is_some()
                && self.hook_install
                && !self.hook_install_blocked
                && self.hook_upgrade_available)
    }
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
        let config_path = rimz::config::MachineConfig::config_path();
        let config_exists = config_path.exists();
        Self {
            mux,
            agents: detect_agents(),
            config_path,
            config_exists,
        }
    }
}

fn detect_agents() -> Vec<DetectedAgent> {
    let login_env = rimz::agents::ambient_env();
    let hook_options = crate::cli::hook_install_options();
    rimz::agents::all_definitions()
        .map(|agent| {
            let definition = agent.spec();
            DetectedAgent {
                name: definition.kind,
                binary: rimz::agents::locate_binary(definition),
                hook_install: definition.has_wired_hook_install(),
                hook_install_blocked: agent
                    .managed_integration()
                    .is_some_and(|integration| integration.install_blocker(&login_env).is_some()),
                hooks_installed: agent.hooks_installed(&login_env),
                hook_upgrade_available: agent.managed_integration().is_some_and(|integration| {
                    !integration
                        .outdated_files(&login_env, hook_options)
                        .is_empty()
                }),
                hooks_untrusted: !agent.untrusted_installed_hooks(&login_env).is_empty(),
            }
        })
        .collect()
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

/// Print what the merge did. Without `receipts` a merged file prints nothing,
/// so only a created file, a skipped key, or an unparseable file shows. Returns
/// whether a file was left unparseable.
fn render_merge_report(report: &MergeReport, receipts: bool) -> Result<bool> {
    let mut left_unparseable = false;
    for file in &report.files {
        match file.action {
            MergeAction::Wrote => {
                print_line(&format!("Wrote {}", file.path.display()))?;
            }
            MergeAction::Merged { kept } if receipts => {
                print_line(&format!(
                    "Merged {} - kept {kept} setting(s)",
                    file.path.display()
                ))?;
            }
            MergeAction::Merged { .. } => {}
            MergeAction::LeftUnparseable { ref diagnosis } => {
                left_unparseable = true;
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
    Ok(left_unparseable)
}

fn print_report(report: &SetupReport) -> std::io::Result<()> {
    render_report(report, &mut render::out())
}

/// The machine report every setup mode opens on: the multiplexer, the agents
/// found with a count of the rest, and the config directory.
fn render_report(report: &SetupReport, out: &mut impl std::io::Write) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "rimz setup · this machine")
    )?;
    writeln!(out)?;
    let row = |label: &str, value: String| format!("  {label:<12} {value}");
    let mux = match &report.mux {
        Ok(mux) => {
            let version = mux.version.as_deref().unwrap_or("version unknown");
            let prefix = format!("{} ", mux.name);
            let version = version.strip_prefix(prefix.as_str()).unwrap_or(version);
            format!("{} {version}", mux.name)
        }
        Err(err) => render::paint(render::palette::alarm(), &format!("unavailable ({err})")),
    };
    writeln!(out, "{}", row("multiplexer", mux))?;
    let mut found: Vec<&str> = report
        .agents
        .iter()
        .filter(|agent| agent.binary.is_some())
        .map(|agent| agent.name)
        .collect();
    found.sort_unstable();
    let rest = report.agents.len() - found.len();
    let found = if found.is_empty() {
        "none found".to_owned()
    } else {
        format!("{} found", found.join(", "))
    };
    let agents = if rest == 0 {
        found
    } else {
        format!("{found} · {rest} more supported: rimz doctor")
    };
    let indent = " ".repeat(row("", String::new()).len());
    for (index, part) in render::wrap_words(&agents, 80 - indent.len())
        .into_iter()
        .enumerate()
    {
        match index {
            0 => writeln!(out, "{}", row("agents", part))?,
            _ => writeln!(out, "{indent}{part}")?,
        }
    }
    let dir = report
        .config_path
        .parent()
        .map(render::home_relative_path)
        .unwrap_or_default();
    let state = if report.config_exists {
        "present"
    } else {
        "new"
    };
    writeln!(out, "{}", row("config", format!("{dir}/ ({state})")))
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
            hooks_untrusted: false,
        }
    }

    fn rendered(report: &SetupReport) -> String {
        let mut out = Vec::new();
        render_report(report, &mut out).unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    }

    #[test]
    fn setup_report_names_found_agents_and_counts_the_rest() {
        let mut report = report();
        report.agents = vec![
            agent("pi"),
            agent("claude"),
            DetectedAgent {
                binary: None,
                ..agent("kiro")
            },
            DetectedAgent {
                binary: None,
                ..agent("amp")
            },
        ];
        assert_eq!(
            rendered(&report),
            concat!(
                "rimz setup · this machine\n",
                "\n",
                "  multiplexer  tmux 3.7c\n",
                "  agents       claude, pi found · 2 more supported: rimz doctor\n",
                "  config       ~/.rimz/ (new)\n",
            )
        );
        report.config_exists = true;
        report.agents.truncate(2);
        let output = rendered(&report);
        assert!(
            output.contains("  agents       claude, pi found\n"),
            "{output}"
        );
        assert!(output.contains("~/.rimz/ (present)"), "{output}");
        for gone in ["project", "trust", "hooks", "not on PATH"] {
            assert!(!output.contains(gone), "{gone}: {output}");
        }

        report.agents = ["antigravity", "claude", "codex", "cursor", "opencode", "pi"]
            .map(agent)
            .into_iter()
            .chain(["amp", "kiro"].map(|name| DetectedAgent {
                binary: None,
                ..agent(name)
            }))
            .collect();
        let output = rendered(&report);
        for line in output.lines() {
            assert!(line.chars().count() <= 80, "{line:?} passes 80 columns");
        }
        assert!(
            output.contains(
                "  agents       antigravity, claude, codex, cursor, opencode, pi found · 2 more\n               supported: rimz doctor\n"
            ),
            "{output}"
        );
    }

    #[test]
    fn setup_report_keeps_probe_errors_visible_and_versions_unrepeated() {
        let mut report = report();
        report.mux = Err("no multiplexer found".into());
        assert!(rendered(&report).contains("  multiplexer  unavailable (no multiplexer found)\n"));
        assert!(rendered(&report).contains("  agents       none found\n"));
        for (name, version) in [(MuxName::Tmux, "3.7c"), (MuxName::Zellij, "0.44.0")] {
            for detected in [version.to_owned(), format!("{name} {version}")] {
                report.mux = Ok(DetectedMux {
                    name,
                    version: Some(detected),
                });
                let output = rendered(&report);
                assert!(
                    output.contains(&format!("multiplexer  {name} {version}\n")),
                    "{output}"
                );
            }
        }
    }

    #[test]
    fn hook_counts_and_rows_cover_only_installable_agents() {
        let blocked = DetectedAgent {
            hook_install_blocked: true,
            ..agent("kiro")
        };
        assert!(!blocked.lacks_hooks() && !blocked.needs_hooks());
        let stale = DetectedAgent {
            hooks_installed: true,
            hook_upgrade_available: true,
            ..agent("pi")
        };
        assert!(!stale.lacks_hooks() && stale.needs_hooks());
        let current = DetectedAgent {
            hooks_installed: true,
            ..agent("claude")
        };
        assert_eq!(
            hooks_row(&[agent("codex")]).as_deref(),
            Some("codex needs an install or refresh")
        );
        assert_eq!(
            hooks_row(&[blocked, stale, agent("codex")]).as_deref(),
            Some("codex, pi need an install or refresh")
        );
        assert_eq!(
            hooks_row(&[current, agent("absent")].map(|agent| DetectedAgent {
                binary: (agent.name == "claude").then(|| PathBuf::from("/opt/claude")),
                ..agent
            }))
            .as_deref(),
            Some("claude installed")
        );
        let untrusted = DetectedAgent {
            hooks_installed: true,
            hooks_untrusted: true,
            ..agent("codex")
        };
        assert_eq!(
            hooks_row(&[untrusted]).as_deref(),
            Some("codex installed · codex: trust pending")
        );
        assert_eq!(hooks_row(&[]), None);
    }
}
