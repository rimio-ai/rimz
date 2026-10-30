use rimz::store::workflow::{Children, TurnEndAction};
use serde_json::Value;

use super::*;

struct HistoryRow<'a> {
    first: &'a records::JournalEntry,
    latest: &'a records::JournalEntry,
    count: u64,
    index: usize,
    cost_usd: Option<f64>,
}

impl HistoryRow<'_> {
    fn note(&self) -> String {
        let (count, first, last, detail) = match &self.latest.event {
            records::JournalEvent::Wait {
                count,
                first,
                last,
                detail,
                ..
            } => (*count, *first, *last, detail.as_deref()),
            _ => (self.count, self.first.at, self.latest.at, None),
        };
        let mut note = if count > 1 {
            let span = last.duration_since(first).as_secs().max(0) as u64;
            format!(" ×{count} over {}", render::age_label(span))
        } else {
            String::new()
        };
        if let Some(detail) = detail {
            note.push_str(&format!(" · {detail}"));
        }
        note
    }
}

#[derive(PartialEq, Eq)]
enum HistoryKey<'a> {
    Evaluation {
        node: &'a str,
        origin: records::EvaluationOrigin,
        outcome: &'a str,
        case: Option<u64>,
    },
    Wake {
        node: &'a str,
        signal: &'a str,
    },
}

fn history_key(entry: &records::JournalEntry) -> Option<HistoryKey<'_>> {
    match &entry.event {
        records::JournalEvent::Evaluation {
            origin,
            outcome,
            object,
            ..
        } => Some(HistoryKey::Evaluation {
            node: &entry.node,
            origin: *origin,
            outcome,
            case: object.get("case").and_then(serde_json::Value::as_u64),
        }),
        records::JournalEvent::Wake { signal } => Some(HistoryKey::Wake {
            node: &entry.node,
            signal,
        }),
        _ => None,
    }
}

fn collapsed_history(journal: &[records::JournalEntry]) -> Vec<HistoryRow<'_>> {
    let mut rows = Vec::<HistoryRow<'_>>::new();
    for (index, entry) in journal.iter().enumerate() {
        if let records::JournalEvent::Wait {
            outcome,
            first,
            last,
            ..
        } = &entry.event
            && wait_row_counts(&rows, &entry.node, outcome, *first, *last)
                .next()
                .is_some()
        {
            continue;
        }
        if let Some(key) = history_key(entry)
            && let Some(row) = rows.last_mut()
            && history_key(row.latest).as_ref() == Some(&key)
        {
            row.latest = entry;
            row.index = index;
            row.count += 1;
            if let Some(cost) = evaluation_cost(entry) {
                *row.cost_usd.get_or_insert(0.0) += cost;
            }
            continue;
        }
        rows.push(HistoryRow {
            first: entry,
            latest: entry,
            count: 1,
            index,
            cost_usd: evaluation_cost(entry),
        });
    }
    rows
}

fn evaluation_cost(entry: &records::JournalEntry) -> Option<f64> {
    match &entry.event {
        records::JournalEvent::Evaluation { cost_usd, .. } => *cost_usd,
        _ => None,
    }
}

fn wait_row_counts<'a>(
    rows: &'a [HistoryRow<'_>],
    node: &'a str,
    outcome: &'a str,
    first: jiff::Timestamp,
    last: jiff::Timestamp,
) -> impl Iterator<Item = u64> + 'a {
    rows.iter()
        .rev()
        .filter(move |row| {
            row.latest.node == node
                && matches!(
                    &row.latest.event,
                    records::JournalEvent::Evaluation { outcome: answer, .. } if answer == outcome
                )
                && row.first.at <= last
                && first <= row.latest.at
        })
        .map(|row| row.count)
}

pub(super) fn waiting_line(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    journal: &[JournalEntry],
    now: Timestamp,
) -> Option<String> {
    let wait = record.wait.as_ref()?;
    let rows = collapsed_history(journal);
    let show_count =
        !wait_row_counts(&rows, &wait.node, &wait.outcome, wait.first, wait.last).eq([wait.count]);
    workflow_view::wait_counted(record, spec, now, show_count)
}

impl View<'_> {
    pub(super) fn history(&self, out: &mut impl Write) -> Result<()> {
        if let Some(wait) = &self.record.wait
            && (workflow_view::with_you(self.record)
                || workflow_view::asking(self.record).is_some()
                || self.hold()?.is_some())
            && let Some(line) = waiting_line(self.record, Some(self.spec), self.journal, self.now)
        {
            writeln!(out, "  waiting: {}, {line}", wait.node)?;
        }
        for row in collapsed_history(self.journal) {
            let entry = row.latest;
            let seq = match (row.count, row.first.seq, entry.seq) {
                (1, _, Some(seq)) => seq.to_string(),
                (_, Some(first), Some(last)) => format!("{first}–{last}"),
                _ => "—".into(),
            };
            let note = row.note();
            write!(out, "  {} #{seq} {} ", self.clock(entry.at), entry.node)?;
            match &entry.event {
                JournalEvent::Evaluation {
                    outcome,
                    object,
                    stderr_tail,
                    digests,
                    retry,
                    ..
                } => {
                    writeln!(
                        out,
                        "evaluation {outcome}{}{note}{}",
                        workflow::evaluation_target(
                            self.spec,
                            entry,
                            &self.journal[row.index + 1..]
                        )
                        .map_or_else(String::new, |target| format!(" → {target}")),
                        cost(row.cost_usd)
                    )?;
                    self.answer(out, object)?;
                    if outcome == "invalid"
                        && let Some(stderr) = stderr_tail
                    {
                        tail(out, stderr)?;
                    }
                    digests_out(out, digests)?;
                    if let Some(retry) = retry {
                        writeln!(out, "    retry: {}", value(retry))?;
                    }
                }
                JournalEvent::Attempt {
                    outcome,
                    visit,
                    started_at,
                    ended_at,
                    handle,
                    code,
                    head_start,
                    head_end,
                    cost_usd,
                    log,
                    reason,
                    children,
                    limited,
                    asked,
                    digests,
                    ..
                } => {
                    writeln!(
                        out,
                        "attempt {} · visit {visit} · {}",
                        outcome.as_str(),
                        render::format_compact_duration(
                            ended_at.duration_since(*started_at).as_secs().max(0) as u64
                        )
                    )?;
                    if self
                        .spec
                        .steps
                        .get(&entry.node)
                        .is_some_and(|step| step.agent.is_some())
                    {
                        writeln!(
                            out,
                            "    {} HEAD {} → {} cost {}",
                            handle.as_deref().unwrap_or("—"),
                            short(head_start),
                            short(head_end),
                            cost_usd.map_or_else(|| "unknown".into(), rimz::theme::fmt::dollars2)
                        )?;
                    } else {
                        writeln!(
                            out,
                            "    exit {} HEAD {} → {}",
                            code.map_or_else(|| "—".into(), |code| code.to_string()),
                            short(head_start),
                            short(head_end)
                        )?;
                        if let Some(cost) = cost_usd {
                            writeln!(out, "    cost: {}", rimz::theme::fmt::dollars2(*cost))?;
                        }
                    }
                    if let Some(log) = log {
                        writeln!(out, "    log: {}", log.display())?;
                    }
                    if let Some(reason) = reason {
                        prose(out, &format!("reason: {reason}"), "    ")?;
                    }
                    if let Some(children) = children {
                        children_out(out, children)?;
                    }
                    if let Some(limited) = limited {
                        writeln!(
                            out,
                            "    reset_at: {}",
                            limited
                                .reset_at
                                .map_or_else(|| "unknown".into(), |at| self.clock(at))
                        )?;
                    }
                    if let Some(asked) = asked {
                        prose(
                            out,
                            &format!("{} ({}): {}", asked.handle, asked.kind, asked.text),
                            "    ",
                        )?;
                    }
                    digests_out(out, digests)?;
                }
                JournalEvent::Route { outcome, input } => {
                    let mut input = input.clone();
                    if let Some(at) = input
                        .get("deadline")
                        .and_then(Value::as_str)
                        .and_then(|raw| raw.parse::<Timestamp>().ok())
                    {
                        input["deadline"] = Value::String(self.clock(at));
                    }
                    writeln!(
                        out,
                        "route {outcome}{} · {}",
                        self.target(&entry.node, outcome),
                        value(&input)
                    )?;
                }
                JournalEvent::Wait {
                    outcome,
                    recheck_at,
                    ..
                } => {
                    writeln!(out, "wait {outcome}{note}")?;
                    if let Some(at) = recheck_at {
                        writeln!(out, "    recheck {}", self.clock(*at))?;
                    }
                }
                JournalEvent::TurnEnd {
                    action,
                    check,
                    outcome,
                    target,
                    reason,
                    ..
                } => {
                    let action = match action {
                        TurnEndAction::Kept => "kept",
                        TurnEndAction::Closed => "closed",
                        TurnEndAction::Discarded => "discarded",
                        TurnEndAction::Interrupted => "interrupted",
                    };
                    writeln!(
                        out,
                        "turn_end {action} · {check} {}{}",
                        outcome.as_deref().unwrap_or("—"),
                        target
                            .as_ref()
                            .map_or_else(String::new, |target| format!(" → {target}"))
                    )?;
                    if let Some(reason) = reason {
                        prose(out, reason, "    ")?;
                    }
                }
                JournalEvent::Park { reason, text } => {
                    writeln!(out, "park {}", reason.as_str())?;
                    if let Some(text) = text {
                        prose(out, text, "    ")?;
                    }
                }
                JournalEvent::Resume {
                    visit_grant,
                    deadline,
                } => {
                    writeln!(
                        out,
                        "resume → {} · visit grant {visit_grant} · deadline {}",
                        entry.node,
                        deadline.map_or_else(|| "none".into(), |at| self.clock(at))
                    )?;
                }
                JournalEvent::Cancel => writeln!(out, "cancel canceled")?,
                JournalEvent::Terminal { state } => writeln!(out, "terminal {}", state.as_str())?,
                JournalEvent::Wake { signal } => writeln!(out, "wake {signal}{note}")?,
            }
        }
        Ok(())
    }

    fn target(&self, node: &str, outcome: &str) -> String {
        self.spec
            .target(node, outcome)
            .map_or_else(String::new, |target| format!(" → {target}"))
    }

    fn answer(&self, out: &mut impl Write, object: &Value) -> Result<()> {
        if let Some(readings) = object.get("readings") {
            writeln!(out, "    {}", value(readings))?;
        }
        if let Some(reason) = object.get("reason").and_then(Value::as_str) {
            prose(out, &format!("reason: {reason}"), "    ")?;
        }
        if let Some(log) = object.get("log").and_then(Value::as_str) {
            writeln!(out, "    log: {log}")?;
        }
        Ok(())
    }
}

pub(super) fn value(value: &Value) -> String {
    match value {
        Value::Null => "unread".into(),
        Value::String(text) => render::one_line(text),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, field)| format!("{key}={}", self::value(field)))
            .collect::<Vec<_>>()
            .join(" · "),
        Value::Array(items) => items.iter().map(self::value).collect::<Vec<_>>().join(", "),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

fn short(head: &Option<String>) -> String {
    head.as_deref().unwrap_or("—").chars().take(8).collect()
}

fn cost(cost: Option<f64>) -> String {
    cost.map_or_else(String::new, |cost| {
        format!(" · {}", rimz::theme::fmt::dollars2(cost))
    })
}

fn children_out(out: &mut impl Write, children: &Children) -> Result<()> {
    for (label, children) in [("stopped", &children.stopped), ("live", &children.live)] {
        let handles = children
            .iter()
            .map(|child| {
                child.team.as_ref().map_or_else(
                    || child.handle.clone(),
                    |team| {
                        format!(
                            "{} ({}#{}, {})",
                            child.handle,
                            team.name,
                            team.channel,
                            team.stage.as_deref().unwrap_or("stage unknown")
                        )
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "    {label} ({}): {handles}", children.len())?;
    }
    Ok(())
}

fn digests_out(
    out: &mut impl Write,
    digests: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for (path, digest) in digests {
        writeln!(out, "    {path}: {digest}")?;
    }
    Ok(())
}

pub(super) fn tail(out: &mut impl Write, text: &str) -> Result<()> {
    let lines = text.lines().collect::<Vec<_>>();
    for line in &lines[lines.len().saturating_sub(8)..] {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use jiff::Timestamp;
    use records::{EvaluationOrigin, JournalEntry, JournalEvent};
    use serde_json::json;

    fn at(minute: i64) -> Timestamp {
        "2026-10-30T19:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .checked_add(jiff::SignedDuration::from_secs(minute * 60))
            .unwrap()
    }

    fn evaluation(seq: u64, minute: i64, outcome: &str, case: Option<u32>) -> JournalEntry {
        let mut object = json!({"outcome":outcome, "reason":format!("answer {seq}")});
        if let Some(case) = case {
            object["case"] = json!(case);
        }
        JournalEntry {
            seq: Some(seq),
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Evaluation {
                origin: EvaluationOrigin::Check,
                outcome: outcome.into(),
                object,
                digests: BTreeMap::new(),
                started_at: at(minute),
                ended_at: at(minute),
                stderr_tail: None,
                cost_usd: None,
                retry: None,
            },
        }
    }

    fn wake(minute: i64, signal: &str) -> JournalEntry {
        JournalEntry {
            seq: None,
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Wake {
                signal: signal.into(),
            },
        }
    }

    fn wait(minute: i64, outcome: &str, first: i64, last: i64, count: u64) -> JournalEntry {
        JournalEntry {
            seq: None,
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Wait {
                outcome: outcome.into(),
                detail: None,
                first: at(first),
                last: at(last),
                count,
                recheck_at: None,
            },
        }
    }

    fn history(journal: &[JournalEntry]) -> String {
        let spec = toml::from_str(
            "[workflow]\nscope='worktree'\nstart='pr'\n\
             [checks.pr]\nrun='sh checks/pr.sh'\non.pending='@wait'\non.done='@done'\n\
             [steps.build]\nagent='codex'\nprompt-file='prompt.md'\nthen='pr'\n",
        )
        .unwrap();
        let record = waiting_record(0, 0, 1);
        let paths = paths::StatePaths::for_project_root(&record.project_root)
            .unwrap()
            .workflow(&record.id);
        let globals = crate::cli::GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal,
            now: at(20),
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut out = Vec::new();
        view.history(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn clock(minute: i64) -> String {
        render::workflow_clock(at(minute), at(20))
    }

    #[test]
    fn evaluations_fold_with_newest_answer_and_suppress_the_closed_wait() {
        let mut journal = (1..=5)
            .map(|seq| evaluation(seq, seq as i64 - 1, "pending", None))
            .collect::<Vec<_>>();
        for entry in &mut journal[1..] {
            if let JournalEvent::Evaluation { cost_usd, .. } = &mut entry.event {
                *cost_usd = Some(0.03);
            }
        }
        journal.push(evaluation(6, 5, "done", None));
        journal.push(wait(5, "pending", 0, 4, 5));
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        assert!(
            text.contains(&format!(
                "{} #1–5 pr evaluation pending → @wait ×5 over 4m · $0.12\n",
                clock(4)
            )),
            "{text}"
        );
        assert!(text.contains("reason: answer 5"), "{text}");
        assert!(!text.contains("answer 1"), "{text}");
        assert!(
            text.contains(&format!("{} #6 pr evaluation done → @done\n", clock(5))),
            "{text}"
        );
        assert!(!text.contains("pr wait"), "{text}");
    }

    #[test]
    fn a_closed_hold_searches_past_a_newer_case_without_breaking_its_run() {
        let journal = [
            evaluation(1, 0, "hold", Some(0)),
            evaluation(2, 1, "hold", Some(0)),
            evaluation(3, 2, "hold", Some(1)),
            wait(2, "hold", 0, 1, 2),
            evaluation(4, 3, "hold", Some(1)),
            evaluation(5, 4, "hold", Some(1)),
        ];
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        assert!(
            text.contains("#1–2 pr evaluation hold → @wait ×2 over 1m"),
            "{text}"
        );
        assert!(
            text.contains("#3–5 pr evaluation hold → @wait ×3 over 2m"),
            "{text}"
        );
        assert!(!text.contains("pr wait"), "{text}");
    }

    #[test]
    fn wakes_break_evaluation_runs_and_only_fold_with_the_same_signal() {
        let mut journal = vec![
            evaluation(1, 0, "pending", None),
            wake(1, "review.ready"),
            evaluation(2, 2, "pending", None),
        ];
        assert_eq!(collapsed_history(&journal).len(), 3);
        journal.extend([
            wake(3, "review.ready"),
            wake(12, "review.ready"),
            wake(13, "review.changed"),
        ]);
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 5, "{text}");
        assert!(
            text.contains(&format!(
                "{} #— pr wake review.ready ×2 over 9m\n",
                clock(12)
            )),
            "{text}"
        );
        assert!(text.contains("pr wake review.changed\n"), "{text}");
    }

    #[test]
    fn unrepresented_waits_show_their_span_and_detail_but_no_singleton_count() {
        let mut window = wait(3, "window", 0, 2, 3);
        window.node = "build".into();
        if let JournalEvent::Wait { detail, .. } = &mut window.event {
            *detail = Some("5h.left=12<42".into());
        }
        let text = history(&[window, wait(4, "pending", 4, 4, 1)]);
        assert_eq!(
            text,
            format!(
                "  {} #— build wait window ×3 over 2m · 5h.left=12<42\n  {} #— pr wait pending\n",
                clock(3),
                clock(4),
            )
        );
    }

    #[test]
    fn attempts_keep_individual_rows_and_their_detail_lines() {
        let journal = [1, 2].map(|seq| JournalEntry {
            seq: Some(seq),
            at: at(seq as i64),
            node: "build".into(),
            event: JournalEvent::Attempt {
                outcome: records::AttemptOutcome::Ended,
                visit: seq,
                key: None,
                digests: BTreeMap::from([("prompt.md".into(), format!("digest-{seq}"))]),
                started_at: at(seq as i64),
                ended_at: at(seq as i64),
                handle: Some(format!("@builder-{seq}")),
                run_id: None,
                head_start: Some("0123456789".into()),
                head_end: Some("abcdef0123".into()),
                code: None,
                log: Some(PathBuf::from(format!("work/{seq}.step.log"))),
                cost_usd: Some(0.25),
                children: None,
                limited: None,
                asked: None,
                reason: None,
            },
        });
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        for seq in [1, 2] {
            assert!(
                text.contains(&format!("#{seq} build attempt ended · visit {seq} · 0s\n")),
                "{text}"
            );
            assert!(
                text.contains(&format!(
                    concat!(
                        "    @builder-{seq} HEAD 01234567 → abcdef01 cost $0.25\n",
                        "    log: work/{seq}.step.log\n    prompt.md: digest-{seq}\n",
                    ),
                    seq = seq,
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn evaluation_keys_keep_origins_nodes_and_outcomes_separate() {
        let check = evaluation(1, 0, "pending", None);
        let mut engine = evaluation(2, 1, "pending", None);
        if let JournalEvent::Evaluation { origin, .. } = &mut engine.event {
            *origin = EvaluationOrigin::Engine;
        }
        let mut other_node = engine.clone();
        other_node.node = "ci".into();
        let mut other_outcome = other_node.clone();
        if let JournalEvent::Evaluation {
            outcome, object, ..
        } = &mut other_outcome.event
        {
            *outcome = "done".into();
            object["outcome"] = json!("done");
        }
        let journal = [check, engine, other_node, other_outcome];
        assert_eq!(collapsed_history(&journal).len(), 4);
    }

    #[test]
    fn wait_rows_match_the_nodes_outcome_and_inclusive_span() {
        let journal = [evaluation(1, 1, "pending", None)];
        let rows = collapsed_history(&journal);
        assert!(wait_row_counts(&rows, "pr", "pending", at(1), at(1)).eq([1]));
        assert!(wait_row_counts(&rows, "pr", "pending", at(0), at(2)).eq([1]));
        for (node, outcome, first, last) in [
            ("pr", "window", 0, 2),
            ("ci", "pending", 0, 2),
            ("pr", "pending", 2, 3),
            ("pr", "pending", 0, 0),
        ] {
            assert!(
                wait_row_counts(&rows, node, outcome, at(first), at(last))
                    .next()
                    .is_none()
            );
        }
    }

    fn waiting_record(first: i64, last: i64, count: u64) -> RunRecord {
        serde_json::from_value(json!({
            "version":2, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "spec_digest":"digest",
            "scope":{"kind":"worktree", "path":"/checkout"}, "project_root":"/checkout",
            "created_at":at(0), "state":"active", "node":"build", "entered_at":at(0),
            "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":4,
            "wait":{"node":"pr", "outcome":"pending", "first":at(first), "last":at(last),
                    "count":count}
        }))
        .unwrap()
    }

    fn kept_turn(minute: i64) -> JournalEntry {
        JournalEntry {
            seq: Some(1),
            at: at(minute),
            node: "build".into(),
            event: JournalEvent::TurnEnd {
                action: records::TurnEndAction::Kept,
                turn: records::TurnEnd {
                    agent_id: "leader".into(),
                    started_at: at(minute - 1),
                    ended_at: at(minute),
                },
                check: "pr".into(),
                outcome: Some("pending".into()),
                target: Some("@wait".into()),
                reason: None,
            },
        }
    }

    #[test]
    fn one_kept_turn_states_the_open_wait_and_suppresses_its_closing_row() {
        let mut journal = vec![
            evaluation(2, 0, "pending", None),
            kept_turn(1),
            evaluation(3, 2, "done", None),
        ];
        let record = waiting_record(0, 0, 1);
        let line = waiting_line(&record, None, &journal, at(20)).unwrap();
        assert!(!line.contains('×'), "{line}");
        assert!(
            workflow_view::wait(&record, None, Timestamp::now())
                .unwrap()
                .contains("×1")
        );
        journal.push(wait(2, "pending", 0, 0, 1));
        assert_eq!(collapsed_history(&journal).len(), 3);
        assert!(!history(&journal).contains("pr wait"));
    }

    #[test]
    fn several_kept_turns_keep_the_header_total_but_not_a_closing_summary() {
        let mut journal = vec![
            evaluation(2, 0, "pending", None),
            kept_turn(1),
            evaluation(3, 2, "pending", None),
            kept_turn(3),
        ];
        let record = waiting_record(0, 2, 2);
        let line = waiting_line(&record, None, &journal, at(20)).unwrap();
        assert!(line.contains("×2"), "{line}");
        journal.push(wait(4, "pending", 0, 2, 2));
        assert_eq!(collapsed_history(&journal).len(), 4);
        assert!(!history(&journal).contains("pr wait"));
    }

    #[test]
    fn wakes_keep_the_open_waits_total_across_rows() {
        let journal = [
            evaluation(1, 0, "pending", None),
            evaluation(2, 1, "pending", None),
            wake(2, "review.ready"),
            evaluation(3, 3, "pending", None),
        ];
        let record = waiting_record(0, 3, 3);
        let line = waiting_line(&record, None, &journal, at(20)).unwrap();
        assert!(line.contains("×3"), "{line}");
    }

    #[test]
    fn early_advances_keep_a_count_no_single_evaluation_states() {
        let mut journal = vec![evaluation(1, 0, "pending", None)];
        let record = waiting_record(0, 2, 3);
        let line = waiting_line(&record, None, &journal, at(20)).unwrap();
        assert!(line.contains("×3"), "{line}");
        journal.push(wait(3, "pending", 0, 2, 3));
        assert_eq!(collapsed_history(&journal).len(), 1);
        assert!(!history(&journal).contains("pr wait"));
    }
}
