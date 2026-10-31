use std::collections::BTreeSet;
use std::fmt::Display;

use rimz::harness::schedule::when::WhenExpr;
use rimz::harness::workflow::{Check, CheckKind, Edge, Fireability, Step, StepKind};
use rimz::store::workflow::StepLifecycle;

use super::*;

pub(super) fn render(name: &str, node: &str) -> Result<()> {
    let spec = definition::load(name)?;
    let check = spec.checks.get(node);
    let step = spec.steps.get(node);
    let (kind, runner, summary) = if let Some(check) = check {
        let runner = match check.kind() {
            Some(CheckKind::Run(_)) => "run",
            Some(CheckKind::Shell(_)) => "shell",
            Some(CheckKind::Agent(_)) => "agent",
            Some(CheckKind::Condition) => "condition",
            None => "invalid runner",
        };
        ("check", runner, &check.summary)
    } else if let Some(step) = step {
        let runner = match step.kind() {
            Some(StepKind::Run(_)) => "run",
            Some(StepKind::Shell(_)) => "shell",
            Some(StepKind::Agent(_)) => "agent",
            None => "invalid runner",
        };
        ("step", runner, &step.summary)
    } else {
        let nodes = spec
            .checks
            .keys()
            .chain(spec.steps.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        bail!(
            "unknown node {node} in {name}; nodes: {}",
            nodes.into_iter().collect::<Vec<_>>().join(", ")
        );
    };
    let mut out = render::out();
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::header(),
            &format!("{node} · {kind} in {name} · {runner}")
        )
    )?;
    if let Some(summary) = summary {
        writeln!(out, "{}", render::paint(render::palette::muted(), summary))?;
    }
    writeln!(out)?;
    let mut facts = render::KeyVals::new();
    let mut from = Vec::new();
    if spec.workflow.start == node {
        from.push("workflow start".into());
    }
    for source in spec.checks.keys().chain(spec.steps.keys()) {
        for edge in spec.edges(source) {
            if edge.target == node && edge.fires != Fireability::Never {
                from.push(format!(
                    "{source} by {}{}",
                    edge.outcome,
                    edge.mark()
                        .map(|mark| format!(" ({mark})"))
                        .unwrap_or_default()
                ));
            }
        }
    }
    facts.push(
        "reached from",
        render::cell(if from.is_empty() {
            "no nodes".into()
        } else {
            from.join(", ")
        }),
    );
    if let Some(check) = check {
        let wakes = check
            .subscriptions()?
            .into_iter()
            .map(|(selector, matches)| {
                if matches.is_empty() {
                    return selector.to_string();
                }
                format!(
                    "{selector} ({})",
                    matches
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<BTreeSet<_>>();
        let mut wakes = wakes.into_iter().collect::<Vec<_>>();
        wakes.push("the minute tick".into());
        facts.push("wakes on", render::cell(wakes.join(", ")));
    }
    facts.render(&mut out)?;
    let edges = spec.edges(node);
    if let Some(check) = check
        && !check.cases.is_empty()
    {
        let walk = spec.walk();
        let order = walk
            .nodes
            .iter()
            .map(|row| row.node.as_str())
            .chain(walk.unreachable.iter().map(String::as_str))
            .collect::<Vec<_>>();
        cases(
            &mut out,
            check,
            &edges,
            rail::case_note(node, &edges, &order).as_deref(),
        )?;
    }
    writeln!(out, "\nedges:")?;
    for edge in &edges {
        let mark = edge
            .mark()
            .map(|mark| format!(", {mark}"))
            .unwrap_or_default();
        writeln!(
            out,
            "  {} → {} ({}{mark})",
            edge.outcome, edge.target, edge.source
        )?;
    }
    let mut facts = render::KeyVals::new();
    if let Some(check) = check {
        check_facts(&mut facts, check);
    }
    if let Some(step) = step {
        step_facts(&mut facts, step);
    }
    writeln!(out)?;
    facts.render(&mut out)?;
    Ok(())
}

fn cases(out: &mut impl Write, check: &Check, edges: &[Edge], note: Option<&str>) -> Result<()> {
    writeln!(
        out,
        "\ncases:{}",
        note.map(|note| format!(" {note}")).unwrap_or_default()
    )?;
    for (index, case) in check.cases.iter().enumerate() {
        let target = edges
            .iter()
            .find(|edge| edge.outcome == case.outcome)
            .map(|edge| edge.target.as_str())
            .unwrap_or("unmapped");
        let hold = case
            .hold
            .as_ref()
            .map(|hold| format!(" for {hold}"))
            .unwrap_or_default();
        let key = case
            .key
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?
            .map(|key| format!(" · key {key}"))
            .unwrap_or_default();
        writeln!(
            out,
            "  {}  {}{hold}  {} → {target}{key}",
            index + 1,
            WhenExpr::parse(&case.when)?,
            case.outcome
        )?;
    }
    if let Some(outcome) = &check.else_outcome {
        let target = edges
            .iter()
            .find(|edge| edge.outcome == *outcome)
            .map(|edge| edge.target.as_str())
            .unwrap_or("unmapped");
        writeln!(out, "     none true  {outcome} → {target}")?;
    }
    Ok(())
}

fn field(facts: &mut render::KeyVals, key: &str, value: Option<impl Display>) {
    if let Some(value) = value {
        facts.push_lines(
            key,
            value
                .to_string()
                .lines()
                .map(|line| vec![render::cell(line)]),
        );
    }
}

fn check_facts(facts: &mut render::KeyVals, check: &Check) {
    field(facts, "run", check.run.as_ref());
    field(facts, "shell", check.shell.as_ref());
    field(facts, "agent", check.agent.as_ref());
    field(facts, "prompt-file", check.prompt_file.as_ref());
    field(facts, "window", check.window.as_ref());
    field(facts, "timeout", check.timeout.as_ref());
    field(facts, "provider", check.provider.as_ref());
    field(facts, "account", check.account.as_ref());
}

fn step_facts(facts: &mut render::KeyVals, step: &Step) {
    field(facts, "run", step.run.as_ref());
    field(facts, "shell", step.shell.as_ref());
    field(facts, "agent", step.agent.as_ref());
    field(facts, "prompt-file", step.prompt_file.as_ref());
    field(
        facts,
        "lifecycle",
        step.lifecycle.map(|life| match life {
            StepLifecycle::Turn => "turn",
            StepLifecycle::Stay => "stay",
            StepLifecycle::Until => "until",
        }),
    );
    field(facts, "takeover", step.takeover);
    field(facts, "window", step.window.as_ref());
    field(facts, "timeout", step.timeout.as_ref());
    field(
        facts,
        "throttle",
        step.throttle.map(|throttle| match throttle {
            rimz::config::ThrottleSwitch::On => "on",
            rimz::config::ThrottleSwitch::Off => "off",
        }),
    );
    field(facts, "capacity-wait", step.capacity_wait.map(String::from));
    field(facts, "max-visits", step.max_visits);
    field(facts, "max-visits-per-key", step.max_visits_per_key);
}
