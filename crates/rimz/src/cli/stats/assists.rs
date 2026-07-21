use super::panel::{kv, two_column};
use super::*;

use rimz::harness::assist_log::{self, Assist, AssistRecord, AssistWindowReset};
use rimz::harness::auto_redeem::RedeemReason;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::AutoCompact;
use rimz::store::event::SessionDeathCause;

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct AssistStats {
    pub(super) window: String,
    pub(super) rollup: AssistRollup,
    pub(super) events: Vec<AssistEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct AssistRollup {
    pub(super) redeems: usize,
    pub(super) resets: usize,
    pub(super) resumes: usize,
    pub(super) recovered_secs: u64,
    pub(super) compacts: usize,
    pub(super) restores: usize,
    pub(super) restored_sessions: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub(super) enum AssistEvent {
    #[serde(rename = "auto_redeem")]
    Redeem {
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
    #[serde(rename = "auto_continue")]
    Continue {
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
    #[serde(rename = "auto_compact")]
    Compact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        threshold: AutoCompact,
        #[serde(skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        message_id: String,
    },
    #[serde(rename = "auto_resume")]
    Resume {
        at: Timestamp,
        workspace_id: rimz::ids::WorkspaceId,
        session_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<SessionDeathCause>,
        recovered: usize,
        labels: Vec<String>,
    },
}

impl AssistStats {
    pub(super) fn load(state_root: &Path, window: Window, now: Timestamp) -> Self {
        let since = window.assist_since(now);
        Self::from_records(window.assist_label(), assist_log::recent(state_root, since))
    }

    pub(super) fn from_records(window: impl Into<String>, records: Vec<AssistRecord>) -> Self {
        let mut events = records
            .into_iter()
            .map(AssistEvent::from_record)
            .collect::<Vec<_>>();
        events.sort_by_key(AssistEvent::at);
        events.reverse();

        let mut rollup = AssistRollup::default();
        for event in &events {
            match event {
                AssistEvent::Redeem { outcome, .. } => {
                    rollup.redeems += 1;
                    rollup.resets += usize::from(outcome.as_deref() == Some("reset"));
                }
                AssistEvent::Continue {
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
                AssistEvent::Compact { .. } => rollup.compacts += 1,
                AssistEvent::Resume { recovered, .. } => {
                    rollup.restores += 1;
                    rollup.restored_sessions += recovered;
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
            } => Self::Redeem {
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
            } => Self::Continue {
                at: record.at,
                kind,
                agent_id,
                label,
                park,
                parked_since,
                delivered,
                message_id,
            },
            Assist::AutoCompact {
                kind,
                agent_id,
                label,
                threshold,
                occupied_tokens,
                message_id,
            } => Self::Compact {
                at: record.at,
                kind,
                agent_id,
                label,
                threshold,
                occupied_tokens,
                message_id,
            },
            Assist::AutoResume {
                workspace_id,
                session_name,
                cause,
                recovered,
                labels,
            } => Self::Resume {
                at: record.at,
                workspace_id,
                session_name,
                cause,
                recovered,
                labels,
            },
        }
    }

    fn at(&self) -> Timestamp {
        match self {
            Self::Redeem { at, .. }
            | Self::Continue { at, .. }
            | Self::Compact { at, .. }
            | Self::Resume { at, .. } => *at,
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

pub(super) fn panel_lines(lines: &mut Vec<String>, stats: &AssistStats, panel_width: usize) {
    let rows = category_rows(&stats.rollup);
    if rows.is_empty() {
        return;
    }
    lines.push(format!(
        "  {}",
        render::paint(render::palette::header(), "Assists")
    ));
    let split = rows.len().div_ceil(2);
    two_column(lines, &rows[..split], &rows[split..], panel_width);
}

pub(super) fn render_full(stats: &AssistStats) -> Result<()> {
    let mut out = render::out();
    if stats.is_empty() {
        writeln!(out, "no assists recorded")?;
        return Ok(());
    }
    let categories = category_entries(&stats.rollup)
        .into_iter()
        .map(|(label, value)| format!("{label} {value}"))
        .collect::<Vec<_>>()
        .join(" · ");
    write!(out, "assists ({})", stats.window)?;
    if !categories.is_empty() {
        write!(out, " — {categories}")?;
    }
    writeln!(out)?;
    let zone = MachineConfig::load_lenient().time_zone();
    for event in &stats.events {
        writeln!(out, "{}", forensic_line(event, &zone))?;
    }
    Ok(())
}

pub(super) fn category_rows(rollup: &AssistRollup) -> Vec<String> {
    category_entries(rollup)
        .into_iter()
        .map(|(label, value)| kv(label, &value))
        .collect()
}

fn category_entries(rollup: &AssistRollup) -> Vec<(&'static str, String)> {
    let mut rows = Vec::with_capacity(4);
    if rollup.resumes > 0 {
        let mut value = rollup.resumes.to_string();
        if rollup.recovered_secs > 0 {
            value.push_str(&format!(" (+{})", format_hours(rollup.recovered_secs)));
        }
        rows.push(("Auto-continue:", value));
    }
    if rollup.compacts > 0 {
        rows.push(("Auto-compact:", rollup.compacts.to_string()));
    }
    if rollup.redeems > 0 {
        let mut value = rollup.redeems.to_string();
        if rollup.resets > 0 {
            value.push_str(&format!(
                " ({} reset{})",
                rollup.resets,
                plural(rollup.resets)
            ));
        }
        rows.push(("Auto-redeem:", value));
    }
    if rollup.restores > 0 {
        let value = format!(
            "{} ({} agent{})",
            rollup.restores,
            rollup.restored_sessions,
            plural(rollup.restored_sessions)
        );
        rows.push(("Auto-resume:", value));
    }
    rows
}

pub(super) fn benefit_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone());
    let time = at.strftime("%H:%M");
    match event {
        AssistEvent::Redeem {
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
        AssistEvent::Continue {
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
        AssistEvent::Compact {
            kind,
            label,
            occupied_tokens,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let detail = occupied_tokens
                .map(|tokens| {
                    format!(
                        " — {} ctx cleared before delivery",
                        compact_token_count(tokens)
                    )
                })
                .unwrap_or_default();
            format!("{time} ⌁ {agent} auto-compact{detail}")
        }
        AssistEvent::Resume {
            cause,
            recovered,
            labels,
            ..
        } => {
            let cause = cause.map_or_else(String::new, |cause| format!(" after {cause}"));
            let labels = if labels.is_empty() {
                String::new()
            } else {
                format!(" ({})", labels.join(", "))
            };
            format!(
                "{time} ⟲ rebirth recovery — {recovered} agent{} restored{cause}{labels}",
                plural(*recovered)
            )
        }
    }
}

pub(super) fn forensic_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M");
    let benefit = benefit_line(event, zone)
        .split_once(' ')
        .map_or_else(|| benefit_line(event, zone), |(_, rest)| rest.to_owned());
    match event {
        AssistEvent::Redeem {
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
        AssistEvent::Continue {
            agent_id,
            message_id,
            delivered,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · delivered {delivered}"
        ),
        AssistEvent::Compact {
            agent_id,
            message_id,
            threshold,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · threshold {}",
            compact_threshold(*threshold),
        ),
        AssistEvent::Resume {
            workspace_id,
            session_name,
            ..
        } => format!("{at} {benefit} · workspace {workspace_id} · session {session_name}"),
    }
}

fn compact_token_count(tokens: u64) -> String {
    rimz::theme::fmt::compact_count(tokens)
}

fn compact_threshold(threshold: AutoCompact) -> String {
    match threshold {
        AutoCompact::Percent(percent) => format!("{percent}%"),
        AutoCompact::Tokens(tokens) => compact_token_count(tokens),
    }
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
        RedeemReason::ScheduledRedeem => "scheduled redeem",
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
