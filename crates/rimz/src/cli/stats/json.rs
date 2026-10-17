use super::panel::*;
use super::*;

#[derive(Serialize)]
struct StatsJson<'a> {
    as_of: String,
    window: Window,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagents_since: Option<String>,
    timezone: &'a str,
    unit: &'static str,
    sessions: u32,
    active_days_28: u32,
    longest_streak: u32,
    current_streak: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    most_active_day: Option<String>,
    windows: WindowsJson,
    models: Vec<ModelJson>,
    agents: Vec<AgentJson>,
    subagents: Vec<SubagentJson>,
    days: Vec<DayJson>,
    assists: &'a assists::AssistRollup,
}

#[derive(Serialize)]
struct WindowsJson {
    today: WindowJson,
    week: WindowJson,
    month: WindowJson,
    year: WindowJson,
}

#[derive(Serialize)]
struct SubagentJson {
    profile: String,
    runs: u32,
    input: u64,
    output: u64,
    cached: u64,
    cache_hit_pct: Option<u8>,
    tokens: u64,
    usd: f64,
    share: f64,
}

fn subagents(stats: &Stats, active: Window) -> Vec<SubagentJson> {
    let total_usd = active.select(&stats.total).usd;
    let mut rows: Vec<_> = stats
        .subagents
        .iter()
        .map(|(profile, tally)| (profile, active.select(tally)))
        .filter(|(_, spend)| spend.usd > 0.0)
        .map(|(profile, spend)| SubagentJson {
            profile: profile.clone(),
            runs: spend.sessions,
            input: spend.input,
            output: spend.output,
            cached: spend.cache_read,
            cache_hit_pct: spend.cache_hit_percent(),
            tokens: spend.display_tokens(),
            usd: spend.usd,
            share: if total_usd > 0.0 {
                spend.usd / total_usd
            } else {
                0.0
            },
        })
        .collect();
    rows.sort_by(|a, b| {
        b.usd
            .total_cmp(&a.usd)
            .then_with(|| a.profile.cmp(&b.profile))
    });
    rows
}

fn stamp(at: &jiff::Zoned) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub(super) fn subagents_value(stats: &Stats, active: Window) -> serde_json::Value {
    let mut value = serde_json::json!({
        "as_of": stamp(&stats.as_of()),
        "window": active,
        "subagents": subagents(stats, active),
    });
    if let Some(since) = stats.recorded_since() {
        value["subagents_since"] = since.strftime("%Y-%m-%d").to_string().into();
    }
    value
}

pub(super) fn emit_subagents(stats: &Stats, active: Window) -> Result<()> {
    render::json_pretty(&subagents_value(stats, active))
}

/// The `--assists --json` envelope, stamped in the zone spend history uses.
pub(super) fn assists_value(
    stats: &AssistStats,
    limit: usize,
    all: bool,
    as_of: &jiff::Zoned,
) -> serde_json::Value {
    let (events, older) = stats.timeline(limit, all);
    serde_json::json!({
        "as_of": stamp(as_of),
        "rollup": stats.rollup,
        "events": events,
        "older": older,
    })
}

#[derive(Serialize)]
struct WindowJson {
    tokens: u64,
    usd: f64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    tool_calls: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tools: BTreeMap<String, u64>,
}

#[derive(Serialize)]
struct ModelJson {
    model: String,
    name: String,
    tokens: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    tool_calls: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tools: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_hit_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_per_sec: Option<u64>,
    usd: f64,
    share: f64,
}

#[derive(Serialize)]
struct AgentJson {
    kind: String,
    name: String,
    tokens: u64,
    usd: f64,
    sessions: u32,
    #[serde(skip_serializing_if = "is_zero_u64")]
    tool_calls: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tools: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_hit_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_per_sec: Option<u64>,
    share: f64,
}

#[derive(Serialize)]
struct DayJson {
    date: String,
    tokens: u64,
    usd: f64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn stats_json<'a>(
    stats: &'a Stats,
    assists: &'a AssistStats,
    today_day: i64,
    dollars: bool,
    active: Window,
) -> StatsJson<'a> {
    // These history fields keep their meaning, notably active_days_28, on every tab.
    let activity = Activity::of(&stats.by_day, today_day, Window::Year);
    let total_usd = active.select(&stats.total).usd;

    let mut models: Vec<ModelJson> = stats
        .by_model
        .iter()
        .filter(|(_, model)| active.select(&model.tally).usd > 0.0)
        .map(|(id, tally)| {
            let spend = active.select(&tally.tally);
            ModelJson {
                model: id.clone(),
                name: if id.is_empty() {
                    "Other".to_string()
                } else {
                    rimz::agents::model_display::display_model(id)
                },
                tokens: spend.display_tokens(),
                input: spend.input,
                output: spend.output,
                cache_read: spend.cache_read,
                tool_calls: spend.tool_calls,
                tools: spend.tools.clone(),
                cache_hit_pct: spend.cache_hit_percent(),
                tokens_per_sec: spend.tokens_per_sec(),
                usd: spend.usd,
                share: if total_usd > 0.0 {
                    spend.usd / total_usd
                } else {
                    0.0
                },
            }
        })
        .collect();
    models.sort_by(|a, b| {
        b.usd
            .partial_cmp(&a.usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.tokens.cmp(&a.tokens))
    });

    let agents = agent_breakdown(stats, active, None)
        .into_iter()
        .map(|agent| AgentJson {
            cache_hit_pct: agent.window.cache_hit_percent(),
            tokens_per_sec: agent.window.tokens_per_sec(),
            kind: agent.kind.to_owned(),
            name: agent.name,
            tokens: agent.window.display_tokens(),
            usd: agent.window.usd,
            sessions: agent.window.sessions,
            tool_calls: agent.window.tool_calls,
            tools: agent.window.tools,
            share: agent.share,
        })
        .collect();

    let days = stats
        .by_day
        .iter()
        .map(|(&day, spend)| DayJson {
            date: utc_date(day.max(0) as u64 * DAY_SECS as u64),
            tokens: spend.tokens,
            usd: spend.usd,
        })
        .collect();

    StatsJson {
        as_of: stamp(&stats.as_of()),
        window: active,
        subagents_since: stats
            .recorded_since()
            .map(|since| since.strftime("%Y-%m-%d").to_string()),
        timezone: &stats.timezone,
        unit: if dollars { "usd" } else { "tokens" },
        sessions: active.select(&stats.total).sessions,
        active_days_28: activity.active_count,
        longest_streak: activity.longest_streak,
        current_streak: activity.current_streak,
        most_active_day: activity
            .most_active
            .map(|day| utc_date(day.max(0) as u64 * DAY_SECS as u64)),
        windows: WindowsJson {
            today: WindowJson {
                tokens: stats.total.day.display_tokens(),
                usd: stats.total.day.usd,
                tool_calls: stats.total.day.tool_calls,
                tools: stats.total.day.tools.clone(),
            },
            week: WindowJson {
                tokens: stats.total.week.display_tokens(),
                usd: stats.total.week.usd,
                tool_calls: stats.total.week.tool_calls,
                tools: stats.total.week.tools.clone(),
            },
            month: WindowJson {
                tokens: stats.total.month.display_tokens(),
                usd: stats.total.month.usd,
                tool_calls: stats.total.month.tool_calls,
                tools: stats.total.month.tools.clone(),
            },
            year: WindowJson {
                tokens: stats.total.year.display_tokens(),
                usd: stats.total.year.usd,
                tool_calls: stats.total.year.tool_calls,
                tools: stats.total.year.tools.clone(),
            },
        },
        models,
        agents,
        subagents: subagents(stats, active),
        days,
        assists: &assists.rollup,
    }
}

pub(super) fn emit_json(
    stats: &Stats,
    assists: &AssistStats,
    today_day: i64,
    dollars: bool,
    active: Window,
) -> Result<()> {
    crate::cli::render::json_pretty(&stats_json(stats, assists, today_day, dollars, active))
}

#[cfg(test)]
pub(super) fn stats_json_value(
    stats: &Stats,
    assists: &AssistStats,
    today_day: i64,
    dollars: bool,
    active: Window,
) -> serde_json::Value {
    serde_json::to_value(stats_json(stats, assists, today_day, dollars, active))
        .expect("stats JSON is serializable")
}
