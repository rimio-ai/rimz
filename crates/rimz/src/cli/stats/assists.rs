use super::*;

use rimz::harness::assist_log::{
    self, Assist, AssistRecord, AssistWindowReset, FocusRepairOutcome,
};
use rimz::harness::auto_redeem::RedeemReason;
use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult, PingWindowOutcome};
use rimz::ids::{AgentKind, AgentSessionId};

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct AssistStats {
    pub(super) window: String,
    pub(super) rollup: AssistRollup,
    pub(super) events: Vec<AssistEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct AssistRollup {
    pub(super) pings: usize,
    pub(super) ping_cost_usd: f64,
    pub(super) redeems: usize,
    pub(super) resets: usize,
    pub(super) resumes: usize,
    pub(super) recovered_secs: u64,
    pub(super) focus_repairs: usize,
    pub(super) focus_confirmed: usize,
    pub(super) focus_failed: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub(super) enum AssistEvent {
    Ping {
        at: Timestamp,
        kind: String,
        task: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        window: Option<PingWindowOutcome>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        transcript_path: Option<String>,
    },
    AutoRedeem {
        at: Timestamp,
        kind: String,
        reason: RedeemReason,
        request_id: String,
        credits: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        soonest_expiry: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        natural_reset: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        windows_reset: bool,
        window_resets: Vec<AssistWindowReset>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AutoContinue {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        park: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parked_since: Option<Timestamp>,
        delivered: bool,
        message_id: String,
    },
    FocusRepair {
        at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        workspace_id: rimz::ids::WorkspaceId,
        session_name: String,
        generation: u64,
        evidence: Vec<rimz::mux::ClientPaneView>,
        target: rimz::ids::PaneId,
        outcome: FocusRepairOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl AssistStats {
    pub(super) fn load(state_root: &Path, window: Window, now: Timestamp) -> Self {
        let since = window.assist_since(now);
        Self::from_records(
            window.assist_label(),
            assist_log::recent(state_root, since),
            run_log::recent(state_root, since),
        )
    }

    pub(super) fn from_records(
        window: impl Into<String>,
        records: Vec<AssistRecord>,
        runs: Vec<LoopRunRecord>,
    ) -> Self {
        let mut events = records
            .into_iter()
            .map(AssistEvent::from_record)
            .collect::<Vec<_>>();
        events.extend(
            runs.into_iter()
                .filter(|run| {
                    run.result == LoopRunResult::Completed
                        && (run.task.starts_with("autoping-") || run.window.is_some())
                })
                .map(AssistEvent::from_ping),
        );
        events.sort_by_key(AssistEvent::at);
        events.reverse();

        let mut rollup = AssistRollup::default();
        for event in &events {
            match event {
                AssistEvent::Ping { cost_usd, .. } => {
                    rollup.pings += 1;
                    rollup.ping_cost_usd += cost_usd
                        .filter(|cost| cost.is_finite() && *cost >= 0.0)
                        .unwrap_or(0.0);
                }
                AssistEvent::AutoRedeem { outcome, .. } => {
                    rollup.redeems += 1;
                    rollup.resets += usize::from(outcome.as_deref() == Some("reset"));
                }
                AssistEvent::AutoContinue {
                    at,
                    parked_since,
                    delivered,
                    ..
                } => {
                    if *delivered {
                        rollup.resumes += 1;
                        rollup.recovered_secs += recovered_secs(*parked_since, *at);
                    }
                }
                AssistEvent::FocusRepair { outcome, .. } => {
                    if *outcome == FocusRepairOutcome::AcceptedUnconfirmed {
                        rollup.focus_repairs += 1;
                    } else if *outcome == FocusRepairOutcome::Confirmed {
                        rollup.focus_confirmed += 1;
                    } else if *outcome == FocusRepairOutcome::Failed {
                        rollup.focus_failed += 1;
                    }
                }
            }
        }
        Self {
            window: window.into(),
            rollup,
            events,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl AssistEvent {
    fn from_record(record: AssistRecord) -> Self {
        match record.assist {
            Assist::AutoRedeem {
                kind,
                reason,
                request_id,
                credits,
                soonest_expiry,
                natural_reset,
                outcome,
                windows_reset,
                window_resets,
                error,
            } => Self::AutoRedeem {
                at: record.at,
                kind,
                reason,
                request_id,
                credits,
                soonest_expiry,
                natural_reset,
                outcome,
                windows_reset,
                window_resets,
                error,
            },
            Assist::AutoContinue {
                kind,
                agent_id,
                label,
                park,
                parked_since,
                delivered,
                message_id,
            } => Self::AutoContinue {
                at: record.at,
                kind,
                agent_id,
                label,
                park,
                parked_since,
                delivered,
                message_id,
            },
            Assist::FocusRepair {
                nonce,
                workspace_id,
                session_name,
                generation,
                evidence,
                target,
                outcome,
                error,
            } => Self::FocusRepair {
                at: record.at,
                nonce,
                workspace_id,
                session_name,
                generation,
                evidence,
                target,
                outcome,
                error,
            },
        }
    }

    fn from_ping(record: LoopRunRecord) -> Self {
        let kind = record
            .task
            .strip_prefix("autoping-")
            .unwrap_or(&record.task)
            .to_owned();
        Self::Ping {
            at: record.at,
            kind,
            task: record.task,
            window: record.window,
            cost_usd: record.cost_usd,
            run_id: record.run_id,
            transcript_path: record.transcript_path,
        }
    }

    fn at(&self) -> Timestamp {
        match self {
            Self::Ping { at, .. }
            | Self::AutoRedeem { at, .. }
            | Self::AutoContinue { at, .. }
            | Self::FocusRepair { at, .. } => *at,
        }
    }
}

impl Window {
    fn assist_since(self, now: Timestamp) -> Option<Timestamp> {
        let days = match self {
            Self::AllTime => return None,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        };
        Some(now - Duration::from_secs(days * DAY_SECS as u64))
    }

    fn assist_label(self) -> &'static str {
        match self {
            Self::AllTime => "all",
            Self::Week => "7d",
            Self::Month => "30d",
            Self::Year => "1y",
        }
    }
}

pub(super) fn panel_lines(
    lines: &mut Vec<String>,
    stats: &AssistStats,
    panel_width: usize,
    limit: usize,
) {
    if stats.is_empty() {
        return;
    }
    let title = format!("── assists ({}) ", stats.window);
    let rule = "─".repeat(panel_width.saturating_sub(title.chars().count() + 2));
    lines.push(format!("  {title}{rule}"));
    lines.push(format!("  {}", summary(&stats.rollup)));
    let zone = MachineConfig::load_lenient().time_zone();
    for event in stats.events.iter().take(limit) {
        lines.push(format!(
            "  {}",
            clip(&benefit_line(event, &zone), panel_width.saturating_sub(2))
        ));
    }
}

pub(super) fn render_full(stats: &AssistStats) -> Result<()> {
    let mut out = render::out();
    if stats.is_empty() {
        writeln!(out, "no assists recorded")?;
        return Ok(());
    }
    writeln!(
        out,
        "assists ({}) — {}",
        stats.window,
        summary(&stats.rollup)
    )?;
    let zone = MachineConfig::load_lenient().time_zone();
    for event in &stats.events {
        writeln!(out, "{}", forensic_line(event, &zone))?;
    }
    Ok(())
}

pub(super) fn summary(rollup: &AssistRollup) -> String {
    let mut redeem = format!("{} redeem{}", rollup.redeems, plural(rollup.redeems));
    if rollup.resets > 0 {
        redeem.push_str(&format!(
            " ({} reset{})",
            rollup.resets,
            plural(rollup.resets)
        ));
    }
    let recovered = format_hours(rollup.recovered_secs);
    let mut summary = format!(
        "{} ping{} ${:.2} · {redeem} · {} resume{} +{recovered}",
        rollup.pings,
        plural(rollup.pings),
        rollup.ping_cost_usd,
        rollup.resumes,
        plural(rollup.resumes),
    );
    if rollup.focus_repairs + rollup.focus_confirmed + rollup.focus_failed > 0 {
        summary.push_str(&format!(
            " · {} focus repair{} ({} confirmed, {} failed)",
            rollup.focus_repairs,
            plural(rollup.focus_repairs),
            rollup.focus_confirmed,
            rollup.focus_failed,
        ));
    }
    summary
}

pub(super) fn benefit_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone());
    let time = at.strftime("%H:%M");
    match event {
        AssistEvent::Ping {
            kind, at, window, ..
        } => {
            let outcome = window
                .as_ref()
                .and_then(|window| ping_window_label(window, *at, zone))
                .unwrap_or_else(|| "window refresh pending".to_owned());
            format!("{time} ⚡ {kind} ping — {outcome}")
        }
        AssistEvent::AutoRedeem {
            kind,
            reason,
            outcome,
            error,
            ..
        } => {
            let result = match (outcome.as_deref(), error.as_deref()) {
                (Some("reset"), _) => "budget reset ✓".to_owned(),
                (Some(outcome), _) => outcome.replace('_', " "),
                (None, Some(error)) => format!("failed: {}", first_line(error)),
                (None, None) => "request failed".to_owned(),
            };
            format!(
                "{time} ↻ {kind} credit — {} → {result}",
                reason_label(*reason)
            )
        }
        AssistEvent::AutoContinue {
            at,
            kind,
            label,
            park,
            parked_since,
            delivered,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let action = if *delivered { "resumed" } else { "resume held" };
            let span = parked_since.map(|parked| {
                format!(
                    ", {}→{} ({} recovered)",
                    parked.to_zoned(zone.clone()).strftime("%H:%M"),
                    at.to_zoned(zone.clone()).strftime("%H:%M"),
                    format_hours(recovered_secs(Some(parked), *at))
                )
            });
            format!(
                "{time} ▶ {agent} {action} — {}{}",
                park_label(park),
                span.unwrap_or_default()
            )
        }
        AssistEvent::FocusRepair {
            target,
            outcome,
            error,
            ..
        } => {
            let result = match (outcome, error.as_deref()) {
                (FocusRepairOutcome::AcceptedUnconfirmed, _) => {
                    "accepted, awaiting observation".to_owned()
                }
                (FocusRepairOutcome::Failed, Some(error)) => {
                    format!("failed: {}", first_line(error))
                }
                (FocusRepairOutcome::Failed, None) => "failed".to_owned(),
                (FocusRepairOutcome::Confirmed, _) => "confirmed by client observation".to_owned(),
                (FocusRepairOutcome::Superseded, _) => {
                    "superseded by newer client observation".to_owned()
                }
                (FocusRepairOutcome::Invalidated, _) => {
                    "invalidated by client/session change".to_owned()
                }
            };
            format!("{time} ⇥ focus repair {target} — {result}")
        }
    }
}

fn forensic_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M");
    let benefit = benefit_line(event, zone)
        .split_once(' ')
        .map_or_else(|| benefit_line(event, zone), |(_, rest)| rest.to_owned());
    match event {
        AssistEvent::Ping {
            task,
            cost_usd,
            run_id,
            transcript_path,
            ..
        } => format!(
            "{at} {benefit} · task {task}{}{}{}",
            cost_usd
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
                .map(|cost| format!(" · ${cost:.2}"))
                .unwrap_or_default(),
            run_id
                .as_deref()
                .map(|id| format!(" · run {id}"))
                .unwrap_or_default(),
            transcript_path
                .as_deref()
                .map(|path| format!(" · transcript {path}"))
                .unwrap_or_default(),
        ),
        AssistEvent::AutoRedeem {
            request_id,
            credits,
            soonest_expiry,
            natural_reset,
            window_resets,
            ..
        } => format!(
            "{at} {benefit} · request {request_id} · {credits} credit{}{}{}{}",
            plural(*credits as usize),
            timestamp_fact("expiry", *soonest_expiry, zone),
            timestamp_fact("natural reset", *natural_reset, zone),
            reset_facts(window_resets, zone),
        ),
        AssistEvent::AutoContinue {
            agent_id,
            message_id,
            delivered,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · delivered {delivered}"
        ),
        AssistEvent::FocusRepair {
            nonce,
            workspace_id,
            session_name,
            generation,
            evidence,
            ..
        } => format!(
            "{at} {benefit} · workspace {workspace_id} · session {session_name} · switch {generation} · clients {}{}",
            evidence.len(),
            nonce
                .as_deref()
                .map(|nonce| format!(" · nonce {nonce}"))
                .unwrap_or_default(),
        ),
    }
}

fn ping_window_label(
    window: &PingWindowOutcome,
    at: Timestamp,
    zone: &jiff::tz::TimeZone,
) -> Option<String> {
    let shortest = window.shortest.as_ref()?;
    let reset = shortest.resets_at?;
    let mut label = format!(
        "window {}→{}",
        at.to_zoned(zone.clone()).strftime("%H:%M"),
        reset.to_zoned(zone.clone()).strftime("%H:%M")
    );
    if let Some(longest) = window.longest.as_ref()
        && longest.duration_mins != shortest.duration_mins
        && let Some(long_reset) = longest.resets_at
    {
        let duration = longest
            .duration_mins
            .map(duration_label)
            .unwrap_or_else(|| "long".to_owned());
        label.push_str(&format!(
            " · {duration}→{}",
            long_reset.to_zoned(zone.clone()).strftime("%b %-d")
        ));
    }
    Some(label)
}

fn reset_facts(windows: &[AssistWindowReset], zone: &jiff::tz::TimeZone) -> String {
    let facts = windows
        .iter()
        .map(|window| {
            let duration = window
                .duration_mins
                .map(duration_label)
                .unwrap_or_else(|| "window".to_owned());
            let reset = window
                .resets_at
                .map(|reset| {
                    reset
                        .to_zoned(zone.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "unknown".to_owned());
            format!("{duration}→{reset}")
        })
        .collect::<Vec<_>>();
    if facts.is_empty() {
        String::new()
    } else {
        format!(" · windows {}", facts.join(", "))
    }
}

fn timestamp_fact(label: &str, timestamp: Option<Timestamp>, zone: &jiff::tz::TimeZone) -> String {
    timestamp
        .map(|timestamp| {
            format!(
                " · {label} {}",
                timestamp.to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M")
            )
        })
        .unwrap_or_default()
}

fn recovered_secs(parked_since: Option<Timestamp>, at: Timestamp) -> u64 {
    parked_since
        .map(|parked| at.duration_since(parked).as_secs().max(0) as u64)
        .unwrap_or(0)
}

fn format_hours(seconds: u64) -> String {
    format!("{:.1}h", seconds as f64 / 3_600.0)
}

fn duration_label(mins: u64) -> String {
    if mins.is_multiple_of(24 * 60) {
        format!("{}d", mins / (24 * 60))
    } else if mins.is_multiple_of(60) {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
    }
}

fn reason_label(reason: RedeemReason) -> &'static str {
    match reason {
        RedeemReason::ExpiryRescue => "expiry rescue",
        RedeemReason::BlockedGain => "blocked gain",
        RedeemReason::DoomedCredit => "doomed credit",
    }
}

fn park_label(park: &str) -> &str {
    if park.contains("rate_limit") {
        "limit park"
    } else if park.contains("overload") {
        "overload park"
    } else if park.contains("budget") {
        "budget park"
    } else {
        "API error park"
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn clip(text: &str, width: usize) -> String {
    let mut chars = text.chars();
    let clipped = chars.by_ref().take(width).collect::<String>();
    if chars.next().is_some() && width >= 3 {
        format!("{}...", clipped.chars().take(width - 3).collect::<String>())
    } else {
        clipped
    }
}
