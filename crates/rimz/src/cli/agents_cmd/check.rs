//! Third-party plugin authoring check: manifest, probes, and envelope replay.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use rimz::agents::plugin::{
    PluginCheckReport, ProbeCheckStatus, ReplayCheckReport, check_from_root,
};

use crate::cli::render;

#[derive(Debug, Args)]
pub(super) struct CheckArgs {
    /// Plugin kind under agents.d to validate.
    kind: String,
    /// Transcript file passed to the declared spend probe.
    #[arg(long, value_name = "PATH")]
    spend_file: Option<PathBuf>,
    /// Canonical JSON envelopes to replay, one object per line.
    #[arg(long, value_name = "JSONL")]
    replay: Option<PathBuf>,
}

pub(super) fn run_check(args: CheckArgs) -> Result<()> {
    let report = check_from_root(
        &rimz::agents::plugin::plugins_root(),
        &args.kind,
        args.spend_file.as_deref(),
        args.replay.as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let passed = report.passed();
    render_report(&report)?;
    if !passed {
        bail!("agent plugin check failed");
    }
    Ok(())
}

fn render_report(report: &PluginCheckReport) -> Result<()> {
    let mut out = render::out();
    writeln!(out, "plugin `{}`", report.kind)?;
    writeln!(out, "manifest: valid ({})", report.manifest_path.display())?;
    writeln!(
        out,
        "coverage: {} wired, {} partial, {} unsupported",
        report.coverage.primary, report.coverage.partial, report.coverage.absent
    )?;
    writeln!(
        out,
        "lifecycle: {} native, {} derived, {} absent",
        report.lifecycle.primary, report.lifecycle.partial, report.lifecycle.absent
    )?;
    if report.probes.is_empty() {
        writeln!(out, "probes: none declared")?;
    } else {
        writeln!(out, "probes:")?;
        for probe in &report.probes {
            let availability = match (probe.present, probe.executable) {
                (true, true) => "present, executable",
                (true, false) => "present, not executable",
                (false, _) => "missing",
            };
            let (status, detail) = match &probe.status {
                ProbeCheckStatus::Passed(detail) => ("ok", detail.as_str()),
                ProbeCheckStatus::Skipped(detail) => ("skipped", detail.as_str()),
                ProbeCheckStatus::Failed(detail) => ("failed", detail.as_str()),
            };
            writeln!(
                out,
                "  {}: {status} ({availability}; {}) — {detail}",
                probe.name, probe.command
            )?;
        }
    }
    if let Some(replay) = &report.replay {
        render_replay(&mut out, replay)?;
    }
    Ok(())
}

fn render_replay(w: &mut impl Write, replay: &ReplayCheckReport) -> std::io::Result<()> {
    writeln!(w, "replay: {}", replay.path.display())?;
    let mut table = render::Table::new(["LINE", "EVENT", "SIGNAL", "STATE", "RESULT"]);
    for row in &replay.rows {
        let result = row
            .error
            .as_deref()
            .map(|error| format!("error: {error}"))
            .or_else(|| {
                row.warning
                    .as_deref()
                    .map(|warning| format!("warning: {warning}"))
            })
            .unwrap_or_else(|| "ok".into());
        table.row([
            render::cell(row.line.to_string()),
            render::cell(row.event.as_str()),
            render::cell(row.signal.as_str()),
            render::cell(row.state.as_str()),
            render::cell(result),
        ]);
    }
    table.render(w)?;
    if replay.final_states.is_empty() {
        writeln!(w, "final AgentState: none")?;
    } else {
        writeln!(w, "final AgentState:")?;
        for state in &replay.final_states {
            writeln!(
                w,
                "  {}: status={}, phase={}, compacting={}",
                state.agent_id,
                state.status.as_str(),
                phase_label(state.phase),
                state.compacting
            )?;
        }
    }
    Ok(())
}

fn phase_label(phase: rimz::agents::TurnPhase) -> &'static str {
    match phase {
        rimz::agents::TurnPhase::Idle => "idle",
        rimz::agents::TurnPhase::Reasoning => "reasoning",
        rimz::agents::TurnPhase::Acting => "acting",
        rimz::agents::TurnPhase::Parked => "parked",
    }
}
