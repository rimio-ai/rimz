use jiff::Timestamp;
use serde_json::{Value, json};

use super::WorkflowSpec;
use super::spec::RESERVED_OUTCOMES;
use crate::disk::paths::WorkflowPaths;
use crate::store::workflow::{
    AttemptKind, AttemptOutcome, Children, EndCause, Ending, EvaluationOrigin, JournalEntry,
    JournalEvent, Key, LiveAttempt, Park, ParkReason, PreviousAttempt, RunRecord, RunState, Wait,
};
use crate::utils::time::{DurationUnit, TimeInputErr, parse_duration_units};

pub(super) struct Transition<'a> {
    pub record: RunRecord,
    pub spec: &'a WorkflowSpec,
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
                    &self.spec.checks[&self.record.node],
                    self.record.key.as_ref(),
                ),
                "invalid",
            ),
            Err(reason) => (Err(reason), result.failure),
        };
        if let Ok(object) = object {
            self.record.check_retry_context = None;
            self.finish_check(seq, Ok(object), result.stderr_tail)?;
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
        let object = self.engine_object(json!({"outcome":failure, "reason":reason}));
        self.close_wait();
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Engine,
                outcome: failure.into(),
                object: object.clone(),
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
        match failure {
            "canceled" => Ok(true),
            "limited" => {
                self.record.recheck_at =
                    self.bounded_recheck(result.limited.and_then(|limited| limited.reset_at))?;
                Ok(false)
            }
            _ if self.record.check_retry == 0 => {
                self.record.check_retry = 1;
                self.record.check_retry_context = Some(json!({"outcome":failure,"reason":reason}));
                Ok(true)
            }
            _ => {
                self.route("@user", object, ParkReason::Check)?;
                Ok(false)
            }
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

    pub(super) fn finish_check(
        &mut self,
        seq: u64,
        object: Result<Value, String>,
        stderr_tail: Option<String>,
    ) -> Result<(), TimeInputErr> {
        if !self.current(seq) {
            return Ok(());
        }
        let check = &self.spec.checks[&self.record.node];
        let edges = check.edges();
        let object =
            object.and_then(|object| normalize_object(object, check, self.record.key.as_ref()));
        let (mut object, mut outcome, mut target, reason, origin) = match object {
            Ok(object) => {
                let outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
                let target = edges[&outcome].clone();
                (
                    object,
                    outcome,
                    target,
                    ParkReason::User,
                    EvaluationOrigin::Check,
                )
            }
            Err(reason) => (
                self.engine_object(
                    json!({"outcome":"invalid", "node":self.record.node, "reason":reason}),
                ),
                "invalid".into(),
                edges["invalid"].clone(),
                ParkReason::Invalid,
                EvaluationOrigin::Engine,
            ),
        };
        if matches!(check.kind(), Some(super::spec::CheckKind::Condition))
            && let Some(case) = case_index(&object)
            && check
                .cases
                .get(case as usize)
                .is_some_and(|case| case.hold.is_some())
            && !self
                .hold_until(case)?
                .is_some_and(|until| until <= self.now)
        {
            outcome = "hold".into();
            object["outcome"] = json!(outcome);
            target = "@wait".into();
        }
        // current verified that this sequence owns the live attempt.
        let attempt = self.record.attempt.take().expect("current check attempt");
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                origin,
                outcome: outcome.clone(),
                cost_usd: None,
                retry: None,
                object: object.clone(),
                digests: attempt.digests,
                started_at: attempt.started_at,
                ended_at: self.now,
                stderr_tail: if reason == ParkReason::Invalid {
                    stderr_tail
                } else {
                    None
                },
            },
        );
        if reason == ParkReason::Invalid {
            self.push(
                Some(seq),
                JournalEvent::Route {
                    outcome,
                    input: object.clone(),
                },
            );
        }
        self.route(&target, object, reason)
    }

    fn hold_until(&self, case: u32) -> Result<Option<Timestamp>, TimeInputErr> {
        let Some(hold) = self.spec.checks[&self.record.node]
            .cases
            .get(case as usize)
            .and_then(|case| case.hold.as_deref())
        else {
            return Ok(None);
        };
        let first = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.is_hold() && wait.case == Some(case))
            .map_or(self.now, |wait| wait.first);
        Ok(first.checked_add(duration(hold)?).ok())
    }

    pub(super) fn close_wait(&mut self) {
        if let Some(wait) = self.record.wait.take() {
            self.push(
                None,
                JournalEvent::Wait {
                    outcome: wait.outcome,
                    detail: wait.detail,
                    first: wait.first,
                    last: wait.last,
                    count: wait.count,
                    recheck_at: wait.recheck_at,
                },
            );
        }
        self.record.recheck_at = None;
    }

    fn route(
        &mut self,
        target: &str,
        object: Value,
        reason: ParkReason,
    ) -> Result<(), TimeInputErr> {
        if self.record.state.is_terminal() {
            return Ok(());
        }
        if target == "@wait" {
            let outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
            let case = (outcome == "hold").then(|| case_index(&object)).flatten();
            let detail = if let Some(case) = case {
                Some(
                    self.spec.checks[&self.record.node].cases[case as usize]
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
            let requested = if let Some(case) = case {
                self.hold_until(case)?
            } else {
                object
                    .get("recheck_at")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Timestamp>().ok())
            };
            let recheck_at = self.bounded_recheck(requested)?;
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
                first: self.now,
                last: self.now,
                count: 0,
                recheck_at,
            });
            wait.last = self.now;
            wait.count += 1;
            wait.detail = detail;
            wait.recheck_at = recheck_at;
            self.record.recheck_at = recheck_at;
        } else {
            self.close_wait();
            match target {
                "@done" | "@failed" => {
                    self.record.state = if target == "@done" {
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
                "@user" => {
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
                node => {
                    self.record.node = node.into();
                    self.record.entered_at = self.now;
                    self.record.check_retry = 0;
                    self.record.check_retry_context = None;
                }
            }
        }
        self.record.attempt = None;
        if self.record.wait.as_ref().is_some_and(Wait::is_hold) {
            return Ok(());
        }
        self.record.key = object
            .get("key")
            .and_then(|key| serde_json::from_value(key.clone()).ok());
        self.record.input = Some(object);
        Ok(())
    }

    fn engine_object(&self, mut object: Value) -> Value {
        if let Some(key) = &self.record.key {
            object["key"] = json!(key);
        }
        object
    }

    pub(super) fn engine_route(
        &mut self,
        outcome: &str,
        object: Value,
        target: &str,
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
        // A step's live attempt and visit are published together by prepare.
        let attempt = self.record.attempt.as_ref().expect("live step attempt");
        let visits = &self.record.visits[&attempt.node];
        let visit = visits.for_key(self.record.key.as_ref());
        PreviousAttempt {
            step: attempt.node.clone(),
            outcome,
            seq: attempt.seq,
            visit,
            key: self.record.key.clone(),
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
        self.push(
            Some(attempt.seq),
            JournalEvent::Attempt {
                outcome: previous.outcome,
                visit: previous.visit,
                key: previous.key.clone(),
                digests: attempt.digests,
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
                reason,
            },
        );
        if ending.cause == EndCause::Cancel {
            self.close_wait();
            self.record.state = RunState::Canceled;
            self.record.terminal_at = Some(self.now);
            self.push(None, JournalEvent::Cancel);
            self.push(
                None,
                JournalEvent::Terminal {
                    state: RunState::Canceled,
                },
            );
            return Ok(());
        }
        if ending.cause == EndCause::Deadline {
            self.record.deadline_fired = true;
            return self.engine_route(
                "deadline",
                json!({"outcome":"deadline", "deadline":self.record.deadline}),
                self.spec.workflow.on.deadline.as_deref().unwrap_or("@user"),
                ParkReason::Deadline,
            );
        }
        if ending.outcome == AttemptOutcome::Asked {
            return self.engine_route(
                "asked",
                json!({
                    "outcome":"asked", "reason":previous.asked.as_ref().map(|ask| &ask.text),
                    "asked":previous.asked
                }),
                self.spec.steps[&self.record.node]
                    .on
                    .asked
                    .as_deref()
                    .unwrap_or("@user"),
                ParkReason::Asked,
            );
        }
        let target = self.spec.steps[&self.record.node]
            .then
            .as_deref()
            .unwrap_or("@user")
            .to_owned();
        self.route(
            &target,
            self.engine_object(self.record.input.clone().unwrap_or_else(|| json!({}))),
            ParkReason::User,
        )
    }

    pub(super) fn finish_script(
        &mut self,
        ending: Ending,
        code: Option<i32>,
        head_end: Option<String>,
        reason: Option<String>,
    ) -> Result<bool, TimeInputErr> {
        if !self.spec.steps.contains_key(&self.record.node) {
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
        let timeout = self.timeout_at()?;
        let deadline = self.record.deadline.filter(|_| !self.record.deadline_fired);
        let limit = timeout.into_iter().chain(deadline).min();
        Ok(at.map(|at| limit.map_or(at, |limit| at.min(limit))))
    }

    pub(super) fn timeout_at(&self) -> Result<Option<Timestamp>, TimeInputErr> {
        let (timeout, start) = if let Some(check) = self.spec.checks.get(&self.record.node) {
            (check.timeout.as_deref(), self.record.entered_at)
        } else {
            let Some(attempt) = self.record.attempt.as_ref() else {
                return Ok(None);
            };
            if !attempt.lifecycle.is_turn() || attempt.asking.is_some() {
                return Ok(None);
            }
            let Ok(start) = attempt
                .started_at
                .checked_add(std::time::Duration::from_millis(attempt.asked_ms))
            else {
                return Ok(None);
            };
            (
                self.spec
                    .steps
                    .get(&self.record.node)
                    .and_then(|step| step.timeout.as_deref()),
                start,
            )
        };
        let Some(raw) = timeout else {
            return Ok(None);
        };
        Ok(start.checked_add(duration(raw)?).ok())
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
        if !self.record.deadline_fired
            && let Some(deadline) = self.record.deadline
            && deadline <= self.now
        {
            if self.finish_script(
                Ending {
                    outcome: AttemptOutcome::Canceled,
                    cause: EndCause::Deadline,
                },
                None,
                None,
                None,
            )? {
                return Ok(true);
            }
            self.record.deadline_fired = true;
            let target = self.spec.workflow.on.deadline.as_deref().unwrap_or("@user");
            self.engine_route(
                "deadline",
                json!({"outcome":"deadline", "deadline":deadline}),
                target,
                ParkReason::Deadline,
            )?;
            return Ok(true);
        }
        if !self.timeout_at()?.is_some_and(|at| at <= self.now) {
            return Ok(false);
        }
        if self.finish_script(
            Ending {
                outcome: AttemptOutcome::Timeout,
                cause: EndCause::Timeout,
            },
            None,
            None,
            None,
        )? {
            return Ok(true);
        }
        let check = &self.spec.checks[&self.record.node];
        let edges = check.edges();
        let target = &edges["timeout"];
        self.engine_route(
            "timeout",
            json!({"outcome":"timeout", "node":self.record.node, "timeout":check.timeout}),
            target,
            ParkReason::Timeout,
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
        self.spec.steps.contains_key(&self.record.node)
            && !crate::proc::process_is_live(child.pid, child.start.as_deref())
            && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref())
    }

    fn kill_child(&self) {
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
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn duration(raw: &str) -> Result<std::time::Duration, TimeInputErr> {
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )
}

fn case_index(object: &Value) -> Option<u32> {
    object
        .get("case")
        .and_then(Value::as_u64)
        .and_then(|case| u32::try_from(case).ok())
}

fn normalize_object(
    object: Value,
    check: &super::Check,
    key: Option<&Key>,
) -> Result<Value, String> {
    let Value::Object(mut object) = object else {
        return Err("stdout must contain one JSON object".into());
    };
    let outcome = object
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or("outcome must be a string")?;
    if (RESERVED_OUTCOMES.contains(&outcome)
        && !(outcome == "unknown"
            && matches!(check.kind(), Some(super::spec::CheckKind::Condition))))
        || !check.edges().contains_key(outcome)
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
