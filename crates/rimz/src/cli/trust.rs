//! `rimz trust` — manage the project's executable-surface trust grant.
//!
//! Three subcommands: `status` (default), `grant`, `revoke`. Status re-hashes
//! the live `.rimz/config.toml` every call, so a drifted hash surfaces as
//! `stale` automatically — no separate sweep needed.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::io::{self, Write};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::GlobalFlags;
use crate::cli::render;
use rimz::trust::{self, SurfaceDiffEntry, SurfaceDiffKind, TrustReport, TrustState};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct TrustArgs {
    #[command(subcommand)]
    command: Option<TrustSubcmd>,
    /// Emit JSON instead of the human-readable summary.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TrustSubcmd {
    /// Show the trust state for the current workspace.
    Status,
    /// Pin the current executable-surface hash as trusted.
    Grant,
    /// Drop the trust grant; the next read of project config is untrusted.
    Revoke,
}

pub fn run(args: TrustArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let report = match args.command.unwrap_or(TrustSubcmd::Status) {
        TrustSubcmd::Status => {
            trust::status(&workspace.project_root).context("reading trust state")?
        }
        TrustSubcmd::Grant => trust::grant(&workspace.project_root).context("granting trust")?,
        TrustSubcmd::Revoke => trust::revoke(&workspace.project_root).context("revoking trust")?,
    };
    print_report(&report, args.json)?;
    Ok(())
}

#[derive(Serialize)]
struct ReportJson<'a> {
    state: &'a str,
    workspace_id: &'a str,
    project_root: String,
    config_path: String,
    record_path: String,
    current_hash: Option<&'a str>,
    granted_hash: Option<&'a str>,
    granted_at: Option<String>,
    surface_diff: Option<&'a [SurfaceDiffEntry]>,
}

fn print_report(report: &TrustReport, as_json: bool) -> Result<()> {
    if as_json {
        return render::json_pretty(&ReportJson {
            state: report.state.as_str(),
            workspace_id: report.workspace_id.as_str(),
            project_root: report.project_root.display().to_string(),
            config_path: report.config_path.display().to_string(),
            record_path: report.record_path.display().to_string(),
            current_hash: report.current_hash.as_deref(),
            granted_hash: report.granted_hash.as_deref(),
            granted_at: report.granted_at.map(|t| t.to_string()),
            surface_diff: report.surface_diff.as_deref(),
        });
    }
    let mut out = render::out();
    writeln!(
        out,
        "{} {}",
        render::paint(render::palette::MUTED, "trust:"),
        render::paint(
            render::status::trust(report.state),
            trust_banner(report.state)
        ),
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "workspace id",
        render::cell(report.workspace_id.as_str()).fg(render::palette::ACCENT),
    );
    kv.push(
        "project root",
        render::cell(report.project_root.display().to_string()),
    );
    kv.push(
        "config path",
        render::cell(report.config_path.display().to_string()),
    );
    kv.push(
        "record path",
        render::cell(report.record_path.display().to_string()),
    );
    if let Some(hash) = &report.current_hash {
        kv.push(
            "current hash",
            render::cell(hash.as_str()).fg(render::palette::BODY),
        );
    }
    if let Some(hash) = &report.granted_hash {
        kv.push(
            "granted hash",
            render::cell(hash.as_str()).fg(render::palette::BODY),
        );
    }
    if let Some(at) = report.granted_at {
        kv.push("granted at", render::cell(at.to_string()));
    }
    kv.render(&mut out)?;
    render_surface_diff(
        &mut out,
        report.surface_diff.as_deref(),
        render::terminal_columns(100),
    )?;
    Ok(())
}

fn trust_banner(state: TrustState) -> &'static str {
    match state {
        TrustState::NoConfig => "no project config",
        TrustState::Untrusted => "untrusted",
        TrustState::Trusted => "trusted",
        TrustState::Stale => "stale — executable surface changed since last grant",
    }
}

/// Show the executable-surface change on stderr and offer to grant it inline.
/// Returns whether the project is trusted when the offer finishes.
pub(crate) fn offer_inline_grant(project_root: &Path, question: &str) -> Result<bool> {
    let report = trust::status(project_root).context("reading trust state")?;
    let mut out = render::err();
    writeln!(
        out,
        "{} {}",
        render::paint(render::palette::MUTED, "trust:"),
        render::paint(
            render::status::trust(report.state),
            trust_banner(report.state)
        ),
    )?;
    writeln!(
        out,
        "  config: {}",
        render::home_relative(&report.config_path.display().to_string())
    )?;
    render_surface_diff(
        &mut out,
        report.surface_diff.as_deref(),
        render::terminal_columns(100),
    )?;
    drop(out);

    if report.state == TrustState::Trusted {
        return Ok(true);
    }
    if report.state == TrustState::NoConfig || !crate::cli::confirm(question)? {
        return Ok(false);
    }

    let granted = trust::grant(project_root).context("granting trust")?;
    if granted.state != TrustState::Trusted {
        return Ok(false);
    }
    writeln!(
        render::err(),
        "{} {}",
        render::paint(render::palette::MUTED, "trust:"),
        render::paint(render::status::trust(granted.state), "granted"),
    )?;
    Ok(true)
}

fn render_surface_diff(
    out: &mut impl Write,
    entries: Option<&[SurfaceDiffEntry]>,
    width: usize,
) -> io::Result<()> {
    let Some(entries) = entries else {
        return Ok(());
    };
    writeln!(
        out,
        "  {}",
        render::paint(render::palette::MUTED, "surface diff:")
    )?;
    if entries.is_empty() {
        writeln!(out, "    no field changes")
    } else {
        for entry in entries {
            let path = format_diff_path(&entry.path);
            match entry.kind {
                SurfaceDiffKind::Added => {
                    writeln!(
                        out,
                        "    {}",
                        render::paint(render::palette::GOOD, &format!("+ {path}"))
                    )?;
                    write_block(
                        out,
                        '+',
                        render::palette::GOOD,
                        &[Span::plain(format_diff_value(entry.current.as_ref()))],
                        width,
                    )?;
                }
                SurfaceDiffKind::Removed => {
                    writeln!(
                        out,
                        "    {}",
                        render::paint(render::palette::ALARM, &format!("- {path}"))
                    )?;
                    write_block(
                        out,
                        '-',
                        render::palette::ALARM,
                        &[Span::plain(format_diff_value(entry.granted.as_ref()))],
                        width,
                    )?;
                }
                SurfaceDiffKind::Changed => {
                    writeln!(
                        out,
                        "    {} {}",
                        render::paint(render::palette::WARN, "~"),
                        render::paint(render::palette::BODY.bold(), &path)
                    )?;
                    if let (
                        Some(serde_json::Value::String(granted)),
                        Some(serde_json::Value::String(current)),
                    ) = (entry.granted.as_ref(), entry.current.as_ref())
                    {
                        let (granted, current) = word_diff_spans(granted, current);
                        write_block(out, '-', render::palette::ALARM, &granted, width)?;
                        write_block(out, '+', render::palette::GOOD, &current, width)?;
                    } else {
                        write_block(
                            out,
                            '-',
                            render::palette::ALARM,
                            &[Span::plain(format_diff_value(entry.granted.as_ref()))],
                            width,
                        )?;
                        write_block(
                            out,
                            '+',
                            render::palette::GOOD,
                            &[Span::plain(format_diff_value(entry.current.as_ref()))],
                            width,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Span {
    text: String,
    emphasized: bool,
}

impl Span {
    fn plain(text: String) -> Self {
        Self {
            text,
            emphasized: false,
        }
    }
}

fn push_span(spans: &mut Vec<Span>, text: &str, emphasized: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.emphasized == emphasized
    {
        last.text.push_str(text);
    } else {
        spans.push(Span {
            text: text.to_owned(),
            emphasized,
        });
    }
}

fn word_diff_spans(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let diff = TextDiff::from_unicode_words(old, new);
    let mut old_spans = Vec::new();
    let mut new_spans = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                push_span(&mut old_spans, change.value(), false);
                push_span(&mut new_spans, change.value(), false);
            }
            ChangeTag::Delete => push_span(&mut old_spans, change.value(), true),
            ChangeTag::Insert => push_span(&mut new_spans, change.value(), true),
        }
    }
    (old_spans, new_spans)
}

enum BlockToken {
    Word(Vec<Span>),
    Break,
}

fn block_tokens(spans: &[Span]) -> Vec<BlockToken> {
    let mut tokens = Vec::new();
    let mut word = Vec::new();
    for span in spans {
        for ch in span.text.chars() {
            if ch == '\n' {
                if !word.is_empty() {
                    tokens.push(BlockToken::Word(std::mem::take(&mut word)));
                }
                tokens.push(BlockToken::Break);
            } else if ch.is_whitespace() {
                if !word.is_empty() {
                    tokens.push(BlockToken::Word(std::mem::take(&mut word)));
                }
            } else if let Some(last) = word.last_mut()
                && last.emphasized == span.emphasized
            {
                last.text.push(ch);
            } else {
                word.push(Span {
                    text: ch.to_string(),
                    emphasized: span.emphasized,
                });
            }
        }
    }
    if !word.is_empty() {
        tokens.push(BlockToken::Word(word));
    }
    tokens
}

fn push_word(
    lines: &mut Vec<Vec<Span>>,
    line: &mut Vec<Span>,
    line_width: &mut usize,
    word: Vec<Span>,
    content_width: usize,
) {
    let word_width = word.iter().map(|span| span.text.width()).sum::<usize>();
    if !line.is_empty() && *line_width + 1 + word_width <= content_width {
        push_span(line, " ", false);
        *line_width += 1;
    } else if !line.is_empty() {
        lines.push(std::mem::take(line));
        *line_width = 0;
    }

    for span in word {
        for ch in span.text.chars() {
            let char_width = ch.width().unwrap_or(0);
            if !line.is_empty() && *line_width + char_width > content_width {
                lines.push(std::mem::take(line));
                *line_width = 0;
            }
            if let Some(last) = line.last_mut()
                && last.emphasized == span.emphasized
            {
                last.text.push(ch);
            } else {
                line.push(Span {
                    text: ch.to_string(),
                    emphasized: span.emphasized,
                });
            }
            *line_width += char_width;
        }
    }
}

fn wrap_block(spans: &[Span], content_width: usize) -> Vec<Vec<Span>> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut line_width = 0;
    let mut ended_with_break = false;
    for token in block_tokens(spans) {
        match token {
            BlockToken::Word(word) => {
                push_word(&mut lines, &mut line, &mut line_width, word, content_width);
                ended_with_break = false;
            }
            BlockToken::Break => {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
                ended_with_break = true;
            }
        }
    }
    if !ended_with_break || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn write_block(
    out: &mut impl Write,
    sigil: char,
    style: anstyle::Style,
    spans: &[Span],
    width: usize,
) -> io::Result<()> {
    const FIRST_PREFIX: &str = "      - ";
    const CONTINUATION_PREFIX: &str = "        ";
    let content_width = width.saturating_sub(FIRST_PREFIX.width()).max(1);
    for (index, line) in wrap_block(spans, content_width).into_iter().enumerate() {
        if index == 0 {
            write!(out, "      {}", render::paint(style, &format!("{sigil} ")))?;
        } else {
            write!(out, "{CONTINUATION_PREFIX}")?;
        }
        for span in line {
            let span_style = if span.emphasized { style.bold() } else { style };
            write!(out, "{}", render::paint(span_style, &span.text))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn format_diff_path(path: &[String]) -> String {
    let mut rendered = String::new();
    for segment in path {
        if segment.starts_with('[') {
            rendered.push_str(segment);
        } else {
            if !rendered.is_empty() {
                rendered.push('.');
            }
            rendered.push_str(segment);
        }
    }
    if rendered.is_empty() {
        "(root)".to_owned()
    } else {
        rendered
    }
}

fn format_diff_value(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => serde_json::to_string(value).expect("diff value serializes"),
        None => "null".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_stripped(entries: &[SurfaceDiffEntry], width: usize) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        render_surface_diff(&mut out, Some(entries), width).expect("render surface diff");
        String::from_utf8(out.into_inner()).expect("utf-8")
    }

    fn changed_string(old: &str, new: &str) -> SurfaceDiffEntry {
        SurfaceDiffEntry {
            kind: SurfaceDiffKind::Changed,
            path: vec!["tasks".to_owned(), "[2]".to_owned(), "prompt".to_owned()],
            granted: Some(serde_json::json!(old)),
            current: Some(serde_json::json!(new)),
        }
    }

    #[test]
    fn changed_string_renders_path_and_raw_value_blocks() {
        let entry = changed_string("Repair the old prompt.", "Repair the new prompt.");
        let rendered = render_stripped(&[entry], 100);

        assert!(rendered.contains("    ~ tasks[2].prompt\n"));
        assert!(rendered.contains("      - Repair the old prompt.\n"));
        assert!(rendered.contains("      + Repair the new prompt.\n"));
    }

    #[test]
    fn value_blocks_wrap_to_width_with_a_hanging_indent() {
        let entry = changed_string(
            "Repair the scheduled repository sync and rerun every required check.",
            "Repair the scheduled repository sync, preserve intent, and rerun every required check.",
        );
        let rendered = render_stripped(&[entry], 32);

        assert!(
            rendered.lines().all(|line| line.width() <= 32),
            "rendered lines stay within the requested width:\n{rendered}"
        );
        assert!(
            rendered.lines().any(|line| line.starts_with("        ")),
            "wrapped lines carry the hanging indent:\n{rendered}"
        );

        let mut out = anstream::StripStream::new(Vec::new());
        write_block(
            &mut out,
            '-',
            render::palette::ALARM,
            &[Span::plain("first line\nsecond line".to_owned())],
            32,
        )
        .expect("render multiline block");
        let multiline = String::from_utf8(out.into_inner()).expect("utf-8");
        assert_eq!(multiline, "      - first line\n        second line\n");
    }

    #[test]
    fn word_changes_are_bold_without_emphasizing_the_shared_prefix() {
        let entry = changed_string("shared old tail", "shared new tail");
        let mut out = Vec::new();
        render_surface_diff(&mut out, Some(&[entry]), 100).expect("render styled diff");
        let rendered = String::from_utf8(out).expect("utf-8");

        assert!(rendered.contains(&render::paint(render::palette::ALARM, "shared ")));
        assert!(rendered.contains(&render::paint(render::palette::ALARM.bold(), "old")));
        assert!(rendered.contains(&render::paint(render::palette::GOOD.bold(), "new")));
        assert!(!rendered.contains(&render::paint(render::palette::ALARM.bold(), "shared ")));
    }

    #[test]
    fn added_removed_and_non_string_values_use_diff_blocks() {
        let entries = [
            SurfaceDiffEntry {
                kind: SurfaceDiffKind::Added,
                path: vec!["tasks".to_owned(), "[1]".to_owned(), "prompt".to_owned()],
                granted: None,
                current: Some(serde_json::json!("new task")),
            },
            SurfaceDiffEntry {
                kind: SurfaceDiffKind::Removed,
                path: vec!["env".to_owned(), "OLD".to_owned()],
                granted: Some(serde_json::json!({"enabled": false})),
                current: None,
            },
            SurfaceDiffEntry {
                kind: SurfaceDiffKind::Changed,
                path: vec!["hooks".to_owned(), "[0]".to_owned(), "args".to_owned()],
                granted: Some(serde_json::json!(["old", 1])),
                current: Some(serde_json::json!(["new", 2])),
            },
        ];
        let rendered = render_stripped(&entries, 100);

        assert!(rendered.contains("    + tasks[1].prompt\n      + new task\n"));
        assert!(rendered.contains("    - env.OLD\n      - {\"enabled\":false}\n"));
        assert!(rendered.contains("    ~ hooks[0].args\n      - [\"old\",1]\n"));
        assert!(rendered.contains("      + [\"new\",2]\n"));
    }

    #[test]
    fn empty_surface_diff_reports_no_field_changes() {
        assert_eq!(
            render_stripped(&[], 100),
            "  surface diff:\n    no field changes\n"
        );
    }
}
