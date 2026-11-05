use std::collections::BTreeSet;

use serde_json::Value;

use super::{Check, Fireability, HopKind, Step, WorkflowSpec, routing_hop};
use crate::disk::paths::WorkflowPaths;
use crate::harness::schedule::{
    catalog::TaskCatalog, span, trigger_clause, verbatim, when::WhenExpr,
};
use crate::store::workflow::{self, AttemptOutcome, Key, RunRecord, StepLifecycle};

const DEFINITION: &str = "a graph of steps and checks that the user defined. RimZ launches an agent for each agent step, and after each step runs a check whose answer picks the next node.";
const CONTEXT: &str =
    "`$RIMZ_WORKFLOW_DIR/context.json` holds this run's input and the previous attempt in full.";

pub(super) fn step(paths: &WorkflowPaths, spec: &WorkflowSpec, record: &RunRecord) -> String {
    let step = &spec.steps[&record.node];
    let mut origin = origin(record);
    origin.push_str(" Your prompt is the step's fixed text, not a message someone just typed.");
    if record.prompt.is_some() {
        origin.push_str(if record.task.is_some() {
            " The loop task's prompt ends it."
        } else {
            " The text given with `--prompt` to `rimz workflow run` ends it."
        });
    }
    let mut arrival = Vec::new();
    if let Ok(journal) = workflow::journal(paths) {
        let sender = routing_hop(spec, &journal, &record.node, record.held.as_ref());
        arrival.push(match sender {
            Some(hop) if hop.kind == HopKind::Resume => {
                "The user resumed the run at this step after it parked.".into()
            }
            Some(hop) => {
                // A routing hop always names its outcome.
                let outcome = hop.outcome.as_deref().expect("routing hop outcome");
                match spec.checks.get(&hop.node) {
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
        let who = previous.handle.as_ref().map_or_else(
            || format!("the step {}", span(&previous.step)),
            |handle| format!("{} in {}", verbatim(handle), span(&previous.step)),
        );
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
    let (after, questions) = lifecycle(spec, step, record);
    let more = format!(
        "{CONTEXT} `rimz workflow show $RIMZ_WORKFLOW_RUN` draws the graph with this run's position and trail."
    );
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
        CONTEXT.into(),
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
    let Some(task) = &record.task else {
        return "The user started this run by hand with `rimz workflow run`.".into();
    };
    if record.manual {
        return format!(
            "The user started this run by hand with {}.",
            span(&format!("rimz loop fire {task}"))
        );
    }
    let catalog = TaskCatalog::load_lenient(Some(&record.project_root));
    let clause = catalog
        .visible()
        .get(task)
        .and_then(|task| {
            let predicate = if task.entry().each_worktree {
                "held in this worktree"
            } else {
                "held"
            };
            trigger_clause(task.trigger().as_ref().ok()?, predicate)
        })
        .map_or_else(String::new, |clause| format!(",{clause}"));
    format!(
        "RimZ started this run automatically{clause} (loop task {}).",
        span(task)
    )
}

fn detail(check: &Check, input: &Value) -> Option<String> {
    let text = if let Some(reason) = input["reason"].as_str() {
        reason.to_owned()
    } else {
        if check.cases.is_empty() {
            return None;
        }
        let readings = input["readings"].as_object()?;
        let keys = input["case"]
            .as_u64()
            .and_then(|case| usize::try_from(case).ok())
            .and_then(|case| check.cases.get(case))
            .and_then(|case| WhenExpr::parse(&case.when).ok())
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
            // Every step has a cap destination, defaulting to @user.
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
    Some(format!("{text}."))
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
    // Validation requires every agent step's `then` to name a check.
    let check = span(step.then.as_deref().expect("validated step then"));
    match step.lifecycle.unwrap_or_default() {
        StepLifecycle::Turn => {
            let timeout = step.timeout.as_ref().map_or_else(String::new, |timeout| {
                format!(", within the step's {} timeout", verbatim(timeout))
            });
            let after = format!(
                "This is one turn. When it ends with nothing owed, RimZ closes your pane and runs the check {check}. Your final message is kept in the run's history but does not choose the next node. A subagent you launch keeps your turn open until it reports. A wait you arm keeps your pane open until it fires{timeout}. No signals reach you."
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
                let watching = if record.task.is_some() && !record.manual {
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
            let after = if lifecycle == StepLifecycle::Stay {
                format!(
                    "You stay open across turns until your provider process exits, as when the user closes your pane. RimZ then closes every cell and runs the check {check}."
                )
            } else {
                format!(
                    "After each of your turns that ends with nothing owed, RimZ runs the check {check}, which either keeps you open for another turn or closes your pane and moves the run on."
                )
            };
            (
                after,
                "The user may answer in this pane later; a question goes in your final message."
                    .into(),
            )
        }
    }
}
