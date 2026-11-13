use std::collections::BTreeSet;
use std::fmt::Display;

use rimz::harness::workflow::{Edge, Fireability, Kind, Node};
use rimz::store::workflow::StepLifecycle;

use super::*;

pub(super) fn render(definition: &definition::Definition, node: &str) -> Result<()> {
    let spec = definition::load(definition)?;
    let name = definition.name();
    let check = spec.check(node);
    let step = spec.step(node);
    let runner = |node: &Node| match node.kind() {
        Some(Kind::Run(_)) => "run",
        Some(Kind::Shell(_)) => "shell",
        Some(Kind::Judge(_) | Kind::Agent(_)) => "agent",
        Some(Kind::Condition(_)) => "condition",
        Some(Kind::Sleep(_)) => "sleep",
        Some(Kind::Message(_)) => "message",
        None => "invalid runner",
    };
    let (kind, runner, summary) = if let Some(check) = check {
        ("check", runner(check), check.summary())
    } else if let Some(step) = step {
        ("step", runner(step), step.summary())
    } else {
        let nodes = spec
            .checks()
            .chain(spec.steps())
            .map(|(name, _)| name.clone())
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
    if spec.start() == node {
        from.push("workflow start".into());
    }
    for (source, _) in spec.checks().chain(spec.steps()) {
        for edge in spec.edges(source) {
            if edge.target.as_str() == node && edge.fires != Fireability::Never {
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
    if let Some(step) = step {
        let receives = step
            .subscription_labels()?
            .into_iter()
            .map(|(selector, scope)| format!("{selector} {scope}"))
            .collect::<Vec<_>>();
        if !receives.is_empty() {
            facts.push("receives", render::cell(receives.join(", ")));
        }
    }
    facts.render(&mut out)?;
    let edges = spec.edges(node);
    if let Some(check) = check
        && !check.listed_cases().is_empty()
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
        if check.is_some_and(|check| check.is_sleep())
            && matches!(edge.outcome.as_str(), "sleeping" | "unknown" | "invalid")
        {
            continue;
        }
        let delay = rail::after(edge);
        let mark = edge
            .mark()
            .map(|mark| format!(", {mark}"))
            .unwrap_or_default();
        writeln!(
            out,
            "  {} → {}{delay} ({}{mark})",
            edge.outcome, edge.target, edge.source
        )?;
    }
    if edges.iter().any(|edge| edge.delay.is_some()) {
        writeln!(
            out,
            "\nDelays apply only while an until session is held; otherwise routes go at once."
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

fn cases(out: &mut impl Write, check: &Node, edges: &[Edge], note: Option<&str>) -> Result<()> {
    writeln!(
        out,
        "\ncases:{}",
        note.map(|note| format!(" {note}")).unwrap_or_default()
    )?;
    for (index, case) in check.listed_cases().iter().enumerate() {
        let target = edges
            .iter()
            .find(|edge| edge.outcome == case.outcome)
            .map(|edge| format!("{}{}", edge.target, rail::after(edge)))
            .unwrap_or_else(|| "unmapped".into());
        let hold = case
            .hold()
            .map(|hold| format!(" for {}", hold.text()))
            .unwrap_or_default();
        writeln!(
            out,
            "  {}  {}{hold}  {} → {target}",
            index + 1,
            case.when().expression().map_err(Clone::clone)?,
            case.outcome
        )?;
    }
    if let Some(outcome) = check.else_outcome() {
        let target = edges
            .iter()
            .find(|edge| edge.outcome == *outcome)
            .map(|edge| format!("{}{}", edge.target, rail::after(edge)))
            .unwrap_or_else(|| "unmapped".into());
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

fn check_facts(facts: &mut render::KeyVals, check: &Node) {
    field(facts, "sleep", check.sleep());
    field(
        facts,
        "when",
        check.when().and_then(|when| when.expression().ok()),
    );
    field(facts, "for", check.hold());
    field(facts, "run", check.run());
    field(facts, "shell", check.shell());
    field(facts, "agent", check.agent());
    field(facts, "prompt-file", check.prompt_file());
    field(facts, "window", check.window().map(|window| window.text()));
    field(
        facts,
        "timeout",
        check.timeout().map(|timeout| timeout.text()),
    );
    field(facts, "provider", check.provider());
    field(facts, "account", check.account());
}

fn step_facts(facts: &mut render::KeyVals, step: &Node) {
    field(facts, "run", step.run());
    field(facts, "shell", step.shell());
    field(facts, "agent", step.agent());
    field(facts, "message", step.message());
    field(facts, "prompt-file", step.prompt_file());
    field(
        facts,
        "lifecycle",
        matches!(step.kind(), Some(Kind::Agent(_))).then(|| match step.lifecycle() {
            StepLifecycle::Turn => "turn",
            StepLifecycle::Stay => "stay",
            StepLifecycle::Until => "until",
        }),
    );
    field(facts, "takeover", step.takeover());
    field(facts, "share", step.share());
    field(facts, "keep", step.keep());
    field(facts, "window", step.window().map(|window| window.text()));
    field(
        facts,
        "timeout",
        step.timeout().map(|timeout| timeout.text()),
    );
    field(
        facts,
        "throttle",
        step.throttle().map(|throttle| match throttle {
            rimz::config::ThrottleSwitch::On => "on",
            rimz::config::ThrottleSwitch::Off => "off",
        }),
    );
    field(facts, "limit", step.limit().map(limit_text));
    field(
        facts,
        "capacity-wait",
        step.capacity_wait().map(String::from),
    );
    field(facts, "max-visits", step.max_visits());
}
