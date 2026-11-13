use jiff::Timestamp;
use serde_json::{Value, json};

use super::Definition;
use super::definition::Target;
use super::spec::{self, Cause};
use crate::disk::paths::WorkflowPaths;
use crate::harness::headless::judge::FailureKind;
use crate::store::workflow::{
    AttemptKind, AttemptOutcome, Children, EndCause, Ending, EvaluationOrigin, JournalEntry,
    JournalEvent, Key, LiveAttempt, Park, ParkReason, PreviousAttempt, RunRecord, RunState,
    TurnEndAction, Verdict, Wait,
};
use crate::utils::time::TimeInputErr;

/// What a failed judge evaluation does next, decided once in `Transition::finish_judge`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JudgeFailure {
    /// The evaluation was canceled.
    Canceled,
    /// A provider limit held the judge; it rechecks after the reset.
    Limited,
    /// The judge's account may not spend: the check fails `limited`.
    Budget,
    /// The first failure; the judge runs once more.
    Retry,
    /// A second failure, routed through the failure chain under its own cause.
    Route,
}

pub(super) struct Transition<'a> {
    pub record: RunRecord,
    pub spec: &'a Definition,
    pub paths: &'a WorkflowPaths,
    pub now: Timestamp,
    pub entries: Vec<JournalEntry>,
}

impl Transition<'_> {
    pub(super) fn finish_judge(
        &mut self,
        seq: u64,
        result: super::script::Evaluation,
    ) -> Result<bool, TimeInputErr> {
        if !self.current(seq) {
            return Ok(false);
        }
        let attempt = self
            .record
            .attempt
            .as_ref()
            .expect("current judge attempt")
            .clone();
        let (object, failure) = match result.object {
            Ok(object) => (
                normalize_object(
                    object,
                    self.spec
                        .check(&self.record.node)
                        .expect("current judge check"),
                    self.record.key.as_ref(),
                    &self.spec.edges(&self.record.node),
                ),
                "invalid",
            ),
            Err(reason) => (Err(reason), result.failure),
        };
        if let Ok(object) = object {
            self.record.check_retry_context = None;
            self.finish_check(seq, Ok(object), json!({}), result.stderr_tail)?;
            for entry in &mut self.entries {
                if entry.seq == Some(seq)
                    && let JournalEvent::Evaluation {
                        cost_usd, retry, ..
                    } = &mut entry.event
                {
                    *cost_usd = result.cost_usd;
                    *retry = attempt.retry.clone();
                }
            }
            return Ok(false);
        }
        // The successful case returned above.
        let reason = object.expect_err("failed judge evaluation");
        let kind = FailureKind::from_label(failure);
        let disposition = match kind {
            _ if failure == "budget" => JudgeFailure::Budget,
            Some(FailureKind::Canceled) => JudgeFailure::Canceled,
            Some(FailureKind::Limited) => JudgeFailure::Limited,
            Some(kind) if kind.retry_owed(self.record.check_retry > 0) => JudgeFailure::Retry,
            _ => JudgeFailure::Route,
        };
        let cause = match disposition {
            JudgeFailure::Budget => Some(Cause::Limited),
            JudgeFailure::Route => Some(Cause::parse(failure).unwrap_or(Cause::Invalid)),
            _ => None,
        };
        let node = self.record.node.clone();
        let object = match cause {
            Some(cause) => self.failure_input(&node, cause, &reason, &json!({})),
            None => self.engine_object(json!({"outcome":failure, "reason":reason})),
        };
        self.close_wait();
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Engine,
                outcome: cause.map_or(failure, Cause::word).into(),
                object,
                digests: attempt.digests,
                started_at: attempt.started_at,
                ended_at: self.now,
                stderr_tail: result.stderr_tail,
                cost_usd: result.cost_usd,
                retry: attempt.retry,
            },
        );
        self.record.attempt = None;
        self.record.recheck_at = None;
        if let Some(cause) = cause {
            self.fail(Some(seq), &node, cause, reason, json!({}))?;
            return Ok(false);
        }
        match disposition {
            JudgeFailure::Limited => {
                self.record.recheck_at =
                    self.bounded_recheck(result.limited.and_then(|limited| limited.reset_at))?;
                Ok(false)
            }
            JudgeFailure::Retry => {
                self.record.check_retry = 1;
                self.record.check_retry_context = Some(json!({"outcome":failure,"reason":reason}));
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    pub(super) fn current(&self, seq: u64) -> bool {
        self.record.state == RunState::Active
            && self.record.attempt.as_ref().is_some_and(|a| a.seq == seq)
    }

    pub(super) fn push(&mut self, seq: Option<u64>, event: JournalEvent) {
        self.entries.push(JournalEntry {
            seq,
            at: self.now,
            node: self.record.node.clone(),
            event,
        });
    }

    /// Settle the current check's answer, or fail it with `detail`, the evidence its process
    /// left (`log`, and `code` once it exited), when the answer is refused.
    pub(super) fn finish_check(
        &mut self,
        seq: u64,
        object: Result<Value, String>,
        detail: Value,
        stderr_tail: Option<String>,
    ) -> Result<(), TimeInputErr> {
        if !self.current(seq) {
            return Ok(());
        }
        // current verified that this sequence owns the live check attempt.
        let check = self.spec.check(&self.record.node).expect("current check");
        let edges = self.spec.edges(&self.record.node);
        let object = object
            .and_then(|object| normalize_object(object, check, self.record.key.as_ref(), &edges));
        let mut object = match object {
            Ok(object) => object,
            Err(reason) => {
                let cause = match check.kind() {
                    Some(super::Kind::Condition(_) | super::Kind::Sleep(_)) => None,
                    Some(super::Kind::Test(_)) => Some(Cause::Died),
                    _ => Some(Cause::Invalid),
                };
                return self.check_failure(seq, cause, reason, detail, stderr_tail);
            }
        };
        let mut outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
        let mut target = self.spec.engine_target(&self.record.node, &outcome);
        if matches!(
            check.kind(),
            Some(super::Kind::Condition(_) | super::Kind::Sleep(_))
        ) && let Some(case) = case_index(&object)
            && check
                .cases()
                .get(case as usize)
                .is_some_and(|case| case.hold().is_some())
            && !self
                .hold_until(case)?
                .is_some_and(|until| until <= self.now)
        {
            outcome = "hold".into();
            object["outcome"] = json!(outcome);
            target = Target::Wait;
        }
        // current verified that this sequence owns the live attempt.
        let attempt = self.record.attempt.take().expect("current check attempt");
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Check,
                outcome,
                cost_usd: None,
                retry: None,
                object: object.clone(),
                digests: attempt.digests,
                started_at: attempt.started_at,
                ended_at: self.now,
                stderr_tail: None,
            },
        );
        self.route(&target, object, ParkReason::User)
    }

    /// Settle the current check's evaluation as a failure of `cause`, or, with none, as an
    /// engine fault that ends the run failed with no route (a condition's or sleep's
    /// reading error).
    pub(super) fn check_failure(
        &mut self,
        seq: u64,
        cause: Option<Cause>,
        reason: String,
        detail: Value,
        stderr_tail: Option<String>,
    ) -> Result<(), TimeInputErr> {
        if !self.current(seq) {
            return Ok(());
        }
        // current verified that this sequence owns the live attempt.
        let attempt = self.record.attempt.take().expect("current check attempt");
        let node = self.record.node.clone();
        let object = match cause {
            Some(cause) => self.failure_input(&node, cause, &reason, &detail),
            None => self.engine_object(json!({"outcome":"fault", "node":node, "reason":reason})),
        };
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Engine,
                outcome: cause.map_or("fault", Cause::word).into(),
                cost_usd: None,
                retry: None,
                object,
                digests: attempt.digests,
                started_at: attempt.started_at,
                ended_at: self.now,
                stderr_tail,
            },
        );
        match cause {
            Some(cause) => self.fail(Some(seq), &node, cause, reason, detail),
            None => self.fault(reason),
        }
    }

    fn hold_until(&self, case: u32) -> Result<Option<Timestamp>, TimeInputErr> {
        super::hold_until(self.spec, &self.record, case, self.now)
    }

    /// Whether the held `case` reads true or unknown on the readings an unknown answer carries.
    fn hold_survives(&self, case: u32, object: &Value) -> bool {
        let readings = object
            .get("readings")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), value.as_str().map(str::to_owned)))
            .collect();
        self.spec
            .check(&self.record.node)
            .and_then(|check| check.cases().get(case as usize))
            .and_then(|case| case.when().expression().ok())
            .is_some_and(|expression| expression.truth(&readings) != Some(false))
    }

    pub(super) fn close_wait(&mut self) {
        if let Some(wait) = self.record.wait.take() {
            self.entries.push(JournalEntry {
                seq: None,
                at: self.now,
                node: wait.node,
                event: JournalEvent::Wait {
                    outcome: wait.outcome,
                    detail: wait.detail,
                    hold: wait.hold,
                    first: wait.first,
                    last: wait.last,
                    count: wait.count,
                    recheck_at: wait.recheck_at,
                },
            });
        }
        self.record.recheck_at = None;
    }

    pub(super) fn restore_session(&mut self) {
        // Callers restore only a published held session.
        let held = self.record.held.take().expect("held session to restore");
        self.record.node = held.attempt.node.clone();
        self.record.entered_at = held.entered_at;
        self.record.attempt = Some(held.attempt);
        self.record.check_retry = 0;
        self.record.check_retry_context = None;
        self.record.launch_retry = false;
        self.record.recheck_at = None;
    }

    pub(super) fn turn_end(
        &mut self,
        attempt: &LiveAttempt,
        action: TurnEndAction,
        verdict: Option<&Verdict>,
        reason: Option<String>,
    ) {
        // A held session consumed a turn before it left the attempt slot.
        let turn = attempt.judged.clone().expect("consumed leader turn end");
        self.entries.push(JournalEntry {
            seq: Some(attempt.seq),
            at: self.now,
            node: attempt.node.clone(),
            event: JournalEvent::TurnEnd {
                action,
                turn,
                check: verdict
                    .map_or_else(|| self.record.node.clone(), |verdict| verdict.node.clone()),
                // A failure verdict names its cause, as its Route entry does.
                outcome: verdict.and_then(|verdict| {
                    let object = &verdict.object;
                    let field = if object["outcome"] == "failed" {
                        "cause"
                    } else {
                        "outcome"
                    };
                    object[field].as_str().map(str::to_owned)
                }),
                target: verdict.map(|verdict| verdict.target.clone()),
                reason,
                visit: (action == TurnEndAction::Pushed)
                    .then_some(attempt.visit)
                    .flatten(),
                message: (action == TurnEndAction::Pushed)
                    .then(|| attempt.push.as_ref().map(|push| push.message.clone()))
                    .flatten(),
            },
        });
    }

    pub(super) fn merge_wait(
        &mut self,
        object: &Value,
        at: Timestamp,
        recheck: bool,
    ) -> Result<(), TimeInputErr> {
        let outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
        let case = (outcome == "hold").then(|| case_index(object)).flatten();
        let detail = if let Some(case) = case {
            Some(
                // A hold wait names a case of its own check.
                self.spec
                    .check(&self.record.node)
                    .expect("held check")
                    .cases()[case as usize]
                    .outcome
                    .clone(),
            )
        } else if outcome == "unknown" {
            object
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        let recheck_at = if recheck {
            let requested = if let Some(case) = case {
                self.hold_until(case)?
            } else {
                object
                    .get("recheck_at")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Timestamp>().ok())
            };
            self.bounded_recheck(requested)?
        } else {
            None
        };
        let held = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.is_hold())
            .and_then(|wait| wait.case);
        // A hold keeps its start through unknown; only its own case reading false ends it.
        if outcome == "unknown"
            && held.is_some_and(|case| self.hold_survives(case, object))
            && let Some(wait) = self.record.wait.as_mut()
        {
            wait.last = at;
            wait.count += 1;
            wait.unknown = Some(detail.unwrap_or_default());
            wait.recheck_at = recheck_at;
            self.record.recheck_at = recheck_at;
            return Ok(());
        }
        if self
            .record
            .wait
            .as_ref()
            .is_some_and(|w| (w.outcome.as_str(), w.case) != (outcome.as_str(), case))
        {
            self.close_wait();
        }
        let wait = self.record.wait.get_or_insert_with(|| Wait {
            node: self.record.node.clone(),
            outcome,
            case,
            detail: None,
            unknown: None,
            hold: None,
            kinds: Vec::new(),
            gives_up_at: None,
            first: at,
            last: at,
            count: 0,
            recheck_at,
        });
        wait.last = at;
        wait.count += 1;
        wait.detail = detail;
        wait.unknown = None;
        wait.recheck_at = recheck_at;
        self.record.recheck_at = recheck_at;
        Ok(())
    }

    pub(super) fn route(
        &mut self,
        target: &Target,
        object: Value,
        reason: ParkReason,
    ) -> Result<(), TimeInputErr> {
        if self.record.state.is_terminal() {
            return Ok(());
        }
        let sleep = self
            .spec
            .check(&self.record.node)
            .is_some_and(super::Node::is_sleep);
        if let Some(held) = &mut self.record.held {
            held.verdict = Some(Verdict {
                node: self.record.node.clone(),
                entered_at: self.record.entered_at,
                at: self.now,
                target: target.to_string(),
                object,
                reason,
            });
            self.record.attempt = None;
            return Ok(());
        }
        if *target == Target::Wait {
            self.merge_wait(&object, self.now, true)?;
        } else {
            self.close_wait();
            match target {
                Target::Done | Target::Failed => {
                    self.record.state = if *target == Target::Done {
                        RunState::Done
                    } else {
                        RunState::Failed
                    };
                    self.record.terminal_at = Some(self.now);
                    self.push(
                        None,
                        JournalEvent::Terminal {
                            state: self.record.state,
                        },
                    );
                }
                Target::User => {
                    let text = object
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    self.record.state = RunState::Parked;
                    self.record.park = Some(Park {
                        reason,
                        text: text.clone(),
                        at: self.now,
                    });
                    self.push(None, JournalEvent::Park { reason, text });
                }
                Target::Node(node) => {
                    // A pending restart belongs to the step it restarts.
                    if *node != self.record.node {
                        self.record.restart = None;
                    }
                    self.enter(node.clone());
                }
                // The outer branch took the wait.
                Target::Wait => unreachable!("routed wait"),
            }
        }
        self.record.attempt = None;
        self.hand_on(object, sleep);
        Ok(())
    }

    /// Move the run onto `node`, its entry fresh.
    pub(super) fn enter(&mut self, node: String) {
        self.record.node = node;
        self.record.entered_at = self.now;
        self.record.check_retry = 0;
        self.record.check_retry_context = None;
        self.record.launch_retry = false;
    }

    /// Hand a route's `object` to the next node as its input and key. A failure always does;
    /// an answer that `keeps_input` (a sleep's, or one between a session's listed rest checks),
    /// and one that leaves a hold standing, keep the input the node was given.
    pub(super) fn hand_on(&mut self, object: Value, keeps_input: bool) {
        let failure = object["outcome"] == "failed";
        if !failure && (keeps_input || self.record.wait.as_ref().is_some_and(Wait::is_hold)) {
            return;
        }
        self.record.key = object
            .get("key")
            .and_then(|key| serde_json::from_value(key.clone()).ok());
        self.record.input = Some(object);
    }

    pub(super) fn engine_object(&self, mut object: Value) -> Value {
        if let Some(key) = &self.record.key {
            object["key"] = json!(key);
        }
        object
    }

    /// The input a failure of `cause` at `node` hands the route it takes, with the run's key.
    fn failure_input(&self, node: &str, cause: Cause, reason: &str, detail: &Value) -> Value {
        self.engine_object(json!({
            "outcome":"failed", "cause":cause.word(), "node":node, "reason":reason,
            "detail":detail,
        }))
    }

    /// The failure entry: every failure, whatever raised it, builds its input here, with the
    /// run's key, journals the route under its cause, and takes the fallback chain's target.
    /// The verdict is for a caller that stops an agent before the route commits; `fail`
    /// commits it at once. `seq` is the evaluation's that raised it, if one did.
    pub(super) fn failure(
        &mut self,
        seq: Option<u64>,
        node: &str,
        cause: Cause,
        reason: String,
        detail: Value,
    ) -> Verdict {
        let object = self.failure_input(node, cause, &reason, &detail);
        self.push(
            seq,
            JournalEvent::Route {
                outcome: cause.word().into(),
                input: object.clone(),
            },
        );
        Verdict {
            node: node.into(),
            entered_at: self.record.entered_at,
            at: self.now,
            target: self.spec.failure_target(node, cause).to_string(),
            object,
            reason: match cause {
                Cause::Visits => ParkReason::Cap,
                Cause::Deadline => ParkReason::Deadline,
                Cause::Timeout => ParkReason::Timeout,
                _ => ParkReason::Invalid,
            },
        }
    }

    /// Take the failure route of `cause` out of `node` now.
    pub(super) fn fail(
        &mut self,
        seq: Option<u64>,
        node: &str,
        cause: Cause,
        reason: String,
        detail: Value,
    ) -> Result<(), TimeInputErr> {
        let verdict = self.failure(seq, node, cause, reason, detail);
        self.route(
            &Target::parse(&verdict.target),
            verdict.object,
            verdict.reason,
        )
    }

    /// The failure a spent visit cap raises at the current node, which counts no visit.
    pub(super) fn visits_failure(&mut self, visits: u64, max: u64) -> Verdict {
        let node = self.record.node.clone();
        self.failure(
            None,
            &node,
            Cause::Visits,
            format!("{node} has used {visits} of {max} visits"),
            json!({"visits":visits, "max":max}),
        )
    }

    /// End the run failed with no route: an engine fault, not a failure of the node.
    pub(super) fn fault(&mut self, reason: String) -> Result<(), TimeInputErr> {
        let object = self.engine_object(json!({"outcome":"fault", "reason":reason}));
        self.route(&Target::Failed, object, ParkReason::Invalid)
    }

    pub(super) fn engine_route(
        &mut self,
        outcome: &str,
        object: Value,
        target: &Target,
        reason: ParkReason,
    ) -> Result<(), TimeInputErr> {
        let object = self.engine_object(object);
        self.push(
            None,
            JournalEvent::Route {
                outcome: outcome.into(),
                input: object.clone(),
            },
        );
        self.route(target, object, reason)
    }

    /// Take back a published agent attempt that opened nothing, with the visit it counted.
    pub(super) fn withdraw_attempt(&mut self) {
        let Some(attempt) = self.record.attempt.take() else {
            return;
        };
        if attempt.visit.is_none() {
            return;
        }
        let Some(visits) = self.record.visits.get_mut(&attempt.node) else {
            return;
        };
        *visits = visits.saturating_sub(1);
        if *visits == 0 {
            self.record.visits.remove(&attempt.node);
        }
    }

    pub(super) fn step_wait(&mut self, outcome: &str, detail: Option<String>) {
        if self
            .record
            .wait
            .as_ref()
            .is_some_and(|wait| wait.outcome != outcome)
        {
            self.close_wait();
        }
        let wait = self.record.wait.get_or_insert_with(|| Wait {
            node: self.record.node.clone(),
            outcome: outcome.into(),
            case: None,
            detail: None,
            unknown: None,
            hold: None,
            kinds: Vec::new(),
            gives_up_at: None,
            first: self.now,
            last: self.now,
            count: 0,
            recheck_at: None,
        });
        wait.last = self.now;
        wait.count += 1;
        wait.detail = detail;
    }

    pub(super) fn stage_agent_end(&mut self, ending: Ending, observed: &super::Observed) {
        // Only published agent attempts enter the stop protocol.
        let outcome = ending.outcome;
        let attempt = self
            .record
            .attempt
            .as_mut()
            .expect("agent ending has an attempt");
        attempt.asking = None;
        attempt.held_since = None;
        attempt.ending = Some(ending);
        let mut previous = self.previous_attempt(outcome);
        previous.children = Some(observed.children.clone());
        previous.limited = (outcome == AttemptOutcome::Limited)
            .then(|| observed.limited.clone())
            .flatten();
        previous.asked = (outcome == AttemptOutcome::Asked)
            .then(|| observed.ask.as_ref().map(|asking| asking.ask.clone()))
            .flatten();
        previous.cost_usd = observed.cost_usd;
        self.record.prev_attempt = Some(previous);
    }

    fn previous_attempt(&self, outcome: AttemptOutcome) -> PreviousAttempt {
        // Both callers retain a live step attempt while recording its end.
        let attempt = self.record.attempt.as_ref().expect("live step attempt");
        let visit = attempt
            .visit
            .unwrap_or_else(|| self.record.visits_to(&attempt.node));
        PreviousAttempt {
            step: attempt.node.clone(),
            outcome,
            seq: attempt.seq,
            visit,
            key: self.record.key.clone(),
            routed_by: self.record.routed_by().map(str::to_owned),
            started_at: attempt.started_at,
            ended_at: self.now,
            handle: attempt.run.as_ref().map(|run| run.handle.clone()),
            run_id: attempt.run.as_ref().map(|run| run.run_id.clone()),
            claim_path: self.paths.claim(attempt.seq),
            head_start: attempt.head_start.clone(),
            head_end: None,
            code: None,
            log: None,
            children: None,
            limited: None,
            asked: None,
            cost_usd: None,
        }
    }

    pub(super) fn settle_agent(
        &mut self,
        mut children: Children,
        cost_usd: Option<f64>,
        head_end: Option<String>,
        reason: Option<String>,
    ) -> Result<(), TimeInputErr> {
        // stage_agent_end durably saves observations before stop can erase asks or limits.
        let attempt = self.record.attempt.take().expect("staged agent attempt");
        let ending = attempt.ending.clone().expect("staged agent ending");
        let previous = self
            .record
            .prev_attempt
            .as_mut()
            .expect("staged agent evidence");
        previous.outcome = ending.outcome;
        previous.handle = attempt.run.as_ref().map(|run| run.handle.clone());
        previous.run_id = attempt.run.as_ref().map(|run| run.run_id.clone());
        previous.ended_at = self.now;
        previous.head_end = head_end;
        previous.cost_usd = cost_usd;
        if let Some(staged) = previous.children.take() {
            for (saved, current) in [
                (staged.stopped, &mut children.stopped),
                (staged.live, &mut children.live),
            ] {
                for child in saved {
                    if !current
                        .iter()
                        .any(|item| item.kind == child.kind && item.handle == child.handle)
                    {
                        current.push(child);
                    }
                }
            }
        }
        children.live.retain(|child| {
            !children
                .stopped
                .iter()
                .any(|stopped| stopped.kind == child.kind && stopped.handle == child.handle)
        });
        previous.children = Some(children);
        if ending.outcome != AttemptOutcome::Asked {
            previous.asked = None;
        }
        if ending.outcome != AttemptOutcome::Limited {
            previous.limited = None;
        }
        self.settle_step(attempt, ending, reason)
    }

    fn settle_step(
        &mut self,
        attempt: LiveAttempt,
        ending: Ending,
        reason: Option<String>,
    ) -> Result<(), TimeInputErr> {
        // Both step runners save their evidence before entering this shared routing tail.
        let previous = self.record.prev_attempt.clone().expect("step end evidence");
        if self
            .record
            .restart
            .as_ref()
            .and_then(|restart| restart.settled)
            != Some(attempt.seq)
        {
            self.push(
                Some(attempt.seq),
                JournalEvent::Attempt {
                    outcome: previous.outcome,
                    visit: previous.visit,
                    started_past_limit: attempt.started_past_limit.clone(),
                    key: previous.key.clone(),
                    routed_by: previous.routed_by.clone(),
                    digests: attempt.digests.clone(),
                    started_at: previous.started_at,
                    ended_at: self.now,
                    handle: previous.handle,
                    run_id: previous.run_id,
                    head_start: previous.head_start,
                    head_end: previous.head_end,
                    code: previous.code,
                    log: previous.log,
                    cost_usd: previous.cost_usd,
                    children: previous.children,
                    limited: previous.limited,
                    asked: previous.asked.clone(),
                    reason: reason.clone(),
                    message: attempt.message.clone(),
                },
            );
        }
        if let Some(verdict) = ending.route {
            if attempt.judged.is_some() {
                self.turn_end(&attempt, TurnEndAction::Closed, Some(&verdict), None);
            }
            self.record.node = verdict.node;
            self.record.entered_at = verdict.entered_at;
            self.route(
                &Target::parse(&verdict.target),
                verdict.object,
                verdict.reason,
            )?;
            if ending.cause == EndCause::Resume {
                // Journaled at the node the run moved to, after the attempt it ended.
                self.push(
                    None,
                    JournalEvent::Resume {
                        visit_grant: self.record.visit_grant,
                        deadline: self.record.deadline,
                    },
                );
            }
            return Ok(());
        }
        if let EndCause::Restart { continued } = ending.cause {
            self.restart_step(&attempt.node, continued);
            return Ok(());
        }
        if ending.cause == EndCause::Cancel {
            self.close_wait();
            self.record.restart = None;
            self.record.state = RunState::Canceled;
            self.record.terminal_at = Some(self.now);
            self.push(
                None,
                JournalEvent::Cancel {
                    reason: ending.reason,
                },
            );
            self.push(
                None,
                JournalEvent::Terminal {
                    state: RunState::Canceled,
                },
            );
            return Ok(());
        }
        if ending.cause == EndCause::Deadline {
            return self.deadline_failure(&attempt.node);
        }
        if ending.outcome == AttemptOutcome::Asked {
            return self.engine_route(
                "asked",
                json!({
                    "outcome":"asked", "reason":previous.asked.as_ref().map(|ask| &ask.text),
                    "asked":previous.asked
                }),
                &self.spec.engine_target(&self.record.node, "asked"),
                ParkReason::Asked,
            );
        }
        let Some(cause) = self
            .spec
            .ending_cause(&attempt.node, ending.outcome.as_str())
        else {
            let target = self.spec.engine_target(&self.record.node, "then");
            return self.route(
                &target,
                self.engine_object(self.record.input.clone().unwrap_or_else(|| json!({}))),
                ParkReason::User,
            );
        };
        // The evidence was saved before this routing tail, as its journal entry read it.
        let evidence = self
            .record
            .prev_attempt
            .as_ref()
            .expect("step end evidence");
        let mut detail = json!({});
        if let Some(code) = evidence.code {
            detail["code"] = json!(code);
        }
        if let Some(log) = &evidence.log {
            detail["log"] = json!(log);
        }
        if let Some(reset_at) = evidence
            .limited
            .as_ref()
            .and_then(|limited| limited.reset_at)
        {
            detail["reset_at"] = json!(reset_at);
        }
        let reason = reason.unwrap_or_else(|| match cause {
            Cause::Timeout => "the step's timeout passed".into(),
            Cause::Limited => "a provider limit or budget stopped the attempt".into(),
            Cause::Undelivered => "the message was not delivered".into(),
            _ => format!("the attempt ended {}", ending.outcome.as_str()),
        });
        self.fail(None, &attempt.node, cause, reason, detail)
    }

    /// The deadline fails the node the run is at, once per grant.
    pub(super) fn deadline_failure(&mut self, node: &str) -> Result<(), TimeInputErr> {
        self.record.deadline_fired = true;
        let deadline = self.record.deadline;
        self.fail(
            None,
            node,
            Cause::Deadline,
            deadline.map_or_else(
                || "the run's deadline passed".into(),
                |deadline| format!("the run's deadline {deadline} passed"),
            ),
            json!({"deadline":deadline}),
        )
    }

    pub(super) fn reset_visits(&mut self, step: &str, outcome: &str) {
        self.record.visits.remove(step);
        self.push(
            None,
            JournalEvent::VisitsReset {
                step: step.into(),
                outcome: outcome.into(),
            },
        );
    }

    pub(super) fn restart_step(&mut self, step: &str, continued: bool) {
        self.close_wait();
        self.reset_visits(step, "restart");
        self.record.node = step.into();
        self.record.entered_at = self.now;
        self.record.park = None;
        self.record.state = RunState::Active;
        self.record.check_retry = 0;
        self.record.check_retry_context = None;
        self.record.launch_retry = false;
        self.push(
            None,
            JournalEvent::Restart {
                step: step.into(),
                continued,
            },
        );
    }

    pub(super) fn finish_script(
        &mut self,
        ending: Ending,
        code: Option<i32>,
        head_end: Option<String>,
        reason: Option<String>,
    ) -> Result<bool, TimeInputErr> {
        if self.spec.step(&self.record.node).is_none() {
            self.abandon_check(if ending.outcome == AttemptOutcome::Timeout {
                "timeout"
            } else {
                "canceled"
            });
            return Ok(false);
        }
        if self.record.attempt.is_none() {
            return Ok(false);
        }
        if matches!(
            ending.outcome,
            AttemptOutcome::Timeout | AttemptOutcome::Canceled
        ) {
            self.kill_child();
        }
        let mut previous = self.previous_attempt(ending.outcome);
        previous.code = code;
        previous.head_end = head_end;
        let log = self.paths.step_log(previous.seq);
        previous.log = log.is_file().then_some(log);
        self.record.prev_attempt = Some(previous);
        // The live attempt was checked above and remains locked through settlement.
        let attempt = self.record.attempt.take().expect("live command step");
        self.settle_step(attempt, ending, reason)?;
        Ok(true)
    }

    /// Settle a message attempt: the resolved target on delivery, the reason when undelivered.
    pub(super) fn finish_message(
        &mut self,
        outcome: AttemptOutcome,
        handle: Option<String>,
        reason: Option<String>,
    ) -> Result<(), TimeInputErr> {
        let mut previous = self.previous_attempt(outcome);
        previous.handle = handle;
        self.record.prev_attempt = Some(previous);
        // The caller checked the live attempt under this lock.
        let attempt = self.record.attempt.take().expect("live message step");
        self.settle_step(
            attempt,
            Ending {
                outcome,
                cause: EndCause::Lifecycle,
                route: None,
                reason: None,
            },
            reason,
        )
    }

    pub(super) fn abandon_check(&mut self, outcome: &str) {
        self.kill_child();
        if let Some(attempt) = self.record.attempt.take() {
            self.push(
                Some(attempt.seq),
                JournalEvent::Evaluation {
                    origin: EvaluationOrigin::Engine,
                    cost_usd: None,
                    retry: attempt.retry,
                    outcome: outcome.into(),
                    object: Value::Null,
                    digests: attempt.digests,
                    started_at: attempt.started_at,
                    ended_at: self.now,
                    stderr_tail: None,
                },
            );
        }
    }

    fn bounded_recheck(&self, at: Option<Timestamp>) -> Result<Option<Timestamp>, TimeInputErr> {
        let limit =
            super::inspect::first_bound(self.spec, &self.record, None, self.now)?.map(|(at, _)| at);
        Ok(at.map(|at| limit.map_or(at, |limit| at.min(limit))))
    }

    pub(super) fn timeout_at(&self) -> Result<Option<Timestamp>, TimeInputErr> {
        super::timeout_at(self.spec, &self.record)
    }

    pub(super) fn bound(&mut self) -> Result<bool, TimeInputErr> {
        if self.record.state != RunState::Active {
            return Ok(false);
        }
        if self
            .record
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.kind == AttemptKind::Agent)
            || self.host_owes_exit()
        {
            return Ok(false);
        }
        if self.record.held.is_none()
            && !self.record.deadline_fired
            && self
                .record
                .deadline
                .is_some_and(|deadline| deadline <= self.now)
        {
            if self.finish_script(
                Ending {
                    outcome: AttemptOutcome::Canceled,
                    cause: EndCause::Deadline,
                    route: None,
                    reason: None,
                },
                None,
                None,
                None,
            )? {
                return Ok(true);
            }
            let node = self.record.node.clone();
            self.deadline_failure(&node)?;
            return Ok(true);
        }
        if !self.timeout_at()?.is_some_and(|at| at <= self.now) {
            return Ok(false);
        }
        if self.finish_script(
            Ending {
                outcome: AttemptOutcome::Timeout,
                cause: EndCause::Timeout,
                route: None,
                reason: None,
            },
            None,
            None,
            None,
        )? {
            return Ok(true);
        }
        let timeout = self
            .spec
            .check(&self.record.node)
            .expect("a check timeout fires at its check")
            .timeout()
            .map(super::definition::Parsed::text);
        let node = self.record.node.clone();
        self.fail(
            None,
            &node,
            Cause::Timeout,
            format!(
                "{node} ran past its timeout {}",
                timeout.unwrap_or_default()
            ),
            json!({}),
        )?;
        Ok(true)
    }

    /// Whether a live host still owes the exit status of its command step's child, which
    /// exited on its own. A bound that fell due since then is the next prepare's, once the
    /// host has settled the step by that status.
    fn host_owes_exit(&self) -> bool {
        let Some(attempt) = &self.record.attempt else {
            return false;
        };
        let Some(child) = &attempt.child else {
            return false;
        };
        self.spec.step(&self.record.node).is_some()
            && !crate::proc::process_is_live(child.pid, child.start.as_deref())
            && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref())
    }

    /// Stop the live attempt's child process group, when the run still owns it.
    pub(super) fn kill_child(&self) {
        let Some(child) = self
            .record
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.child.as_ref())
        else {
            return;
        };
        let owns_group = match crate::proc::process_start_token(child.pid) {
            Some(start) => child.start.as_ref() == Some(&start),
            None => crate::proc::process_group_has_env(
                child.pid,
                "RIMZ_WORKFLOW_RUN",
                self.record.id.as_str(),
            ),
        };
        if owns_group && let Ok(pid) = i32::try_from(child.pid) {
            use nix::sys::signal::{Signal, kill, killpg};

            let pid = nix::unistd::Pid::from_raw(pid);
            // A child that has not reached its trampoline's `setsid` leads no group yet.
            if killpg(pid, Signal::SIGKILL) == Err(nix::errno::Errno::ESRCH) {
                let _ = kill(pid, Signal::SIGKILL);
            }
        }
    }
}

fn case_index(object: &Value) -> Option<u32> {
    object
        .get("case")
        .and_then(Value::as_u64)
        .and_then(|case| u32::try_from(case).ok())
}

fn normalize_object(
    object: Value,
    check: &super::Node,
    key: Option<&Key>,
    edges: &[super::Edge],
) -> Result<Value, String> {
    let Value::Object(mut object) = object else {
        return Err("stdout must contain one JSON object".into());
    };
    let outcome = object
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or("outcome must be a string")?;
    if (spec::reserved(outcome)
        && !(outcome == "unknown"
            && matches!(
                check.kind(),
                Some(super::Kind::Condition(_) | super::Kind::Sleep(_))
            )))
        || !edges.iter().any(|edge| edge.outcome == outcome)
    {
        return Err(format!("reserved or unrouted outcome {outcome:?}"));
    }
    if object.get("reason").is_some_and(|v| !v.is_string()) {
        return Err("reason must be a string".into());
    }
    if let Some(at) = object.get("recheck_at") {
        at.as_str()
            .and_then(|s| s.parse::<Timestamp>().ok())
            .ok_or("recheck_at must be an RFC 3339 instant")?;
    }
    if let Some(key) = object.get("key") {
        serde_json::from_value::<Key>(key.clone())
            .map_err(|_| "key must be a string or integer")?;
    } else if let Some(key) = key {
        object.insert("key".into(), json!(key));
    }
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests;
