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
    AgentContext, AgentCost, AgentRateLimits, AgentTokenUsage, ModReport, RateLimitWindow,
    SubagentContext, SubagentObservation, clamp_pct,
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
pub(super) const MOD_VERSION: &str = "1.1.0";

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

/// The settings entry that lists `dir` in `env.CLAUDE_CODE_PLUGIN_DIRS`.
pub(super) fn env_path(dir: &Path) -> Result<ManagedEnvPath<'_>> {
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
const EVENTS: &[&str] = &["session.start", "agent.spawn", "tool.call", "turn.complete"];

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
    let context = envelope.usage.as_ref().map_or_else(
        || AgentContext::new("claude", observed_at),
        |usage| observe_usage(usage, observed_at),
    );
    Some(ModReport {
        session_id: non_empty(envelope.session_id)?,
        mod_version: non_empty(envelope.v)?,
        provider_version: non_empty(envelope.claude_version),
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

/// How many more times an `aborted` turn end reads the transcript for the
/// interruption row, and how long it waits before each read: Claude reports
/// the turn's end 45 to 100 ms before that row reaches the file.
const ABORT_REREADS: u32 = 5;
pub(super) const ABORT_REREAD_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the resting-turn scan over the transcript the report names, at root
/// scope, and read the newest request's fast mode beside it. The scan alone
/// decides a marker. A main-loop `aborted` turn end whose read finds none
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
        if let Some(fast) = super::usage_from_transcript_tail(&tail, None, false).fast {
            report.context.fast = Some(fast);
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
mod tests {
    use super::*;
    use serde_json::json;

    fn at() -> Timestamp {
        Timestamp::from_second(1_794_946_800).unwrap()
    }

    #[test]
    fn observe_maps_the_agent_list_onto_child_fields() {
        let report = observe(
            &json!({
                "v": MOD_VERSION,
                "event": "turn.complete",
                "session_id": "sess-1",
                "transcript_path": "/t/sess-1.jsonl",
                "claude_version": "2.1.326",
                "agent_id": "aaf8677e2225c6981",
                "future_field": {"ignored": true},
                "agents": [
                    {"id": "aaf8677e2225c6981", "description": "Run printf child-ok",
                     "type": "general-purpose", "status": "completed", "kind": "local_agent",
                     "startTime": 1_794_946_718_622_i64, "model": "claude-haiku-5-5",
                     "cost": {"usd": 0.01},
                     "lastRequest": {"at": 1, "model": "claude-haiku-5-5", "effort": "low"}},
                    {"id": "b2", "type": "Explore", "effort": "high",
                     "lastRequest": {"effort": "low"}},
                    {"description": "no id"},
                    "not an object"
                ]
            }),
            at(),
        )
        .unwrap();
        assert_eq!(report.session_id, "sess-1");
        assert_eq!(report.mod_version, MOD_VERSION);
        assert_eq!(report.provider_version.as_deref(), Some("2.1.326"));
        assert_eq!(
            report.subagents,
            vec![
                SubagentObservation {
                    agent_id: "aaf8677e2225c6981".into(),
                    context: SubagentContext {
                        usage: None,
                        agent_type: Some("general-purpose".into()),
                        model: Some("claude-haiku-5-5".into()),
                        effort: Some("low".into()),
                        description: Some("Run printf child-ok".into()),
                        cost_usd: None,
                        started_at: Some(Timestamp::from_millisecond(1_794_946_718_622).unwrap()),
                        observed_at: at(),
                    },
                },
                SubagentObservation {
                    agent_id: "b2".into(),
                    context: SubagentContext {
                        usage: None,
                        agent_type: Some("Explore".into()),
                        model: None,
                        effort: Some("high".into()),
                        description: None,
                        cost_usd: None,
                        started_at: None,
                        observed_at: at(),
                    },
                },
            ],
            "the configured effort wins over the last request's"
        );
    }

    #[test]
    fn observe_keeps_a_child_whose_optional_fields_surprise() {
        let report = observe(
            &json!({
                "v": MOD_VERSION,
                "event": "turn.complete",
                "session_id": "s",
                "agents": [
                    {"id": "configured", "type": "Explore", "model": "m", "effort": 32000},
                    {"id": "requested", "type": "Explore", "model": "m",
                     "lastRequest": {"effort": 2.5}},
                    {"id": "odd", "type": "Explore", "model": "m", "effort": true,
                     "description": {"text": "x"}, "startTime": [1], "lastRequest": "x"},
                    {"id": 7, "type": "Explore"}
                ]
            }),
            at(),
        )
        .unwrap();
        let children: Vec<_> = report
            .subagents
            .iter()
            .map(|child| {
                (
                    child.agent_id.as_str(),
                    child.context.agent_type.as_deref(),
                    child.context.model.as_deref(),
                    child.context.effort.as_deref(),
                    child.context.description.as_deref(),
                    child.context.started_at,
                )
            })
            .collect();
        assert_eq!(
            children,
            [
                (
                    "configured",
                    Some("Explore"),
                    Some("m"),
                    Some("32000"),
                    None,
                    None
                ),
                (
                    "requested",
                    Some("Explore"),
                    Some("m"),
                    Some("2.5"),
                    None,
                    None
                ),
                ("odd", Some("Explore"), Some("m"), None, None, None),
            ],
            "a surprising field drops alone; only an entry without a usable id drops"
        );
    }

    #[test]
    fn observe_refuses_envelopes_not_to_act_on() {
        for payload in [
            json!(null),
            json!([]),
            json!({}),
            json!({"v": "1.0.0", "event": "session.measure", "session_id": "s"}),
            json!({"v": "1.0.0", "event": "session.start"}),
            json!({"v": "1.0.0", "event": "session.start", "session_id": ""}),
            json!({"event": "session.start", "session_id": "s"}),
            json!({"v": 1, "event": "session.start", "session_id": "s"}),
        ] {
            assert_eq!(observe(&payload, at()), None, "{payload}");
        }
        let listless = observe(
            &json!({"v": "1.0.0", "event": "session.start", "session_id": "s", "agents": "x"}),
            at(),
        );
        assert_eq!(listless, None, "a malformed list is a malformed envelope");
        let bare = observe(
            &json!({"v": "1.0.0", "event": "session.start", "session_id": "s"}),
            at(),
        )
        .unwrap();
        assert!(bare.subagents.is_empty());
        assert_eq!(bare.provider_version, None);
    }

    /// `$.session.usage()` as the live probe saw it beside a statusline push
    /// carrying the same figures.
    fn probe_usage() -> Value {
        json!({
            "startedAt": 1_794_967_641_885_i64,
            "context": {"tokens": 38_328, "window": 1_000_000, "percent": 4},
            "rateLimits": [
                {"kind": "seven_day", "percentUsed": 45, "resetsAt": "2026-11-23T06:00:00.000Z"},
                {"kind": "five_hour", "percentUsed": 12.5},
                {"kind": "spend_limit", "percentUsed": 130, "resetsAt": "2026-12-01T00:00:00Z"},
                {"kind": "future_window", "percentUsed": 1}
            ],
            "cost": {"usd": 0.004_591_13}
        })
    }

    #[test]
    fn observe_maps_the_usage_onto_the_statusline_fields() {
        let report = observe(
            &json!({"v": MOD_VERSION, "event": "tool.call", "session_id": "s",
                    "usage": probe_usage()}),
            at(),
        )
        .unwrap();
        assert!(report.readings);
        assert!(
            !report.turn_report,
            "only a transcript read is a turn report"
        );
        let tokens = report.context.tokens.unwrap();
        assert_eq!(
            (
                tokens.current_context_tokens,
                tokens.context_window_size,
                tokens.used_percentage
            ),
            (Some(38_328), Some(1_000_000), Some(4))
        );
        assert_eq!(
            report.context.cost.unwrap().total_cost_usd,
            Some(0.004_591_13)
        );
        let statusline: super::super::statusline::StatuslinePayload =
            serde_json::from_value(json!({"rate_limits": {
                "seven_day": {"used_percentage": 45.0, "resets_at": 1_795_413_600},
                "five_hour": {"used_percentage": 12.5},
                "spend_limit": {"used_percentage": 130.0, "resets_at": 1_796_083_200}
            }}))
            .unwrap();
        let mut from_statusline = statusline
            .into_context("claude", at())
            .rate_limits
            .unwrap()
            .windows;
        let mut from_mod = report.context.rate_limits.unwrap().windows;
        from_statusline.sort_by_key(RateLimitWindow::key);
        from_mod.sort_by_key(RateLimitWindow::key);
        assert_eq!(
            from_mod, from_statusline,
            "ISO `resetsAt` lands on the instant the statusline's epoch seconds name; an \
             unknown kind drops"
        );
        assert_eq!(report.context.fast, None, "fast comes from the transcript");
    }

    #[test]
    fn observe_drops_a_usage_field_of_a_surprising_shape_alone() {
        for (usage, window, tokens, windows, cost) in [
            (json!("not an object"), None, None, 0, None),
            (
                json!({"context": {"window": "big", "tokens": 5}, "cost": {"usd": 0.5}}),
                None,
                Some(5),
                0,
                Some(0.5),
            ),
            (
                json!({"context": {"window": 200_000}, "rateLimits": {"kind": "seven_day"},
                    "cost": {"usd": "free"}}),
                Some(200_000),
                None,
                0,
                None,
            ),
            (
                json!({"rateLimits": [{"kind": 7}, "x", {"kind": "seven_day", "percentUsed": "a",
                    "resetsAt": 3}], "cost": {"usd": -1.0}}),
                None,
                None,
                0,
                None,
            ),
            (
                json!({"rateLimits": [{"kind": "seven_day", "percentUsed": 3,
                    "resetsAt": "not a time"}]}),
                None,
                None,
                1,
                None,
            ),
        ] {
            let report = observe(
                &json!({"v": MOD_VERSION, "event": "turn.complete", "session_id": "s",
                        "usage": usage, "reason": 7}),
                at(),
            )
            .expect("a surprising usage still checks in");
            let reading = report.context.tokens.as_ref();
            assert_eq!(
                reading.and_then(|tokens| tokens.context_window_size),
                window,
                "{usage}"
            );
            assert_eq!(report.readings, window.is_some(), "{usage}");
            assert_eq!(
                reading.and_then(|tokens| tokens.current_context_tokens),
                tokens,
                "{usage}"
            );
            assert_eq!(
                report
                    .context
                    .rate_limits
                    .as_ref()
                    .map_or(0, |limits| limits.windows.len()),
                windows,
                "{usage}"
            );
            assert_eq!(
                report.context.cost.and_then(|cost| cost.total_cost_usd),
                cost,
                "{usage}"
            );
        }
    }

    const PROMPT: &str = r#"{"type":"user","timestamp":"2026-11-18T02:08:30.000Z","message":{"role":"user","content":"run it"}}"#;
    const INTERRUPTED: &str = r#"{"type":"user","timestamp":"2026-11-18T02:08:34.490Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
    const DIED: &str = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-11-18T02:08:35.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
    const FAST_REQUEST: &str = r#"{"type":"assistant","timestamp":"2026-11-18T02:08:31.000Z","message":{"model":"claude-opus-5-5","usage":{"input_tokens":3,"cache_read_input_tokens":20000,"output_tokens":9,"speed":"fast"}}}"#;

    /// Scan `envelope` over the tails `reads` hands out in turn, counting the
    /// reads and pauses.
    fn scan(envelope: Value, reads: &[Option<&str>]) -> (ModReport, usize, usize) {
        let mut report = observe(&envelope, at()).unwrap();
        let mut tails = reads.iter();
        let (mut read, mut paused) = (0, 0);
        scan_transcript(
            &envelope,
            &mut report,
            |path| {
                assert_eq!(path, Path::new("/t/s.jsonl"));
                read += 1;
                tails.next().copied().flatten().map(str::to_owned)
            },
            || paused += 1,
        );
        (report, read, paused)
    }

    fn envelope(event: &str, extra: Value) -> Value {
        let mut envelope = json!({"v": MOD_VERSION, "event": event, "session_id": "s",
                                  "transcript_path": "/t/s.jsonl"});
        envelope
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        envelope
    }

    #[test]
    fn scan_stores_what_the_resting_turn_scan_finds() {
        let interrupted = format!("{FAST_REQUEST}\n{INTERRUPTED}\n");
        let (report, read, paused) = scan(envelope("tool.call", json!({})), &[Some(&interrupted)]);
        assert_eq!((read, paused), (1, 0));
        assert!(report.turn_report);
        assert_eq!(
            report.context.fast,
            Some(true),
            "the newest request ran fast"
        );
        assert_eq!(
            report.context.settle,
            super::super::statusline::detect_turn_interrupted(&interrupted)
                .map(|at| TurnSettle::new(at, TurnSettleOutcome::Interrupted)),
        );
        assert!(report.context.settle.is_some());
        assert_eq!(report.context.turn_error, None);

        let died = format!("{PROMPT}\n{DIED}\n");
        let (report, _, _) = scan(
            envelope("turn.complete", json!({"reason": "answer"})),
            &[Some(&died)],
        );
        assert_eq!(
            report.context.turn_error,
            super::super::statusline::detect_turn_error(&died)
        );
        assert!(report.context.turn_error.is_some());
        assert_eq!(report.context.settle, None);

        let (report, read, _) = scan(envelope("session.start", json!({})), &[Some(&interrupted)]);
        assert_eq!(read, 1, "every report that names a transcript scans it");
        assert!(report.context.settle.is_some());
        assert!(!report.turn_report, "a session start is no turn report");
    }

    #[test]
    fn an_aborted_turn_end_rereads_until_the_interruption_lands() {
        let aborted = envelope("turn.complete", json!({"reason": "aborted"}));
        let lands = format!("{PROMPT}\n{INTERRUPTED}\n");
        let (report, read, paused) =
            scan(aborted.clone(), &[Some(PROMPT), Some(PROMPT), Some(&lands)]);
        assert_eq!((read, paused), (3, 2), "the read stops at the first marker");
        assert!(report.context.settle.is_some());

        let (report, read, paused) = scan(aborted.clone(), &[Some(PROMPT); 9]);
        assert_eq!(
            (read, paused),
            (1 + ABORT_REREADS as usize, ABORT_REREADS as usize),
            "bounded: {ABORT_REREADS} more reads"
        );
        assert_eq!(
            report.context.settle, None,
            "the reason itself is no marker"
        );
        assert_eq!(report.context.turn_error, None);
        assert!(report.turn_report);

        let (_, read, _) = scan(
            envelope(
                "turn.complete",
                json!({"reason": "aborted", "agent_id": "child"}),
            ),
            &[Some(PROMPT); 9],
        );
        assert_eq!(read, 1, "a child's turn end never decides the root's");
        let (_, read, _) = scan(
            envelope("turn.complete", json!({"reason": "answer"})),
            &[Some(PROMPT); 9],
        );
        assert_eq!(read, 1);
    }

    #[test]
    fn an_unreadable_transcript_keeps_the_readings_and_sends_no_turn_report() {
        let (report, read, paused) = scan(
            envelope(
                "turn.complete",
                json!({"reason": "aborted", "usage": probe_usage()}),
            ),
            &[None],
        );
        assert_eq!((read, paused), (1, 0));
        assert!(report.readings && !report.turn_report);
        assert!(report.context.tokens.is_some());
        let mut pathless = envelope("tool.call", json!({}));
        pathless.as_object_mut().unwrap().remove("transcript_path");
        let (report, read, _) = scan(pathless, &[Some(INTERRUPTED)]);
        assert_eq!(read, 0);
        assert!(!report.turn_report);
    }

    #[test]
    fn manifest_and_module_carry_the_mod_version() {
        let manifest: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
        assert_eq!(manifest["version"], MOD_VERSION);
        assert_eq!(manifest["name"], "rimz");
        let hooks: serde_json::Value = serde_json::from_str(FILES[1].1).unwrap();
        assert_eq!(hooks["modules"], serde_json::json!(["./rimz.ts"]));
        assert!(
            FILES[2]
                .1
                .contains(&format!("const MOD_VERSION = '{MOD_VERSION}'")),
            "the hooks module reports the version the manifest names"
        );
    }

    #[test]
    fn write_touches_only_files_that_differ() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("mods/claude");
        assert_eq!(
            outdated(&dir).len(),
            FILES.len(),
            "a missing folder is drift"
        );
        let first = write(&dir).unwrap();
        assert_eq!(first.len(), FILES.len());
        assert!(first.iter().all(|file| !file.existed));
        assert!(outdated(&dir).is_empty());
        assert!(
            write(&dir).unwrap().is_empty(),
            "a reinstall writes nothing"
        );

        let module = dir.join("hooks/rimz.ts");
        std::fs::write(&module, "stale").unwrap();
        std::fs::create_dir_all(dir.join(".claude-plugin/types")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/types/index.d.ts"),
            "laid by Claude",
        )
        .unwrap();
        assert_eq!(outdated(&dir), vec![module.clone()]);
        let refreshed = write(&dir).unwrap();
        assert_eq!(
            refreshed,
            vec![HookInstallFileReport {
                path: module.clone(),
                existed: true
            }]
        );
        assert!(dir.join(".claude-plugin/types/index.d.ts").is_file());

        std::fs::remove_file(&module).unwrap();
        std::fs::create_dir(&module).unwrap();
        assert_eq!(
            outdated(&dir),
            vec![module],
            "a mod file that cannot be read is stale"
        );
    }
}
