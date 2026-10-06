use super::panel::{kv, two_column};
use super::*;

use rimz::harness::assist_log::{self, Assist, AssistRecord, AssistWindowReset};
use rimz::harness::auto_redeem::RedeemReason;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::SessionDeathCause;
use rimz::store::message::AutoCompact;

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct AssistStats {
    pub(super) window: String,
    pub(super) rollup: AssistRollup,
    pub(super) events: Vec<AssistEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct AssistRollup {
    pub(super) resident_launches: usize,
    pub(super) model_aliases: usize,
    pub(super) tier_fallbacks: usize,
    pub(super) redeems: usize,
    pub(super) resets: usize,
    pub(super) resumes: usize,
    pub(super) recovered_secs: u64,
    pub(super) compacts: usize,
    pub(super) keepalives: usize,
    pub(super) idle_stops: usize,
    pub(super) launch_retries: usize,
    pub(super) restores: usize,
    pub(super) restored_sessions: usize,
    pub(super) sweeps: usize,
    pub(super) reclaimed_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub(super) enum AssistEvent {
    ResidentLaunch {
        at: Timestamp,
        task: String,
        checkout: std::path::PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<rimz::harness::schedule::when::ConditionEvidence>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        stopped: Vec<String>,
        handles: Vec<String>,
    },
    ModelAlias {
        at: Timestamp,
        kind: AgentKind,
        login: rimz::ids::LoginKey,
        alias: String,
        from: String,
        to: String,
    },
    TierFallback {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        profile: String,
        tier: rimz::config::tiers::ModelTier,
        model: String,
        skipped: Vec<rimz::agents::TierSkip>,
    },
    #[serde(rename = "auto_redeem")]
    Redeem {
        at: Timestamp,
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        login: Option<rimz::ids::LoginName>,
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
    CacheKeepalive {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        waits: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        horizon_secs: Option<u64>,
        message_id: String,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        capped: bool,
    },
    #[serde(rename = "idle_compact")]
    IdleCompact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        idle_after_secs: Option<u64>,
        occupied_tokens: u64,
        message_id: String,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    IdleStop {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        label: String,
        idle_secs: u64,
        idle_after_secs: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
        stopped: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    LaunchRetry {
        at: Timestamp,
        kind: AgentKind,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<rimz::RunId>,
        attempt: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        startup_ms: u64,
        relaunched: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    FlipCompact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        role: String,
        threshold: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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
    #[serde(rename = "auto_gc")]
    Gc {
        at: Timestamp,
        workspace_id: rimz::ids::WorkspaceId,
        scope: rimz::harness::auto_gc::GcScope,
        older_than_secs: u64,
        reclaimed_bytes: u64,
        worktrees_removed: usize,
        workspaces_pruned: usize,
        files_removed: usize,
        messages_archived: usize,
        problems: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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
                AssistEvent::ResidentLaunch { .. } => rollup.resident_launches += 1,
                AssistEvent::ModelAlias { .. } => rollup.model_aliases += 1,
                AssistEvent::TierFallback { .. } => rollup.tier_fallbacks += 1,
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
                AssistEvent::CacheKeepalive { delivered, .. } => {
                    rollup.keepalives += usize::from(*delivered)
                }
                AssistEvent::IdleStop { stopped, .. } => rollup.idle_stops += usize::from(*stopped),
                AssistEvent::LaunchRetry { relaunched, .. } => {
                    rollup.launch_retries += usize::from(*relaunched);
                }
                AssistEvent::IdleCompact { delivered, .. }
                | AssistEvent::FlipCompact { delivered, .. } => {
                    rollup.compacts += usize::from(*delivered);
                }
                AssistEvent::Resume { recovered, .. } => {
                    rollup.restores += 1;
                    rollup.restored_sessions += recovered;
                }
                AssistEvent::Gc {
                    reclaimed_bytes,
                    error,
                    ..
                } => {
                    if error.is_none() {
                        rollup.sweeps += 1;
                        rollup.reclaimed_bytes += reclaimed_bytes;
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
            Assist::ResidentLaunch {
                task,
                checkout,
                condition,
                stopped,
                handles,
            } => Self::ResidentLaunch {
                at: record.at,
                task,
                checkout,
                condition,
                stopped,
                handles,
            },
            Assist::ModelAlias {
                kind,
                login,
                alias,
                from,
                to,
            } => Self::ModelAlias {
                at: record.at,
                kind,
                login,
                alias,
                from,
                to,
            },
            Assist::TierFallback {
                kind,
                agent_id,
                label,
                profile,
                tier,
                model,
                skipped,
            } => Self::TierFallback {
                at: record.at,
                kind,
                agent_id,
                label,
                profile,
                tier,
                model,
                skipped,
            },
            Assist::AutoRedeem {
                kind,
                login,
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
                login,
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
            Assist::CacheKeepalive {
                kind,
                agent_id,
                label,
                idle_secs,
                waits,
                horizon_secs,
                message_id,
                delivered,
                error,
                capped,
            } => Self::CacheKeepalive {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                waits,
                horizon_secs,
                message_id,
                delivered,
                error,
                capped,
            },
            Assist::IdleStop {
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                requested_by,
                stopped,
                error,
            } => Self::IdleStop {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                requested_by,
                stopped,
                error,
            },
            Assist::LaunchRetry {
                kind,
                label,
                run_id,
                attempt,
                exit_code,
                startup_ms,
                relaunched,
                error,
            } => Self::LaunchRetry {
                at: record.at,
                kind,
                label,
                run_id,
                attempt,
                exit_code,
                startup_ms,
                relaunched,
                error,
            },
            Assist::IdleCompact {
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                occupied_tokens,
                message_id,
                delivered,
                error,
            } => Self::IdleCompact {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                occupied_tokens,
                message_id,
                delivered,
                error,
            },
            Assist::FlipCompact {
                kind,
                agent_id,
                role,
                threshold,
                from,
                to,
                occupied_tokens,
                message_id,
                delivered,
                error,
            } => Self::FlipCompact {
                at: record.at,
                kind,
                agent_id,
                role,
                threshold,
                from,
                to,
                occupied_tokens,
                message_id,
                delivered,
                error,
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
            Assist::AutoGc {
                workspace_id,
                scope,
                older_than_secs,
                reclaimed_bytes,
                worktrees_removed,
                workspaces_pruned,
                files_removed,
                messages_archived,
                problems,
                error,
                ..
            } => Self::Gc {
                at: record.at,
                workspace_id,
                scope,
                older_than_secs,
                reclaimed_bytes,
                worktrees_removed,
                workspaces_pruned,
                files_removed,
                messages_archived,
                problems,
                error,
            },
        }
    }

    fn at(&self) -> Timestamp {
        match self {
            Self::ResidentLaunch { at, .. }
            | Self::ModelAlias { at, .. }
            | Self::Redeem { at, .. }
            | Self::Continue { at, .. }
            | Self::Compact { at, .. }
            | Self::IdleCompact { at, .. }
            | Self::IdleStop { at, .. }
            | Self::LaunchRetry { at, .. }
            | Self::CacheKeepalive { at, .. }
            | Self::FlipCompact { at, .. }
            | Self::Resume { at, .. }
            | Self::Gc { at, .. }
            | Self::TierFallback { at, .. } => *at,
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
    let mut rows = Vec::with_capacity(5);
    if rollup.resident_launches > 0 {
        rows.push(("Resident launches:", rollup.resident_launches.to_string()));
    }
    if rollup.model_aliases > 0 {
        rows.push(("Model aliases:", rollup.model_aliases.to_string()));
    }
    if rollup.tier_fallbacks > 0 {
        rows.push(("Tier fallback:", rollup.tier_fallbacks.to_string()));
    }
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
    if rollup.keepalives > 0 {
        rows.push(("Keepalive:", rollup.keepalives.to_string()));
    }
    if rollup.idle_stops > 0 {
        rows.push(("Idle stop:", rollup.idle_stops.to_string()));
    }
    if rollup.launch_retries > 0 {
        rows.push(("Launch retry:", rollup.launch_retries.to_string()));
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
    if rollup.sweeps > 0 {
        let mut value = rollup.sweeps.to_string();
        if rollup.reclaimed_bytes > 0 {
            value.push_str(&format!(" ({})", render::fmt_bytes(rollup.reclaimed_bytes)));
        }
        rows.push(("Auto-gc:", value));
    }
    rows
}

pub(super) fn benefit_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone());
    let time = at.strftime("%H:%M");
    match event {
        AssistEvent::ResidentLaunch { task, handles, .. } => {
            format!("{time} loop {task} opened {}", handles.join(", "))
        }
        AssistEvent::ModelAlias {
            kind,
            alias,
            from,
            to,
            ..
        } => format!("{time} {kind} alias {alias} now resolves to {to} (was {from})"),
        AssistEvent::TierFallback {
            profile,
            model,
            skipped,
            ..
        } => {
            let first = skipped
                .first()
                .map(|skip| format!("{} → {model} ({})", skip.model, skip.reason))
                .unwrap_or_else(|| model.clone());
            format!("{time} {profile}: {first}")
        }
        AssistEvent::Redeem {
            kind,
            login,
            reason,
            outcome,
            error,
            ..
        } => {
            let named_login = login
                .as_ref()
                .filter(|login| !login.is_default())
                .map(|login| format!("@{login}"))
                .unwrap_or_default();
            let result = match (outcome.as_deref(), error.as_deref()) {
                (Some("reset"), _) => "budget reset ✓".to_owned(),
                (Some(outcome), _) => outcome.replace('_', " "),
                (None, Some(error)) => format!("failed: {}", first_line(error)),
                (None, None) => "request failed".to_owned(),
            };
            format!(
                "{time} ↻ {kind}{named_login} credit — {} → {result}",
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
        AssistEvent::CacheKeepalive {
            kind,
            label,
            idle_secs,
            waits,
            horizon_secs,
            error,
            capped,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let compact =
                |secs| rimz::utils::time::format_duration_compact(Duration::from_secs(secs));
            let idle = compact(*idle_secs);
            let holding = horizon_secs
                .map(|horizon| format!(", holding warm ({})", compact(horizon)))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let noun = if *waits == 1 { "wait" } else { "waits" };
            let waits = if *waits == 0 && horizon_secs.is_some() {
                String::new()
            } else {
                format!(" — {waits} {noun} pending")
            };
            let limit = if *capped { ", limit reached" } else { "" };
            format!("{time} ◷ {agent} cache keepalive after {idle}{holding}{waits}{error}{limit}")
        }
        AssistEvent::IdleStop {
            label,
            idle_secs,
            idle_after_secs,
            requested_by,
            stopped,
            error,
            ..
        } => {
            let outcome = if *stopped { "stopped" } else { "stop failed" };
            let compact =
                |secs: u64| rimz::utils::time::format_duration_compact(Duration::from_secs(secs));
            let requester = requested_by
                .as_deref()
                .map(|by| format!(" — requested by {by}"))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            format!(
                "{time} ■ {label} idle {outcome} after {} (threshold {}){requester}{error}",
                compact(*idle_secs),
                compact(*idle_after_secs),
            )
        }
        AssistEvent::LaunchRetry {
            kind,
            label,
            attempt,
            exit_code,
            startup_ms,
            relaunched,
            error,
            ..
        } => {
            let exit = exit_code.map_or_else(
                || "on a signal".to_owned(),
                |code| format!("with code {code}"),
            );
            let startup = if *startup_ms < 1000 {
                "<1s".to_owned()
            } else {
                rimz::utils::time::format_duration_compact(Duration::from_secs(startup_ms / 1000))
            };
            let outcome = if *relaunched {
                format!("relaunched, attempt {attempt}")
            } else {
                format!("relaunch failed, attempt {attempt}")
            };
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            format!(
                "{time} ↺ {label} {kind} exited {exit} after {startup} before its session opened — {outcome}{error}"
            )
        }
        AssistEvent::IdleCompact {
            kind,
            label,
            idle_secs,
            idle_after_secs,
            occupied_tokens,
            delivered,
            error,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let outcome = if *delivered {
                "compacted"
            } else {
                "compact held"
            };
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let (idle, threshold) = idle_after_secs.map_or_else(
                || (format_hours(*idle_secs), String::new()),
                |threshold| {
                    (
                        rimz::utils::time::format_duration_compact(std::time::Duration::from_secs(
                            *idle_secs,
                        )),
                        format!(
                            " (threshold {})",
                            rimz::utils::time::format_duration_compact(
                                std::time::Duration::from_secs(threshold)
                            )
                        ),
                    )
                },
            );
            format!(
                "{time} ⌁ {agent} idle {outcome} after {idle}{threshold} — {} ctx{error}",
                compact_token_count(*occupied_tokens),
            )
        }
        AssistEvent::FlipCompact {
            role,
            threshold,
            from,
            to,
            occupied_tokens,
            delivered,
            error,
            ..
        } => {
            let agent = format!("@{role}");
            let outcome = if *delivered { "" } else { " held" };
            let from = from.as_deref().unwrap_or("(none)");
            let context = occupied_tokens
                .map(|tokens| format!(" — {} ctx", compact_token_count(tokens)))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let threshold = compact_token_count(*threshold);
            format!(
                "{time} ⌁ {agent} flip compaction{outcome} — {from} → {to}{context}, threshold {threshold}{error}"
            )
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
        AssistEvent::Gc {
            scope,
            reclaimed_bytes,
            worktrees_removed,
            workspaces_pruned,
            files_removed,
            messages_archived,
            problems,
            error: None,
            ..
        } => {
            let facts = [
                (*worktrees_removed, "worktree"),
                (*workspaces_pruned, "workspace"),
                (*files_removed, "file"),
                (*messages_archived, "message"),
                (*problems, "problem"),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, noun)| format!(", {count} {noun}{}", plural(count)))
            .collect::<String>();
            let scope = match scope {
                rimz::harness::auto_gc::GcScope::Room => "",
                rimz::harness::auto_gc::GcScope::Machine => "machine ",
            };
            format!(
                "{time} ♻ {scope}gc swept — {} reclaimed{facts}",
                render::fmt_bytes(*reclaimed_bytes)
            )
        }
        AssistEvent::Gc {
            error: Some(error), ..
        } => format!("{time} ♻ gc failed — {}", first_line(error)),
    }
}

pub(super) fn forensic_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M");
    let benefit = benefit_line(event, zone)
        .split_once(' ')
        .map_or_else(|| benefit_line(event, zone), |(_, rest)| rest.to_owned());
    match event {
        AssistEvent::ResidentLaunch {
            checkout,
            condition,
            stopped,
            ..
        } => format!(
            "{at} {benefit} · checkout {}{}{}",
            checkout.display(),
            condition
                .as_ref()
                .map_or_else(String::new, |evidence| format!(" · when {}", evidence.when)),
            if stopped.is_empty() {
                String::new()
            } else {
                format!(" · took over from {}", stopped.join(", "))
            },
        ),
        AssistEvent::ModelAlias { login, .. } => format!("{at} {benefit} · login {login}"),
        AssistEvent::TierFallback { agent_id, .. } => format!("{at} {benefit} · agent {agent_id}"),
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
        }
        | AssistEvent::CacheKeepalive {
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
        AssistEvent::IdleStop {
            agent_id, stopped, ..
        } => format!("{at} {benefit} · agent {agent_id} · stopped {stopped}"),
        AssistEvent::LaunchRetry {
            run_id, relaunched, ..
        } => {
            let run = run_id
                .as_ref()
                .map(|run_id| format!(" · run {run_id}"))
                .unwrap_or_default();
            format!("{at} {benefit}{run} · relaunched {relaunched}")
        }
        AssistEvent::IdleCompact {
            agent_id,
            message_id,
            delivered,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · delivered {delivered}"
        ),
        AssistEvent::FlipCompact {
            agent_id,
            message_id,
            delivered,
            ..
        } => {
            let message = message_id
                .as_deref()
                .map(|id| format!(" · message {id}"))
                .unwrap_or_default();
            format!("{at} {benefit} · agent {agent_id}{message} · delivered {delivered}")
        }
        AssistEvent::Resume {
            workspace_id,
            session_name,
            ..
        } => format!("{at} {benefit} · workspace {workspace_id} · session {session_name}"),
        AssistEvent::Gc {
            workspace_id,
            older_than_secs,
            ..
        } => format!(
            "{at} {benefit} · workspace {workspace_id} · cutoff {}",
            rimz::utils::time::format_duration_compact(Duration::from_secs(*older_than_secs))
        ),
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
                .map(rimz::theme::fmt::duration_label)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_alias_moves_fold_and_render() {
        let record: AssistRecord = serde_json::from_value(serde_json::json!({
            "at": "2026-01-01T00:00:00Z", "assist": "model_alias", "kind": "codex",
            "login": "codex@default", "alias": "sol", "from": "gpt-6-sol", "to": "gpt-6.1-sol"
        }))
        .unwrap();
        let stats = AssistStats::from_records("all", vec![record]);
        assert!(
            category_rows(&stats.rollup)
                .iter()
                .any(|row| row.contains("Model aliases:"))
        );
        let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(line.contains("sol now resolves to gpt-6.1-sol (was gpt-6-sol)"));
    }

    #[test]
    fn resident_launch_assists_roll_up_and_render() {
        let record = serde_json::from_value::<AssistRecord>(serde_json::json!({
            "at": "2026-01-01T00:00:00Z", "assist": "resident_launch",
            "task": "fixer", "checkout": "/repo-worktrees/auth", "stopped": ["@coder", "@sweep"],
            "condition": {"when":"ci=passed", "hold":null, "held_ms":0, "readings":{"ci":"passed"}},
            "handles": ["@otter", "@fox"]
        }))
        .expect("resident launch assist wire format");
        let stats = AssistStats::from_records("all", vec![record]);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["rollup"]["resident_launches"], 1);
        assert_eq!(json["events"][0]["condition"]["readings"]["ci"], "passed");
        assert_eq!(
            json["events"][0]["stopped"],
            serde_json::json!(["@coder", "@sweep"])
        );
        assert!(
            category_rows(&stats.rollup)
                .join(" ")
                .contains("Resident launches:")
        );
        let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        for fact in [
            "fixer",
            "@otter",
            "@fox",
            "/repo-worktrees/auth",
            "took over from @coder, @sweep",
            "ci=passed",
        ] {
            assert!(line.contains(fact), "{line}");
        }
    }

    #[test]
    fn resident_launch_assist_written_before_takeover_still_loads() {
        let record = serde_json::from_value::<AssistRecord>(serde_json::json!({
            "at": "2026-01-01T00:00:00Z", "assist": "resident_launch",
            "task": "fixer", "checkout": "/repo-worktrees/auth", "stopped_team": "forge",
            "handles": ["@otter"]
        }))
        .expect("resident launch assist written with the old field");
        let stats = AssistStats::from_records("all", vec![record]);
        let json = serde_json::to_value(&stats).unwrap();
        assert!(json["events"][0].get("stopped").is_none(), "{json}");
        let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(!line.contains("took over"), "{line}");
    }

    #[test]
    fn launch_retries_count_relaunches_and_preserve_spawn_failures() {
        let records = [(false, 999), (false, 1000), (true, 17_250)]
            .into_iter()
            .map(|(relaunched, startup_ms)| {
                serde_json::from_value::<AssistRecord>(serde_json::json!({
                    "at": "2026-01-01T00:00:00Z", "assist": "launch_retry",
                    "kind": "codex", "label": "@otter",
                    "run_id": "run_0123456789abcdef0123456789abcdef",
                    "exit_code": 1, "startup_ms": startup_ms, "relaunched": relaunched,
                    "error": if relaunched { None } else { Some("codex: not found\ncaused by") }
                }))
                .expect("launch retry assist wire format")
            })
            .collect();
        let stats = AssistStats::from_records("all", records);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["rollup"]["launch_retries"], 1);
        assert_eq!(json["events"][0]["assist"], "launch_retry");
        assert_eq!(json["events"][0]["exit_code"], 1);
        assert_eq!(json["events"][0]["startup_ms"], 17_250);
        assert_eq!(
            json["events"][0]["run_id"],
            "run_0123456789abcdef0123456789abcdef"
        );
        assert_eq!(category_rows(&stats.rollup).len(), 1);
        assert!(category_rows(&stats.rollup)[0].contains("Launch retry:"));
        let lines = stats
            .events
            .iter()
            .map(|event| benefit_line(event, &jiff::tz::TimeZone::UTC))
            .collect::<Vec<_>>();
        assert!(
            lines.iter().any(|line| line.contains(
                "@otter codex exited with code 1 after 17s before its session opened — relaunched, attempt 1"
            )),
            "{lines:?}"
        );
        assert_eq!(json["events"][2]["startup_ms"], 999);
        for startup in ["<1s", "1s"] {
            let failed = format!(
                "after {startup} before its session opened — relaunch failed, attempt 1 (codex: not found)"
            );
            assert!(lines.iter().any(|line| line.contains(&failed)), "{lines:?}");
        }
        let forensic = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(
            forensic.contains("run run_0123456789abcdef0123456789abcdef"),
            "{forensic}"
        );

        let rootless = serde_json::from_value::<AssistRecord>(serde_json::json!({
            "at": "2026-01-02T00:00:00Z", "assist": "launch_retry", "kind": "codex",
            "label": "@fox", "attempt": 2, "exit_code": 1, "startup_ms": 400,
            "relaunched": true
        }))
        .expect("a launch retry without a run");
        let stats = AssistStats::from_records("all", vec![rootless]);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["rollup"]["launch_retries"], 1);
        assert_eq!(json["events"][0]["attempt"], 2);
        assert!(json["events"][0].get("run_id").is_none(), "{json}");
        let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(
            line.contains("@fox codex exited with code 1 after <1s before its session opened — relaunched, attempt 2"),
            "{line}"
        );
        let forensic = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(forensic.ends_with("relaunched true"), "{forensic}");
        assert!(!forensic.contains("run "), "{forensic}");

        let failed = serde_json::from_value::<AssistRecord>(serde_json::json!({
            "at": "2026-01-03T00:00:00Z", "assist": "launch_retry", "kind": "codex",
            "label": "@fox", "attempt": 3, "exit_code": 1, "startup_ms": 400,
            "relaunched": false, "error": "codex: not found"
        }))
        .expect("a failed launch retry without a run");
        let stats = AssistStats::from_records("all", vec![failed]);
        let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        assert!(
            line.ends_with("— relaunch failed, attempt 3 (codex: not found)"),
            "{line}"
        );
    }

    #[test]
    fn idle_stop_stats_count_stops_and_preserve_failures() {
        let records = [true, false]
            .into_iter()
            .map(|stopped| {
                serde_json::from_value::<AssistRecord>(serde_json::json!({
                    "at": "2026-01-01T00:00:00Z", "assist": "idle_stop",
                    "kind": "claude", "agent_id": "session-1", "label": "@coder",
                    "idle_secs": 200, "idle_after_secs": 180, "requested_by": "@lead",
                    "stopped": stopped,
                    "error": if stopped { None } else { Some("no bound pane") }
                }))
                .expect("idle stop assist wire format")
            })
            .collect();
        let stats = AssistStats::from_records("all", records);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["rollup"]["idle_stops"], 1);
        assert_eq!(json["events"][0]["assist"], "idle_stop");
        assert_eq!(json["events"][0]["requested_by"], "@lead");
        assert_eq!(category_rows(&stats.rollup).len(), 1);
        assert!(category_rows(&stats.rollup)[0].contains("Idle stop:"));
        let lines = stats
            .events
            .iter()
            .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            lines.contains("■ @coder idle stopped after 200s (threshold 3m) — requested by @lead"),
            "{lines}"
        );
        assert!(lines.contains("agent session-1 · stopped true"), "{lines}");
        assert!(
            lines.contains("@coder idle stop failed after 200s (threshold 3m) — requested by @lead (no bound pane)"),
            "{lines}"
        );
    }

    #[test]
    fn keepalive_stats_count_deliveries_and_preserve_failures() {
        let records = [(true, false), (false, false), (true, true)]
            .into_iter()
            .map(|(delivered, capped)| {
                let mut record = serde_json::json!({
                    "at": "2026-01-01T00:00:00Z", "assist": "cache_keepalive",
                    "kind": "claude", "agent_id": "session-1", "label": "@coder",
                    "idle_secs": 3540, "waits": 2, "message_id": "msg_1",
                    "delivered": delivered, "error": if delivered { None } else { Some("gate closed") }
                });
                if capped {
                    record["capped"] = serde_json::json!(true);
                }
                serde_json::from_value::<AssistRecord>(record).expect("keepalive assist wire format")
            })
            .collect();
        let stats = AssistStats::from_records("all", records);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(
            json["rollup"]["keepalives"], 2,
            "a capped delivery still counts"
        );
        assert_eq!(json["events"][0]["assist"], "cache_keepalive");
        let capped = json["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["capped"].clone())
            .collect::<Vec<_>>();
        assert_eq!(capped.iter().filter(|value| **value == true).count(), 1);
        assert_eq!(capped.iter().filter(|value| **value == false).count(), 2);
        assert!(
            category_rows(&stats.rollup)
                .join(" ")
                .contains("Keepalive:")
        );
        let lines = stats
            .events
            .iter()
            .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(lines.contains("@coder cache keepalive after 59m — 2 waits pending"));
        assert!(lines.contains("message msg_1 · delivered true"));
        assert!(lines.contains("gate closed"));
        assert_eq!(lines.matches("2 waits pending, limit reached").count(), 1);
    }

    #[test]
    fn keep_warm_pings_count_as_keepalives_and_name_their_horizon() {
        let record = |horizon: Option<u64>, waits: usize| {
            serde_json::from_value::<AssistRecord>(serde_json::json!({
                "at": "2026-01-01T00:00:00Z", "assist": "cache_keepalive",
                "kind": "codex", "agent_id": "session-1", "label": "@coder",
                "idle_secs": 1740, "waits": waits, "horizon_secs": horizon,
                "message_id": "msg_1", "delivered": true,
            }))
            .expect("keep-warm assist wire format")
        };
        let stats = AssistStats::from_records(
            "all",
            vec![
                record(Some(7200), 0),
                record(Some(5400), 1),
                record(None, 1),
            ],
        );
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["rollup"]["keepalives"], 3);
        // Newest first; equal stamps reverse the record order.
        assert_eq!(json["events"][2]["horizon_secs"], 7200);
        assert!(json["events"][0].get("horizon_secs").is_none());
        let lines = stats
            .events
            .iter()
            .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
            .collect::<Vec<_>>();
        assert!(
            lines[2].contains("@coder cache keepalive after 29m, holding warm (2h)")
                && !lines[2].contains("pending"),
            "{}",
            lines[2]
        );
        assert!(
            lines[1].contains("cache keepalive after 29m, holding warm (90m) — 1 wait pending"),
            "{}",
            lines[1]
        );
        assert!(lines[0].contains("cache keepalive after 29m — 1 wait pending"));
    }

    #[test]
    fn flip_compaction_fold_and_render_preserve_skipped_attempts() {
        let records = [true, false]
            .into_iter()
            .map(|delivered| AssistRecord {
                at: Timestamp::from_second(if delivered { 0 } else { 60 }).expect("timestamp"),
                assist: Assist::FlipCompact {
                    kind: AgentKind::new_unchecked("codex"),
                    agent_id: AgentSessionId::from("session-1"),
                    role: "coder".to_owned(),
                    threshold: 180_000,
                    from: delivered.then(|| "Implement".to_owned()),
                    to: "Review".to_owned(),
                    occupied_tokens: delivered.then_some(180_000),
                    message_id: delivered.then(|| "msg_1".to_owned()),
                    delivered,
                    error: (!delivered).then(|| "already compacting\nmore detail".to_owned()),
                },
            })
            .collect::<Vec<_>>();
        for record in &records {
            let json = serde_json::to_string(record).expect("serialize");
            let decoded: AssistRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&decoded, record);
        }

        let stats = AssistStats::from_records("all", records);
        assert_eq!(stats.rollup.compacts, 1);
        assert_eq!(stats.events.len(), 2);
        let json = serde_json::to_value(&stats).expect("stats JSON");
        let skipped = &json["events"][0];
        assert_eq!(skipped["assist"], "flip_compact");
        assert_eq!(skipped["role"], "coder");
        assert_eq!(skipped["threshold"], 180_000);
        assert_eq!(skipped["delivered"], false);
        for field in ["message_id", "from", "occupied_tokens", "label"] {
            assert!(skipped.get(field).is_none(), "omits absent {field}");
        }
        assert_eq!(json["events"][1]["message_id"], "msg_1");

        let lines = stats
            .events
            .iter()
            .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(lines, @"
        1970-01-01 00:01 ⌁ @coder flip compaction held — (none) → Review, threshold 180k (already compacting) · agent session-1 · delivered false
        1970-01-01 00:00 ⌁ @coder flip compaction — Implement → Review — 180k ctx, threshold 180k · agent session-1 · message msg_1 · delivered true
        ");
    }
}
