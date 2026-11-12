use std::collections::BTreeSet;

use serde_json::Value;

use super::{Check, CheckKind, Fireability, HopKind, Step, WorkflowSpec, routing_hop};
use crate::disk::paths::WorkflowPaths;
use crate::disk::summary::FileSummary;
use crate::harness::prose::{span, verbatim};
use crate::harness::when::WhenExpr;
use crate::store::workflow::{
    self, AttemptOutcome, JournalEvent, Key, RunFire, RunRecord, StepLifecycle, TurnEndAction,
};

const DEFINITION: &str = "a graph of steps and checks that the user defined. RimZ launches an agent for each agent step, and after each step runs a check whose answer picks the next node.";
const MORE: &str = "`rimz workflow show` draws the graph with this run's position and trail, and `rimz workflow show --history` lists every step and check so far; a subagent of yours runs the same commands to find this run.";

pub(super) fn step(paths: &WorkflowPaths, spec: &WorkflowSpec, record: &RunRecord) -> String {
    let step = &spec.steps[&record.node];
    let mut origin = origin(record);
    origin.push_str(" Your prompt is the step's fixed text, not a message someone just typed.");
    if record.prompt.is_some() {
        origin.push_str(if record.fire.is_some() {
            " The loop fire's prompt ends it: the task's prompt, what fired it, or both."
        } else {
            " The text given with `--prompt` to `rimz workflow run` ends it."
        });
    }
    let mut arrival = Vec::new();
    if let Ok(journal) = workflow::journal(paths) {
        let sender = routing_hop(spec, &journal, &record.node, record.held.as_ref());
        arrival.push(match sender {
            Some(hop) if hop.kind == HopKind::Restart => {
                "The user restarted this step; its visits start over.".into()
            }
            Some(hop) if hop.kind == HopKind::Resume => {
                "The user resumed the run at this step after it parked.".into()
            }
            Some(hop) => {
                // A routing hop always names its outcome.
                let outcome = hop.outcome.as_deref().expect("routing hop outcome");
                match spec.checks.get(&hop.node) {
                    Some(check) if check.is_sleep() && outcome == "then" => {
                        format!(
                            "The run slept at {} before reaching this step.",
                            span(&hop.node)
                        )
                    }
                    Some(check) if !matches!(outcome, "invalid" | "timeout" | "deadline") => {
                        let detail = record
                            .input
                            .as_ref()
                            .filter(|input| input["outcome"] == outcome)
                            .and_then(|input| detail(check, input))
                            .map_or_else(String::new, |detail| format!(" ({detail})"));
                        format!(
                            "You were launched because the check {} answered {}{detail}.",
                            span(&hop.node),
                            span(outcome)
                        )
                    }
                    _ => format!(
                        "The {} route from {} sent the run here.",
                        span(outcome),
                        span(&hop.node)
                    ),
                }
            }
            None => "You are the run's first step.".into(),
        });
    }
    if let Some(visits) = visits(spec, step, record) {
        arrival.push(visits);
    }
    if let Some(previous) = &record.prev_attempt {
        let messaged = spec
            .steps
            .get(&previous.step)
            .is_some_and(|step| matches!(step.kind(), Some(super::StepKind::Message(_))));
        let who = match &previous.handle {
            None => format!("the step {}", span(&previous.step)),
            Some(handle) if messaged => {
                format!("{} messaged {}", span(&previous.step), span(handle))
            }
            Some(handle) => format!("{} in {}", verbatim(handle), span(&previous.step)),
        };
        let ended = match previous.outcome {
            AttemptOutcome::Ended => "ended normally",
            AttemptOutcome::Died => "died",
            AttemptOutcome::Timeout => "timed out",
            AttemptOutcome::Canceled => "was canceled",
            AttemptOutcome::Limited => "hit a provider limit",
            AttemptOutcome::Asked => "ended on a blocking question",
        };
        arrival.push(format!("The previous attempt, {who}, {ended}."));
    }
    let (mut after, questions) = lifecycle(spec, step, record);
    for check in &step.then {
        if let Some(note) = check_note(spec, check) {
            after.push(' ');
            after.push_str(&note);
        }
    }
    let memory = match record.notes.as_ref().filter(|_| step.notes != Some(false)) {
        Some(_) => paths.notes().canonicalize().ok().map(|folder| {
            format!(
                "`{}/{}-notes.md` is this step's working memory. Only this step's agent writes it, across visits, restarts and its subagents. Write it for someone arriving cold. Note any change you made outside this repository: pushes, PR or issue comments, databases, remote services. {MORE}",
                folder.display(), record.node,
            )
        }),
        None => None,
    };
    let more = memory.unwrap_or_else(|| MORE.into());
    [
        position(spec, record, "one step of", "step"),
        origin,
        arrival.join(" "),
        after,
        questions,
        more,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

pub(super) fn judge(spec: &WorkflowSpec, record: &RunRecord) -> String {
    [
        position(spec, record, "a check in", "check"),
        origin(record),
        "Your answer decides where the run goes next.".into(),
        "`rimz workflow show $RIMZ_WORKFLOW_RUN --history` lists every step and check so far."
            .into(),
    ]
    .join("\n\n")
}

fn position(spec: &WorkflowSpec, record: &RunRecord, what: &str, node_kind: &str) -> String {
    format!(
        "You are {what} a RimZ workflow: {DEFINITION} You are the {node_kind} {} of the workflow {} (→ a route, ⇄ a step that returns to its check, [ ] you):\n\n{}",
        span(&record.node),
        span(&record.workflow),
        rail(spec, &record.node),
    )
}

fn rail(spec: &WorkflowSpec, current: &str) -> String {
    let walk = spec.walk();
    let mut groups: Vec<Vec<&str>> = Vec::new();
    for node in &walk.nodes {
        if node.hangs_under.is_some() {
            // Walk places a returning step immediately after its sending check or sibling.
            groups
                .last_mut()
                .expect("walk placed the parent")
                .push(&node.node);
        } else {
            groups.push(vec![&node.node]);
        }
    }
    let name = |node: &str| {
        let node_text = verbatim(node);
        if node == current {
            format!("[{node_text}]")
        } else {
            node_text
        }
    };
    let mut text = String::new();
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            let routed = groups[index - 1].iter().any(|node| {
                spec.edges(node).iter().any(|edge| {
                    edge.target == group[0]
                        && edge.fires != Fireability::Never
                        && edge.outcome != "deadline"
                })
            });
            text.push_str(if routed { " → " } else { " · " });
        }
        text.push_str(&name(group[0]));
        if group.len() > 1 {
            text.push_str(" ⇄ ");
            text.push_str(
                &group[1..]
                    .iter()
                    .map(|node| name(node))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
    text
}

fn origin(record: &RunRecord) -> String {
    match &record.fire {
        None => "The user started this run by hand with `rimz workflow run`.".into(),
        Some(RunFire::Manual { task }) => format!(
            "The user started this run by hand with {}.",
            span(&format!("rimz loop fire {task}"))
        ),
        Some(RunFire::Scheduled { task, trigger }) => {
            let clause = trigger
                .as_ref()
                .map_or_else(String::new, |clause| format!(", {clause}"));
            format!(
                "RimZ started this run automatically{clause} (loop task {}).",
                span(task)
            )
        }
    }
}

fn detail(check: &Check, input: &Value) -> Option<String> {
    let text = if let Some(reason) = input["reason"].as_str() {
        reason.to_owned()
    } else {
        if check.condition_cases().is_empty() {
            return None;
        }
        let readings = input["readings"].as_object()?;
        let keys = input["case"]
            .as_u64()
            .and_then(|case| usize::try_from(case).ok())
            .and_then(|case| {
                check
                    .condition_cases()
                    .get(case)
                    .and_then(|case| WhenExpr::parse(&case.when).ok())
            })
            .map(|expression| {
                expression
                    .terms()
                    .map(|term| term.key.as_str().to_owned())
                    .collect::<BTreeSet<_>>()
            });
        readings
            .iter()
            .filter(|(key, _)| keys.as_ref().is_none_or(|keys| keys.contains(*key)))
            .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("unread")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if text.is_empty() {
        return None;
    }
    let mut bounded: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        bounded.push('…');
    }
    Some(verbatim(&bounded))
}

fn check_note(spec: &WorkflowSpec, name: &str) -> Option<String> {
    let check = spec.checks.get(name)?;
    if !check.condition_cases().is_empty() {
        let cases = check
            .condition_cases()
            .iter()
            .map(|case| {
                let expression = WhenExpr::parse(&case.when).ok()?;
                Some(format!(
                    "{}: {} → {}",
                    span(&case.outcome),
                    span(&expression.to_string()),
                    span(&spec.engine_target(name, &case.outcome)),
                ))
            })
            .collect::<Option<Vec<_>>>()?
            .join("; ");
        return Some(format!(
            "The check {} tests these cases in order: {cases}.",
            span(name)
        ));
    }
    let name = span(name);
    let summary = check.summary.as_deref().map(span);
    let clause = summary
        .as_ref()
        .map_or_else(String::new, |summary| format!(" ({summary})"));
    Some(match check.kind()? {
        CheckKind::Run(command) => format!(
            "The check {name} runs {}{clause}. Words containing a `/` must name files inside the definition folder; bare commands use PATH. It runs from the checkout. You may run this command yourself.",
            span(command)
        ),
        CheckKind::Shell(command) => format!(
            "The check {name} runs {}{clause} through `sh -c`; it runs from the checkout. `$RIMZ_WORKFLOW_DEFINITION` names the definition folder for a script there. You may run this command yourself.",
            span(command)
        ),
        CheckKind::Agent(_) => summary.map_or_else(
            || format!("The check {name} is an agent judge."),
            |summary| format!("The check {name} tests {summary}."),
        ),
        CheckKind::Sleep => format!(
            "The check {name} tests {}.",
            span(check.summary.as_deref().or(check.sleep.as_deref())?)
        ),
        CheckKind::Condition => unreachable!("condition cases returned their note above"),
    })
}

pub(super) fn push(
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Result<String, super::SpecErr> {
    // A pending push has already applied this check's answer as the step's input.
    let pushed = workflow::journal(paths).ok().and_then(|journal| {
        let message = &record.attempt.as_ref()?.push.as_ref()?.message;
        let mut entries = journal.iter().rev();
        let name = entries.by_ref().find_map(|entry| match &entry.event {
            JournalEvent::TurnEnd {
                action: TurnEndAction::Pushed,
                message: Some(id),
                check,
                ..
            } if id == message => Some(check),
            _ => None,
        })?;
        let seq = entries
            .find(|entry| {
                entry.node == *name && matches!(entry.event, JournalEvent::Evaluation { .. })
            })
            .and_then(|entry| entry.seq);
        Some((name.clone(), seq))
    });
    let name = pushed.as_ref().map(|(name, _)| name).unwrap_or_else(|| {
        spec.steps[&record.node]
            .then
            .first()
            .expect("validated step then")
    });
    let check = &spec.checks[name];
    let input = record.input.as_ref().expect("published push-back answer");
    let mut text = format!(
        "The workflow check {} answered {}.",
        span(name),
        span(input["outcome"].as_str().unwrap_or_default())
    );
    if check.condition_cases().is_empty() {
        if let Some(reason) = input["reason"].as_str() {
            text.push_str(&format!("\n\n{reason}"));
        }
    } else {
        let marks = super::case_marks(check, input, None)?;
        for (case, mark) in check.condition_cases().iter().zip(marks.cases) {
            if !matches!(mark, super::CaseMark::False | super::CaseMark::Unread) {
                break;
            }
            let expression = WhenExpr::parse(&case.when)?;
            let mut terms = Vec::new();
            for (term, negated) in expression.leaves() {
                let reading = input["readings"][&term.key].as_str();
                let label = format!("{}{term}", if negated { "!" } else { "" });
                match reading {
                    None => terms.push(format!("{label}: unread")),
                    Some(reading) if term.matches(reading) == negated => {
                        terms.push(format!(
                            "{label}: false ({})",
                            term.comparison_evidence(reading)
                        ));
                    }
                    Some(_) => {}
                }
            }
            text.push_str(&format!(
                "\n\nCase {} ({}): {}. {}",
                span(&case.outcome),
                span(&expression.to_string()),
                if mark == super::CaseMark::False {
                    "false"
                } else {
                    "unread"
                },
                terms.join("; ")
            ));
        }
    }
    if let Some((_, seq)) = &pushed
        && matches!(
            check.kind(),
            Some(CheckKind::Run(_) | CheckKind::Shell(_) | CheckKind::Agent(_))
        )
    {
        let output = seq
            .map(|seq| {
                if check.agent.is_some() {
                    paths.check_output(seq)
                } else {
                    paths.check_log(seq)
                }
            })
            .and_then(|path| {
                let summary = FileSummary::measure(&path)
                    .ok()
                    .filter(|summary| !summary.is_empty())?;
                Some(format!("output: {} ({})", path.display(), summary.label()))
            })
            .unwrap_or_else(|| "no output".into());
        text.push_str(&format!("\n\n{output}"));
    }
    text.push_str("\n\nContinue working on this step. The run moves on when the check passes.");
    Ok(text)
}

fn visits(spec: &WorkflowSpec, step: &Step, record: &RunRecord) -> Option<String> {
    let (outcome, key) = record.visit_bucket();
    let mut counts = Vec::new();
    let mut last = Vec::new();
    let resets = step
        .max_visits_per_outcome
        .as_ref()
        .map_or(&[][..], |cap| cap.reset_on())
        .iter()
        .map(|outcome| span(outcome))
        .collect::<Vec<_>>()
        .join(" or ");
    for (bound, used, max) in step.visit_bounds(
        record.visits.get(&record.node),
        record.visit_bucket(),
        record.visit_grant,
    ) {
        let next = if bound == "max-visits" {
            counts.push(format!(
                "This is visit {used} of {max} to {}",
                span(&record.node)
            ));
            "one more visit".to_owned()
        } else {
            let Some(outcome) = outcome else {
                continue;
            };
            let mut count = if counts.is_empty() {
                format!(
                    "This is the {} visit to {} for {}",
                    ordinal(used),
                    span(&record.node),
                    span(outcome)
                )
            } else {
                format!("the {} for {}", ordinal(used), span(outcome))
            };
            if let Some(key) = key {
                let key = match key {
                    Key::String(key) => key.clone(),
                    Key::Integer(key) => key.to_string(),
                    Key::Unsigned(key) => key.to_string(),
                };
                count.push_str(&format!(" with key {}", span(&key)));
            }
            let mut next = format!("one more {}", span(outcome));
            if !resets.is_empty() {
                count.push_str(&format!(" since the last {resets}"));
                next.push_str(&format!(" before a {resets}"));
            }
            counts.push(count);
            next
        };
        if used == max {
            // Every step has a cap destination, including an unfireable cap.
            let target = spec.target(&record.node, "cap").expect("step cap target");
            let destination = match target.as_str() {
                "@user" => "parks the run for the user".into(),
                "@done" => "ends the run as done".into(),
                "@failed" => "ends the run as failed".into(),
                _ => format!("sends the run to {}", span(&target)),
            };
            last.push(format!("{next} {destination}"));
        }
    }
    if counts.is_empty() {
        return None;
    }
    let mut text = counts.join(", and ");
    if !last.is_empty() {
        text.push_str(&format!(": {}", last.join("; ")));
    }
    Some(format!("{text}. Launches and push-backs count as visits."))
}

fn ordinal(count: u64) -> String {
    match count {
        1 => "first".into(),
        2 => "second".into(),
        3 => "third".into(),
        4 => "fourth".into(),
        5 => "fifth".into(),
        6 => "sixth".into(),
        7 => "seventh".into(),
        8 => "eighth".into(),
        9 => "ninth".into(),
        10 => "tenth".into(),
        11 => "eleventh".into(),
        12 => "twelfth".into(),
        _ => {
            let suffix = if (11..=13).contains(&(count % 100)) {
                "th"
            } else {
                match count % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                }
            };
            format!("{count}{suffix}")
        }
    }
}

fn lifecycle(spec: &WorkflowSpec, step: &Step, record: &RunRecord) -> (String, String) {
    if spec.is_break(&record.node) {
        return (
            "You stay open across turns. Fix the cause and resume the run, cancel it, or wait for the user. RimZ detaches rather than stops you on resume or cancel. Closing your pane ends the run @failed.".into(),
            "If the user must act now, ask through your own native question; otherwise end your turn and wait for their instructions.".into(),
        );
    }
    // Validation requires a nonempty `then`: checks, or one terminal on a turn or stay step.
    let first = step.then.first().expect("validated step then");
    let check = span(first);
    let next = if matches!(first.as_str(), "@done" | "@failed") {
        format!("ends the run at {check}")
    } else {
        format!("runs the check {check}")
    };
    match step.lifecycle() {
        StepLifecycle::Turn => {
            let cleanup = if step.keep.unwrap_or(false) {
                "leaves your pane open outside this run"
            } else {
                "closes your pane"
            };
            let timeout = step.timeout.as_ref().map_or_else(String::new, |timeout| {
                format!(", within the step's {} timeout", verbatim(timeout))
            });
            let after = format!(
                "This is one turn. When it ends cleanly with nothing owed, RimZ {cleanup} and {next}. Your final message is kept in the run's history but does not choose the next node. A subagent you launch keeps your turn open until it reports. A wait you arm keeps your pane open until it fires{timeout}. No signals reach you. Errors, provider limits, interrupts, and budget parks leave you open for a later clean turn."
            );
            let questions = if step.on.asked.is_some() {
                // An explicit asked edge always has a resolved target.
                let target = spec
                    .target(&record.node, "asked")
                    .expect("step asked target");
                format!(
                    "A blocking question ends this attempt, and the run goes to {}.",
                    span(&target)
                )
            } else {
                let watching = if matches!(record.fire, Some(RunFire::Scheduled { .. })) {
                    "Nobody is watching. "
                } else {
                    ""
                };
                let pause = if step.timeout.is_some() {
                    ", and the timeout pauses meanwhile"
                } else {
                    ""
                };
                format!("{watching}A question waits in your pane until the user answers it{pause}.")
            };
            (after, questions)
        }
        lifecycle @ (StepLifecycle::Stay | StepLifecycle::Until) => {
            let mut after = if lifecycle == StepLifecycle::Stay {
                format!(
                    "You stay open across turns until your provider process exits, as when the user closes your pane. RimZ then closes every cell and {next}."
                )
            } else {
                let close = if step.keep.unwrap_or(false) {
                    "Other routes move the run on; RimZ leaves your pane open outside this run."
                } else {
                    "Other routes close your pane and move the run on."
                };
                let listed = if step.then.len() > 1 {
                    " You stay open while the run moves between the listed checks; your next turn end starts at the first check."
                } else {
                    ""
                };
                let timeout = step.timeout.as_ref().map_or_else(String::new, |timeout| {
                    format!(
                        " The step's {} timeout counts from your launch and ends this session when it passes.",
                        verbatim(timeout)
                    )
                });
                format!(
                    "After each of your turns that rests cleanly with nothing owed, RimZ runs the check {check}.{listed} A not-done answer routed back to this step becomes your next prompt in this session. A @wait answer keeps you open while signals and the minute tick recheck it. {close} Errors, provider limits, interrupts, and budget parks wait for a later clean turn.{timeout}"
                )
            };
            if let Some(subscriptions) = subscriptions(step) {
                after.push_str(&format!(
                    " You subscribe to {subscriptions}. Each arrives as a SIGNAL message at one of your turn boundaries while this step lasts."
                ));
            }
            (
                after,
                "The user may answer in this pane later; a question goes in your final message."
                    .into(),
            )
        }
    }
}

fn subscriptions(step: &Step) -> Option<String> {
    let mut labels = step
        .subscription_labels()
        .ok()?
        .into_iter()
        .map(|(selector, scope)| format!("{} {scope}", span(&selector)))
        .collect::<Vec<_>>();
    let last = labels.pop()?;
    Some(match labels.is_empty() {
        true => last,
        false => format!("{}, and {last}", labels.join(", ")),
    })
}
