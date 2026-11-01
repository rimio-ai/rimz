use super::*;

use crate::cli::{Ctx, render};
use rimz::agents::turns::TurnRecord;
use rimz::store::message::{
    HarnessNotice, HeaderKind, parse_message_header, prompt_is_keepalive_only, split_batched_prompt,
};

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
    if json {
        if let Some(tail) = tail {
            turns = turns.split_off(turns.len().saturating_sub(tail));
        }
        return render::json_pretty(&turns);
    }

    let mut out = render::out();
    render_history(
        &mut out,
        &turns,
        &crate::cli::machine_config().time_zone(),
        jiff::Timestamp::now(),
        render::terminal_columns(120),
        tail,
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
    tail: Option<usize>,
) -> std::io::Result<()> {
    let rows = turns
        .chunk_by(|left, right| {
            left.outcome == right.outcome
                && prompt_is_keepalive_only(&left.prompt)
                && prompt_is_keepalive_only(&right.prompt)
        })
        .collect::<Vec<_>>();
    let rows = &rows[rows.len().saturating_sub(tail.unwrap_or(rows.len()))..];
    let count = rows.iter().map(|row| row.len()).sum::<usize>();
    let turns = &turns[turns.len() - count..];
    let today = now.to_zoned(time_zone.clone()).date();
    let format = if rows
        .iter()
        .all(|row| row[0].started_at.to_zoned(time_zone.clone()).date() == today)
    {
        "%H:%M"
    } else {
        "%Y-%m-%d %H:%M"
    };
    let mut table =
        render::Table::new(["START", "DUR", "FRESH", "COST", "OUTCOME", "FROM", "PROMPT"])
            .right(&[1, 2, 3])
            .max_width(max_width);
    for row in rows {
        let first = &row[0];
        let turn = &row[row.len() - 1];
        let sections = split_batched_prompt(&turn.prompt);
        let prompt = sections.first().copied().unwrap_or(&turn.prompt);
        let (mut from, prompt) = match parse_message_header(prompt.trim_start()) {
            Some((kind, sender, body)) => {
                let from = match kind {
                    HeaderKind::Agent => sender,
                    HeaderKind::User => "user".to_owned(),
                    HeaderKind::Notice(notice) => match notice {
                        HarnessNotice::SubagentReport
                        | HarnessNotice::AgentReport
                        | HarnessNotice::TeamReport
                        | HarnessNotice::Other(_) => sender,
                        HarnessNotice::CacheKeepalive => "keepalive".to_owned(),
                        HarnessNotice::Deadline => "deadline".to_owned(),
                        HarnessNotice::Wait => "wait".to_owned(),
                        HarnessNotice::Signal => "signal".to_owned(),
                        HarnessNotice::Stage => "stage".to_owned(),
                        HarnessNotice::SubagentPaused => "paused".to_owned(),
                        HarnessNotice::SubagentStalled => "stalled".to_owned(),
                        HarnessNotice::MessageUndelivered => "undelivered".to_owned(),
                        HarnessNotice::MessageQueued => "queued".to_owned(),
                    },
                };
                (from, body)
            }
            None => ("user".to_owned(), prompt.to_owned()),
        };
        let duration = row
            .iter()
            .filter_map(|turn| {
                turn.ended_at
                    .map(|ended| ended.duration_since(turn.started_at).as_secs().max(0) as u64)
            })
            .map(|duration| (duration, duration))
            .reduce(|(min, max), (next_min, next_max)| (min.min(next_min), max.max(next_max)));
        let duration = duration.map_or_else(
            || "-".to_owned(),
            |(min, max)| render::range_label(render::age_label(min), render::age_label(max)),
        );
        let tokens = turn_tokens(row);
        let cost = row
            .iter()
            .filter_map(|turn| turn.cost_usd)
            .reduce(|a, b| a + b);
        let mut prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        if row.len() > 1 {
            from = format!("{from} ×{}", row.len());
            if let Some(span) = render::folded_span(first.started_at, turn.started_at) {
                prompt = format!("{span} · {prompt}");
            }
        }
        table.row([
            render::cell(
                first
                    .started_at
                    .to_zoned(time_zone.clone())
                    .strftime(format)
                    .to_string(),
            ),
            render::cell(duration),
            render::cell(format!(
                "↘{} ↗{}",
                render::compact_count(tokens.input),
                render::compact_count(tokens.output)
            )),
            render::cell(
                cost.map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            render::cell(turn.outcome.as_str()),
            render::cell(from),
            render::cell(prompt),
        ]);
    }
    table.render(w)?;
    let tokens = turn_tokens(turns);
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

fn turn_tokens(turns: &[TurnRecord]) -> rimz::agents::spending::EffortTokens {
    turns.iter().fold(
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

        render_history(
            &mut out,
            &turns,
            &jiff::tz::TimeZone::UTC,
            started_at,
            120,
            None,
        )
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
            (
                "Type: SUBAGENT_PAUSED\nFrom: @rimz\nContent:\nchild paused",
                "paused",
                "child paused",
            ),
            (
                "Type: SUBAGENT_STALLED\nFrom: @rimz\nContent:\nchild stalled",
                "stalled",
                "child stalled",
            ),
            (
                "Type: MESSAGE_UNDELIVERED\nFrom: @rimz\nContent:\nmessage failed",
                "undelivered",
                "message failed",
            ),
            (
                "Type: MESSAGE_QUEUED\nFrom: @rimz\nContent:\nmessage pending",
                "queued",
                "message pending",
            ),
            (
                "Type: FUTURE_NOTICE\nFrom: @rimz\nContent:\nnew notice",
                "@rimz",
                "new notice",
            ),
            (
                "Type: USER_MESSAGE\nFrom: @user\nContent:\nnext task",
                "user",
                "next task",
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
                None,
            )
            .unwrap();
            let rendered = String::from_utf8(out).unwrap();
            assert_eq!(rendered.lines().count() - 2, 2, "{rendered}");
            assert!(rendered.contains("FROM"));
            for row in rendered.lines().skip(1).take(2) {
                assert_eq!(row.split_whitespace().nth(7), Some(from), "{row}");
            }
            assert!(rendered.contains(body));
            assert!(!rendered.contains("Type:"));
            assert!(!rendered.contains("@user"));
            assert!(!rendered.contains("second section"));
            assert!(!rendered.contains("cut"));
            assert!(rendered.contains("2024-01-01 00:30"));
            assert!(rendered.contains("2024-01-02 00:30"));
            assert!(rendered.contains("2 turns · 60 fresh + 140 cached tokens · $0.0000"));
        }
    }

    fn keepalive(age: &str) -> String {
        format!("Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nElapsed age: {age}")
    }

    fn turn(index: usize, prompt: String) -> TurnRecord {
        let started_at =
            jiff::Timestamp::from_second(1_704_067_200 + index as i64 * 300).expect("timestamp");
        TurnRecord {
            started_at,
            ended_at: Some(started_at + jiff::SignedDuration::from_secs(65)),
            prompt,
            fresh_input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cost_usd: Some(0.125),
            api_calls: 1,
            outcome: TurnOutcome::Done,
        }
    }

    fn render(turns: &[TurnRecord], tail: Option<usize>) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        render_history(
            &mut out,
            turns,
            &jiff::tz::TimeZone::UTC,
            jiff::Timestamp::from_second(1_704_067_200).unwrap(),
            120,
            tail,
        )
        .unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    #[test]
    fn folds_keepalives_with_different_text_and_sums_their_work() {
        let turns = [1, 65, 120]
            .into_iter()
            .enumerate()
            .map(|(index, duration)| {
                let mut turn = turn(index, keepalive(&format!("{}m", index * 5)));
                turn.ended_at = Some(turn.started_at + jiff::SignedDuration::from_secs(duration));
                turn.fresh_input = (index as u64 + 1) * 1_000;
                turn.output = (index as u64 + 1) * 10;
                turn.cost_usd = Some((index as f64 + 1.0) * 0.1);
                turn
            })
            .collect::<Vec<_>>();
        let rendered = render(&turns, None);
        assert_eq!(rendered.lines().count() - 2, 1, "{rendered}");
        let row = rendered.lines().nth(1).unwrap();
        assert!(row.starts_with("00:00"), "{rendered}");
        for expected in [
            "1s–2m",
            "↘6k ↗60",
            "$0.6000",
            "done",
            "keepalive ×3",
            "over 10m · Elapsed age: 10m",
        ] {
            assert!(row.contains(expected), "missing {expected}: {rendered}");
        }
        assert!(
            rendered.contains("3 turns · 6k fresh + 210 cached tokens · $0.6000"),
            "{rendered}"
        );
    }

    #[test]
    fn typed_boundaries_split_keepalive_runs() {
        let mut actual = Vec::new();
        for prompt in [
            "user work",
            "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nwork",
            "Type: WAIT\nFrom: @rimz\nContent:\nwoke",
        ] {
            let turns = [
                keepalive("0m"),
                keepalive("5m"),
                prompt.into(),
                keepalive("15m"),
                keepalive("20m"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, prompt)| turn(index, prompt))
            .collect::<Vec<_>>();
            let rendered = render(&turns, None);
            actual.push((
                rendered.lines().count() - 2,
                rendered.matches("keepalive ×2").count(),
            ));
        }
        let mut unknown = turn(1, keepalive("5m"));
        unknown.outcome = TurnOutcome::Unknown;
        let rendered = render(&[turn(0, keepalive("0m")), unknown], None);
        actual.push((rendered.lines().count() - 2, rendered.matches('×').count()));
        let mixed = format!(
            "{}\n\nType: WAIT\nFrom: @rimz\nContent:\nwoke",
            keepalive("5m")
        );
        let rendered = render(
            &[
                turn(0, keepalive("0m")),
                turn(1, mixed),
                turn(2, keepalive("10m")),
            ],
            None,
        );
        actual.push((rendered.lines().count() - 2, rendered.matches('×').count()));
        assert_eq!(rendered.matches("keepalive").count(), 3, "{rendered}");
        assert_eq!(actual, [(3, 2), (3, 2), (3, 2), (2, 0), (3, 0)]);
    }

    #[test]
    fn folded_duration_and_prompt_edges() {
        let first = turn(0, keepalive("0m"));
        let single = render(std::slice::from_ref(&first), None);
        let mut last = turn(1, format!("\n  {}", keepalive("5m")));
        last.ended_at = Some(last.started_at + jiff::SignedDuration::from_secs(66));
        let equal = render(&[first.clone(), last.clone()], None);
        let mut unfinished = first.clone();
        unfinished.ended_at = None;
        unfinished.cost_usd = None;
        last.ended_at = None;
        last.cost_usd = None;
        let missing = render(&[unfinished.clone(), last], None);
        let partial = render(&[first, unfinished], None);
        let burst = [0, 999].map(|millis| {
            let mut turn = turn(0, keepalive("999ms"));
            turn.started_at += jiff::SignedDuration::from_millis(millis);
            turn.ended_at = Some(turn.started_at);
            turn
        });
        let burst = render(&burst, None);
        let actual = [
            !single.contains('×') && !single.contains("over "),
            equal.contains("keepalive ×2") && equal.contains("over 5m") && !equal.contains("1m–"),
            missing.lines().nth(1).unwrap().split_whitespace().nth(1) == Some("-"),
            partial.lines().nth(1).unwrap().split_whitespace().nth(1) == Some("1m"),
            burst
                .lines()
                .nth(1)
                .unwrap()
                .ends_with("keepalive ×2  Elapsed age: 999ms"),
        ];
        assert_eq!(
            actual, [true; 5],
            "{single}\n{equal}\n{missing}\n{partial}\n{burst}"
        );
    }

    #[test]
    fn tail_counts_folded_rows_and_keeps_the_complete_run() {
        let mut turns = (0..5)
            .map(|index| turn(index, keepalive(&format!("{index}m"))))
            .collect::<Vec<_>>();
        turns.extend([turn(5, "first user".into()), turn(6, "last user".into())]);
        let actual = [0, 1, 2, 3].map(|tail| {
            let rendered = render(&turns, Some(tail));
            (
                rendered.lines().count() - 2,
                rendered.contains("keepalive ×5"),
                rendered
                    .lines()
                    .last()
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned(),
            )
        });
        assert_eq!(
            actual,
            [
                (0, false, "0".into()),
                (1, false, "1".into()),
                (2, false, "2".into()),
                (3, true, "7".into())
            ]
        );
        turns[0].started_at -= jiff::SignedDuration::from_hours(24);
        let rendered = render(&turns, Some(2));
        assert!(!rendered.contains("2024-"), "{rendered}");
    }

    #[test]
    fn documented_history_example_matches_the_renderer() {
        let at = |time: &str| {
            format!("2024-01-01T{time}:00Z")
                .parse::<jiff::Timestamp>()
                .unwrap()
        };
        let mut first = turn(0, "implement refresh-token rotation".into());
        first.started_at = at("15:12");
        first.ended_at = Some(first.started_at + jiff::SignedDuration::from_secs(480));
        first.fresh_input = 4_000;
        first.output = 510;
        first.cache_read = 16_000;
        first.cache_write = 0;
        first.cost_usd = Some(0.421);
        let mut turns = vec![first];
        for (index, time) in ["15:21", "15:26", "15:31"].into_iter().enumerate() {
            let mut ping = turn(
                index,
                "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed."
                    .into(),
            );
            ping.started_at = at(time);
            ping.ended_at =
                Some(ping.started_at + jiff::SignedDuration::from_secs(index as i64 + 1));
            ping.fresh_input = 100;
            ping.output = 10;
            ping.cache_read = 10_000;
            ping.cache_write = 0;
            ping.cost_usd = Some(0.001);
            turns.push(ping);
        }
        let mut last = turn(
            4,
            "Type: WAIT\nFrom: @rimz\nContent:\nrun the focused integration tests".into(),
        );
        last.started_at = at("15:44");
        last.ended_at = Some(last.started_at + jiff::SignedDuration::from_secs(12));
        last.fresh_input = 320;
        last.output = 0;
        last.cache_read = 2_000;
        last.cache_write = 0;
        last.cost_usd = Some(0.031);
        last.outcome = TurnOutcome::Open;
        turns.push(last);
        let rendered = render(&turns, Some(3));
        assert_eq!(rendered.lines().count() - 2, 3, "{rendered}");
        for doc in [
            include_str!("../../../../../docs/reference/cli/agents.md"),
            include_str!("../../../../../docs/guide/fleet.md"),
        ] {
            let example = doc
                .split("$ rimz agents history @coder -n 3\n")
                .nth(1)
                .unwrap()
                .split("```")
                .next()
                .unwrap();
            assert_eq!(rendered, example);
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
