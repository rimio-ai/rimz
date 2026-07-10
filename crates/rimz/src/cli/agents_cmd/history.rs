use super::*;

use crate::cli::render;
use rimz::agents::turns::TurnRecord;

pub(super) fn history_agent(
    reference: String,
    tail: Option<usize>,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let snapshot = crate::cli::alive_snapshot(&store, &runtime, &workspace.session_name)?;
    let live_result = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    );
    let (agent, resolved_live) = match live_result {
        Ok(agent) => (agent.clone(), true),
        Err(live_error) => {
            match super::commands::resolve_audit_agent(&store, &workspace, &runtime, &reference)? {
                Some(agent) => (agent, false),
                None => return Err(live_error),
            }
        }
    };
    let transcript = agent
        .transcript_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| anyhow::anyhow!("agent has no recorded transcript"))?;
    let path = Path::new(transcript);
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading agent transcript `{}`", path.display()))?;
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let prices = rimz::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    let messages = adapter.parse_transcript_messages(&contents);
    let spend = adapter.parse_spend(path, None, &prices);
    let session_open = resolved_live
        && matches!(
            agent.status,
            rimz::agents::AgentStatus::Running | rimz::agents::AgentStatus::Waiting
        );
    let mut turns = rimz::agents::turns::session_turns(
        &messages,
        &spend.entries,
        agent.agent_id.as_str(),
        session_open,
    );
    if let Some(tail) = tail {
        turns = turns.split_off(turns.len().saturating_sub(tail));
    }
    if json {
        return supervised::output::print_json(&turns);
    }

    let mut out = render::out();
    render_history(
        &mut out,
        &turns,
        &crate::cli::machine_config().time_zone(),
        render::terminal_columns(120),
    )?;
    Ok(())
}

fn render_history(
    w: &mut impl Write,
    turns: &[TurnRecord],
    time_zone: &jiff::tz::TimeZone,
    max_width: usize,
) -> std::io::Result<()> {
    let mut table = render::Table::new(["START", "DUR", "TOKENS", "COST", "OUTCOME", "PROMPT"])
        .right(&[1, 2, 3])
        .clip_last(max_width);
    for turn in turns {
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
                    .strftime("%Y-%m-%d %H:%M")
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
            render::cell(turn.prompt.split_whitespace().collect::<Vec<_>>().join(" ")),
        ]);
    }
    table.render(w)?;
    let tokens = turns.iter().fold(0u64, |total, turn| {
        total
            .saturating_add(turn.fresh_input)
            .saturating_add(turn.output)
            .saturating_add(turn.cache_read)
            .saturating_add(turn.cache_write)
    });
    let cost = turns
        .iter()
        .filter_map(|turn| turn.cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.0)
        .sum::<f64>();
    writeln!(
        w,
        "{} turns · {} tokens · ${cost:.4}",
        turns.len(),
        render::compact_count(tokens)
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

        render_history(&mut out, &turns, &jiff::tz::TimeZone::UTC, 120).expect("render");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("2024-01-01 00:00"));
        assert!(rendered.contains("1m"));
        assert!(rendered.contains("↘1k ↗800"));
        assert!(rendered.contains("done"));
        assert!(rendered.contains("fix login"));
        assert!(rendered.contains("1 turns · 4k tokens · $0.1250"));
    }
}
