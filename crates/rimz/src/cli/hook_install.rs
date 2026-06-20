use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;
use rimz::agents::{HookInstallPreview, HookInstallReport, StatusLineChange};
use similar::TextDiff;
use unicode_width::UnicodeWidthStr;

use super::render;

const DIFF_CONTEXT_LINES: usize = 3;
const CARD_TEXT_WIDTH: usize = 44;

const CONSENT_INTRO: &str = "Rimz routes attention across your coding agents into one sidebar.";
const CONSENT_BOUNDARY: &str =
    "These hooks only report events to Rimz. They never answer a prompt for you.";
const CONSENT_REVERSIBLE: &str = "Reversible any time with `rimz hooks uninstall`.";

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

pub(super) fn ensure_detected_agent_hooks() -> Result<()> {
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
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        print_noninteractive_notice(&missing)?;
        return Ok(());
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = render::err();
    let selected = prompt_consent(&missing, &mut input, &mut out)?;
    install_selected(&missing, &selected, &mut out)
}

fn prompt_consent(
    previews: &[HookInstallPreview],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<&'static str>> {
    write_intro(out, previews)?;
    let mut selected = Vec::new();
    for (idx, preview) in previews.iter().enumerate() {
        write_agent_block(out, preview, idx, previews.len())?;
        loop {
            let mut answer = String::new();
            if input.read_line(&mut answer)? == 0 {
                return Ok(selected);
            }
            match answer.trim() {
                "" | "y" | "Y" | "yes" | "YES" | "Yes" => {
                    selected.push(preview.agent);
                    break;
                }
                "n" | "N" | "no" | "NO" | "No" => break,
                "d" | "D" => {
                    write_diff(out, preview)?;
                    write_prompt(out, idx + 1 < previews.len())?;
                }
                "s" | "S" | "q" | "Q" if idx + 1 < previews.len() => return Ok(selected),
                _ => {
                    let skip_hint = if idx + 1 < previews.len() {
                        "; s skips the rest"
                    } else {
                        ""
                    };
                    writeln!(
                        out,
                        "  Enter adds this agent; n skips; d shows the diff{skip_hint}."
                    )?;
                    write_prompt(out, idx + 1 < previews.len())?;
                }
            }
        }
    }
    Ok(selected)
}

fn install_selected(
    previews: &[HookInstallPreview],
    selected: &[&'static str],
    out: &mut dyn Write,
) -> Result<()> {
    writeln!(out)?;
    if selected.is_empty() {
        writeln!(
            out,
            "Nothing changed - wire agents any time with `rimz hooks install`."
        )?;
        return Ok(());
    }

    let installed = selected.iter().copied().collect::<BTreeSet<_>>();
    for name in selected {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let report = agent.install_hooks()?;
        write_install_result(out, &report)?;
        write_untrusted_hooks_notice(name, &agent.untrusted_installed_hooks(), out)?;
    }

    for preview in previews {
        if !installed.contains(preview.agent) {
            write_skipped_note(out, preview)?;
        }
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
    write_intro_context(out, previews)?;
    if previews.len() == 1 {
        writeln!(out, "One quick question. {CONSENT_REVERSIBLE}")?;
    } else {
        writeln!(
            out,
            "{} quick questions — one per agent. {CONSENT_REVERSIBLE}",
            previews.len()
        )?;
    }
    Ok(())
}

fn write_intro_context(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    for line in intro_card_lines(terminal_columns()) {
        writeln!(out, "{line}")?;
    }
    writeln!(out)?;
    let agent_word = if previews.len() == 1 {
        "agent"
    } else {
        "agents"
    };
    let agent_names = previews
        .iter()
        .map(|preview| preview.agent)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "Rimz found {} coding {agent_word} on this machine: {agent_names}.",
        previews.len()
    )?;
    writeln!(
        out,
        "To show what an agent is doing, Rimz adds reporting hooks to the agent's config."
    )?;
    Ok(())
}

fn intro_card_lines(term_cols: usize) -> Vec<String> {
    let card_text = intro_card_text();
    let box_width = CARD_TEXT_WIDTH + 4;
    if term_cols < box_width {
        return card_text
            .iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("  {line}")
                }
            })
            .collect();
    }

    let rule = "─".repeat(CARD_TEXT_WIDTH + 2);
    let mut lines = Vec::with_capacity(card_text.len() + 2);
    lines.push(format!("╭{rule}╮"));
    for line in card_text {
        let pad = CARD_TEXT_WIDTH.saturating_sub(line.width());
        lines.push(format!("│ {line}{:pad$} │", "", pad = pad));
    }
    lines.push(format!("╰{rule}╯"));
    lines
}

fn intro_card_text() -> Vec<String> {
    let mut lines = vec!["rimz · first-run setup".to_owned(), String::new()];
    lines.extend(wrap_words(CONSENT_INTRO, CARD_TEXT_WIDTH));
    lines.push(String::new());
    lines.extend(wrap_words(CONSENT_BOUNDARY, CARD_TEXT_WIDTH));
    lines
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let next_width = if current.is_empty() {
            word.width()
        } else {
            current.width() + 1 + word.width()
        };
        if next_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn terminal_columns() -> usize {
    ratatui::crossterm::terminal::size()
        .map(|(cols, _)| usize::from(cols))
        .unwrap_or(80)
}

fn write_agent_block(
    out: &mut dyn Write,
    preview: &HookInstallPreview,
    idx: usize,
    total: usize,
) -> Result<()> {
    writeln!(out)?;
    write_agent_body(out, preview, idx, total)?;
    write_prompt(out, idx + 1 < total)
}

fn write_agent_body(
    out: &mut dyn Write,
    preview: &HookInstallPreview,
    idx: usize,
    total: usize,
) -> Result<()> {
    let counter = format!("· {} of {}", idx + 1, total);
    writeln!(
        out,
        "  {} {}",
        render::paint(render::palette::ACCENT.bold(), preview.agent),
        render::paint(render::palette::FAINT, &counter)
    )?;
    let config_path = home_relative_path(&preview.config_path);
    let tag = if preview.merged {
        "(additive — existing hooks kept)"
    } else {
        "(new file)"
    };
    writeln!(
        out,
        "    {} hooks → {} {}",
        preview.planned_events.len(),
        config_path,
        render::paint(render::palette::FAINT, tag)
    )?;
    for summary in status_line_summaries(preview) {
        writeln!(
            out,
            "    {}",
            render::paint(render::palette::FAINT, &format!("also {summary}"))
        )?;
    }
    writeln!(
        out,
        "    {}",
        render::paint(
            render::palette::FAINT,
            &format!("undo → rimz hooks uninstall {}", preview.agent)
        )
    )?;
    Ok(())
}

fn write_prompt(out: &mut dyn Write, offer_skip_rest: bool) -> Result<()> {
    write!(
        out,
        "  Add hooks?  {} · {}",
        render::paint(render::palette::ACCENT.bold(), "[Y/n]"),
        render::paint(render::palette::ACCENT.bold(), "d=diff")
    )?;
    if offer_skip_rest {
        write!(
            out,
            " · {}",
            render::paint(render::palette::ACCENT.bold(), "s=skip remaining")
        )?;
    }
    write!(out, " ")?;
    out.flush()?;
    Ok(())
}

fn write_diff(out: &mut dyn Write, preview: &HookInstallPreview) -> Result<()> {
    writeln!(out)?;
    for line in preview_diff(preview).lines() {
        writeln!(out, "    {}", color_diff_line(line))?;
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

fn write_skipped_note(out: &mut dyn Write, preview: &HookInstallPreview) -> Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::FAINT,
            &format!(
                "· {}  skipped — wire later with `rimz hooks install {}`",
                preview.agent, preview.agent
            )
        )
    )?;
    Ok(())
}

fn print_noninteractive_notice(previews: &[HookInstallPreview]) -> Result<()> {
    let mut out = render::err();
    write_noninteractive_notice(&mut out, previews)
}

fn write_noninteractive_notice(out: &mut dyn Write, previews: &[HookInstallPreview]) -> Result<()> {
    write_intro_context(out, previews)?;
    for (idx, preview) in previews.iter().enumerate() {
        writeln!(out)?;
        write_agent_body(out, preview, idx, previews.len())?;
    }
    writeln!(out)?;
    writeln!(out, "{CONSENT_REVERSIBLE}")?;
    writeln!(
        out,
        "No terminal input is available, so Rimz installs nothing and continues into the room.",
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

fn status_line_summaries(preview: &HookInstallPreview) -> Vec<String> {
    let mut summaries = Vec::new();
    push_status_line_summary(
        &mut summaries,
        "statusLine",
        "report context to Rimz",
        &preview.status_line_change,
    );
    push_status_line_summary(
        &mut summaries,
        "subagentStatusLine",
        "report subagent activity to Rimz",
        &preview.subagent_status_line_change,
    );
    summaries
}

fn push_status_line_summary(
    summaries: &mut Vec<String>,
    key: &str,
    purpose: &str,
    change: &Option<StatusLineChange>,
) {
    match change {
        Some(StatusLineChange::Added) => {
            summaries.push(format!(
                "sets your {key} to {purpose} (removed on uninstall)"
            ));
        }
        Some(StatusLineChange::Wrapping { original }) => {
            summaries.push(format!(
                "wraps your {key} command ({original}) — restored on uninstall"
            ));
        }
        Some(StatusLineChange::Unchanged) | None => {}
    }
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
    fn prompt_selects_and_skips_agents() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];

        let (selected, _) = drive(&previews, b"\nn\n");

        assert_eq!(selected, vec!["claude"]);
    }

    #[test]
    fn prompt_prints_diff_and_reprompts() {
        let previews = [preview("claude", Some("old\n"), "new\n")];

        let (selected, rendered) = drive(&previews, b"d\n\n");

        assert_eq!(selected, vec!["claude"]);
        assert!(rendered.contains("-old"));
        assert!(rendered.contains("+new"));
        assert!(rendered.matches("Add hooks?").count() >= 2);
    }

    #[test]
    fn prompt_eof_keeps_prior_choices_without_approving_current_agent() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];

        let (selected, _) = drive(&previews, b"\n");

        assert_eq!(selected, vec!["claude"]);
    }

    #[test]
    fn prompt_skip_rest_keeps_prior_choices_and_stops() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("opencode", Some("{}\n"), "{\"hooks\": []}\n"),
        ];

        let (selected, rendered) = drive(&previews, b"\ns\n");

        assert_eq!(selected, vec!["claude"]);
        assert!(rendered.contains("codex"));
        assert!(!rendered.contains("opencode · 3 of 3"));
    }

    #[test]
    fn prompt_content_names_intro_agent_path_and_change_kind() {
        let mut additive = preview("claude", Some("{}\n"), "{\"hooks\": []}\n");
        additive.status_line_change = Some(StatusLineChange::Added);
        let created = preview("codex", None, "{\"hooks\": []}\n");
        let previews = [additive, created];

        let (_, rendered) = drive(&previews, b"n\nn\n");

        assert!(rendered.contains("first-run setup"));
        assert!(rendered.contains("Rimz found 2 coding agents on this machine: claude, codex."));
        assert!(rendered.contains("Add hooks?"));
        assert!(rendered.contains("claude"));
        assert!(rendered.contains("~/.claude/settings.json"));
        assert!(rendered.contains("(additive"));
        assert!(rendered.contains("(new file)"));
        assert!(rendered.contains("sets your statusLine"));
    }

    #[test]
    fn noninteractive_notice_has_no_questions_and_one_reversible_line() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", None, "{\"hooks\": []}\n"),
        ];

        let rendered = strip(|w| write_noninteractive_notice(w, &previews));

        assert!(rendered.contains("Rimz found 2 coding agents on this machine: claude, codex."));
        assert!(rendered.contains("claude · 1 of 2"));
        assert!(rendered.contains("codex · 2 of 2"));
        assert!(!rendered.contains("quick question"));
        assert_eq!(rendered.matches(CONSENT_REVERSIBLE).count(), 1);
        assert!(rendered.contains(
            "No terminal input is available, so Rimz installs nothing and continues into the room."
        ));
    }

    #[test]
    fn intro_card_lines_use_border_when_wide_and_plain_when_narrow() {
        let wide = intro_card_lines(80).join("\n");
        assert!(wide.contains('╭'));
        assert!(wide.contains('╰'));

        let narrow = intro_card_lines(20).join("\n");
        assert!(!narrow.contains('╭'));
        assert!(narrow.contains("These hooks only report events to Rimz."));
        assert!(narrow.contains("never answer a prompt for you."));
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
    fn install_renderers_show_results_and_skipped_agents() {
        let report = HookInstallReport {
            agent: "claude",
            config_path: home_config_path("claude"),
            installed_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            merged: true,
        };
        let skipped = preview("codex", None, "{}\n");

        let rendered = strip(|w| {
            write_install_result(w, &report)?;
            write_skipped_note(w, &skipped)?;
            install_selected(&[skipped], &[], w)
        });

        assert!(rendered.contains("✓ claude  2 hooks → ~/.claude/settings.json"));
        assert!(rendered.contains("· codex  skipped — wire later with `rimz hooks install codex`"));
        assert!(
            rendered.contains("Nothing changed - wire agents any time with `rimz hooks install`.")
        );
    }
}
