use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use super::super::{
    CheckKind, Issue, Limit, RESERVED_OUTCOMES, Route, StepKind, WorkflowSpec, duration,
    parse_deadline, parse_window,
};
use super::ignored_issue;
use crate::harness::schedule::when::WhenExpr;

pub(super) fn apply(spec: &mut WorkflowSpec, indices: &BTreeMap<String, Vec<usize>>) {
    let ignored = &mut spec.ignored;
    summary(&mut spec.workflow.summary, "workflow.summary", ignored);
    sanitize(
        &mut spec.workflow.deadline,
        "workflow.deadline",
        ignored,
        |raw| parse_deadline(raw).map(|_| ()),
    );
    limit(&mut spec.workflow.limit, "workflow.limit", ignored);
    for (node, check) in &mut spec.checks {
        let path = format!("checks.{node}");
        summary(&mut check.summary, &format!("{path}.summary"), ignored);
        sanitize(
            &mut check.timeout,
            &format!("{path}.timeout"),
            ignored,
            |raw| duration(raw).map(|_| ()),
        );
        sanitize(
            &mut check.window,
            &format!("{path}.window"),
            ignored,
            |raw| parse_window(raw).map(|_| ()),
        );
        let condition = matches!(check.kind(), Some(CheckKind::Condition));
        let sleep = check.is_sleep();
        let agent = matches!(check.kind(), Some(CheckKind::Agent(_)));
        let shell = matches!(check.kind(), Some(CheckKind::Shell(_)));
        if condition || sleep {
            ignore(
                &mut check.wake,
                &format!("{path}.wake"),
                ignored,
                "field is not supported by this check kind",
            );
        }
        if !condition {
            ignore(
                &mut check.else_outcome,
                &format!("{path}.else"),
                ignored,
                "field is not supported by this check kind",
            );
        }
        if !condition && !sleep {
            ignore(
                &mut check.provider,
                &format!("{path}.provider"),
                ignored,
                "field is not supported by this check kind",
            );
            ignore(
                &mut check.account,
                &format!("{path}.account"),
                ignored,
                "field is not supported by this check kind",
            );
        }
        if sleep {
            ignore(
                &mut check.timeout,
                &format!("{path}.timeout"),
                ignored,
                "field is not supported by this check kind",
            );
            sanitize(&mut check.hold, &format!("{path}.for"), ignored, |raw| {
                let hold = duration(raw).map_err(|error| error.to_string())?;
                if hold.is_zero() {
                    return Err("for must be nonzero".to_owned());
                }
                if check.when.is_none() {
                    return Err("for requires when".into());
                }
                if check
                    .sleep
                    .as_deref()
                    .and_then(|sleep| duration(sleep).ok())
                    .is_some_and(|sleep| hold >= sleep)
                {
                    return Err("for must be shorter than sleep".into());
                }
                Ok(())
            });
        } else if check.kind().is_some() {
            ignore(
                &mut check.when,
                &format!("{path}.when"),
                ignored,
                "field is not supported by this check kind",
            );
            ignore(
                &mut check.hold,
                &format!("{path}.for"),
                ignored,
                "field is not supported by this check kind",
            );
            ignore(
                &mut check.then,
                &format!("{path}.then"),
                ignored,
                "field is not supported by this check kind",
            );
        }
        if !agent {
            ignore(
                &mut check.prompt_file,
                &format!("{path}.prompt-file"),
                ignored,
                "field is not supported by this check kind",
            );
            ignore(
                &mut check.window,
                &format!("{path}.window"),
                ignored,
                "field is not supported by this check kind",
            );
        }
        if let Some(wakes) = &mut check.wake {
            let wake_path = format!("{path}.wake");
            *wakes = std::mem::take(wakes)
                .into_iter()
                .enumerate()
                .filter_map(|(index, wake)| match wake.subscription() {
                    Ok(_) => Some(wake),
                    Err(error) => {
                        let index = indices
                            .get(&wake_path)
                            .map_or(index, |indices| indices[index]);
                        ignored.push(ignored_issue(&format!("{wake_path}.{index}"), error));
                        None
                    }
                })
                .collect();
        }
        let outcomes = check
            .cases
            .iter()
            .map(|case| case.outcome.as_str())
            .chain(check.else_outcome.as_deref())
            .collect::<BTreeSet<_>>();
        check.on.retain(|outcome, _| {
            let engine = RESERVED_OUTCOMES.contains(&outcome.as_str())
                && !matches!(outcome.as_str(), "timeout" | "invalid")
                && !(condition && outcome == "unknown");
            let dead = if shell {
                !matches!(outcome.as_str(), "pass" | "fail" | "timeout" | "invalid")
            } else {
                condition
                    && !outcomes.contains(outcome.as_str())
                    && !matches!(outcome.as_str(), "unknown" | "timeout" | "invalid")
            };
            if sleep || engine || dead {
                ignored.push(ignored_issue(
                    &format!("{path}.on.{outcome}"),
                    "dead route: this check cannot answer that outcome",
                ));
                return false;
            }
            true
        });
        for (index, case) in check.cases.iter_mut().enumerate() {
            let case_path = format!("{path}.cases.{index}.for");
            sanitize(&mut case.hold, &case_path, ignored, |raw| {
                match duration(raw) {
                    Ok(duration) if duration.is_zero() => Err("for must be nonzero".into()),
                    Ok(_) => Ok(()),
                    Err(error) => Err(error.to_string()),
                }
            });
            if check
                .on
                .get(&case.outcome)
                .is_some_and(|route| route.target() == "@wait")
            {
                ignore(
                    &mut case.hold,
                    &case_path,
                    ignored,
                    "a held case must route away from @wait",
                );
            }
        }
        if condition || sleep {
            sanitize(
                &mut check.provider,
                &format!("{path}.provider"),
                ignored,
                |provider| {
                    crate::agents::find_definition(provider.as_str())
                        .map(|_| ())
                        .ok_or_else(|| format!("unknown provider {provider}"))
                },
            );
            let window = check
                .condition_cases()
                .iter()
                .filter_map(|case| WhenExpr::parse(&case.when).ok())
                .any(|expr| expr.terms().any(|term| term.key.starts_with("window.")));
            if !window {
                ignore(
                    &mut check.provider,
                    &format!("{path}.provider"),
                    ignored,
                    "provider requires a window term",
                );
            }
            if check.provider.is_none() {
                ignore(
                    &mut check.account,
                    &format!("{path}.account"),
                    ignored,
                    "account requires provider",
                );
            }
        }
    }
    for (node, step) in &mut spec.steps {
        let path = format!("steps.{node}");
        if !matches!(step.kind(), Some(StepKind::Agent(_)))
            || step.lifecycle() == crate::store::workflow::StepLifecycle::Stay
        {
            ignore(
                &mut step.keep,
                &format!("{path}.keep"),
                ignored,
                "keep is only for turn and until agent steps",
            );
        }
        summary(&mut step.summary, &format!("{path}.summary"), ignored);
        sanitize(
            &mut step.timeout,
            &format!("{path}.timeout"),
            ignored,
            |raw| duration(raw).map(|_| ()),
        );
        sanitize(
            &mut step.window,
            &format!("{path}.window"),
            ignored,
            |raw| parse_window(raw).map(|_| ()),
        );
        sanitize(
            &mut step.max_visits,
            &format!("{path}.max-visits"),
            ignored,
            positive,
        );
        sanitize(
            &mut step.max_visits_per_outcome,
            &format!("{path}.max-visits-per-outcome"),
            ignored,
            |cap| positive(&cap.limit()),
        );
        if let (Some(cap), Some(total)) = (&step.max_visits_per_outcome, step.max_visits)
            && cap.limit() >= total
        {
            ignore(
                &mut step.max_visits_per_outcome,
                &format!("{path}.max-visits-per-outcome"),
                ignored,
                "must be below max-visits, since the total cap trips first",
            );
        }
        limit(&mut step.limit, &format!("{path}.limit"), ignored);
        if matches!(step.kind(), Some(StepKind::Run(_) | StepKind::Shell(_))) {
            ignore(
                &mut step.limit,
                &format!("{path}.limit"),
                ignored,
                "a command step has no turn to count",
            );
            ignore(
                &mut step.takeover,
                &format!("{path}.takeover"),
                ignored,
                "a command step has no occupancy to take over",
            );
            ignore(
                &mut step.throttle,
                &format!("{path}.throttle"),
                ignored,
                "capacity admission is only for agent steps",
            );
            ignore(
                &mut step.capacity_wait,
                &format!("{path}.capacity-wait"),
                ignored,
                "capacity admission is only for agent steps",
            );
            ignore(
                &mut step.on.capacity,
                &format!("{path}.on.capacity"),
                ignored,
                "capacity admission is only for agent steps",
            );
            ignore(
                &mut step.lifecycle,
                &format!("{path}.lifecycle"),
                ignored,
                "lifecycle is for agent steps",
            );
            ignore(
                &mut step.window,
                &format!("{path}.window"),
                ignored,
                "windows gate provider usage, which a command step does not use",
            );
            ignore(
                &mut step.on.asked,
                &format!("{path}.on.asked"),
                ignored,
                "a command cannot ask; its outcome goes to prev_attempt",
            );
            ignore(
                &mut step.prompt_file,
                &format!("{path}.prompt-file"),
                ignored,
                "a command step takes no prompt; it reads context.json",
            );
        }
        if matches!(step.kind(), Some(StepKind::Agent(_))) && !step.lifecycle().is_turn() {
            ignore(
                &mut step.timeout,
                &format!("{path}.timeout"),
                ignored,
                "a staying step has no step timeout; remove timeout or use lifecycle = \"turn\"",
            );
            ignore(
                &mut step.on.asked,
                &format!("{path}.on.asked"),
                ignored,
                "a staying step never ends on a question; remove on.asked or use lifecycle = \"turn\"",
            );
        }
    }
    for (node, check) in &mut spec.checks {
        let held_steps = spec
            .steps
            .iter()
            .filter(|(_, step)| {
                step.lifecycle() == crate::store::workflow::StepLifecycle::Until
                    && step.then.contains(node)
            })
            .collect::<Vec<_>>();
        for (outcome, route) in &mut check.on {
            let Route::Delayed { to, delay } = route else {
                continue;
            };
            let path = format!("checks.{node}.on.{outcome}.delay");
            sanitize(delay, &path, ignored, |raw| match duration(raw) {
                Ok(duration) if duration.is_zero() => Err("delay must be nonzero".into()),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            });
            if held_steps.is_empty()
                || to == "@wait"
                || held_steps
                    .iter()
                    .any(|(name, step)| *name == to || step.then.contains(to))
            {
                ignore(
                    delay,
                    &path,
                    ignored,
                    "delay requires an until check route that closes the held session",
                );
            }
        }
    }
}

fn limit(value: &mut Option<Limit>, path: &str, ignored: &mut Vec<Issue>) {
    let Some(limit) = value else {
        return;
    };
    sanitize(
        &mut limit.max_parallel,
        &format!("{path}.max-parallel"),
        ignored,
        |max| {
            if *max == 0 {
                Err("must be at least 1")
            } else {
                Ok(())
            }
        },
    );
    sanitize(
        &mut limit.stagger,
        &format!("{path}.stagger"),
        ignored,
        |raw| match duration(raw) {
            Ok(stagger) if stagger.is_zero() => Err("must be greater than zero".to_string()),
            Ok(_) => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    );
    if limit.max_parallel.is_none() && limit.stagger.is_none() {
        ignore(value, path, ignored, "set max-parallel or stagger");
    }
}

fn positive(value: &u64) -> Result<(), &'static str> {
    if *value == 0 {
        Err("must be at least 1")
    } else {
        Ok(())
    }
}

fn summary(value: &mut Option<String>, path: &str, ignored: &mut Vec<Issue>) {
    sanitize(value, path, ignored, |text| {
        if text.trim().is_empty() || text.contains(['\n', '\r']) {
            Err("a summary is one nonempty line")
        } else {
            Ok(())
        }
    });
}

fn sanitize<T, E: Display>(
    value: &mut Option<T>,
    path: &str,
    ignored: &mut Vec<Issue>,
    validate: impl FnOnce(&T) -> Result<(), E>,
) {
    if let Some(error) = value.as_ref().and_then(|value| validate(value).err()) {
        ignore(value, path, ignored, error);
    }
}

fn ignore<T>(value: &mut Option<T>, path: &str, ignored: &mut Vec<Issue>, reason: impl Display) {
    if value.take().is_some() {
        ignored.push(ignored_issue(path, reason));
    }
}
