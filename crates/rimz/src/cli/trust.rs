//! `rimz trust` — manage the project's executable-surface trust grant.
//!
//! Three subcommands: `status` (default), `grant`, `revoke`. Status re-hashes
//! the live `.rimz/config.toml` every call, so a drifted hash surfaces as
//! `stale` automatically — no separate sweep needed.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::io::Write;
use std::path::Path;

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

fn print_report(report: &TrustReport, as_json: bool) -> std::io::Result<()> {
    if as_json {
        let rendered = serde_json::to_string_pretty(&ReportJson {
            state: report.state.as_str(),
            workspace_id: report.workspace_id.as_str(),
            project_root: report.project_root.display().to_string(),
            config_path: report.config_path.display().to_string(),
            record_path: report.record_path.display().to_string(),
            current_hash: report.current_hash.as_deref(),
            granted_hash: report.granted_hash.as_deref(),
            granted_at: report.granted_at.map(|t| t.to_string()),
            surface_diff: report.surface_diff.as_deref(),
        })
        .expect("trust report serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
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
    render_surface_diff(&mut out, report.surface_diff.as_deref())
}

fn trust_banner(state: TrustState) -> &'static str {
    match state {
        TrustState::NoConfig => "no project config",
        TrustState::Untrusted => "untrusted",
        TrustState::Trusted => "trusted",
        TrustState::Stale => "stale (executable surface drifted since last grant)",
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
    writeln!(out, "  config: {}", report.config_path.display())?;
    render_surface_diff(&mut out, report.surface_diff.as_deref())?;
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
    out: &mut impl std::io::Write,
    entries: Option<&[SurfaceDiffEntry]>,
) -> std::io::Result<()> {
    let Some(entries) = entries else {
        return Ok(());
    };
    writeln!(out, "  surface diff:")?;
    if entries.is_empty() {
        writeln!(out, "    no field changes")
    } else {
        for entry in entries {
            match entry.kind {
                SurfaceDiffKind::Added => writeln!(
                    out,
                    "    added {} = {}",
                    format_diff_path(&entry.path),
                    format_diff_value(entry.current.as_ref())
                )?,
                SurfaceDiffKind::Removed => writeln!(
                    out,
                    "    removed {} (was {})",
                    format_diff_path(&entry.path),
                    format_diff_value(entry.granted.as_ref())
                )?,
                SurfaceDiffKind::Changed => writeln!(
                    out,
                    "    changed {}: {} -> {}",
                    format_diff_path(&entry.path),
                    format_diff_value(entry.granted.as_ref()),
                    format_diff_value(entry.current.as_ref())
                )?,
            }
        }
        Ok(())
    }
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
    value
        .map(|value| serde_json::to_string(value).expect("diff value serializes"))
        .unwrap_or_else(|| "null".to_owned())
}
