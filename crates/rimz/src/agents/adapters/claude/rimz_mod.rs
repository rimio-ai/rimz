//! RimZ's Claude Code mod: the embedded plugin folder hook install writes under
//! the RimZ home, and the settings entry that loads it in every session.
//!
//! The folder's content is stable across RimZ releases and carries its own
//! [`MOD_VERSION`]: every rewrite of an installed file prints a reload line in
//! each open session of the account, so install writes only a file whose bytes
//! differ. Claude also lays its own `.claude-plugin/types/` beside the files at
//! load; only the files listed here are RimZ's, so only they count as drift.
//!
//! The mod reports through `rimz mod feed --source claude` with one envelope
//! on stdin, `$.agent.list()` inside it as Claude returned it; [`observe`]
//! maps that list onto the child sidecar fields here, so the mod stays a thin
//! forwarder. An unknown field or event is a newer mod's and is ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::statusline;
use super::subagent_statusline::value_as_timestamp;
use crate::agents::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, ModReport,
    RateLimitWindow, SubagentContext, SubagentObservation, clamp_pct,
};
use crate::agents::managed_env_list::ManagedEnvPath;
use crate::agents::{AgentErr, HookInstallFilePreview, HookInstallFileReport, Result};
use crate::agents::{TurnSettle, TurnSettleOutcome};

/// The mod's own version, bumped by hand whenever a file below changes.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only the unit test reads it, pinning the manifest and module to it"
    )
)]
pub(super) const MOD_VERSION: &str = "1.2.0";

/// The settings `env` variable Claude reads its plugin folders from.
pub(super) const PLUGIN_DIRS_KEY: &str = "CLAUDE_CODE_PLUGIN_DIRS";

const FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("rimz_mod/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("rimz_mod/hooks.json")),
    ("hooks/rimz.ts", include_str!("rimz_mod/rimz.ts")),
];

/// The mod folder for this login, shared by every Claude account.
pub(super) fn dir(login_env: &BTreeMap<String, String>) -> PathBuf {
    crate::disk::paths::claude_mod_dir_in(login_env)
}

/// The settings entry that lists `dir` in `env.CLAUDE_CODE_PLUGIN_DIRS`, or,
/// when not `wanted`, that install removes from it.
pub(super) fn env_path(dir: &Path, wanted: bool) -> Result<ManagedEnvPath<'_>> {
    let path = dir.to_str().ok_or_else(|| AgentErr::Install {
        agent: "claude",
        reason: format!(
            "the RimZ mod folder {} is not a UTF-8 path; Claude cannot list it",
            dir.display()
        ),
    })?;
    Ok(ManagedEnvPath {
        key: PLUGIN_DIRS_KEY,
        path,
        wanted,
    })
}

/// Each mod file under `dir` whose content differs from the embedded one, with
/// what is there now (`None` when missing).
pub(super) fn previews(dir: &Path) -> Result<Vec<HookInstallFilePreview>> {
    let mut previews = Vec::new();
    for (relative, content) in FILES {
        let path = dir.join(relative);
        let original = crate::agents::read_optional_file("claude", &path)?;
        if original.as_deref() == Some(*content) {
            continue;
        }
        previews.push(HookInstallFilePreview {
            existed: original.is_some(),
            original,
            candidate: (*content).to_owned(),
            path,
        });
    }
    Ok(previews)
}

/// Write each mod file whose content differs, reporting the files written.
pub(super) fn write(dir: &Path) -> Result<Vec<HookInstallFileReport>> {
    let mut written = Vec::new();
    for preview in previews(dir)? {
        crate::agents::provider_file::write_bytes(&preview.path, preview.candidate.as_bytes())?;
        written.push(HookInstallFileReport {
            path: preview.path,
            existed: preview.existed,
        });
    }
    Ok(written)
}

/// Mod files missing, unreadable, or different from the embedded ones. Reads
/// only.
pub(super) fn outdated(dir: &Path) -> Vec<PathBuf> {
    FILES
        .iter()
        .map(|(relative, expected)| (dir.join(relative), *expected))
        .filter(|(path, expected)| {
            std::fs::read(path).map_or(true, |content| content != expected.as_bytes())
        })
        .map(|(path, _)| path)
        .collect()
}

/// The events the mod reports at.
const EVENTS: &[&str] = &[
    "session.start",
    "agent.spawn",
    "tool.call",
    "turn.complete",
    "turn.step",
];

/// The envelope the mod writes. Every field but `v`, `event` and
/// `session_id` may be absent.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Envelope {
    v: Option<String>,
    event: Option<String>,
    session_id: Option<String>,
    transcript_path: Option<String>,
    claude_version: Option<String>,
    agent_id: Option<String>,
    agents: Option<Vec<Value>>,
    /// `$.session.usage()` as Claude answered it, read field by field.
    usage: Option<Value>,
    /// `turn.complete`'s reason (`answer`, `aborted`, ...), kept as Claude
    /// sent it so a reason of another shape drops alone.
    reason: Option<Value>,
    /// A main-loop `turn.step`'s `result.usage`: the request's token split and
    /// the model that answered it, by the id the API reports.
    step: Option<Value>,
}

/// One mod report, or `None` for an envelope not to act on. An agent entry
/// without a usable id drops alone, and so does a usage field of a shape RimZ
/// does not expect. The transcript is read by [`scan_transcript`].
pub(super) fn observe(payload: &Value, observed_at: Timestamp) -> Option<ModReport> {
    let envelope = Envelope::deserialize(payload).ok()?;
    let event = envelope.event?;
    if !EVENTS.contains(&event.as_str()) {
        return None;
    }
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let subagents = envelope
        .agents
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| observe_agent(entry, observed_at))
        .collect();
    let mut context = envelope.usage.as_ref().map_or_else(
        || AgentContext::new("claude", observed_at),
        |usage| observe_usage(usage, observed_at),
    );
    let provider_version = non_empty(envelope.claude_version);
    context.agent_version.clone_from(&provider_version);
    if let Some(step) = envelope.step.filter(|_| envelope.agent_id.is_none()) {
        observe_step(&step, &mut context);
    }
    Some(ModReport {
        session_id: non_empty(envelope.session_id)?,
        mod_version: non_empty(envelope.v)?,
        provider_version,
        subagents,
        readings: context
            .tokens
            .as_ref()
            .is_some_and(|tokens| tokens.context_window_size.is_some()),
        turn_report: false,
        context,
    })
}

/// `$.session.usage()` (`{context: {tokens?, window, percent?}, rateLimits:
/// [{kind, percentUsed, resetsAt?}], cost: {usd}}`) as the session's sparse
/// reading: the same engine figures the statusline carries as
/// `total_input_tokens`, `context_window_size`, `used_percentage`, its rate
/// windows and `total_cost_usd`. `resetsAt` is ISO text where the statusline
/// sends epoch seconds.
fn observe_usage(usage: &Value, observed_at: Timestamp) -> AgentContext {
    let context = usage.get("context");
    let number = |field: &str| context?.get(field)?.as_u64();
    let tokens = AgentTokenUsage {
        context_window_size: number("window").filter(|window| *window > 0),
        used_percentage: clamp_pct(context.and_then(|c| c.get("percent")?.as_f64())),
        current_context_tokens: number("tokens"),
        ..AgentTokenUsage::default()
    };
    let windows: Vec<RateLimitWindow> = usage
        .get("rateLimits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let resets_at = window
                .get("resetsAt")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok());
            statusline::rate_window(
                window.get("kind")?.as_str()?,
                window.get("percentUsed").and_then(Value::as_f64),
                resets_at,
            )
        })
        .collect();
    let cost = usage
        .get("cost")
        .and_then(|cost| cost.get("usd")?.as_f64())
        .filter(|usd| usd.is_finite() && *usd >= 0.0)
        .map(|usd| AgentCost {
            total_cost_usd: Some(usd),
            ..AgentCost::default()
        });
    AgentContext {
        tokens: (tokens != AgentTokenUsage::default()).then_some(tokens),
        rate_limits: (!windows.is_empty())
            .then(|| AgentRateLimits { windows }.stamped_at(observed_at)),
        cost,
        ..AgentContext::new("claude", observed_at)
    }
}

/// A main-loop step's `result.usage` onto `context`: the model that answered,
/// and the split with the current-window count as its input side, so the count
/// and the split always describe one response. A split needs all four counts
/// and one above zero; a field of another shape drops its reading alone.
fn observe_step(step: &Value, context: &mut AgentContext) {
    context.model_id = step
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_owned);
    let count = |field: &str| step.get(field)?.as_u64();
    let (Some(input), Some(output), Some(cache_creation), Some(cache_read)) = (
        count("input_tokens"),
        count("output_tokens"),
        count("cache_creation_input_tokens"),
        count("cache_read_input_tokens"),
    ) else {
        return;
    };
    let split = AgentCurrentUsage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_creation_input_tokens: Some(cache_creation),
        cache_read_input_tokens: Some(cache_read),
    };
    if split.is_zero() {
        return;
    }
    let tokens = context.tokens.get_or_insert_with(AgentTokenUsage::default);
    tokens.current_context_tokens = Some(input + cache_creation + cache_read);
    tokens.current_usage = Some(split);
}

/// How many more times an `aborted` turn end reads the transcript for the
/// interruption row, and how long it waits before each read: Claude reports
/// the turn's end 45 to 100 ms before that row reaches the file.
const ABORT_REREADS: u32 = 5;
pub(super) const ABORT_REREAD_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the resting-turn scan over the transcript the report names, at root
/// scope, and read the newest request's fast mode beside it. A report that is
/// no `turn.step` takes that request's model when it carries none, and its
/// split only when the split's input side is the report's own count, so a
/// lagging or pre-compaction tail never brings back a retired window. The scan
/// alone decides a marker. A main-loop `aborted` turn end whose read finds none
/// reads again after each `pause`, up to [`ABORT_REREADS`] times, and stops at
/// the first marker; the reason itself never becomes one.
pub(super) fn scan_transcript(
    payload: &Value,
    report: &mut ModReport,
    mut read_tail: impl FnMut(&Path) -> Option<String>,
    mut pause: impl FnMut(),
) {
    let Ok(envelope) = Envelope::deserialize(payload) else {
        return;
    };
    let Some(path) = envelope.transcript_path.filter(|path| !path.is_empty()) else {
        return;
    };
    let aborted = envelope.reason.as_ref().and_then(Value::as_str) == Some("aborted")
        && envelope.agent_id.is_none();
    let rereads = if aborted { ABORT_REREADS } else { 0 };
    for attempt in 0..=rereads {
        if attempt > 0 {
            pause();
        }
        let Some(tail) = read_tail(Path::new(&path)) else {
            return;
        };
        report.turn_report |= matches!(
            envelope.event.as_deref(),
            Some("tool.call" | "turn.complete")
        );
        let usage = super::usage_from_transcript_tail(&tail, None, false);
        if let Some(fast) = usage.fast {
            report.context.fast = Some(fast);
        }
        if envelope.event.as_deref() != Some("turn.step") {
            fall_back_to_the_tail(&mut report.context, usage);
        }
        match statusline::detect_resting_turn(&tail) {
            Some(statusline::RestingTurnOutcome::Interrupted(at)) => {
                report.context.settle = Some(TurnSettle::new(at, TurnSettleOutcome::Interrupted));
                return;
            }
            Some(statusline::RestingTurnOutcome::Died(error)) => {
                report.context.turn_error = Some(error);
                return;
            }
            None => {}
        }
    }
}

/// The model and the correlated split of the tail's newest request, for a
/// report whose own reading lacks them.
fn fall_back_to_the_tail(context: &mut AgentContext, usage: super::TranscriptUsage) {
    if context.model_id.is_none() {
        context.model_id = usage.model;
    }
    let Some(tokens) = context.tokens.as_mut() else {
        return;
    };
    let input_side = [
        usage.fresh_input_tokens,
        usage.cache_read_input_tokens,
        usage.cache_write_input_tokens,
    ]
    .into_iter()
    .sum::<Option<u64>>();
    if tokens.current_usage.is_some()
        || input_side.is_none()
        || input_side != tokens.current_context_tokens
    {
        return;
    }
    tokens.current_usage = Some(AgentCurrentUsage {
        input_tokens: usage.fresh_input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_write_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
    });
}

/// One `$.agent.list()` entry (Claude's `AgentInfo`), read field by field: a
/// field of a shape RimZ does not expect drops alone, and only an entry
/// without a usable `id` drops whole.
fn observe_agent(entry: &Value, observed_at: Timestamp) -> Option<SubagentObservation> {
    let text = |field: &str| {
        entry
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    // Claude types an effort as a level name or a number.
    let effort = |holder: Option<&Value>| match holder?.get("effort")? {
        Value::String(level) if !level.is_empty() => Some(level.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    Some(SubagentObservation {
        agent_id: text("id")?,
        context: SubagentContext {
            usage: None,
            agent_type: text("type"),
            model: text("model"),
            // The configured effort, absent when the child runs on the
            // session's, wins over the one its newest request carried.
            effort: effort(Some(entry)).or_else(|| effort(entry.get("lastRequest"))),
            description: text("description"),
            cost_usd: None,
            started_at: entry.get("startTime").and_then(value_as_timestamp),
            observed_at,
        },
    })
}

#[cfg(test)]
mod tests;
