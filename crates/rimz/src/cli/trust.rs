//! `rimz trust` — manage the project's executable-surface trust grant.
//!
//! Three subcommands: `status` (default), `grant`, `revoke`. Status re-hashes
//! the live `.rimz/config.toml` every call, so a drifted hash surfaces as
//! `stale` automatically — no separate sweep needed.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::GlobalFlags;
use crate::cli::render;
use rimz::trust::{self, TrustReport, TrustState};
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
        })
        .expect("trust report serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }
    let banner = match report.state {
        TrustState::NoConfig => "no project config",
        TrustState::Untrusted => "untrusted",
        TrustState::Trusted => "trusted",
        TrustState::Stale => "stale (executable surface drifted since last grant)",
    };
    use std::io::Write;
    let mut out = render::out();
    writeln!(
        out,
        "{} {}",
        render::paint(render::palette::MUTED, "trust:"),
        render::paint(render::status::trust(report.state), banner),
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
    kv.render(&mut out)
}
