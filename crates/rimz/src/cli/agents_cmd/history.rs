use super::*;

use crate::cli::{Ctx, render};
use rimz::agents::turns::TurnRecord;
use rimz::store::message::{HeaderKind, parse_message_header, split_batched_prompt};

pub(super) fn history_agent(
    reference: String,
    tail: Option<usize>,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (agent, _) = resolve_live_or_audit(
        &ctx.store,
        &ctx.workspace,
        ctx.runtime(),
        &snapshot,
        &reference,
        &ctx.address_context(),
    )?;
    let transcript = agent
        .transcript_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| anyhow::anyhow!("agent has no recorded transcript"))?;
    let path = Path::new(transcript);
    let adapter = rimz::agents::find_definition(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let prices = rimz::agents::pricing::cached_book(&ctx.runtime().shared_pricing_cache_path());
    let messages = adapter
        .read_transcript_messages(path, Some(&agent.agent_id))
        .with_context(|| format!("reading agent transcript `{}`", path.display()))?;
    let spend = adapter.parse_spend(path, None, &prices);
    let mut turns = rimz::agents::turns::session_turns(
        &messages,
        &spend.entries,
        agent.agent_id.as_str(),
        session_open(&agent),
    );
    if let Some(tail) = tail {
        turns = turns.split_off(turns.len().saturating_sub(tail));
    }
    if json {
        return render::json_pretty(&turns);
    }

    let mut out = render::out();
    render_history(
        &mut out,
        &turns,
        &crate::cli::machine_config().time_zone(),
        jiff::Timestamp::now(),
        render::terminal_columns(120),
    )?;
    Ok(())
}

fn session_open(agent: &AgentState) -> bool {
    agent.ended_at.is_none()
        && rimz::store::runtime::agent_liveness(agent) != rimz::store::runtime::AgentLiveness::Dead
        && matches!(
            agent.status,
            rimz::agents::AgentStatus::Running
                | rimz::agents::AgentStatus::Waiting
                | rimz::agents::AgentStatus::Paused
        )
}

fn render_history(
    w: &mut impl Write,
    turns: &[TurnRecord],
    time_zone: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
    max_width: usize,
) -> std::io::Result<()> {
    let today = now.to_zoned(time_zone.clone()).date();
    let format = if turns
        .iter()
        .all(|turn| turn.started_at.to_zoned(time_zone.clone()).date() == today)
    {
        "%H:%M"
    } else {
        "%Y-%m-%d %H:%M"
    };
    let mut table =
        render::Table::new(["START", "DUR", "FRESH", "COST", "OUTCOME", "FROM", "PROMPT"])
            .right(&[1, 2, 3])
            .max_width(max_width);
    for turn in turns {
        let sections = split_batched_prompt(&turn.prompt);
        let prompt = sections.first().copied().unwrap_or(&turn.prompt);
        let (from, prompt) = match parse_message_header(prompt) {
            Some((kind, sender, body)) => {
                let from = match kind {
                    HeaderKind::Agent | HeaderKind::Subagent | HeaderKind::User => sender,
                    HeaderKind::CacheKeepalive => "keepalive".to_owned(),
                    HeaderKind::Deadline => "deadline".to_owned(),
                    HeaderKind::Wait => "wait".to_owned(),
                    HeaderKind::Signal => "signal".to_owned(),
                    HeaderKind::Stage => "stage".to_owned(),
                };
                (from, body)
            }
            None => ("user".to_owned(), prompt.to_owned()),
        };
        let duration =
            turn.ended_at.map_or_else(
                || "-".to_owned(),
                |ended| {
                    render::age_label(ended.duration_since(turn.started_at).as_secs().max(0) as u64)
                },
            );
        table.row([
            render::cell(
                turn.started_at
                    .to_zoned(time_zone.clone())
                    .strftime(format)
                    .to_string(),
            ),
            render::cell(duration),
            render::cell(format!(
                "↘{} ↗{}",
                render::compact_count(turn.fresh_input),
                render::compact_count(turn.output)
            )),
            render::cell(
                turn.cost_usd
                    .map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(turn.outcome.as_str()),
            render::cell(from),
            render::cell(prompt.split_whitespace().collect::<Vec<_>>().join(" ")),
        ]);
    }
    table.render(w)?;
    let tokens = turns.iter().fold(
        rimz::agents::spending::EffortTokens::default(),
        |mut total, turn| {
            total.add_assign(rimz::agents::spending::EffortTokens {
                input: turn.fresh_input,
                output: turn.output,
                cache_read: turn.cache_read,
                cache_write: turn.cache_write,
            });
            total
        },
    );
    let cost = turns
        .iter()
        .filter_map(|turn| turn.cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.0)
        .fold(0.0, |total, cost| total + cost);
    writeln!(
        w,
        "{} turn{} · {} fresh + {} cached tokens · ${cost:.4}",
        turns.len(),
        if turns.len() == 1 { "" } else { "s" },
        render::compact_count(tokens.input.saturating_add(tokens.output)),
        render::compact_count(tokens.cache_read.saturating_add(tokens.cache_write))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::turns::TurnOutcome;

    #[test]
    fn renders_turn_rows_and_footer() {
        let started_at = jiff::Timestamp::from_second(1_704_067_200).expect("timestamp");
        let turns = [TurnRecord {
            started_at,
            ended_at: Some(started_at + jiff::SignedDuration::from_secs(65)),
            prompt: "fix\nlogin".to_owned(),
            fresh_input: 1_200,
            output: 800,
            cache_read: 2_000,
            cache_write: 0,
            cost_usd: Some(0.125),
            api_calls: 1,
            outcome: TurnOutcome::Done,
        }];
        let mut out = Vec::new();

        render_history(&mut out, &turns, &jiff::tz::TimeZone::UTC, started_at, 120)
            .expect("render");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(!rendered.contains("2024-01-01"));
        assert!(rendered.contains("1m"));
        assert!(rendered.contains("↘1k ↗800"));
        assert!(rendered.contains("done"));
        assert!(rendered.contains("fix login"));
        assert!(rendered.contains("1 turn · 2k fresh + 2k cached tokens · $0.1250"));
    }

    #[test]
    fn renders_envelopes_unknown_outcomes_and_mixed_dates() {
        let now: jiff::Timestamp = "2024-01-02T00:30:00Z".parse().unwrap();
        let prompts = [
            (
                "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nfix\nlogin\n\nType: WAIT\nFrom: @rimz\nContent:\nsecond section",
                "@planner",
                "fix login",
            ),
            (
                "Type: WAIT\nFrom: @rimz\nContent:\ntests finished",
                "wait",
                "tests finished",
            ),
            ("plain prompt", "user", "plain prompt"),
        ];
        for (prompt, from, body) in prompts {
            let first = TurnRecord {
                started_at: now - jiff::SignedDuration::from_hours(24),
                ended_at: None,
                prompt: prompt.to_owned(),
                fresh_input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
                cost_usd: None,
                api_calls: 1,
                outcome: TurnOutcome::Unknown,
            };
            let mut second = first.clone();
            second.started_at = now;
            let mut out = Vec::new();
            render_history(
                &mut out,
                &[first, second],
                &jiff::tz::TimeZone::UTC,
                now,
                240,
            )
            .unwrap();
            let rendered = String::from_utf8(out).unwrap();
            assert!(rendered.contains("FROM"));
            assert!(rendered.contains(from));
            assert!(rendered.contains(body));
            assert!(!rendered.contains("Type:"));
            assert!(!rendered.contains("second section"));
            assert!(!rendered.contains("cut"));
            assert!(rendered.contains("2024-01-01 00:30"));
            assert!(rendered.contains("2024-01-02 00:30"));
            assert!(rendered.contains("2 turns · 60 fresh + 140 cached tokens · $0.0000"));
        }
    }

    #[test]
    fn open_session_uses_status_and_end_record() {
        let mut agent = AgentState::stub("codex", "session", rimz::agents::AgentStatus::Idle);
        for status in [
            rimz::agents::AgentStatus::Running,
            rimz::agents::AgentStatus::Waiting,
            rimz::agents::AgentStatus::Paused,
        ] {
            agent.status = status;
            assert!(session_open(&agent));
        }
        agent.runtime_owner = Some(rimz::RuntimeOwner::new(
            rimz::RuntimeOwnerKind::Agent,
            "session",
            u32::MAX,
            None,
        ));
        assert!(!session_open(&agent), "a dead owner never ended cleanly");
        agent.runtime_owner = None;
        agent.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
        assert!(!session_open(&agent));
        agent.ended_at = None;
        agent.status = rimz::agents::AgentStatus::Idle;
        assert!(!session_open(&agent));
    }
}
