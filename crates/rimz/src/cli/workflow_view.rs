//! Workflow run phrases shared by workflow and loop inspection.

use jiff::Timestamp;
use rimz::config::GlyphRole;
use rimz::harness::workflow::WorkflowSpec;
use rimz::store::workflow::{Asking, ParkReason, RunRecord, RunState};

use super::render;

pub(super) mod history;
pub(super) mod resolve;

pub(super) fn definitions() -> rimz::config::definitions::LoadedDefinitions {
    let machine = super::machine_config();
    rimz::config::definitions::load(
        &rimz::disk::paths::agents_home(),
        rimz::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &rimz::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    )
}

/// A workflow name resolves under `~/.rimz/workflows/`; any other path stays as given.
pub(super) fn definition_dir(name: &std::path::Path) -> std::path::PathBuf {
    let mut components = name.components();
    if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        rimz::disk::paths::workflows_dir().join(name)
    } else {
        name.to_owned()
    }
}

/// The deployed definition a run starts from, refused with every validation error; warnings
/// print to stderr. `workflow run` and `loop add --workflow` share this check.
pub(super) fn deployed_definition(
    name: &std::path::Path,
    project_root: Option<&std::path::Path>,
) -> anyhow::Result<std::path::PathBuf> {
    use std::io::Write;
    let dir = definition_dir(name);
    let home = rimz::disk::paths::workflows_dir();
    let deployed = dir.canonicalize().is_ok_and(|dir| {
        home.canonicalize()
            .is_ok_and(|home| dir != home && dir.starts_with(home))
    });
    if !deployed {
        anyhow::bail!(
            "{}: directory must exist inside {}",
            name.display(),
            home.display()
        );
    }
    let report = rimz::harness::workflow::validate(&dir, &definitions(), project_root);
    if !report.errors.is_empty() {
        anyhow::bail!(
            "{}",
            report
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    for warning in &report.warnings {
        writeln!(render::err(), "warning: {}", warning.message)?;
    }
    Ok(dir)
}

pub(super) fn sealed_specs(
    state: &rimz::StatePaths,
    records: &[RunRecord],
) -> std::collections::BTreeMap<rimz::ids::WorkflowRunId, WorkflowSpec> {
    records
        .iter()
        .filter(|record| !record.state.is_terminal())
        .filter_map(|record| {
            rimz::harness::workflow::load_run(&state.workflow(&record.id).home, record)
                .ok()
                .map(|spec| (record.id.clone(), spec))
        })
        .collect()
}

pub(super) fn with_you(record: &RunRecord) -> bool {
    record.state == RunState::Active
        && record
            .session_attempt()
            .is_some_and(|attempt| !attempt.lifecycle.is_turn() && attempt.run.is_some())
}

pub(super) fn asking(record: &RunRecord) -> Option<&Asking> {
    if record.state != RunState::Active {
        return None;
    }
    record.session_attempt()?.asking.as_ref()
}

pub(super) fn since(record: &RunRecord) -> Timestamp {
    if record.state == RunState::Parked
        && let Some(park) = &record.park
    {
        return park.at;
    }
    asking(record).map_or(record.entered_at, |asking| asking.since)
}

pub(super) fn wait(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
) -> Option<String> {
    let wait = record.wait.as_ref()?;
    if let Some(check) = spec.and_then(|spec| spec.checks.get(&wait.node))
        && check.is_sleep()
    {
        let when = check
            .when
            .as_ref()
            .and_then(|when| rimz::harness::when::WhenExpr::parse(when).ok())
            .map_or_else(String::new, |expression| format!(" when {expression}"));
        return Some(format!(
            "sleeping {}/{}{when} → {}",
            render::age_short(record.entered_at, now),
            check.sleep.as_deref().unwrap_or_default(),
            check.then.as_deref().unwrap_or_default()
        ));
    }
    if wait.outcome == "capacity" {
        return Some(format!(
            "{} · {}",
            rimz::theme::fmt::held_start(wait.first, wait.gives_up_at, now),
            render::one_line(wait.detail.as_deref().unwrap_or("—")),
        ));
    }
    let detail = match wait.outcome.as_str() {
        "hold" => {
            let hold = spec
                .and_then(|spec| spec.checks.get(&wait.node))
                .and_then(|check| {
                    wait.case.and_then(|index| {
                        check
                            .condition_cases()
                            .get(index as usize)
                            .and_then(|case| case.hold.clone())
                    })
                })
                .unwrap_or_else(|| "?".into());
            format!(
                "holding {} {}/{hold}",
                render::one_line(wait.detail.as_deref().unwrap_or("—")),
                render::age_short(wait.first, now),
            )
        }
        "unknown" | "retry" | "window" => match wait.detail.as_deref() {
            Some(detail) => format!(
                "{}{}{detail}",
                wait.outcome,
                if wait.outcome == "window" { " " } else { ": " },
                detail = render::one_line(detail),
            ),
            None => wait.outcome.clone(),
        },
        "takeover" | "occupied" => wait.outcome.clone(),
        _ => wait.outcome.clone(),
    };
    let recheck = wait
        .recheck_at
        .map(|at| format!(", recheck {}", render::workflow_clock(at, now)))
        .unwrap_or_default();
    Some(format!("{detail}{recheck}"))
}

pub(super) fn park(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
) -> Option<String> {
    if record.state != RunState::Parked {
        return None;
    }
    let park = record.park.as_ref()?;
    let derived = match park.reason {
        ParkReason::Cap => spec
            .and_then(|spec| spec.steps.get(&record.node))
            .and_then(|step| step.tripped_visit_bound(record))
            .map(|(_, used, max)| format!("{used} of {max} visits")),
        ParkReason::Timeout => spec
            .and_then(|spec| {
                spec.checks
                    .get(&record.node)
                    .and_then(|check| check.timeout.as_ref())
                    .or_else(|| spec.steps.get(&record.node)?.timeout.as_ref())
            })
            .map(|timeout| format!("waited {timeout}")),
        ParkReason::Deadline => record
            .deadline
            .map(|at| format!("passed {}", render::workflow_clock(at, now))),
        _ => None,
    };
    Some(match derived.as_deref().or(park.text.as_deref()) {
        Some(text) => format!("{}: {}", park.reason.as_str(), render::one_line(text)),
        None => park.reason.as_str().into(),
    })
}

pub(super) fn detail(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
    agent: Option<&str>,
) -> String {
    if record.state.is_terminal() {
        return String::new();
    }
    if let Some(asking) = asking(record) {
        let ask = &asking.ask;
        return format!(
            "asking you: {} ({}): {}",
            ask.handle,
            ask.kind,
            render::one_line(&ask.text),
        );
    }
    if with_you(record) {
        let agent = agent.map_or_else(String::new, |agent| format!(" in {agent}"));
        return format!("with you{agent}{}", with_you_suffix(record, spec, now));
    }
    if let Some(park) = park(record, spec, now) {
        return park;
    }
    if let Some(wait) = wait(record, spec, now) {
        return wait;
    }
    if let Some(step) = spec.and_then(|spec| spec.steps.get(&record.node)) {
        return step
            .visit_bounds(
                record.visits.get(&record.node),
                record.visit_bucket(),
                record.visit_grant,
            )
            .next()
            .map_or_else(
                || "running".into(),
                |(_, used, max)| format!("running · visit {used} of {max}"),
            );
    }
    "checking".into()
}

pub(super) fn with_you_suffix(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
) -> String {
    let phrase = spec
        .and_then(|spec| rimz::harness::workflow::close_after(spec, record))
        .filter(|(at, _)| *at > now)
        .map(|(at, target)| format!("closing {} → {target}", render::rel_until(at, now)))
        .or_else(|| wait(record, spec, now));
    if record.held.is_some() {
        let wait = phrase.map_or_else(String::new, |phrase| format!(": {phrase}"));
        return format!(" · checking {}{wait}", record.node);
    }
    match (record.wait.as_ref(), phrase) {
        (Some(wait), Some(phrase)) => format!(" · {}: {phrase}", wait.node),
        _ => String::new(),
    }
}

pub(super) fn glyph_role(record: &RunRecord) -> GlyphRole {
    match record.state {
        RunState::Done => return GlyphRole::StatusDone,
        RunState::Failed => return GlyphRole::StatusAttention,
        RunState::Canceled => return GlyphRole::StatusIdle,
        RunState::Parked => return GlyphRole::WorkflowHeld,
        RunState::Active => {}
    }
    if asking(record).is_some() {
        return GlyphRole::StatusWaiting;
    }
    if let Some(wait) = record
        .wait
        .as_ref()
        .filter(|wait| wait.outcome == "capacity")
    {
        return if wait.hold.is_some_and(rimz::ids::HoldKind::needs_fix) {
            GlyphRole::WorkflowHeld
        } else {
            GlyphRole::PipelineCurrent
        };
    }
    if with_you(record) {
        return GlyphRole::StatusWorking;
    }
    if record.wait.is_some() {
        return GlyphRole::StatusSleeping;
    }
    GlyphRole::StatusWorking
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::config::GlyphRole;
    use serde_json::json;

    fn record() -> RunRecord {
        serde_json::from_value(json!({
            "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"land",
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/lane"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"active", "node":"triage", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":2,
        }))
        .unwrap()
    }

    fn session(record: &mut RunRecord, held: bool) {
        let attempt = json!({
            "kind":"agent", "lifecycle":"until", "seq":1, "node":"triage",
            "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
            "run":{"run_id":rimz::ids::RunId::new(), "handle":"@fox"},
        });
        if held {
            record.node = "ruled".into();
            record.held = Some(
                serde_json::from_value(json!({
                    "attempt":attempt, "entered_at":Timestamp::UNIX_EPOCH,
                }))
                .unwrap(),
            );
        } else {
            record.attempt = Some(serde_json::from_value(attempt).unwrap());
        }
    }

    fn waiting(record: &mut RunRecord, outcome: &str) {
        record.wait = Some(
            serde_json::from_value(json!({
                "node":"ruled", "outcome":outcome, "detail":"pending",
                "first":Timestamp::UNIX_EPOCH, "last":Timestamp::UNIX_EPOCH, "count":3,
            }))
            .unwrap(),
        );
    }

    #[test]
    fn wait_outcomes_are_count_free_status_phrases() {
        let cases = [
            ("pending", "pending"),
            ("hold", "holding pending 0s/?"),
            ("unknown", "unknown: pending"),
            ("window", "window pending"),
            ("retry", "retry: pending"),
            ("takeover", "takeover"),
            ("occupied", "occupied"),
            ("capacity", "held 0s · pending"),
        ];
        let actual = cases.map(|(outcome, _)| {
            let mut record = record();
            waiting(&mut record, outcome);
            wait(&record, None, Timestamp::UNIX_EPOCH).unwrap()
        });
        assert_eq!(actual, cases.map(|(_, expected)| expected.to_owned()));
        let mut record = record();
        waiting(&mut record, "pending");
        record.wait.as_mut().unwrap().recheck_at = Some(Timestamp::UNIX_EPOCH);
        assert_eq!(
            wait(&record, None, Timestamp::UNIX_EPOCH).unwrap(),
            format!(
                "pending, recheck {}",
                render::workflow_clock(Timestamp::UNIX_EPOCH, Timestamp::UNIX_EPOCH)
            )
        );
    }

    #[test]
    fn with_you_names_the_check_and_optional_agent() {
        let cases = [
            (true, false, None, "with you · checking ruled"),
            (true, true, None, "with you · checking ruled: open"),
            (false, true, None, "with you · ruled: open"),
            (
                true,
                false,
                Some("@yagni"),
                "with you in @yagni · checking ruled",
            ),
            (
                true,
                true,
                Some("@yagni"),
                "with you in @yagni · checking ruled: open",
            ),
            (
                false,
                true,
                Some("@yagni"),
                "with you in @yagni · ruled: open",
            ),
        ];
        let actual = cases.map(|(held, has_wait, agent, _)| {
            let mut record = record();
            session(&mut record, held);
            if has_wait {
                waiting(&mut record, "open");
            }
            detail(&record, None, Timestamp::UNIX_EPOCH, agent)
        });
        assert_eq!(actual, cases.map(|(_, _, _, expected)| expected.to_owned()));
    }

    #[test]
    fn a_delayed_close_names_its_remaining_time_until_it_is_due() {
        let mut record = record();
        session(&mut record, true);
        record.held.as_mut().unwrap().verdict = Some(
            serde_json::from_value(json!({
                "node":"ruled", "entered_at":Timestamp::UNIX_EPOCH,
                "at":Timestamp::UNIX_EPOCH, "target":"@done",
                "object":{"outcome":"settled"}, "reason":"user",
            }))
            .unwrap(),
        );
        let spec = toml::from_str(
            "[workflow]\nscope='worktree'\nstart='triage'\n\
             [steps.triage]\nagent='claude'\nlifecycle='until'\nthen='ruled'\n\
             [checks.ruled]\nrun='true'\non.settled={to='@done',delay='30m'}\n",
        )
        .unwrap();
        assert_eq!(
            detail(
                &record,
                Some(&spec),
                Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(7),
                None
            ),
            "with you · checking ruled: closing in 23m → @done"
        );
        assert_eq!(
            detail(
                &record,
                Some(&spec),
                Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(30),
                None
            ),
            "with you · checking ruled"
        );
    }

    #[test]
    fn sleeps_replace_every_wait_phrase_with_the_entry_based_clock() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(12);
        let actual = ["", "when=['ci=passed']\n"].map(|when| {
            let spec: WorkflowSpec = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='ruled'\n\
                 [checks.ruled]\nsleep='30m'\nthen='fix'\n{when}"
            ))
            .unwrap();
            ["sleeping", "unknown", "hold"].map(|outcome| {
                let mut record = record();
                record.node = "ruled".into();
                waiting(&mut record, outcome);
                record.wait.as_mut().unwrap().recheck_at = Some(now);
                detail(&record, Some(&spec), now, None)
            })
        });
        assert_eq!(
            actual,
            [
                std::array::from_fn(|_| "sleeping 12m/30m → fix".to_owned()),
                std::array::from_fn(|_| "sleeping 12m/30m when ci=passed → fix".to_owned()),
            ]
        );
    }

    #[test]
    fn run_glyph_prioritizes_attention_capacity_and_staying_sessions() {
        let cases = [
            ("active", true, Some("open"), None, GlyphRole::StatusWorking),
            (
                "active",
                false,
                Some("open"),
                None,
                GlyphRole::StatusSleeping,
            ),
            (
                "active",
                false,
                Some("hold"),
                None,
                GlyphRole::StatusSleeping,
            ),
            (
                "active",
                false,
                Some("capacity"),
                Some("queue"),
                GlyphRole::PipelineCurrent,
            ),
            (
                "active",
                false,
                Some("capacity"),
                Some("check_failed"),
                GlyphRole::WorkflowHeld,
            ),
            (
                "active",
                false,
                Some("capacity"),
                Some("disk"),
                GlyphRole::WorkflowHeld,
            ),
            (
                "active",
                false,
                Some("capacity"),
                Some("store"),
                GlyphRole::WorkflowHeld,
            ),
            ("active", false, None, None, GlyphRole::StatusWorking),
            ("parked", false, None, None, GlyphRole::WorkflowHeld),
            ("done", false, None, None, GlyphRole::StatusDone),
            ("failed", false, None, None, GlyphRole::StatusAttention),
            ("canceled", false, None, None, GlyphRole::StatusIdle),
        ];
        let actual = cases.map(|(state, staying, outcome, hold, _)| {
            let mut record = record();
            record.state = serde_json::from_value(json!(state)).unwrap();
            if staying {
                session(&mut record, true);
            }
            if let Some(outcome) = outcome {
                waiting(&mut record, outcome);
                record.wait.as_mut().unwrap().hold =
                    hold.map(|hold| serde_json::from_value(json!(hold)).unwrap());
            }
            glyph_role(&record)
        });
        assert_eq!(actual, cases.map(|(_, _, _, _, expected)| expected));
        for held in [false, true] {
            let mut record = record();
            session(&mut record, held);
            let attempt = if held {
                &mut record.held.as_mut().unwrap().attempt
            } else {
                record.attempt.as_mut().unwrap()
            };
            attempt.asking = Some(
                serde_json::from_value(json!({
                    "since":Timestamp::UNIX_EPOCH, "text":"Which fix?",
                    "kind":"question", "handle":"@fox",
                }))
                .unwrap(),
            );
            assert_eq!(glyph_role(&record), GlyphRole::StatusWaiting);
        }
    }
}
