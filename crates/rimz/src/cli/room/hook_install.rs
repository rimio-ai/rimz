//! Start-time agent hook auto-install prompt.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;
use rimz::agents::{HookInstallPreview, HookInstallReport, StatusLineChange};
use similar::TextDiff;
use unicode_width::UnicodeWidthStr;

use crate::cli::{first_run, render};

const DIFF_CONTEXT_LINES: usize = 3;

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
            tracing::warn!(
                agent = descriptor.kind,
                reason,
                "detected agent cannot be wired automatically",
            );
            continue;
        }

        detected.push(*agent);
    }
    detected
}

pub(crate) fn ensure_detected_agent_hooks() -> Result<bool> {
    let mut missing = Vec::new();

    for agent in detected_installable_adapters() {
        let descriptor = agent.descriptor();
        if !agent.hooks_installed() {
            missing.push(agent.preview_hook_install()?);
            continue;
        }

        warn_untrusted_hooks(descriptor.kind, &agent.untrusted_installed_hooks())?;
    }

    if missing.is_empty() {
        return Ok(false);
    }

    if !std::io::stdin().is_terminal() {
        print_noninteractive_notice(&missing)?;
        return Ok(true);
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = render::err();
    let selected = prompt_consent(&missing, &mut input, &mut out)?;
    install_selected(&selected, &mut out)?;
    Ok(true)
}

fn prompt_consent(
    previews: &[HookInstallPreview],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<&'static str>> {
    write_intro(out, previews)?;
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
                    "  Enter installs hooks for every listed agent; n skips."
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
            "Nothing changed - wire agents any time with `rimz hooks install`."
        )?;
        return Ok(());
    }

    for name in selected {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let report = agent.install_hooks()?;
        write_install_result(out, &report)?;
        write_untrusted_hooks_notice(name, &agent.untrusted_installed_hooks(), out)?;
    }

    writeln!(
        out,
        "All set — your agents appear in the sidebar as they run."
    )?;
    Ok(())
}

/// Stderr notice for hooks the agent's own trust gate still skips: the gate
/// silences every untrusted hook with no signal of its own, so the start
/// notice is where the dead channel becomes visible. Rimz cannot trust on
/// the user's behalf — only the agent's own UI can — so this warns with the
/// fix rather than gating the start. No-op when `untrusted` is empty.
fn warn_untrusted_hooks(kind: &str, untrusted: &[String]) -> Result<()> {
    let mut out = render::err();
    write_untrusted_hooks_notice(kind, untrusted, &mut out)
}

fn write_untrusted_hooks_notice(
    kind: &str,
    untrusted: &[String],
    out: &mut dyn Write,
) -> Result<()> {
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

fn write_intro(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    write_intro_context(out, previews)
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
        "To show {pronoun} live in the sidebar, it adds reporting hooks to each agent's config."
    )?;
    Ok(())
}

fn write_agent_table(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
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
) -> Result<()> {
    let cell = agent_hook_cell(preview);
    let annotation = if preview.merged {
        "existing kept"
    } else {
        "new file"
    };
    writeln!(
        out,
        "  {}{:name_pad$}  {}{:cell_pad$}  {}",
        render::paint(render::palette::ACCENT.bold(), preview.agent),
        "",
        cell,
        "",
        render::paint(render::palette::MUTED, annotation),
        name_pad = layout.name_width.saturating_sub(preview.agent.width()),
        cell_pad = layout.cell_width.saturating_sub(cell.width())
    )?;

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
        "Add reporting hooks? {} ",
        render::paint(render::palette::ACCENT.bold(), "[Y/n]")
    )?;
    out.flush()?;
    Ok(())
}

pub(crate) fn render_dry_run(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    let layout = AgentTableLayout::from_previews(previews);
    for (idx, preview) in previews.iter().enumerate() {
        if idx > 0 {
            writeln!(out)?;
        }
        write_agent_table_entry(out, preview, &layout)?;
        for line in preview_diff(preview).lines() {
            writeln!(out, "    {}", color_diff_line(line))?;
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

fn write_install_result(out: &mut dyn Write, report: &HookInstallReport) -> Result<()> {
    writeln!(
        out,
        "{} {}  {} hooks → {}",
        render::paint(render::palette::GOOD.bold(), "✓"),
        report.agent,
        report.installed_events.len(),
        home_relative_path(&report.config_path)
    )?;
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
        "No terminal input — nothing installed. Rimz continues into the room; wire agents later with rimz hooks install.",
    )?;
    Ok(())
}

fn preview_diff(preview: &HookInstallPreview) -> String {
    let path = preview.config_path.display().to_string();
    match preview.original_config.as_deref() {
        Some(original) => {
            let diff = TextDiff::from_lines(original, &preview.candidate_config);
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
            for line in preview.candidate_config.lines() {
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
                .map(|preview| agent_hook_cell(preview).width())
                .max()
                .unwrap_or_default(),
        }
    }

    fn continuation_indent(&self) -> usize {
        2 + self.name_width + 2
    }
}

fn agent_hook_cell(preview: &HookInstallPreview) -> String {
    format!(
        "{} hooks → {}",
        preview.planned_events.len(),
        home_relative_path(&preview.config_path)
    )
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
        HookInstallPreview {
            agent,
            config_path: home_config_path(agent),
            planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            original_config: original.map(str::to_owned),
            candidate_config: candidate.to_owned(),
            merged: original.is_some(),
            status_line_change: None,
            subagent_status_line_change: None,
        }
    }

    fn home_config_path(agent: &str) -> PathBuf {
        let home = std::env::var("HOME").expect("HOME is set for CLI render tests");
        PathBuf::from(home).join(format!(".{agent}/settings.json"))
    }

    fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> Result<()>) -> String {
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
    }

    #[test]
    fn prompt_eof_declines_every_agent() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];

        let (selected, _) = drive(&previews, b"");

        assert!(selected.is_empty());
    }

    #[test]
    fn prompt_content_names_intro_agent_path_and_change_kind() {
        let mut additive = preview("claude", Some("{}\n"), "{\"hooks\": []}\n");
        additive.status_line_change = Some(StatusLineChange::Wrapping {
            original: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed".to_owned(),
        });
        let created = preview("codex", None, "{\"hooks\": []}\n");
        let previews = [additive, created];

        let (_, rendered) = drive(&previews, b"n\n");

        assert!(rendered.contains("first-run setup"));
        assert!(rendered.contains("Rimz found 2 coding agents: claude, codex."));
        assert!(rendered.contains(
            "To show them live in the sidebar, it adds reporting hooks to each agent's config."
        ));
        assert!(rendered.contains("Each hook is one `rimz hooks feed` line"));
        assert_eq!(rendered.matches("rimz hooks uninstall").count(), 1);
        assert!(rendered.contains("rimz hooks install --dry-run"));
        assert!(rendered.contains("Add reporting hooks?"));
        assert_eq!(rendered.matches("[Y/n]").count(), 1);
        assert!(!rendered.contains("One quick question."));
        assert!(!rendered.contains("rimz hooks feed --source"));
        assert!(!rendered.contains("undo →"));
        assert!(!rendered.contains("1 of 2"));
        assert!(!rendered.contains("$PPID"));
        assert!(!rendered.contains("d=diff"));
        assert!(!rendered.contains("skip remaining"));
        assert!(rendered.contains("claude  2 hooks → ~/.claude/settings.json"));
        assert!(rendered.contains("codex   2 hooks → ~/.codex/settings.json"));
        assert!(rendered.contains("existing kept"));
        assert!(rendered.contains("new file"));
        assert!(
            rendered
                .contains("+ wraps your statusline for live context — yours restored on uninstall")
        );

        let existing_row = rendered
            .lines()
            .find(|line| line.contains("existing kept"))
            .expect("existing row");
        let new_row = rendered
            .lines()
            .find(|line| line.contains("new file"))
            .expect("new row");
        assert_eq!(existing_row.find("existing kept"), new_row.find("new file"));
    }

    #[test]
    fn noninteractive_notice_has_table_footer_and_no_question() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", None, "{\"hooks\": []}\n"),
        ];

        let rendered = strip(|w| write_noninteractive_notice(w, &previews));

        assert!(rendered.contains("Rimz found 2 coding agents: claude, codex."));
        assert!(rendered.contains("claude  2 hooks → ~/.claude/settings.json"));
        assert!(rendered.contains("codex   2 hooks → ~/.codex/settings.json"));
        assert!(rendered.contains("Each hook is one `rimz hooks feed` line"));
        assert_eq!(rendered.matches("rimz hooks uninstall").count(), 1);
        assert!(rendered.contains("rimz hooks install --dry-run"));
        assert!(!rendered.contains("Add reporting hooks?"));
        assert!(!rendered.contains("quick question"));
        assert!(!rendered.contains("1 of 2"));
        assert!(rendered.contains(
            "No terminal input — nothing installed. Rimz continues into the room; wire agents later with rimz hooks install."
        ));
    }

    #[test]
    fn preview_diff_covers_existing_and_new_config_files() {
        let existing = preview(
            "claude",
            Some("alpha\nkeep\nold\nomega\n"),
            "alpha\nkeep\nnew\nomega\n",
        );
        let diff = preview_diff(&existing);
        assert!(diff.contains(".claude/settings.json"));
        assert!(diff.contains("@@"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
        assert!(!diff.contains("@@ original @@"));
        assert!(!diff.contains("@@ candidate @@"));

        let created = preview("claude", None, "one\ntwo\n");
        let diff = preview_diff(&created);
        assert!(diff.starts_with("--- /dev/null\n+++ "));
        assert!(diff.contains(".claude/settings.json\n@@ new file @@\n"));
        assert!(diff.contains("+one\n+two\n"));
    }

    #[test]
    fn dry_run_renders_agent_bodies_and_diffs_without_prompt() {
        let previews = [
            preview("claude", Some("old\n"), "new\n"),
            preview("codex", None, "one\ntwo\n"),
        ];

        let rendered = strip(|w| render_dry_run(w, &previews));

        assert!(rendered.contains("claude  2 hooks → ~/.claude/settings.json"));
        assert!(rendered.contains("codex   2 hooks → ~/.codex/settings.json"));
        assert!(!rendered.contains("1 of 2"));
        assert!(rendered.contains("-old"));
        assert!(rendered.contains("+new"));
        assert!(rendered.contains("+one\n"));
        assert!(!rendered.contains("Add reporting hooks?"));
    }

    #[test]
    fn install_renderers_show_results_and_noop_message() {
        let report = HookInstallReport {
            agent: "claude",
            config_path: home_config_path("claude"),
            installed_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            merged: true,
        };

        let rendered = strip(|w| {
            write_install_result(w, &report)?;
            install_selected(&[], w)
        });

        assert!(rendered.contains("✓ claude  2 hooks → ~/.claude/settings.json"));
        assert!(
            rendered.contains("Nothing changed - wire agents any time with `rimz hooks install`.")
        );
    }
}
