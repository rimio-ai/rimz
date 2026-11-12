//! Hook install consent, previews, diffs, and result presentation.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::agents::{
    HookInstallFilePreview, HookInstallPreview, HookInstallReport, StatusLineChange,
};
use unicode_width::UnicodeWidthStr;

use crate::cli::{first_run, render};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum HookInstallMode {
    Attended,
    NoTerminal,
    Reconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InstallDisposition {
    Installed,
    Refreshed,
    Current,
}

fn install_disposition(
    agent: &rimz::agents::AgentDefinition,
    login_env: &BTreeMap<String, String>,
    options: rimz::agents::HookInstallOptions,
) -> InstallDisposition {
    if !agent.hooks_installed(login_env) {
        InstallDisposition::Installed
    } else if agent
        .managed_integration()
        .is_some_and(|integration| !integration.outdated_files(login_env, options).is_empty())
    {
        InstallDisposition::Refreshed
    } else {
        InstallDisposition::Current
    }
}

pub(super) fn detected_installable_adapters(
    login_env: &BTreeMap<String, String>,
) -> Vec<&'static rimz::agents::AgentDefinition> {
    let mut detected = Vec::new();
    for agent in rimz::agents::all_definitions() {
        let definition = agent.spec();
        if rimz::agents::locate_binary(definition).is_none() {
            continue;
        }

        if !definition.has_wired_hook_install() {
            let reason = definition
                .hook_install_failure_detail()
                .unwrap_or("hook install is not supported for this adapter");
            tracing::debug!(
                agent = definition.kind,
                reason,
                "agent integrates without rimz-managed hooks; skipping hook install",
            );
            continue;
        }

        if let Some(reason) = agent
            .managed_integration()
            .and_then(|integration| integration.install_blocker(login_env))
        {
            tracing::debug!(
                agent = definition.kind,
                reason,
                "installed agent cannot take rimz-managed hooks; skipping hook install",
            );
            continue;
        }

        detected.push(agent);
    }
    detected
}

/// Ask for, or report, the hooks every detected agent lacks or has outdated.
/// `heading` replaces the first-run header (setup's `N of M` step). An agent
/// whose preview refuses is reported on its own `!` line and left out of the
/// offer. Returns whether any agent needed an install or refresh.
pub(in crate::cli) fn ensure_detected_agent_hooks(
    mode: HookInstallMode,
    heading: Option<&str>,
) -> Result<bool> {
    let login_env = rimz::agents::ambient_env();
    let options = crate::cli::hook_install_options();
    let mut actionable = Vec::new();
    let mut refused = Vec::new();
    let mut untrusted_pending = false;

    for agent in detected_installable_adapters(&login_env) {
        let definition = agent.spec();
        if !agent.hooks_installed(&login_env)
            || agent.managed_integration().is_some_and(|integration| {
                !integration.outdated_files(&login_env, options).is_empty()
            })
        {
            match agent.preview_hook_install(&login_env, options) {
                Ok(preview) => actionable.push(preview),
                Err(rimz::agents::AgentErr::Install { reason, .. }) => {
                    refused.push(format!("{} hooks not installed: {reason}", definition.kind));
                }
                Err(error) => refused.push(format!(
                    "{} hooks not installed: {:#}",
                    definition.kind,
                    anyhow::Error::from(error)
                )),
            }
            continue;
        }

        if mode != HookInstallMode::Reconnect {
            let untrusted = agent.untrusted_installed_hooks(&login_env);
            untrusted_pending |= !untrusted.is_empty();
            warn_untrusted_hooks(definition.kind, &untrusted)?;
        }
    }

    let mut out = render::err();
    if actionable.is_empty() {
        if !refused.is_empty()
            && let Some(heading) = heading
        {
            writeln!(out, "{}", render::paint(render::palette::header(), heading))?;
            writeln!(out)?;
        }
        write_pending_lines(&mut out, &refused)?;
        return Ok(!refused.is_empty());
    }

    if mode != HookInstallMode::Attended {
        write_noninteractive_notice(&mut out, &actionable, mode, heading)?;
        write_pending_lines(&mut out, &refused)?;
        return Ok(true);
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let selected = prompt_consent(&actionable, heading, &mut input, &mut out)?;
    let pending = untrusted_pending || !refused.is_empty();
    install_selected(&selected, &refused, pending, &mut out)?;
    Ok(true)
}

fn prompt_consent(
    previews: &[HookInstallPreview],
    heading: Option<&str>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<&'static str>> {
    write_intro_context(out, previews, heading)?;
    writeln!(out)?;
    write_agent_table(out, previews)?;
    writeln!(out)?;
    write_consent_footer(out)?;
    writeln!(out)?;
    loop {
        write_prompt(out)?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(Vec::new());
        }
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" | "Yes" => {
                return Ok(previews.iter().map(|preview| preview.agent).collect());
            }
            "n" | "N" | "no" | "NO" | "No" => return Ok(Vec::new()),
            _ => {
                writeln!(
                    out,
                    "  Enter installs or refreshes hooks for every listed agent; n skips."
                )?;
            }
        }
    }
}

/// Install the selected agents and report each. `refused` are the agents the
/// preview turned away, and `pending` says a step already waits (an untrusted
/// hook on a current agent): either withholds "All set".
fn install_selected(
    selected: &[&'static str],
    refused: &[String],
    mut pending: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let login_env = rimz::agents::ambient_env();
    writeln!(out)?;
    if selected.is_empty() {
        writeln!(
            out,
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        )?;
        write_pending_lines(out, refused)?;
        return Ok(());
    }

    let width = selected
        .iter()
        .map(|name| name.width())
        .max()
        .unwrap_or_default();
    for name in selected {
        let installed = rimz::agents::definition_by_kind(name)
            .map_err(anyhow::Error::from)
            .and_then(|agent| install_hooks_into(agent, &login_env, width, out));
        match installed {
            Ok(step_pending) => pending |= step_pending,
            Err(error) => {
                pending = true;
                write_pending_line(out, &format!("{name} hooks not installed: {error:#}"))?;
            }
        }
    }
    write_pending_lines(out, refused)?;

    Ok(write_post_install_footer(out, pending)?)
}

/// Install one agent's hooks into the home `login_env` resolves, then report
/// the result on one line padded to `name_width`, and any hooks the agent's
/// own trust gate still skips. Returns whether a step is still pending.
pub(in crate::cli) fn install_hooks_into(
    agent: &rimz::agents::AgentDefinition,
    login_env: &BTreeMap<String, String>,
    name_width: usize,
    out: &mut dyn Write,
) -> Result<bool> {
    let options = crate::cli::hook_install_options();
    let disposition = install_disposition(agent, login_env, options);
    let (report, untrusted) = install_hooks_quietly(agent, login_env)?;
    write_install_result(out, &report, disposition, name_width)?;
    write_untrusted_hooks_notice(report.agent, &untrusted, out)?;
    Ok(!untrusted.is_empty())
}

/// Install without printing, returning the report and hooks still needing trust.
pub(in crate::cli) fn install_hooks_quietly(
    agent: &rimz::agents::AgentDefinition,
    login_env: &BTreeMap<String, String>,
) -> Result<(HookInstallReport, Vec<String>)> {
    let report = agent.install_hooks(login_env, crate::cli::hook_install_options())?;
    Ok((report, agent.untrusted_installed_hooks(login_env)))
}

/// Stderr notice for hooks the agent's own trust gate still skips: the gate
/// silences every untrusted hook with no signal of its own, so the start
/// notice is where the dead channel becomes visible. RimZ cannot trust on
/// the user's behalf — only the agent's own UI can — so this warns with the
/// fix rather than gating the start. No-op when `untrusted` is empty.
fn warn_untrusted_hooks(kind: &str, untrusted: &[String]) -> Result<()> {
    let mut out = render::err();
    Ok(write_untrusted_hooks_notice(kind, untrusted, &mut out)?)
}

pub(super) fn write_untrusted_hooks_notice(
    kind: &str,
    untrusted: &[String],
    out: &mut dyn Write,
) -> std::io::Result<()> {
    if untrusted.is_empty() {
        return Ok(());
    }
    write_pending_line(
        out,
        &format!(
            "{kind} ignores {} until trusted",
            if untrusted.len() == 1 {
                "1 hook".to_owned()
            } else {
                format!("{} hooks", untrusted.len())
            }
        ),
    )?;
    writeln!(out, "  fix: {}", rimz::agents::hook_trust_fix(kind))?;
    Ok(())
}

/// A step the install could not finish on its own, on its own `!` line,
/// wrapped under a two-column indent within 80 columns without splitting a word.
fn write_pending_line(out: &mut dyn Write, text: &str) -> std::io::Result<()> {
    let mark = render::paint(render::palette::warn().bold(), "!");
    let mut line = String::new();
    let mut first = true;
    for word in text.split_whitespace() {
        if !line.is_empty() && 2 + line.width() + 1 + word.width() > RESULT_WIDTH {
            write_pending_row(out, first, &mark, &line)?;
            first = false;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    write_pending_row(out, first, &mark, &line)
}

fn write_pending_row(
    out: &mut dyn Write,
    first: bool,
    mark: &str,
    text: &str,
) -> std::io::Result<()> {
    if first {
        writeln!(out, "{mark} {text}")
    } else {
        writeln!(out, "  {text}")
    }
}

fn write_pending_lines(out: &mut dyn Write, lines: &[String]) -> std::io::Result<()> {
    for line in lines {
        write_pending_line(out, line)?;
    }
    Ok(())
}

fn write_intro_context(
    out: &mut dyn Write,
    previews: &[HookInstallPreview],
    heading: Option<&str>,
) -> Result<()> {
    match heading {
        Some(heading) => writeln!(out, "{}", render::paint(render::palette::header(), heading))?,
        None => first_run::write_header(out)?,
    }
    writeln!(out)?;
    let agent_word = if previews.len() == 1 {
        "agent"
    } else {
        "agents"
    };
    let pronoun = if previews.len() == 1 { "it" } else { "them" };
    let agent_names = previews
        .iter()
        .map(|preview| preview.agent)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "RimZ found {} coding {agent_word}: {}.",
        previews.len(),
        render::paint(render::palette::accent(), &agent_names)
    )?;
    writeln!(
        out,
        "To show {pronoun} live in the sidebar, RimZ installs or refreshes reporting hooks in each agent's config."
    )?;
    if let Some(file) = previews
        .iter()
        .filter(|preview| preview.agent == "codex")
        .flat_map(|preview| &preview.files)
        .find(|file| {
            file.path
                .file_name()
                .is_some_and(|name| name == "rimz.rules")
        })
    {
        writeln!(
            out,
            "{}",
            if file.existed && file.candidate.is_empty() {
                "Removing Codex's RimZ permission rules file ends its prompt and sandbox bypass for routine `rimz` commands in every Codex session, including outside RimZ (`[agents] allow-routine-rimz = false`)."
            } else {
                "Codex also gets a permission rules file: routine `rimz` commands run without a prompt or sandbox in every Codex session, including outside RimZ. Turn it off with `[agents] allow-routine-rimz = false`."
            }
        )?;
    }
    Ok(())
}

fn write_agent_table(out: &mut dyn Write, previews: &[HookInstallPreview]) -> std::io::Result<()> {
    let layout = AgentTableLayout::from_previews(previews);
    for preview in previews {
        write_agent_table_entry(out, preview, &layout)?;
    }
    Ok(())
}

fn write_agent_table_entry(
    out: &mut dyn Write,
    preview: &HookInstallPreview,
    layout: &AgentTableLayout,
) -> std::io::Result<()> {
    for (index, file) in preview.files.iter().enumerate() {
        let name = if index == 0 { preview.agent } else { "" };
        let cell = agent_file_cell(preview, file, index);
        let annotation = if file.existed && file.candidate.is_empty() {
            "removes file"
        } else if file.existed {
            "updates existing config"
        } else {
            "new file"
        };
        writeln!(
            out,
            "  {}{:name_pad$}  {}{:cell_pad$}  {}",
            render::paint(render::palette::accent().bold(), name),
            "",
            cell,
            "",
            render::paint(render::palette::muted(), annotation),
            name_pad = layout.name_width.saturating_sub(name.width()),
            cell_pad = layout.cell_width.saturating_sub(cell.width())
        )?;
    }

    if let Some(summary) = status_line_summary(preview) {
        writeln!(
            out,
            "{}{}",
            " ".repeat(layout.continuation_indent()),
            render::paint(render::palette::muted(), &format!("+ {summary}"))
        )?;
    }
    Ok(())
}

fn write_prompt(out: &mut dyn Write) -> Result<()> {
    write!(
        out,
        "Install or refresh reporting hooks? {} ",
        render::paint(render::palette::accent().bold(), "[Y/n]")
    )?;
    out.flush()?;
    Ok(())
}

pub(super) fn render_dry_run(
    out: &mut dyn Write,
    previews: &[HookInstallPreview],
) -> std::io::Result<()> {
    let layout = AgentTableLayout::from_previews(previews);
    for (idx, preview) in previews.iter().enumerate() {
        if idx > 0 {
            writeln!(out)?;
        }
        write_agent_table_entry(out, preview, &layout)?;
        for file in &preview.files {
            render::diff::preview_file_diff(
                out,
                &file.path,
                file.original.as_deref(),
                &file.candidate,
            )?;
        }
    }
    Ok(())
}

/// A headline per agent within 80 columns that names the first file when it
/// fits, then one indented line for each file the headline does not name. A
/// file that existed before the install and is gone after it reads `removed`.
pub(super) fn write_install_result(
    out: &mut dyn Write,
    report: &HookInstallReport,
    disposition: InstallDisposition,
    name_width: usize,
) -> std::io::Result<()> {
    let count = report.installed_events.len();
    let summary = match disposition {
        InstallDisposition::Installed => format!("installed {count} hooks"),
        InstallDisposition::Refreshed => format!("refreshed {count} hooks"),
        InstallDisposition::Current => format!("hooks up to date ({count})"),
    };
    let name = format!("{:<name_width$}", report.agent);
    let mut line = format!("{name}  {summary}");
    let removed = |file: &rimz::agents::HookInstallFileReport| {
        file.existed && std::fs::symlink_metadata(&file.path).is_err()
    };
    let mut unnamed = report.files.as_slice();
    if let Some((file, rest)) = report.files.split_first()
        && !removed(file)
    {
        let with_path = format!("{line} → {}", home_relative_path(&file.path));
        if 2 + with_path.width() <= RESULT_WIDTH {
            line = with_path;
            unnamed = rest;
        }
    }
    writeln!(
        out,
        "{} {line}",
        render::paint(render::palette::good().bold(), "✓")
    )?;
    let indent = 2 + name_width + 2;
    for file in unnamed {
        let label = if removed(file) { "removed" } else { "config" };
        writeln!(
            out,
            "{:indent$}{label} → {}",
            "",
            home_relative_path(&file.path)
        )?;
    }
    Ok(())
}

const RESULT_WIDTH: usize = 80;

pub(super) fn write_uninstall_result(
    out: &mut dyn Write,
    report: &rimz::agents::HookUninstallReport,
) -> std::io::Result<()> {
    if report.files.is_empty() {
        writeln!(out, "{} — no RimZ-managed hooks found", report.agent)?;
        return Ok(());
    }

    for (index, file) in report.files.iter().enumerate() {
        if index == 0 {
            writeln!(
                out,
                "{} {}  removed {} hooks → {}",
                render::paint(render::palette::good().bold(), "✓"),
                report.agent,
                report.removed_events.len(),
                home_relative_path(&file.path),
            )?;
        } else {
            writeln!(out, "       config → {}", home_relative_path(&file.path))?;
        }
    }
    Ok(())
}

fn write_noninteractive_notice(
    out: &mut dyn Write,
    previews: &[HookInstallPreview],
    mode: HookInstallMode,
    heading: Option<&str>,
) -> Result<()> {
    write_intro_context(out, previews, heading)?;
    writeln!(out)?;
    write_agent_table(out, previews)?;
    writeln!(out)?;
    write_consent_footer(out)?;
    writeln!(
        out,
        "{}",
        if mode == HookInstallMode::Reconnect {
            "Not asked on a reconnect — nothing installed or refreshed. Run `rimz hooks install` in a room pane to install. `rimz doctor` lists missing hooks."
        } else {
            "No terminal input — nothing installed or refreshed. RimZ continues into the room; install or refresh agents later with rimz hooks install."
        },
    )?;
    Ok(())
}

fn write_consent_footer(out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you."
    )?;
    Ok(write_undo_preview_hints(out)?)
}

/// The undo and preview hints, then "All set" only when no step is `pending`.
pub(super) fn write_post_install_footer(out: &mut dyn Write, pending: bool) -> std::io::Result<()> {
    writeln!(out)?;
    write_undo_preview_hints(out)?;
    if !pending {
        writeln!(
            out,
            "All set — your agents appear in the sidebar as they run."
        )?;
    }
    Ok(())
}

fn write_undo_preview_hints(out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "  {}     rimz hooks uninstall",
        render::paint(render::palette::muted(), "undo")
    )?;
    writeln!(
        out,
        "  {}  rimz hooks install --dry-run",
        render::paint(render::palette::muted(), "preview")
    )?;
    Ok(())
}

struct AgentTableLayout {
    name_width: usize,
    cell_width: usize,
}

impl AgentTableLayout {
    fn from_previews(previews: &[HookInstallPreview]) -> Self {
        Self {
            name_width: previews
                .iter()
                .map(|preview| preview.agent.width())
                .max()
                .unwrap_or_default(),
            cell_width: previews
                .iter()
                .flat_map(|preview| {
                    preview
                        .files
                        .iter()
                        .enumerate()
                        .map(|(index, file)| agent_file_cell(preview, file, index).width())
                })
                .max()
                .unwrap_or_default(),
        }
    }

    fn continuation_indent(&self) -> usize {
        2 + self.name_width + 2
    }
}

fn agent_file_cell(
    preview: &HookInstallPreview,
    file: &HookInstallFilePreview,
    index: usize,
) -> String {
    let label = if index == 0 {
        format!("{} hooks", preview.planned_events.len())
    } else {
        "config".to_owned()
    };
    format!("{label} → {}", home_relative_path(&file.path))
}

fn status_line_summary(preview: &HookInstallPreview) -> Option<&'static str> {
    if status_line_is_wrapping(&preview.status_line_change)
        || status_line_is_wrapping(&preview.subagent_status_line_change)
    {
        Some("wraps your statusline for live context — yours restored on uninstall")
    } else if status_line_is_added(&preview.status_line_change)
        || status_line_is_added(&preview.subagent_status_line_change)
    {
        Some("sets your statusline to show live context")
    } else {
        None
    }
}

fn status_line_is_wrapping(change: &Option<StatusLineChange>) -> bool {
    matches!(change, Some(StatusLineChange::Wrapping { .. }))
}

fn status_line_is_added(change: &Option<StatusLineChange>) -> bool {
    matches!(change, Some(StatusLineChange::Added))
}

fn home_relative_path(path: &std::path::Path) -> String {
    render::home_relative(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use super::*;

    fn preview(agent: &'static str, original: Option<&str>, candidate: &str) -> HookInstallPreview {
        let path = home_config_path(agent);
        HookInstallPreview {
            agent,
            files: vec![HookInstallFilePreview {
                path,
                original: original.map(str::to_owned),
                candidate: candidate.to_owned(),
                existed: original.is_some(),
            }],
            planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            status_line_change: None,
            subagent_status_line_change: None,
        }
    }

    fn home_config_path(agent: &str) -> PathBuf {
        let home = std::env::var("HOME").expect("HOME is set for CLI render tests");
        PathBuf::from(home).join(format!(".{agent}/settings.json"))
    }

    fn strip<E: std::fmt::Debug>(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::result::Result<(), E>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render");
        String::from_utf8(stream.into_inner()).expect("utf8")
    }

    fn drive(previews: &[HookInstallPreview], input: &[u8]) -> (Vec<&'static str>, String) {
        let mut input = Cursor::new(input.to_vec());
        let mut stream = anstream::StripStream::new(Vec::new());
        let selected = prompt_consent(previews, None, &mut input, &mut stream).expect("prompt");
        let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
        (selected, rendered)
    }

    #[test]
    fn prompt_accepts_or_declines_all_agents() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];

        let (selected, _) = drive(&previews, b"y\n");
        assert_eq!(selected, vec!["claude", "codex"]);

        let (selected, _) = drive(&previews, b"\n");
        assert_eq!(selected, vec!["claude", "codex"]);

        let (selected, _) = drive(&previews, b"n\n");
        assert!(selected.is_empty());

        let (selected, rendered) = drive(&previews, b"maybe\nn\n");
        assert!(selected.is_empty());
        assert!(
            rendered.contains("Enter installs or refreshes hooks for every listed agent; n skips.")
        );

        let (selected, rendered) = drive(&previews, b"");
        assert!(selected.is_empty());
        assert!(rendered.starts_with("rimz · first-run setup\n"));

        let mut stream = anstream::StripStream::new(Vec::new());
        let heading = Some("1 of 6 · Reporting hooks");
        prompt_consent(
            &previews,
            heading,
            &mut Cursor::new(b"n\n".to_vec()),
            &mut stream,
        )
        .expect("prompt");
        let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
        assert!(rendered.starts_with("1 of 6 · Reporting hooks\n\nRimZ found 2 coding agents"));
        assert!(!rendered.contains("first-run setup"));

        let rendered = strip(|out| install_selected(&selected, &[], false, out));
        assert!(rendered.contains(
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        ));
    }

    #[test]
    fn codex_rules_consent_and_dry_run_show_creation_and_removal() {
        for removed in [false, true] {
            let mut codex = preview("codex", Some("hooks\n"), "hooks\n");
            let path = home_config_path("codex")
                .with_file_name("rules")
                .join("rimz.rules");
            codex.files.push(HookInstallFilePreview {
                path,
                original: removed.then(|| "# _rimz_managed\nrule\n".into()),
                candidate: if removed {
                    ""
                } else {
                    "# _rimz_managed\nrule\n"
                }
                .into(),
                existed: removed,
            });
            let previews = [codex];
            let (_, consent) = drive(&previews, b"n\n");
            let notice = strip(|out| {
                write_noninteractive_notice(out, &previews, HookInstallMode::NoTerminal, None)
            });
            for rendered in [&consent, &notice] {
                assert!(rendered.contains(if removed {
                    "Removing Codex's RimZ permission rules file ends its prompt and sandbox bypass for routine `rimz` commands"
                } else {
                    "Codex also gets a permission rules file: routine `rimz` commands run without a prompt or sandbox"
                }));
                assert!(rendered.contains("in every Codex session, including outside RimZ"));
                assert!(rendered.contains("[agents] allow-routine-rimz = false"));
            }
            let dry_run = strip(|out| render_dry_run(out, &previews));
            for rendered in [&consent, &dry_run] {
                assert!(rendered.contains("rules/rimz.rules"));
                assert!(rendered.contains(if removed { "removes file" } else { "new file" }));
            }
            assert!(dry_run.contains(if removed {
                "-# _rimz_managed"
            } else {
                "+# _rimz_managed"
            }));
            assert!(dry_run.contains(if removed { "-rule" } else { "@@ new file @@" }));
        }
    }

    #[test]
    fn hook_notices_cover_interactive_and_noninteractive_workflows() {
        let mut additive = preview("claude", Some("{}\n"), "{\"hooks\": []}\n");
        additive.status_line_change = Some(StatusLineChange::Wrapping {
            original: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed".to_owned(),
        });
        let created = preview("codex", None, "{\"hooks\": []}\n");
        let previews = [additive, created];

        let (_, interactive) = drive(&previews, b"n\n");
        let noninteractive = strip(|out| {
            write_noninteractive_notice(out, &previews, HookInstallMode::NoTerminal, None)
        });
        let reconnect = strip(|out| {
            write_noninteractive_notice(out, &previews, HookInstallMode::Reconnect, None)
        });

        for rendered in [&interactive, &noninteractive, &reconnect] {
            assert!(rendered.contains("RimZ found 2 coding agents: claude, codex."));
            assert!(rendered.contains("~/.claude/settings.json"));
            assert!(rendered.contains("~/.codex/settings.json"));
            assert!(rendered.contains(
                "Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you."
            ));
            assert!(rendered.contains("rimz hooks uninstall"));
            assert!(rendered.contains("rimz hooks install --dry-run"));
            assert!(rendered.contains(
                "+ wraps your statusline for live context — yours restored on uninstall"
            ));
        }
        assert!(interactive.contains("Install or refresh reporting hooks? [Y/n]"));
        assert!(!interactive.contains("No terminal input"));
        assert!(!noninteractive.contains("Install or refresh reporting hooks? [Y/n]"));
        assert!(noninteractive.contains(
            "No terminal input — nothing installed or refreshed. RimZ continues into the room; install or refresh agents later with rimz hooks install."
        ));
        assert!(!reconnect.contains("Install or refresh reporting hooks? [Y/n]"));
        assert!(!reconnect.contains("No terminal input"));
        assert!(reconnect.contains("Not asked on a reconnect — nothing installed or refreshed. Run `rimz hooks install` in a room pane to install."));
        assert!(reconnect.contains("rimz doctor"));
    }

    #[test]
    fn mixed_install_and_upgrade_share_one_summary_and_consent() {
        let created = preview("claude", None, "new-hook\n");
        let upgraded = preview("pi", Some("stale-extension\n"), "current-extension\n");
        let mut antigravity = preview("antigravity", None, "new-hooks\n");
        antigravity.files.push(HookInstallFilePreview {
            path: home_config_path("antigravity-statusline"),
            original: Some("old-statusline\n".to_owned()),
            candidate: "new-statusline\n".to_owned(),
            existed: true,
        });
        let previews = [created, upgraded, antigravity];

        let (selected, summary) = drive(&previews, b"y\n");
        assert_eq!(selected, vec!["claude", "pi", "antigravity"]);
        assert_eq!(
            summary
                .matches("Install or refresh reporting hooks? [Y/n]")
                .count(),
            1
        );
        for path in [
            "~/.claude/settings.json",
            "~/.pi/settings.json",
            "~/.antigravity/settings.json",
            "~/.antigravity-statusline/settings.json",
        ] {
            assert!(summary.contains(path), "missing {path}:\n{summary}");
        }
        assert!(summary.contains("new file"));
        assert!(summary.contains("updates existing config"));
        assert!(!summary.contains("@@"), "consent stays concise:\n{summary}");

        let (selected, _) = drive(&previews, b"n\n");
        assert!(selected.is_empty());

        let dry_run = strip(|out| render_dry_run(out, &previews));
        for content in [
            "+new-hook",
            "-stale-extension",
            "+current-extension",
            "+new-hooks",
            "-old-statusline",
            "+new-statusline",
        ] {
            assert!(dry_run.contains(content), "missing {content}:\n{dry_run}");
        }
    }

    #[test]
    fn dry_run_renders_existing_and_new_unified_diffs_without_prompt() {
        let mut claude = preview(
            "claude",
            Some("alpha\nkeep\nold\nomega\n"),
            "alpha\nkeep\nnew\nomega\n",
        );
        claude.files.push(HookInstallFilePreview {
            path: home_config_path("claude-statusline"),
            original: Some("old-status\n".to_owned()),
            candidate: "new-status\n".to_owned(),
            existed: true,
        });
        let previews = [claude, preview("codex", None, "one\ntwo\n")];

        let rendered = strip(|w| render_dry_run(w, &previews));

        assert!(rendered.contains("/dev/null"));
        assert!(rendered.contains("~/.claude/settings.json"));
        assert!(rendered.contains("~/.codex/settings.json"));
        assert!(rendered.contains("@@"));
        assert!(rendered.contains("@@ new file @@"));
        assert!(rendered.contains("-old"));
        assert!(rendered.contains("+new"));
        assert!(rendered.contains(".claude-statusline/settings.json"));
        assert!(rendered.contains("-old-status"));
        assert!(rendered.contains("+new-status"));
        assert!(rendered.contains("+one"));
        assert!(!rendered.contains("Install or refresh reporting hooks?"));
    }

    #[test]
    fn dry_run_names_every_cursor_config_file() {
        let mut cursor = preview("cursor", Some("{}\n"), "{\"hooks\": {}}\n");
        cursor.files[0].path = cursor.files[0].path.with_file_name("hooks.json");
        let cli_config = home_config_path("cursor").with_file_name("cli-config.json");
        cursor.files.push(HookInstallFilePreview {
            path: cli_config.clone(),
            original: None,
            candidate: "{\"statusLine\": {}}\n".to_owned(),
            existed: false,
        });
        let rendered = strip(|out| render_dry_run(out, &[cursor]));
        assert!(rendered.contains("~/.cursor/hooks.json"));
        assert!(rendered.contains("~/.cursor/cli-config.json"));
        assert_eq!(rendered.matches("@@").count(), 4);
    }

    #[test]
    fn completed_hook_reports_render_dispositions_files_and_footer() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks.json");
        let cli_config = dir.path().join("cli-config.json");
        std::fs::write(&hooks, "{}").unwrap();
        std::fs::write(&cli_config, "{}").unwrap();
        let report = HookInstallReport {
            agent: "cursor",
            files: vec![
                rimz::agents::HookInstallFileReport {
                    path: hooks.clone(),
                    existed: true,
                },
                rimz::agents::HookInstallFileReport {
                    path: cli_config.clone(),
                    existed: false,
                },
                rimz::agents::HookInstallFileReport {
                    path: dir.path().join("rimz.rules"),
                    existed: true,
                },
            ],
            installed_events: vec!["stop".to_owned()],
        };

        for (disposition, summary) in [
            (InstallDisposition::Installed, "installed 1 hooks"),
            (InstallDisposition::Refreshed, "refreshed 1 hooks"),
            (InstallDisposition::Current, "hooks up to date (1)"),
        ] {
            let rendered = strip(|out| write_install_result(out, &report, disposition, 8));
            let mut lines = rendered.lines();
            let headline = lines.next().unwrap();
            assert!(headline.starts_with(&format!("✓ cursor    {summary}")));
            assert!(
                headline.width() <= RESULT_WIDTH,
                "headline passes 80 columns:\n{rendered}"
            );
            let path = format!(" → {}", hooks.display());
            let named = format!("✓ cursor    {summary}{path}").width() <= RESULT_WIDTH;
            assert_eq!(
                headline,
                if named {
                    format!("✓ cursor    {summary}{path}")
                } else {
                    format!("✓ cursor    {summary}")
                },
                "the headline names the first file exactly when it fits:\n{rendered}"
            );
            let unnamed = if named {
                &report.files[1..]
            } else {
                &report.files[..]
            };
            let rest: Vec<_> = lines.collect();
            let expected: Vec<_> = unnamed
                .iter()
                .map(|file| {
                    let label = if file.path.exists() {
                        "config"
                    } else {
                        "removed"
                    };
                    format!("            {label} → {}", file.path.display())
                })
                .collect();
            assert_eq!(
                rest, expected,
                "every unnamed file gets its own line:\n{rendered}"
            );
            assert!(!rendered.contains(" +"), "no file is counted:\n{rendered}");
        }
        let long = HookInstallReport {
            agent: "cursor",
            files: vec![rimz::agents::HookInstallFileReport {
                path: PathBuf::from(format!("/{}/hooks.json", "d".repeat(80))),
                // A missing path that existed reads as removed, so this
                // stand-in for an on-disk file says it was not there before.
                existed: false,
            }],
            installed_events: vec!["stop".to_owned()],
        };
        let rendered =
            strip(|out| write_install_result(out, &long, InstallDisposition::Current, 6));
        assert_eq!(
            rendered,
            format!(
                "✓ cursor  hooks up to date (1)\n          config → {}\n",
                long.files[0].path.display()
            )
        );
        let two = HookInstallReport {
            agent: "codex",
            files: vec![
                rimz::agents::HookInstallFileReport {
                    path: PathBuf::from("/x/config.toml"),
                    existed: false,
                },
                rimz::agents::HookInstallFileReport {
                    path: PathBuf::from("/x/rules/rimz.rules"),
                    existed: false,
                },
            ],
            installed_events: vec!["stop".to_owned()],
        };
        let rendered =
            strip(|out| write_install_result(out, &two, InstallDisposition::Installed, 6));
        assert_eq!(
            rendered,
            "✓ codex   installed 1 hooks → /x/config.toml\n          config → /x/rules/rimz.rules\n"
        );
        let gone = HookInstallReport {
            agent: "codex",
            files: vec![
                rimz::agents::HookInstallFileReport {
                    path: PathBuf::from("/x/config.toml"),
                    existed: false,
                },
                rimz::agents::HookInstallFileReport {
                    path: PathBuf::from("/x/rules/rimz.rules"),
                    existed: true,
                },
            ],
            installed_events: vec!["stop".to_owned()],
        };
        let rendered =
            strip(|out| write_install_result(out, &gone, InstallDisposition::Refreshed, 6));
        assert_eq!(
            rendered,
            "✓ codex   refreshed 1 hooks → /x/config.toml\n          removed → /x/rules/rimz.rules\n"
        );
        let short = HookInstallReport {
            agent: "pi",
            files: vec![rimz::agents::HookInstallFileReport {
                path: PathBuf::from("/x/hooks.json"),
                existed: false,
            }],
            installed_events: vec!["stop".to_owned()],
        };
        let rendered =
            strip(|out| write_install_result(out, &short, InstallDisposition::Installed, 2));
        assert_eq!(rendered, "✓ pi  installed 1 hooks → /x/hooks.json\n");

        let uninstall = rimz::agents::HookUninstallReport {
            agent: "cursor",
            files: report.files,
            removed_events: vec!["stop".to_owned()],
        };
        let rendered = strip(|out| write_uninstall_result(out, &uninstall));
        assert!(rendered.contains(&format!("✓ cursor  removed 1 hooks → {}", hooks.display())));
        assert!(rendered.contains(&format!("config → {}", cli_config.display())));

        let empty = rimz::agents::HookUninstallReport {
            agent: "codex",
            files: Vec::new(),
            removed_events: Vec::new(),
        };
        let rendered = strip(|out| write_uninstall_result(out, &empty));
        assert_eq!(rendered, "codex — no RimZ-managed hooks found\n");

        for pending in [false, true] {
            let rendered = strip(|out| write_post_install_footer(out, pending));
            assert!(rendered.contains("rimz hooks uninstall"));
            assert!(rendered.contains("rimz hooks install --dry-run"));
            assert_eq!(
                rendered.contains("All set — your agents appear in the sidebar as they run."),
                !pending,
                "All set prints only when nothing is pending:\n{rendered}"
            );
        }

        let refusal = format!(
            "pi hooks not installed: refusing to overwrite an unmarked user extension at {}; move it aside or remove it",
            "/home/me/.pi/agent/extensions/rimz.ts"
        );
        let rendered = strip(|out| write_pending_line(out, &refusal));
        let lines: Vec<_> = rendered.lines().collect();
        assert!(lines.len() > 1, "{rendered}");
        assert!(
            lines[0].starts_with("! pi hooks not installed: "),
            "{rendered}"
        );
        for line in &lines {
            assert!(line.width() <= RESULT_WIDTH, "{line:?} passes 80 columns");
        }
        for line in &lines[1..] {
            assert!(
                line.starts_with("  ") && !line.starts_with("   "),
                "{line:?}"
            );
        }
        assert!(rendered.contains(" /home/me/.pi/agent/extensions/rimz.ts;"));

        let rendered = strip(|out| write_untrusted_hooks_notice("codex", &["stop".into()], out));
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("! codex ignores 1 hook until trusted"));
        assert!(lines.next().is_some_and(|line| line.starts_with("  fix: ")));
        assert_eq!(lines.next(), None);
        assert_eq!(
            strip(|out| write_untrusted_hooks_notice("codex", &[], out)),
            ""
        );
    }
}
