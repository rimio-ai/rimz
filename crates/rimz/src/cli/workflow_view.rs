//! Workflow run phrases shared by workflow and loop inspection.

use jiff::Timestamp;
use rimz::config::GlyphRole;
use rimz::harness::workflow::{Definition, Target};
use rimz::store::workflow::{
    Asking, AttemptKind, Hold, LiveAttempt, ParkReason, RunEnding, RunRecord, RunState,
};

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

/// The deployed definition a `workflow run` starts from, refused with every validation error;
/// warnings print to stderr.
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

/// The first error admission reports for a live run's definition, the one `workflow advance`
/// refuses with while the run waits on it; none once the definition is admitted.
pub(super) fn definition_refusal(
    record: &RunRecord,
    dir: &std::path::Path,
    loaded: &rimz::config::definitions::LoadedDefinitions,
) -> Option<String> {
    let error =
        rimz::harness::workflow::admit(dir, &record.workflow, loaded, Some(&record.project_root))
            .err()?;
    let text = error.to_string();
    Some(render::one_line(text.lines().next().unwrap_or_default()))
}

pub(super) fn live_specs(
    state: &rimz::StatePaths,
    records: &[RunRecord],
) -> std::collections::BTreeMap<rimz::ids::WorkflowRunId, Definition> {
    records
        .iter()
        .filter(|record| !record.state.is_terminal())
        .filter_map(|record| {
            rimz::harness::workflow::load(&record.definition_dir(&state.workflow(&record.id)))
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

/// The ask the run's live agent attempt shows, with the attempt that shows it.
pub(super) fn asker(record: &RunRecord) -> Option<(&LiveAttempt, &Asking)> {
    record
        .live_attempts()
        .find_map(|attempt| Some((attempt, attempt.asking.as_ref()?)))
}

pub(super) fn asking(record: &RunRecord) -> Option<&Asking> {
    asker(record).map(|(_, asking)| asking)
}

/// The handle of the agent judging the run's current check.
pub(super) fn judge(record: &RunRecord) -> Option<&str> {
    let attempt = record.attempt.as_ref()?;
    let run = attempt.run.as_ref()?;
    (attempt.kind == AttemptKind::Judge).then_some(run.handle.as_str())
}

/// The stop the run's live agent attempt is held on, with the attempt it holds.
pub(super) fn holder(record: &RunRecord) -> Option<(&LiveAttempt, &Hold)> {
    record
        .live_attempts()
        .find_map(|attempt| Some((attempt, attempt.held_on.as_ref()?)))
}

pub(super) fn held(record: &RunRecord) -> Option<&Hold> {
    holder(record).map(|(_, hold)| hold)
}

/// The agent a hold names: its own handle, else its attempt's agent.
pub(super) fn hold_handle<'r>(attempt: &'r LiveAttempt, hold: &'r Hold) -> Option<&'r str> {
    hold.handle
        .as_deref()
        .or_else(|| attempt.run.as_ref().map(|run| run.handle.as_str()))
}

/// [`RunRecord::calls_you`].
pub(super) fn calls_you(record: &RunRecord) -> bool {
    record.calls_you()
}

/// [`RunRecord::needs_you`].
pub(super) fn needs_you(record: &RunRecord) -> bool {
    record.needs_you()
}

/// The capacity wait an active run's record holds.
pub(super) fn capacity_wait(record: &RunRecord) -> Option<&rimz::store::workflow::Wait> {
    record
        .wait
        .as_ref()
        .filter(|wait| record.state == RunState::Active && wait.outcome == "capacity")
}

pub(super) fn since(record: &RunRecord) -> Timestamp {
    if record.state == RunState::Parked
        && let Some(park) = &record.park
    {
        return park.at;
    }
    if let Some(asking) = asking(record) {
        return asking.since;
    }
    held(record).map_or(record.entered_at, |hold| hold.since)
}

/// `<handle> held <age>: <what holds it>`, the hold as one phrase.
pub(super) fn hold_phrase(attempt: &LiveAttempt, hold: &Hold, now: Timestamp) -> String {
    format!(
        "{} held {}: {}",
        hold_handle(attempt, hold).unwrap_or("—"),
        render::age_short(hold.since, now),
        render::one_line(&hold.label()),
    )
}

pub(super) fn wait(
    record: &RunRecord,
    spec: Option<&Definition>,
    now: Timestamp,
) -> Option<String> {
    let wait = record.wait.as_ref()?;
    if let Some(check) = spec.and_then(|spec| spec.check(&wait.node))
        && check.is_sleep()
    {
        let when = check
            .when()
            .and_then(|when| when.expression().ok())
            .map_or_else(String::new, |expression| format!(" when {expression}"));
        return Some(format!(
            "sleeping {}/{}{when} → {}",
            render::age_short(record.entered_at, now),
            check.sleep().unwrap_or_default(),
            check.then().first().map_or("", Target::as_str)
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
        "hold" if wait.unknown.is_some() => match wait.unknown.as_deref().unwrap_or_default() {
            "" => "unknown".into(),
            reason => format!("unknown: {}", render::one_line(reason)),
        },
        "hold" => {
            let hold = spec
                .and_then(|spec| spec.check(&wait.node))
                .and_then(|check| {
                    wait.case.and_then(|index| {
                        check
                            .cases()
                            .get(index as usize)
                            .and_then(|case| Some(case.hold()?.text().to_owned()))
                    })
                })
                .unwrap_or_else(|| "?".into());
            format!(
                "holding {} {}/{hold}",
                render::one_line(wait.detail.as_deref().unwrap_or("—")),
                render::age_short(wait.first, now),
            )
        }
        "unknown" | "retry" | "window" | "budget" => match wait.detail.as_deref() {
            Some(detail) => format!(
                "{}{}{detail}",
                wait.outcome,
                if matches!(wait.outcome.as_str(), "window" | "budget") {
                    " "
                } else {
                    ": "
                },
                detail = render::one_line(detail),
            ),
            None => wait.outcome.clone(),
        },
        "occupied" => occupied(wait.detail.as_deref()),
        _ => wait.outcome.clone(),
    };
    let recheck = wait
        .recheck_at
        .map(|at| format!(", recheck {}", render::workflow_clock(at, now)))
        .unwrap_or_default();
    Some(format!("{detail}{recheck}"))
}

/// An `occupied` wait's phrase: its detail holds the occupants' handles, absent on a wait
/// written before they were kept.
pub(super) fn occupied(occupants: Option<&str>) -> String {
    occupants.map_or_else(
        || "occupied".into(),
        |occupants| format!("occupied by {occupants}"),
    )
}

pub(super) fn park(
    record: &RunRecord,
    spec: Option<&Definition>,
    now: Timestamp,
) -> Option<String> {
    if record.state != RunState::Parked {
        return None;
    }
    let park = record.park.as_ref()?;
    let derived = match park.reason {
        ParkReason::Cap => spec
            .and_then(|spec| spec.step(&record.node))
            .and_then(|step| step.tripped_visit_bound(record))
            .map(|(used, max)| format!("{used} of {max} visits")),
        ParkReason::Timeout => spec
            .and_then(|spec| {
                spec.check(&record.node)
                    .and_then(|check| check.timeout())
                    .or_else(|| spec.step(&record.node)?.timeout())
            })
            .map(|timeout| format!("waited {}", timeout.text())),
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
    spec: Option<&Definition>,
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
    if let Some((attempt, hold)) = holder(record) {
        let suffix = if with_you(record) {
            with_you_suffix(record, spec, now)
        } else {
            String::new()
        };
        return format!("{}{suffix}", hold_phrase(attempt, hold, now));
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
    if let Some(step) = spec.and_then(|spec| spec.step(&record.node)) {
        return step.visit_cap(record.visit_grant).map_or_else(
            || "running".into(),
            |max| {
                format!(
                    "running · visit {} of {max}",
                    record.visits_to(&record.node)
                )
            },
        );
    }
    judge(record).map_or_else(|| "checking".into(), |judge| format!("checking in {judge}"))
}

pub(super) fn with_you_suffix(
    record: &RunRecord,
    spec: Option<&Definition>,
    now: Timestamp,
) -> String {
    let phrase = wait(record, spec, now);
    if record.held.is_some() {
        let wait = phrase.map_or_else(String::new, |phrase| format!(": {phrase}"));
        return format!(" · checking {}{wait}", record.node);
    }
    match (record.wait.as_ref(), phrase) {
        (Some(wait), Some(phrase)) => format!(" · {}: {phrase}", wait.node),
        _ => String::new(),
    }
}

/// The word a run's state is shown by: `tried` for a try that ended done, else the state's own.
pub(super) fn state_word(state: RunState, ending: Option<&RunEnding>) -> &'static str {
    if ending == Some(&RunEnding::Tried) {
        "tried"
    } else {
        state.as_str()
    }
}

/// The glyph of a run in a state that ends it; any other state reads as failed.
pub(super) fn ended_glyph_role(state: RunState) -> GlyphRole {
    match state {
        RunState::Done => GlyphRole::StatusDone,
        RunState::Canceled => GlyphRole::StatusIdle,
        RunState::Failed | RunState::Active | RunState::Parked => GlyphRole::StatusAttention,
    }
}

pub(super) fn glyph_role(record: &RunRecord) -> GlyphRole {
    match record.state {
        state if state.is_terminal() => return ended_glyph_role(state),
        RunState::Parked => return GlyphRole::WorkflowHeld,
        _ => {}
    }
    if calls_you(record) {
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
    use rimz::store::workflow::ScopeKind;
    use serde_json::json;

    fn record() -> RunRecord {
        serde_json::from_value(json!({
            "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"land",
            "spec_dir":"/spec", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/lane"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"active", "node":"triage", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":2,
        }))
        .unwrap()
    }

    fn session(record: &mut RunRecord, held: bool) {
        let attempt = json!({
            "kind":"agent", "lifecycle":"session", "seq":1, "node":"triage",
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
    fn a_judge_reads_as_checking_in_its_pane_and_shows_its_ask_and_hold() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
        let mut record = record();
        session(&mut record, false);
        let attempt = record.attempt.as_mut().unwrap();
        attempt.kind = AttemptKind::Judge;
        assert_eq!(detail(&record, None, now, None), "checking in @fox");
        let attempt = record.attempt.as_mut().unwrap();
        attempt.held_on = Some(
            serde_json::from_value(
                json!({"since":Timestamp::UNIX_EPOCH, "handle":"@fox", "stop":"error"}),
            )
            .unwrap(),
        );
        assert_eq!(
            detail(&record, None, now, None),
            "@fox held 2m: errored turn"
        );
        assert!(calls_you(&record));
        let attempt = record.attempt.as_mut().unwrap();
        attempt.asking = Some(
            serde_json::from_value(json!({"since":Timestamp::UNIX_EPOCH,
                "text":"Which base?", "kind":"question", "handle":"@fox"}))
            .unwrap(),
        );
        assert_eq!(
            detail(&record, None, now, None),
            "asking you: @fox (question): Which base?"
        );
    }

    /// A held session's rest-check judge asks or is held: the phrase names the judge's agent,
    /// and a hold that carries no handle names its attempt's.
    #[test]
    fn a_rest_check_judge_held_without_a_handle_names_its_own_agent() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
        let mut record = record();
        session(&mut record, true);
        let mut judge = record.held.as_ref().unwrap().attempt.clone();
        judge.kind = AttemptKind::Judge;
        judge.node = "ruled".into();
        judge.seq = 2;
        judge.run.as_mut().unwrap().handle = "@owl".into();
        judge.held_on = Some(
            serde_json::from_value(json!({"since":Timestamp::UNIX_EPOCH, "stop":"error"})).unwrap(),
        );
        record.attempt = Some(judge);
        assert_eq!(
            detail(&record, None, now, None),
            "@owl held 2m: errored turn · checking ruled"
        );
    }

    #[test]
    fn a_header_names_its_run_by_the_records_own_address() {
        let cases = [
            (ScopeKind::Worktree, "/repo/a#b", "land#a-b"),
            (ScopeKind::Project, "/repo/a#b", "land#main"),
            (ScopeKind::Worktree, "/repo", "land#main"),
        ];
        for (kind, path, address) in cases {
            let mut record = record();
            record.scope.kind = kind;
            record.scope.path = path.into();
            assert_eq!(
                history::address(&record.header()).as_deref(),
                Some(address),
                "{kind:?} {path}"
            );
            assert_eq!(record.address().to_string(), address, "{kind:?} {path}");
        }
    }

    #[test]
    fn wait_outcomes_are_count_free_status_phrases() {
        let cases = [
            ("pending", "pending"),
            ("hold", "holding pending 0s/?"),
            ("unknown", "unknown: pending"),
            ("window", "window pending"),
            ("budget", "budget pending"),
            ("retry", "retry: pending"),
            ("takeover", "takeover"),
            ("occupied", "occupied by pending"),
            ("capacity", "held 0s · pending"),
        ];
        let actual = cases.map(|(outcome, _)| {
            let mut record = record();
            waiting(&mut record, outcome);
            wait(&record, None, Timestamp::UNIX_EPOCH).unwrap()
        });
        assert_eq!(actual, cases.map(|(_, expected)| expected.to_owned()));
        let mut occupied = record();
        waiting(&mut occupied, "occupied");
        occupied.wait.as_mut().unwrap().detail = Some("@otter, @fox".into());
        assert_eq!(
            wait(&occupied, None, Timestamp::UNIX_EPOCH).unwrap(),
            "occupied by @otter, @fox"
        );
        // A wait written before the occupants were stored names nobody.
        occupied.wait.as_mut().unwrap().detail = None;
        assert_eq!(
            wait(&occupied, None, Timestamp::UNIX_EPOCH).unwrap(),
            "occupied"
        );
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
    fn a_hold_names_its_stop_and_calls_you_only_when_no_wait_clears_it() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
        let error = json!({"stop":"error", "class":"failed", "text":"API Error: 500"});
        let cases = [
            (
                false,
                error.clone(),
                "@fox held 2m: errored turn (provider error): API Error: 500",
                true,
            ),
            (
                false,
                json!({"stop":"error"}),
                "@fox held 2m: errored turn",
                true,
            ),
            (
                false,
                json!({"stop":"error", "class":"paused_rate_limit"}),
                "@fox held 2m: rate limit",
                false,
            ),
            (
                false,
                json!({"stop":"budget"}),
                "@fox held 2m: budget park",
                false,
            ),
            (
                true,
                error,
                "@fox held 2m: errored turn (provider error): API Error: 500 · checking ruled",
                true,
            ),
        ];
        let actual = cases.clone().map(|(held, stop, _, _)| {
            let mut record = record();
            session(&mut record, held);
            let mut hold = json!({"since":Timestamp::UNIX_EPOCH, "handle":"@fox"});
            hold.as_object_mut()
                .unwrap()
                .extend(stop.as_object().unwrap().clone());
            let attempt = match record.held.as_mut() {
                Some(held) => &mut held.attempt,
                None => record.attempt.as_mut().unwrap(),
            };
            attempt.held_on = Some(serde_json::from_value(hold).unwrap());
            (
                detail(&record, None, now, Some("@yagni")),
                calls_you(&record),
                since(&record),
            )
        });
        assert_eq!(
            actual,
            cases.map(|(_, _, detail, calls)| (detail.to_owned(), calls, Timestamp::UNIX_EPOCH))
        );
    }

    #[test]
    fn sleeps_replace_every_wait_phrase_with_the_entry_based_clock() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(12);
        let actual = ["", "when=['ci=passed']\n"].map(|when| {
            let spec: Definition = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='ruled'\n\
                 [sleep.ruled]\nsleep='30m'\nthen='fix'\n{when}"
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
    fn run_glyph_prioritizes_attention_capacity_and_live_sessions() {
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
        let actual = cases.map(|(state, live, outcome, hold, _)| {
            let mut record = record();
            record.state = serde_json::from_value(json!(state)).unwrap();
            if live {
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
