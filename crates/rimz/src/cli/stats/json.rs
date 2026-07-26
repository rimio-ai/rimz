use super::panel::*;
use super::*;

#[derive(Serialize)]
struct StatsJson<'a> {
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
    days: Vec<DayJson>,
    assists: &'a AssistStats,
}

#[derive(Serialize)]
struct WindowsJson {
    week: WindowJson,
    month: WindowJson,
    year: WindowJson,
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
    stats: &Stats,
    assists: &'a AssistStats,
    today_day: i64,
    dollars: bool,
) -> StatsJson<'a> {
    let active = Window::AllTime;
    let activity = Activity::of(&stats.by_day, today_day, active);
    let total_usd: f64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally).usd)
        .sum();

    let mut models: Vec<ModelJson> = stats
        .by_model
        .iter()
        .map(|(id, tally)| {
            let spend = active.select(tally);
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
        unit: if dollars { "usd" } else { "tokens" },
        sessions: stats.total.year.sessions,
        active_days_28: activity.active_count,
        longest_streak: activity.longest_streak,
        current_streak: activity.current_streak,
        most_active_day: activity
            .most_active
            .map(|day| utc_date(day.max(0) as u64 * DAY_SECS as u64)),
        windows: WindowsJson {
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
        days,
        assists,
    }
}

pub(super) fn emit_json(
    stats: &Stats,
    assists: &AssistStats,
    today_day: i64,
    dollars: bool,
) -> Result<()> {
    crate::cli::render::json_pretty(&stats_json(stats, assists, today_day, dollars))
}

#[cfg(test)]
pub(super) fn stats_json_value(
    stats: &Stats,
    assists: &AssistStats,
    today_day: i64,
    dollars: bool,
) -> serde_json::Value {
    serde_json::to_value(stats_json(stats, assists, today_day, dollars))
        .expect("stats JSON is serializable")
}
