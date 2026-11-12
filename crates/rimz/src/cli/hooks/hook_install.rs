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

/// The `!` lines detection collects for steps the install cannot finish on its
/// own: agents whose preview refused, and current agents whose hooks still wait
/// for the agent's own trust. They print after the heading, never above it.
#[derive(Default)]
struct PendingNotes {
    refused: Vec<String>,
    untrusted: Vec<(&'static str, Vec<String>)>,
}

impl PendingNotes {
    fn is_empty(&self) -> bool {
        self.refused.is_empty() && self.untrusted.is_empty()
    }

    fn write(&self, out: &mut dyn Write) -> std::io::Result<()> {
        for (kind, hooks) in &self.untrusted {
            write_untrusted_hooks_notice(kind, hooks, out)?;
        }
        write_pending_lines(out, &self.refused)
    }
}

/// Ask for, or report, the hooks every detected agent lacks or has outdated.
/// `heading` replaces the first-run header (setup's `N of M` step). An agent
/// whose preview refuses is reported on its own `!` line and left out of the
/// offer; a current agent's untrusted hooks are reported the same way. Returns
/// whether any agent needed an install or refresh.
pub(in crate::cli) fn ensure_detected_agent_hooks(
    mode: HookInstallMode,
    heading: Option<&str>,
) -> Result<bool> {
    let login_env = rimz::agents::ambient_env();
    let options = crate::cli::hook_install_options();
    let mut actionable = Vec::new();
    let mut notes = PendingNotes::default();

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
                    notes
                        .refused
                        .push(format!("{} hooks not installed: {reason}", definition.kind));
                }
                Err(error) => notes.refused.push(format!(
                    "{} hooks not installed: {:#}",
                    definition.kind,
                    anyhow::Error::from(error)
                )),
            }
            continue;
        }

        if mode != HookInstallMode::Reconnect {
            let untrusted = agent.untrusted_installed_hooks(&login_env);
            if !untrusted.is_empty() {
                notes.untrusted.push((definition.kind, untrusted));
            }
        }
    }

    let mut out = render::err();
    if actionable.is_empty() {
        // Setup counts the hooks step only when this returns true, so the
        // heading goes out only beside a refusal, never for trust notices alone.
        if !notes.refused.is_empty()
            && let Some(heading) = heading
        {
            writeln!(out, "{}", render::paint(render::palette::header(), heading))?;
            writeln!(out)?;
        }
        notes.write(&mut out)?;
        return Ok(!notes.refused.is_empty());
    }

    if mode != HookInstallMode::Attended {
        write_noninteractive_notice(&mut out, &actionable, mode, heading)?;
        notes.write(&mut out)?;
        return Ok(true);
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let selected = prompt_consent(&actionable, heading, &mut input, &mut out)?;
    install_selected(&selected, &notes, &mut out)?;
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

/// Install the selected agents and report each, then the `notes` detection
/// collected (agents the preview turned away, current agents with untrusted
/// hooks): any note withholds "All set".
fn install_selected(
    selected: &[&'static str],
    notes: &PendingNotes,
    out: &mut dyn Write,
) -> Result<()> {
    let login_env = rimz::agents::ambient_env();
    let mut pending = !notes.is_empty();
    writeln!(out)?;
    if selected.is_empty() {
        writeln!(
            out,
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        )?;
        notes.write(out)?;
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
    notes.write(out)?;

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

/// Notice for hooks the agent's own trust gate still skips: the gate silences
/// every untrusted hook with no signal of its own, so the start notice is
/// where the dead channel becomes visible. RimZ cannot trust on the user's
/// behalf — only the agent's own UI can — so this warns with the fix rather
/// than gating the start. No-op when `untrusted` is empty.
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
mod tests;
