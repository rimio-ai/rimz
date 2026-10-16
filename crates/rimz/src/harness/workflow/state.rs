use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::WorkflowSpec;
use crate::disk::paths::WorkflowPaths;
use crate::store::workflow::{
    AttemptOutcome, JournalEntry, JournalEvent, Key, Park, ParkReason, PreviousAttempt, RunRecord,
    RunState, Wait,
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
        digests: BTreeMap<String, String>,
        stderr_tail: Option<String>,
    ) -> Result<(), TimeInputErr> {
        if !self.current(seq) {
            return Ok(());
        }
        let check = &self.spec.checks[&self.record.node];
        let object =
            object.and_then(|object| normalize_object(object, check, self.record.key.as_ref()));
        let (object, outcome, target, reason) = match object {
            Ok(object) => {
                let outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
                let target = check.on[&outcome].clone();
                (object, outcome, target, ParkReason::User)
            }
            Err(reason) => (
                self.engine_object(
                    json!({"outcome":"invalid", "node":self.record.node, "reason":reason}),
                ),
                "invalid".into(),
                check
                    .on
                    .get("invalid")
                    .cloned()
                    .unwrap_or_else(|| "@user".into()),
                ParkReason::Invalid,
            ),
        };
        let started_at = self
            .record
            .attempt
            .as_ref()
            .map(|a| a.started_at)
            .unwrap_or(self.now);
        self.push(
            Some(seq),
            JournalEvent::Evaluation {
                outcome: outcome.clone(),
                cost_usd: None,
                retry: None,
                object: object.clone(),
                digests,
                started_at,
                ended_at: self.now,
                stderr_tail: if reason == ParkReason::Invalid {
                    stderr_tail
                } else {
                    None
                },
            },
        );
        self.record.attempt = None;
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

    pub(super) fn close_wait(&mut self) {
        if let Some(wait) = self.record.wait.take() {
            self.push(
                None,
                JournalEvent::Wait {
                    outcome: wait.outcome,
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
            let limit = self.timeout_at()?;
            let outcome = object["outcome"].as_str().unwrap_or_default().to_owned();
            let recheck_at = object
                .get("recheck_at")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Timestamp>().ok())
                .map(|at| limit.map_or(at, |limit| at.min(limit)));
            if self
                .record
                .wait
                .as_ref()
                .is_some_and(|w| w.outcome != outcome)
            {
                self.close_wait();
            }
            let wait = self.record.wait.get_or_insert_with(|| Wait {
                node: self.record.node.clone(),
                outcome,
                first: self.now,
                last: self.now,
                count: 0,
                recheck_at,
            });
            wait.last = self.now;
            wait.count += 1;
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
                }
            }
        }
        self.record.attempt = None;
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

    pub(super) fn end_step(&mut self, outcome: AttemptOutcome, digests: BTreeMap<String, String>) {
        self.kill_child();
        let Some(attempt) = self.record.attempt.take() else {
            return;
        };
        let visits = &self.record.visits[&attempt.node];
        let visit = visits
            .keys
            .iter()
            .find(|v| v.key == self.record.key)
            .map_or(0, |v| v.visits);
        self.push(
            Some(attempt.seq),
            JournalEvent::Attempt {
                handle: None,
                run_id: None,
                head_start: None,
                head_end: None,
                cost_usd: None,
                children: None,
                limited: None,
                asked: None,
                reason: None,
                outcome,
                visit,
                key: self.record.key.clone(),
                digests,
                started_at: attempt.started_at,
                ended_at: self.now,
            },
        );
        self.record.prev_attempt = Some(PreviousAttempt {
            step: attempt.node,
            outcome,
            seq: attempt.seq,
            visit,
            key: self.record.key.clone(),
            started_at: attempt.started_at,
            ended_at: self.now,
            handle: None,
            run_id: None,
            claim_path: self.paths.claim(attempt.seq),
            head_start: None,
            head_end: None,
            children: None,
            limited: None,
            asked: None,
            cost_usd: None,
        });
    }

    pub(super) fn finish_step(
        &mut self,
        outcome: AttemptOutcome,
        digests: BTreeMap<String, String>,
    ) -> Result<(), TimeInputErr> {
        self.end_step(outcome, digests);
        let target = self.spec.steps[&self.record.node]
            .then
            .as_deref()
            .unwrap_or("@user")
            .to_owned();
        self.route(
            &target,
            self.record.input.clone().unwrap_or_else(|| json!({})),
            ParkReason::User,
        )
    }

    pub(super) fn abandon_check(&mut self, outcome: &str) {
        self.kill_child();
        if let Some(attempt) = self.record.attempt.take() {
            self.push(
                Some(attempt.seq),
                JournalEvent::Evaluation {
                    cost_usd: None,
                    retry: None,
                    outcome: outcome.into(),
                    object: Value::Null,
                    digests: BTreeMap::new(),
                    started_at: attempt.started_at,
                    ended_at: self.now,
                    stderr_tail: None,
                },
            );
        }
    }

    pub(super) fn timeout_at(&self) -> Result<Option<Timestamp>, TimeInputErr> {
        let (timeout, start) = if let Some(check) = self.spec.checks.get(&self.record.node) {
            (check.timeout.as_deref(), self.record.entered_at)
        } else {
            let Some(attempt) = self.record.attempt.as_ref() else {
                return Ok(None);
            };
            (
                self.spec
                    .steps
                    .get(&self.record.node)
                    .and_then(|step| step.timeout.as_deref()),
                attempt.started_at,
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
        if !self.record.deadline_fired
            && let Some(deadline) = self.record.deadline
            && deadline <= self.now
        {
            if self.spec.steps.contains_key(&self.record.node) {
                self.end_step(AttemptOutcome::Canceled, BTreeMap::new());
            } else {
                self.abandon_check("canceled");
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
        if self.spec.steps.contains_key(&self.record.node) {
            self.finish_step(AttemptOutcome::Timeout, BTreeMap::new())?;
        } else {
            self.abandon_check("timeout");
            let check = &self.spec.checks[&self.record.node];
            let target = check
                .on
                .get("timeout")
                .map(String::as_str)
                .unwrap_or("@user");
            self.engine_route(
                "timeout",
                json!({"outcome":"timeout", "node":self.record.node, "timeout":check.timeout}),
                target,
                ParkReason::Timeout,
            )?;
        }
        Ok(true)
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
        // An absent token cannot prove ownership of a PID recycled since publication.
        if let Some(start) = &child.start
            && crate::proc::process_start_token(child.pid).as_ref() == Some(start)
            && let Ok(pid) = i32::try_from(child.pid)
        {
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
    if matches!(outcome, "timeout" | "invalid" | "cap" | "deadline")
        || !check.on.contains_key(outcome)
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
