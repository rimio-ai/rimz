use std::collections::BTreeSet;

use serde_json::Value;

use super::{Definition, Fireability, HopKind, Invocation, Kind, Node, Target, routing_hop};
use crate::disk::paths::WorkflowPaths;
use crate::disk::summary::FileSummary;
use crate::harness::prose::{span, verbatim};
use crate::store::workflow::{
    self, AttemptOutcome, JournalEvent, RunFire, RunRecord, TurnEndAction,
};

const DEFINITION: &str = "a graph of steps and checks that the user defined. RimZ launches an agent for each agent step, and after each step runs a check whose answer picks the next node.";
const MORE: &str = "`rimz workflow show` draws the graph with this run's position and trail, and `rimz workflow show --history` lists every step and check so far; a subagent of yours runs the same commands to find this run.";

pub(super) fn step(paths: &WorkflowPaths, spec: &Definition, record: &RunRecord) -> String {
    let step = spec
        .step(&record.node)
        .expect("a step brief is composed at the current step");
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
                match spec.check(&hop.node) {
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
        let messaged = matches!(spec.step_kind(&previous.step), Some(Kind::Message(_)));
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
    let mut noted = Vec::new();
    for check in step.rest_check().iter().chain(step.then()) {
        if noted.contains(&check) {
            continue;
        }
        noted.push(check);
        if let Some(note) = check_note(spec, check.as_str()) {
            after.push(' ');
            after.push_str(&note);
        }
    }
    let memory = match record.notes.as_ref().filter(|_| step.notes() != Some(false)) {
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

pub(super) fn judge(spec: &Definition, record: &RunRecord) -> String {
    [
        position(spec, record, "a check in", "check"),
        origin(record),
        "Your answer decides where the run goes next.".into(),
        "`rimz workflow show $RIMZ_WORKFLOW_RUN --history` lists every step and check so far."
            .into(),
    ]
    .join("\n\n")
}

fn position(spec: &Definition, record: &RunRecord, what: &str, node_kind: &str) -> String {
    format!(
        "You are {what} a RimZ workflow: {DEFINITION} You are the {node_kind} {} of the workflow {} (→ a route, ⇄ a step that returns to its check, [ ] you):\n\n{}",
        span(&record.node),
        span(&record.workflow),
        rail(spec, &record.node),
    )
}

fn rail(spec: &Definition, current: &str) -> String {
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
                    edge.target.node() == Some(&*group[0])
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
        Some(RunFire::Manual { task, .. }) => format!(
            "The user started this run by hand with {}.",
            span(&format!("rimz loop fire {task}"))
        ),
        Some(RunFire::Scheduled { task, trigger, .. }) => {
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

fn detail(check: &Node, input: &Value) -> Option<String> {
    let text = if let Some(reason) = input["reason"].as_str() {
        reason.to_owned()
    } else {
        if check.cases().is_empty() {
            return None;
        }
        let readings = input["readings"].as_object()?;
        let keys = input["case"]
            .as_u64()
            .and_then(|case| usize::try_from(case).ok())
            .and_then(|case| {
                check
                    .cases()
                    .get(case)
                    .and_then(|case| case.when().expression().ok())
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

fn check_note(spec: &Definition, name: &str) -> Option<String> {
    let node = spec.check(name)?;
    if !node.cases().is_empty() {
        let cases = node
            .cases()
            .iter()
            .map(|case| {
                let expression = case.when().expression().ok()?;
                Some(format!(
                    "{}: {} → {}",
                    span(&case.outcome),
                    span(&expression.to_string()),
                    span(spec.engine_target(name, &case.outcome).as_str()),
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
    let summary = node.summary().map(span);
    let clause = summary
        .as_ref()
        .map_or_else(String::new, |summary| format!(" ({summary})"));
    Some(match node.kind()? {
        Kind::Test(Invocation::Run(command)) | Kind::Script(Invocation::Run(command)) => format!(
            "The check {name} runs {}{clause}. Words containing a `/` must name files inside the definition folder; bare commands use PATH. It runs from the checkout. You may run this command yourself.",
            span(command.text())
        ),
        Kind::Test(Invocation::Shell(command)) | Kind::Script(Invocation::Shell(command)) => {
            format!(
                "The check {name} runs {}{clause} through `sh -c`; it runs from the checkout. `$RIMZ_WORKFLOW_DEFINITION` names the definition folder for a script there. You may run this command yourself.",
                span(command)
            )
        }
        Kind::Judge(_) => summary.map_or_else(
            || format!("The check {name} is an agent judge."),
            |summary| format!("The check {name} tests {summary}."),
        ),
        Kind::Sleep(sleep) => format!(
            "The check {name} tests {}.",
            span(node.summary().unwrap_or(sleep.duration().text()))
        ),
        Kind::Condition(_) => unreachable!("condition cases returned their note above"),
        Kind::Command(_) | Kind::Agent(_) | Kind::Message(_) => {
            unreachable!("a check runs no step kind")
        }
    })
}

pub(super) fn push(
    paths: &WorkflowPaths,
    spec: &Definition,
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
    let name = pushed.as_ref().map_or_else(
        || {
            spec.step(&record.node)
                .and_then(|step| step.rest_check().first())
                .expect("a push-back comes from a rest check")
                .as_str()
        },
        |(name, _)| name.as_str(),
    );
    let check = spec.check(name).expect("pushed check");
    let input = record.input.as_ref().expect("published push-back answer");
    let mut text = format!(
        "The workflow check {} answered {}.",
        span(name),
        span(input["outcome"].as_str().unwrap_or_default())
    );
    if check.cases().is_empty() {
        if let Some(reason) = input["reason"].as_str() {
            text.push_str(&format!("\n\n{reason}"));
        }
    } else {
        let marks = super::case_marks(check, input, None)?;
        for (case, mark) in check.cases().iter().zip(marks.cases) {
            if !matches!(mark, super::CaseMark::False | super::CaseMark::Unread) {
                break;
            }
            let expression = case.when().expression().map_err(|error| error.clone())?;
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
            Some(Kind::Test(_) | Kind::Script(_) | Kind::Judge(_))
        )
    {
        let output = seq
            .map(|seq| {
                if matches!(check.kind(), Some(Kind::Judge(_))) {
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

fn visits(spec: &Definition, step: &Node, record: &RunRecord) -> Option<String> {
    let max = step.visit_cap(record.visit_grant)?;
    let used = record.visits_to(&record.node);
    let mut text = format!("This is visit {used} of {max} to {}", span(&record.node));
    if used == max {
        // Every step has a cap destination, including an unfireable cap.
        let target = spec.target(&record.node, "cap").expect("step cap target");
        let destination = match &target {
            Target::User => "parks the run for the user".into(),
            Target::Done => "ends the run as done".into(),
            Target::Failed => "ends the run as failed".into(),
            target => format!("sends the run to {}", span(target.as_str())),
        };
        text.push_str(&format!(": one more visit {destination}"));
    }
    Some(format!("{text}. Launches and push-backs count as visits."))
}

fn lifecycle(spec: &Definition, step: &Node, record: &RunRecord) -> (String, String) {
    if spec.is_break(&record.node) {
        return (
            "You stay open across turns. Fix the cause and resume the run, cancel it, or wait for the user. RimZ detaches rather than stops you on resume or cancel. Closing your pane ends the run @failed.".into(),
            "If the user must act now, ask through your own native question; otherwise end your turn and wait for their instructions.".into(),
        );
    }
    // Validation requires `then`: a check, @done, or @failed.
    let then = step.then().first().expect("validated step then");
    let next = if matches!(then, Target::Done | Target::Failed) {
        format!("ends the run at {}", span(then.as_str()))
    } else {
        format!("runs the check {}", span(then.as_str()))
    };
    let kept = step.keep().unwrap_or(false);
    let questions = if step.routes("asked") {
        // An explicit asked edge always has a resolved target.
        let target = spec
            .target(&record.node, "asked")
            .expect("step asked target");
        format!(
            "A blocking question ends this attempt, and the run goes to {}.",
            span(target.as_str())
        )
    } else if step.lifecycle().is_turn() {
        let watching = if matches!(record.fire, Some(RunFire::Scheduled { .. })) {
            "Nobody is watching. "
        } else {
            ""
        };
        let pause = if step.timeout().is_some() {
            ", and the timeout pauses meanwhile"
        } else {
            ""
        };
        format!("{watching}A question waits in your pane until the user answers it{pause}.")
    } else {
        "The user may answer in this pane later; a question goes in your final message.".into()
    };
    if step.lifecycle().is_turn() {
        let cleanup = if kept {
            "leaves your pane open outside this run"
        } else {
            "closes your pane"
        };
        let timeout = step.timeout().map_or_else(String::new, |timeout| {
            format!(", within the step's {} timeout", verbatim(timeout.text()))
        });
        let after = format!(
            "This is one turn. When it ends cleanly with nothing owed, RimZ {cleanup} and {next}. Your final message is kept in the run's history but does not choose the next node. A subagent you launch keeps your turn open until it reports. A wait you arm keeps your pane open until it fires{timeout}. No signals reach you. Errors, provider limits, interrupts, and budget parks leave you open for a later clean turn."
        );
        return (after, questions);
    }
    let timeout = step.timeout().map_or_else(String::new, |timeout| {
        format!(
            " The step's {} timeout counts from your launch and ends this session when it passes.",
            verbatim(timeout.text())
        )
    });
    let exit = if kept {
        format!(
            "When your provider process exits, RimZ leaves your pane open outside this run and {next}."
        )
    } else {
        format!(
            "When your provider process exits, as when the user closes your pane, RimZ closes every cell and {next}."
        )
    };
    let mut after = match step.rest_check() {
        [] => format!("You stay open across turns. {exit}{timeout}"),
        checks @ [first, ..] => {
            let close = if kept {
                "Other routes move the run on; RimZ leaves your pane open outside this run."
            } else {
                "Other routes close your pane and move the run on."
            };
            let listed = if checks.len() > 1 {
                " You stay open while the run moves between the listed checks; your next turn end starts at the first check."
            } else {
                ""
            };
            format!(
                "After each of your turns that rests cleanly with nothing owed, RimZ runs the check {}.{listed} A not-done answer routed back to this step becomes your next prompt in this session. A @wait answer keeps you open while signals and the minute tick recheck it. {close} Errors, provider limits, interrupts, and budget parks wait for a later clean turn. {exit}{timeout}",
                span(first.as_str())
            )
        }
    };
    if let Some(subscriptions) = subscriptions(step) {
        after.push_str(&format!(
            " You subscribe to {subscriptions}. Each arrives as a SIGNAL message at one of your turn boundaries while this step lasts."
        ));
    }
    (after, questions)
}

fn subscriptions(step: &Node) -> Option<String> {
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
