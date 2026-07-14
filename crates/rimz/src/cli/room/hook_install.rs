//! Start-time agent hook auto-install prompt.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::agents::{
    HookInstallFilePreview, HookInstallPreview, HookInstallReport, StatusLineChange,
};
use similar::TextDiff;
use unicode_width::UnicodeWidthStr;

use crate::cli::{first_run, render};

const DIFF_CONTEXT_LINES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallDisposition {
    Installed,
    Refreshed,
    Current,
}

pub(crate) fn install_disposition(agent: &dyn rimz::agents::AgentAdapter) -> InstallDisposition {
    if !agent.hooks_installed() {
        InstallDisposition::Installed
    } else if agent.hook_upgrade_available() {
        InstallDisposition::Refreshed
    } else {
        InstallDisposition::Current
    }
}

pub(crate) fn detected_installable_adapters() -> Vec<&'static dyn rimz::agents::AgentAdapter> {
    let mut detected = Vec::new();
    for agent in rimz::agents::ADAPTERS {
        let descriptor = agent.descriptor();
        if rimz::agents::locate_binary(descriptor).is_none() {
            continue;
        }

        if !descriptor.capabilities.hook_install {
            let reason = descriptor
                .hook_install_unavailable
                .unwrap_or("hook install is not supported for this adapter");
            tracing::debug!(
                agent = descriptor.kind,
                reason,
                "agent integrates without rimz-managed hooks; skipping hook install",
            );
            continue;
        }

        detected.push(*agent);
    }
    detected
}

pub(crate) fn ensure_detected_agent_hooks(attended: bool) -> Result<bool> {
    let mut actionable = Vec::new();

    for agent in detected_installable_adapters() {
        let descriptor = agent.descriptor();
        if !agent.hooks_installed() || agent.hook_upgrade_available() {
            actionable.push(agent.preview_hook_install()?);
            continue;
        }

        warn_untrusted_hooks(descriptor.kind, &agent.untrusted_installed_hooks())?;
    }

    if actionable.is_empty() {
        return Ok(false);
    }

    if !attended {
        print_noninteractive_notice(&actionable)?;
        return Ok(true);
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = render::err();
    let selected = prompt_consent(&actionable, &mut input, &mut out)?;
    install_selected(&selected, &mut out)?;
    Ok(true)
}

fn prompt_consent(
    previews: &[HookInstallPreview],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<&'static str>> {
    write_intro_context(out, previews)?;
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

fn install_selected(selected: &[&'static str], out: &mut dyn Write) -> Result<()> {
    writeln!(out)?;
    if selected.is_empty() {
        writeln!(
            out,
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        )?;
        return Ok(());
    }

    for name in selected {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let disposition = install_disposition(agent);
        let report = agent.install_hooks()?;
        write_install_result(out, &report, disposition)?;
        write_untrusted_hooks_notice(name, &agent.untrusted_installed_hooks(), out)?;
    }

    Ok(write_post_install_footer(out)?)
}

/// Stderr notice for hooks the agent's own trust gate still skips: the gate
/// silences every untrusted hook with no signal of its own, so the start
/// notice is where the dead channel becomes visible. Rimz cannot trust on
/// the user's behalf — only the agent's own UI can — so this warns with the
/// fix rather than gating the start. No-op when `untrusted` is empty.
fn warn_untrusted_hooks(kind: &str, untrusted: &[String]) -> Result<()> {
    let mut out = render::err();
    Ok(write_untrusted_hooks_notice(kind, untrusted, &mut out)?)
}

pub(crate) fn write_untrusted_hooks_notice(
    kind: &str,
    untrusted: &[String],
    out: &mut dyn Write,
) -> std::io::Result<()> {
    if untrusted.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::WARN,
            &format!(
                "{kind} hooks are installed but untrusted ({}) — {kind} silently skips them; {}",
                untrusted.join(", "),
                rimz::agents::hook_trust_fix(kind),
            )
        )
    )?;
    Ok(())
}

fn write_intro_context(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    first_run::write_header(out)?;
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
        "Rimz found {} coding {agent_word}: {}.",
        previews.len(),
        render::paint(render::palette::ACCENT, &agent_names)
    )?;
    writeln!(
        out,
        "To show {pronoun} live in the sidebar, Rimz installs or refreshes reporting hooks in each agent's config."
    )?;
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
        let annotation = if file.existed {
            "updates existing config"
        } else {
            "new file"
        };
        writeln!(
            out,
            "  {}{:name_pad$}  {}{:cell_pad$}  {}",
            render::paint(render::palette::ACCENT.bold(), name),
            "",
            cell,
            "",
            render::paint(render::palette::MUTED, annotation),
            name_pad = layout.name_width.saturating_sub(name.width()),
            cell_pad = layout.cell_width.saturating_sub(cell.width())
        )?;
    }

    if let Some(summary) = status_line_summary(preview) {
        writeln!(
            out,
            "{}{}",
            " ".repeat(layout.continuation_indent()),
            render::paint(render::palette::MUTED, &format!("+ {summary}"))
        )?;
    }
    Ok(())
}

fn write_prompt(out: &mut dyn Write) -> Result<()> {
    write!(
        out,
        "Install or refresh reporting hooks? {} ",
        render::paint(render::palette::ACCENT.bold(), "[Y/n]")
    )?;
    out.flush()?;
    Ok(())
}

pub(crate) fn render_dry_run(
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
            for line in preview_file_diff(file).lines() {
                writeln!(out, "    {}", color_diff_line(line))?;
            }
        }
    }
    Ok(())
}

fn color_diff_line(line: &str) -> String {
    if line.starts_with("+++") || line.starts_with("---") {
        render::paint(render::palette::ACCENT.bold(), line)
    } else if line.starts_with('+') {
        render::paint(render::palette::GOOD, line)
    } else if line.starts_with('-') {
        render::paint(render::palette::ALARM, line)
    } else if line.starts_with("@@") {
        render::paint(render::palette::WARN.bold(), line)
    } else {
        render::paint(render::palette::FAINT, line)
    }
}

pub(crate) fn write_install_result(
    out: &mut dyn Write,
    report: &HookInstallReport,
    disposition: InstallDisposition,
) -> std::io::Result<()> {
    let summary = match disposition {
        InstallDisposition::Installed => {
            format!("installed {} hooks", report.installed_events.len())
        }
        InstallDisposition::Refreshed => {
            format!("refreshed {} hooks", report.installed_events.len())
        }
        InstallDisposition::Current => {
            format!("hooks up to date ({})", report.installed_events.len())
        }
    };
    for (index, file) in report.files.iter().enumerate() {
        let annotation = if file.existed {
            "(updated existing config)"
        } else {
            "(new file)"
        };
        let annotation = if disposition == InstallDisposition::Current {
            String::new()
        } else {
            format!("  {}", render::paint(render::palette::MUTED, annotation))
        };
        if index == 0 {
            writeln!(
                out,
                "{} {}  {} → {}{}",
                render::paint(render::palette::GOOD.bold(), "✓"),
                report.agent,
                summary,
                home_relative_path(&file.path),
                annotation,
            )?;
        } else {
            writeln!(
                out,
                "       config → {}{}",
                home_relative_path(&file.path),
                annotation,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn write_uninstall_result(
    out: &mut dyn Write,
    report: &rimz::agents::HookUninstallReport,
) -> std::io::Result<()> {
    if report.files.is_empty() {
        writeln!(out, "{} — no Rimz-managed hooks found", report.agent)?;
        return Ok(());
    }

    for (index, file) in report.files.iter().enumerate() {
        if index == 0 {
            writeln!(
                out,
                "{} {}  removed {} hooks → {}",
                render::paint(render::palette::GOOD.bold(), "✓"),
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

fn print_noninteractive_notice(previews: &[HookInstallPreview]) -> Result<()> {
    let mut out = render::err();
    write_noninteractive_notice(&mut out, previews)
}

fn write_noninteractive_notice(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    write_intro_context(out, previews)?;
    writeln!(out)?;
    write_agent_table(out, previews)?;
    writeln!(out)?;
    write_consent_footer(out)?;
    writeln!(
        out,
        "No terminal input — nothing installed or refreshed. Rimz continues into the room; install or refresh agents later with rimz hooks install.",
    )?;
    Ok(())
}

fn preview_file_diff(file: &HookInstallFilePreview) -> String {
    let path = file.path.display().to_string();
    match file.original.as_deref() {
        Some(original) => {
            let diff = TextDiff::from_lines(original, &file.candidate);
            let rendered = diff
                .unified_diff()
                .context_radius(DIFF_CONTEXT_LINES)
                .header(&path, &path)
                .to_string();
            if rendered.is_empty() {
                format!("--- {path}\n+++ {path}\n@@ no changes @@\n")
            } else {
                rendered
            }
        }
        None => {
            let mut out = format!("--- /dev/null\n+++ {path}\n@@ new file @@\n");
            for line in file.candidate.lines() {
                out.push('+');
                out.push_str(line);
                out.push('\n');
            }
            out
        }
    }
}

fn write_consent_footer(out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you."
    )?;
    Ok(write_undo_preview_hints(out)?)
}

pub(crate) fn write_post_install_footer(out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out)?;
    write_undo_preview_hints(out)?;
    writeln!(
        out,
        "All set — your agents appear in the sidebar as they run."
    )?;
    Ok(())
}

fn write_undo_preview_hints(out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "  {}     rimz hooks uninstall",
        render::paint(render::palette::MUTED, "undo")
    )?;
    writeln!(
        out,
        "  {}  rimz hooks install --dry-run",
        render::paint(render::palette::MUTED, "preview")
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
        let selected = prompt_consent(previews, &mut input, &mut stream).expect("prompt");
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

        let (selected, _) = drive(&previews, b"");
        assert!(selected.is_empty());

        let rendered = strip(|out| install_selected(&selected, out));
        assert!(rendered.contains(
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        ));
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
        let noninteractive = strip(|out| write_noninteractive_notice(out, &previews));

        for rendered in [&interactive, &noninteractive] {
            assert!(rendered.contains("Rimz found 2 coding agents: claude, codex."));
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
            "No terminal input — nothing installed or refreshed. Rimz continues into the room; install or refresh agents later with rimz hooks install."
        ));
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
        let cli_config = home_config_path("cursor").with_file_name("cli-config.json");
        let report = HookInstallReport {
            agent: "cursor",
            files: vec![
                rimz::agents::HookInstallFileReport {
                    path: home_config_path("cursor").with_file_name("hooks.json"),
                    existed: true,
                },
                rimz::agents::HookInstallFileReport {
                    path: cli_config,
                    existed: false,
                },
            ],
            installed_events: vec!["stop".to_owned()],
        };

        for (disposition, summary) in [
            (InstallDisposition::Installed, "installed 1 hooks"),
            (InstallDisposition::Refreshed, "refreshed 1 hooks"),
            (InstallDisposition::Current, "hooks up to date (1)"),
        ] {
            let rendered = strip(|out| write_install_result(out, &report, disposition));
            assert!(rendered.contains(&format!("✓ cursor  {summary}")));
            assert!(rendered.contains("~/.cursor/hooks.json"));
            assert!(rendered.contains("config → ~/.cursor/cli-config.json"));
            if disposition == InstallDisposition::Current {
                assert!(!rendered.contains("(updated existing config)"));
                assert!(!rendered.contains("(new file)"));
            } else {
                assert!(rendered.contains("(updated existing config)"));
                assert!(rendered.contains("(new file)"));
            }
        }

        let uninstall = rimz::agents::HookUninstallReport {
            agent: "cursor",
            files: report.files,
            removed_events: vec!["stop".to_owned()],
        };
        let rendered = strip(|out| write_uninstall_result(out, &uninstall));
        assert!(rendered.contains("✓ cursor  removed 1 hooks → ~/.cursor/hooks.json"));
        assert!(rendered.contains("config → ~/.cursor/cli-config.json"));

        let empty = rimz::agents::HookUninstallReport {
            agent: "codex",
            files: Vec::new(),
            removed_events: Vec::new(),
        };
        let rendered = strip(|out| write_uninstall_result(out, &empty));
        assert_eq!(rendered, "codex — no Rimz-managed hooks found\n");

        let rendered = strip(|out| write_post_install_footer(out));
        assert!(rendered.contains("rimz hooks uninstall"));
        assert!(rendered.contains("rimz hooks install --dry-run"));
        assert!(rendered.contains("All set — your agents appear in the sidebar as they run."));
    }
}
