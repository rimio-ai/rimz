use super::panel::*;
use super::*;

#[derive(Serialize)]
struct StatsJson<'a> {
    as_of: String,
    window: Window,
    #[serde(skip_serializing_if = "Option::is_none")]
    roles_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort_since: Option<String>,
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
    providers: Vec<ProviderJson>,
    agents: Vec<RoleJson>,
    teams: Vec<TeamJson>,
    subagents: Vec<RoleJson>,
    no_owner: NoOwnerJson,
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
struct NoOwnerJson {
    usd: f64,
    share: f64,
    subagent_usd: f64,
}

#[derive(Serialize)]
struct RoleJson {
    profile: String,
    sessions: u32,
    active_secs: u64,
    #[serde(flatten)]
    spend: SpendJson,
}

#[derive(Serialize)]
struct SpendJson {
    input: u64,
    output: u64,
    cached: u64,
    cache_hit_pct: Option<u8>,
    tokens: u64,
    usd: f64,
    share: f64,
}

fn spend_json(spend: &SpendWindow, total_usd: f64) -> SpendJson {
    SpendJson {
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
    }
}

fn role_rows(stats: &Stats, active: Window, role: RoleView) -> Vec<RoleJson> {
    let total_usd = active.select(&stats.total).usd;
    let mut rows: Vec<_> = role
        .rows(stats, active)
        .into_iter()
        .filter(|(_, row)| row.has_usage())
        .map(|(profile, row)| RoleJson {
            profile,
            sessions: row.spend.sessions,
            active_secs: row.effort.active_ms / 1000,
            spend: spend_json(&row.spend, total_usd),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.spend
            .usd
            .total_cmp(&a.spend.usd)
            .then_with(|| a.profile.cmp(&b.profile))
    });
    rows
}

#[derive(Serialize)]
struct TeamJson {
    team: String,
    sessions: u32,
    active_secs: u64,
    messages: TeamMessagesJson,
    #[serde(flatten)]
    spend: SpendJson,
    roles: Vec<TeamRoleJson>,
}

#[derive(Serialize)]
struct TeamMessagesJson {
    total: u64,
    from_user: u64,
}

#[derive(Serialize)]
struct TeamRoleJson {
    role: String,
    sessions: u32,
    active_secs: u64,
    from_user: u64,
    from_teammates: u64,
    to_teammates: u64,
    #[serde(flatten)]
    spend: SpendJson,
}

fn team_rows(stats: &Stats, active: Window) -> Vec<TeamJson> {
    let total_usd = active.select(&stats.total).usd;
    let mut rows: Vec<_> = RoleView::Teams
        .rows(stats, active)
        .into_iter()
        .filter(|(_, row)| row.has_usage())
        .map(|(team, row)| {
            let mut roles: Vec<_> = stats.teams[&team]
                .iter()
                .map(|(role, tally)| (role, active.select_role(tally)))
                .filter(|(_, row)| row.has_usage())
                .map(|(role, row)| TeamRoleJson {
                    role: role.clone(),
                    sessions: row.spend.sessions,
                    active_secs: row.effort.active_ms / 1000,
                    from_user: row.effort.from_user,
                    from_teammates: row.effort.from_teammates,
                    to_teammates: row.effort.to_teammates,
                    spend: spend_json(&row.spend, total_usd),
                })
                .collect();
            roles.sort_by(|a, b| {
                b.spend
                    .usd
                    .total_cmp(&a.spend.usd)
                    .then_with(|| a.role.cmp(&b.role))
            });
            TeamJson {
                team,
                sessions: row.spend.sessions,
                active_secs: row.effort.active_ms / 1000,
                messages: TeamMessagesJson {
                    total: row.messages(),
                    from_user: row.effort.from_user,
                },
                spend: spend_json(&row.spend, total_usd),
                roles,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.spend
            .usd
            .total_cmp(&a.spend.usd)
            .then_with(|| a.team.cmp(&b.team))
    });
    rows
}

fn stamp(at: &jiff::Zoned) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub(super) fn roles_value(stats: &Stats, active: Window, role: RoleView) -> serde_json::Value {
    let rows = if role == RoleView::Teams {
        serde_json::json!(team_rows(stats, active))
    } else {
        serde_json::json!(role_rows(stats, active, role))
    };
    let mut value = serde_json::json!({
        "as_of": stamp(&stats.as_of()),
        "window": active,
        (role.key()): rows,
    });
    if let Some(since) = stats.recorded_since() {
        value["roles_since"] = since.strftime("%Y-%m-%d").to_string().into();
    }
    if let Some(since) = stats.effort_since() {
        value["effort_since"] = since.strftime("%Y-%m-%d").to_string().into();
    }
    value
}

pub(super) fn emit_roles(stats: &Stats, active: Window, role: RoleView) -> Result<()> {
    render::json_pretty(&roles_value(stats, active, role))
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
struct ProviderJson {
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
        .filter(|(_, model)| {
            let spend = active.select(&model.tally);
            spend.usd > 0.0 || spend.tokens > 0
        })
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

    let providers = provider_breakdown(stats, active)
        .into_iter()
        .map(|provider| ProviderJson {
            cache_hit_pct: provider.window.cache_hit_percent(),
            tokens_per_sec: provider.window.tokens_per_sec(),
            kind: provider.kind.to_owned(),
            name: provider.name,
            tokens: provider.window.display_tokens(),
            usd: provider.window.usd,
            sessions: provider.window.sessions,
            tool_calls: provider.window.tool_calls,
            tools: provider.window.tools,
            share: provider.share,
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

    let no_owner = stats.no_owner(active);
    StatsJson {
        as_of: stamp(&stats.as_of()),
        window: active,
        roles_since: stats
            .recorded_since()
            .map(|since| since.strftime("%Y-%m-%d").to_string()),
        effort_since: stats
            .effort_since()
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
        providers,
        agents: role_rows(stats, active, RoleView::Agents),
        teams: team_rows(stats, active),
        subagents: role_rows(stats, active, RoleView::Subagents),
        no_owner: NoOwnerJson {
            usd: no_owner,
            share: if total_usd > 0.0 {
                no_owner / total_usd
            } else {
                0.0
            },
            subagent_usd: active.select(&stats.ownerless_subagents).usd,
        },
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
