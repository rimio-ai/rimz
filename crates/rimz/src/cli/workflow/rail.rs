use std::io::Write;
use unicode_width::UnicodeWidthStr;

use rimz::config::GlyphRole;
use rimz::harness::schedule::when::WhenExpr;
use rimz::harness::workflow::{
    CheckKind, Edge, EdgeSource, Fireability, Hop, StepKind, WalkNode, WorkflowSpec, reached,
};
use rimz::store::workflow::{JournalEntry, RunRecord, RunState, StepLifecycle};

use super::render;

struct TrackNode {
    name: String,
    parent: Option<String>,
    glyph: String,
    glyph_style: anstyle::Style,
    name_style: anstyle::Style,
    reached: bool,
    current: bool,
    ending: bool,
    visits: u64,
}

impl TrackNode {
    fn width(&self) -> usize {
        self.glyph.width() + 1 + self.name.width()
    }

    fn repeat(&self) -> String {
        if self.visits > 1 {
            format!(" ×{}", self.visits)
        } else {
            String::new()
        }
    }

    fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        write!(
            out,
            "{} {}",
            render::paint(self.glyph_style, &self.glyph),
            render::paint(self.name_style, &self.name)
        )
    }
}

fn track_positions<'a>(
    main: &[&'a TrackNode],
    left: usize,
    right: usize,
    elide_between: Option<&[usize]>,
) -> Vec<(usize, &'a TrackNode, &'static str)> {
    main[left..right]
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let index = left + index;
            elide_between.is_none_or(|anchors| {
                anchors.contains(&index)
                    || anchors.first().is_none_or(|first| index < *first)
                    || anchors.last().is_none_or(|last| index > *last)
            })
        })
        .scan(
            (if left > 0 { 2 } else { 0 }, None),
            |(column, previous), (index, node)| {
                let separator = match *previous {
                    None => "",
                    Some(previous) if index > previous + 1 => " ─ … ─ ",
                    Some(_) => " ─ ",
                };
                *column += separator.width();
                let start = *column;
                *column += node.width();
                *previous = Some(index);
                Some((start, *node, separator))
            },
        )
        .collect()
}

fn track_width(positions: &[(usize, &TrackNode, &str)], hangs: &[&TrackNode]) -> usize {
    positions
        .iter()
        .map(|(column, node, _)| {
            let hang = hangs
                .iter()
                .filter(|hang| hang.parent.as_deref() == Some(node.name.as_str()))
                .map(|hang| 3 + hang.width() + hang.repeat().width() + 2)
                .max()
                .unwrap_or(0);
            column + node.width().max(hang)
        })
        .max()
        .unwrap_or(0)
}

pub(super) fn horizontal(
    out: &mut impl Write,
    spec: &WorkflowSpec,
    record: &RunRecord,
    hops: &[Hop],
    journal: &[JournalEntry],
    width: Option<usize>,
    indent: usize,
) -> std::io::Result<()> {
    let counts = reached(hops, journal);
    let role = super::workflow_view::glyph_role(record);
    let machine = super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    let walk = spec.walk();
    let reachable = walk.nodes.len();
    let nodes = walk
        .nodes
        .into_iter()
        .chain(walk.unreachable.into_iter().map(|node| WalkNode {
            node,
            hangs_under: None,
        }))
        .map(|node| {
            let current = !record.state.is_terminal()
                && (node.node == record.node
                    || record
                        .held
                        .as_ref()
                        .is_some_and(|held| held.attempt.node == node.node));
            let reached = counts.contains_key(node.node.as_str());
            let check = spec.checks.contains_key(&node.node);
            let terminal = record.state.is_terminal() && node.node == record.node;
            let state_style = render::status::workflow(
                record.state,
                matches!(role, GlyphRole::StatusWaiting | GlyphRole::WorkflowHeld),
            );
            let (glyph_style, name_style) = if current {
                (state_style.bold(), state_style.bold())
            } else if reached {
                let style = if terminal && record.state == RunState::Failed {
                    render::palette::alarm()
                } else {
                    render::palette::good()
                };
                (
                    style,
                    if terminal {
                        style
                    } else {
                        render::palette::body()
                    },
                )
            } else {
                (render::palette::muted(), render::palette::muted())
            };
            TrackNode {
                glyph: glyph(if current {
                    role
                } else {
                    match (check, reached) {
                        (true, true) => GlyphRole::PipelineCheckPassed,
                        (true, false) => GlyphRole::PipelineCheckFuture,
                        (false, true) => GlyphRole::PipelinePassed,
                        (false, false) => GlyphRole::PipelineFuture,
                    }
                }),
                glyph_style,
                name_style,
                reached,
                current,
                ending: terminal,
                visits: record
                    .visits
                    .get(&node.node)
                    .map_or(0, |visits| visits.total),
                name: node.node,
                parent: node.hangs_under,
            }
        })
        .collect::<Vec<_>>();
    let (nodes, unreachable) = nodes.split_at(reachable);
    let main = nodes
        .iter()
        .filter(|node| node.parent.is_none())
        .collect::<Vec<_>>();
    let mut hangs = nodes
        .iter()
        .filter(|node| node.parent.is_some() && (node.visits > 0 || node.current || node.ending))
        .collect::<Vec<_>>();
    let anchors = main
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.current
                || node.ending
                || hangs.iter().any(|hang| {
                    (hang.current || hang.ending)
                        && hang.parent.as_deref() == Some(node.name.as_str())
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let first = anchors
        .first()
        .copied()
        .unwrap_or(main.len().saturating_sub(1));
    let last = anchors
        .last()
        .copied()
        .unwrap_or(main.len().saturating_sub(1));
    let (mut left, mut right) = (0, main.len());
    let mut elide_between = None;
    let positions = loop {
        let positions = track_positions(&main, left, right, elide_between);
        if width.is_none_or(|width| track_width(&positions, &hangs) <= width.saturating_sub(indent))
        {
            break positions;
        }
        if left < first.saturating_sub(1) {
            left += 1;
            continue;
        }
        if right > last + 1 {
            right -= 1;
            continue;
        }
        if elide_between.is_none() && anchors.windows(2).any(|pair| pair[1] > pair[0] + 1) {
            elide_between = Some(anchors.as_slice());
            continue;
        }
        if left < first {
            left += 1;
            continue;
        }
        if hangs.iter().any(|hang| !(hang.current || hang.ending)) {
            hangs.retain(|hang| hang.current || hang.ending);
            (left, right) = (0, main.len());
            elide_between = None;
            continue;
        }
        break positions;
    };
    if positions.is_empty() {
        return Ok(());
    }
    write!(out, "{:indent$}", "")?;
    if left > 0 {
        write!(out, "{}", render::paint(render::palette::faint(), "… "))?;
    }
    for (_, node, separator) in &positions {
        if !separator.is_empty() {
            write!(
                out,
                "{}",
                render::paint(render::palette::faint(), separator)
            )?;
        }
        node.write(out)?;
    }
    writeln!(out)?;
    for (column, parent, _) in &positions {
        for hang in hangs
            .iter()
            .filter(|hang| hang.parent.as_deref() == Some(parent.name.as_str()))
        {
            write!(
                out,
                "{:padding$}{}",
                "",
                render::paint(render::palette::faint(), "╰─ "),
                padding = indent + column
            )?;
            hang.write(out)?;
            writeln!(
                out,
                "{}{}",
                hang.repeat(),
                render::paint(render::palette::faint(), " ↑")
            )?;
        }
    }
    for node in unreachable
        .iter()
        .filter(|node| node.current || node.reached)
    {
        write!(out, "{:indent$}", "")?;
        node.write(out)?;
        writeln!(out)?;
    }
    Ok(())
}

pub(super) struct Rail {
    pub(super) rows: Vec<Row>,
    pub(super) unreachable: Vec<Row>,
    start: String,
}

pub(super) struct Row {
    pub(super) name: String,
    pub(super) hangs_under: Option<String>,
    pub(super) glyph: GlyphRole,
    pub(super) glyph_style: anstyle::Style,
    pub(super) style: anstyle::Style,
    pub(super) heading: String,
    pub(super) summary: Option<String>,
    pub(super) details: Vec<String>,
    pub(super) cases: Vec<CaseRow>,
}

pub(super) struct CaseRow {
    pub(super) mark: String,
    pub(super) style: anstyle::Style,
    pub(super) number: Option<usize>,
    pub(super) when: String,
    pub(super) outcome: String,
    pub(super) target: String,
    pub(super) readings: Vec<String>,
}

impl Rail {
    pub(super) fn definition(spec: &WorkflowSpec) -> Self {
        let walk = spec.walk();
        let order = walk
            .nodes
            .iter()
            .map(|row| row.node.as_str())
            .chain(walk.unreachable.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let rows = walk
            .nodes
            .iter()
            .map(|row| Row::definition(spec, &row.node, row.hangs_under.clone(), &order))
            .collect();
        let unreachable = walk
            .unreachable
            .iter()
            .map(|name| Row::definition(spec, name, None, &order))
            .collect();
        Self {
            rows,
            unreachable,
            start: spec.workflow.start.clone(),
        }
    }

    pub(super) fn write(&self, out: &mut impl Write, width: usize) -> std::io::Result<()> {
        for row in &self.rows {
            row.write(out, width)?;
        }
        if !self.unreachable.is_empty() {
            writeln!(out, "\nunreachable from {}", self.start)?;
            for row in &self.unreachable {
                row.write(out, width)?;
            }
        }
        Ok(())
    }
}

impl Row {
    fn definition(
        spec: &WorkflowSpec,
        name: &str,
        hangs_under: Option<String>,
        order: &[&str],
    ) -> Self {
        let edges = spec.edges(name);
        let live = edges
            .iter()
            .filter(|edge| edge.fires != Fireability::Never)
            .collect::<Vec<_>>();
        let mut row = Self {
            name: name.into(),
            hangs_under,
            glyph: if spec.checks.contains_key(name) {
                GlyphRole::PipelineCheckFuture
            } else {
                GlyphRole::PipelineFuture
            },
            glyph_style: render::palette::body(),
            style: render::palette::body(),
            heading: String::new(),
            summary: None,
            details: Vec::new(),
            cases: Vec::new(),
        };
        if let Some(check) = spec.checks.get(name) {
            let runner = match check.kind() {
                Some(CheckKind::Run(run)) => render::one_line(run),
                Some(CheckKind::Shell(shell)) => format!("sh: {}", render::one_line(shell)),
                Some(CheckKind::Agent(agent)) => format!("{agent}, headless"),
                Some(CheckKind::Condition) => "condition".into(),
                Some(CheckKind::Sleep) => {
                    format!("sleep {}", check.sleep.as_deref().unwrap_or_default())
                }
                None => "invalid runner; see workflow validate".into(),
            };
            row.heading = format!("check  {runner}");
            row.summary.clone_from(&check.summary);
            if check.is_sleep() {
                if let Some(when) = &check.when {
                    row.details.push(format!(
                        "when {}",
                        WhenExpr::parse(when)
                            .map(|expression| expression.to_string())
                            .unwrap_or_else(|_| format!("invalid condition: {when:?}"))
                    ));
                }
                if let Some(hold) = &check.hold {
                    row.details.push(format!("for {hold}"));
                }
                if let Some(then) = &check.then {
                    row.details
                        .push(format!("then {}", target(name, then, order)));
                }
            }
            if let Some(timeout) = &check.timeout
                && let Some(edge) = live.iter().find(|edge| edge.outcome == "timeout")
            {
                row.heading.push_str(&format!(
                    " · waits ≤{timeout}, then {}",
                    edge_target(name, edge, order)
                ));
            }
            if let Some(window) = &check.window {
                row.heading.push_str(&format!(" · window {window}"));
            }
            if !check.cases.is_empty() {
                if let Some(note) = case_note(name, &edges, order) {
                    row.details.push(note);
                }
                for (index, case) in check.cases.iter().enumerate() {
                    if let Some(edge) = live.iter().find(|edge| edge.outcome == case.outcome) {
                        let hold = case
                            .hold
                            .as_ref()
                            .map(|hold| format!(" for {hold}"))
                            .unwrap_or_default();
                        row.cases.push(CaseRow {
                            mark: String::new(),
                            style: match edge.target.as_str() {
                                "@done" => render::palette::good(),
                                "@failed" => render::palette::alarm(),
                                _ => render::palette::body(),
                            },
                            number: Some(index + 1),
                            when: format!(
                                "{}{hold}",
                                WhenExpr::parse(&case.when)
                                    .map(|expression| expression.to_string())
                                    .unwrap_or_else(|_| format!(
                                        "invalid condition: {:?}",
                                        case.when
                                    ))
                            ),
                            outcome: case.outcome.clone(),
                            target: edge_target(name, edge, order),
                            readings: Vec::new(),
                        });
                    }
                }
                if let Some(outcome) = &check.else_outcome
                    && let Some(edge) = live.iter().find(|edge| edge.outcome == *outcome)
                {
                    row.cases.push(CaseRow {
                        mark: String::new(),
                        style: match edge.target.as_str() {
                            "@done" => render::palette::good(),
                            "@failed" => render::palette::alarm(),
                            _ => render::palette::body(),
                        },
                        number: None,
                        when: "none true".into(),
                        outcome: outcome.clone(),
                        target: edge_target(name, edge, order),
                        readings: Vec::new(),
                    });
                }
            }
            let mut routes = Vec::new();
            for edge in live {
                if check.is_sleep()
                    && matches!(edge.outcome.as_str(), "then" | "sleeping" | "unknown")
                {
                    continue;
                }
                if edge.outcome == "timeout"
                    || edge.outcome == "deadline"
                    || (edge.outcome == "invalid" && edge.source == EdgeSource::Default)
                    || (!check.cases.is_empty()
                        && (edge.outcome == "unknown"
                            || check.cases.iter().any(|case| case.outcome == edge.outcome)
                            || check.else_outcome.as_deref() == Some(&edge.outcome)))
                {
                    continue;
                }
                routes.push(edge_text(name, edge, order));
            }
            if !routes.is_empty() {
                row.details.push(routes.join(" · "));
            }
        } else if let Some(step) = spec.steps.get(name) {
            let runner = match step.kind() {
                Some(StepKind::Run(run)) => render::one_line(run),
                Some(StepKind::Shell(shell)) => format!("sh: {}", render::one_line(shell)),
                Some(StepKind::Agent(agent)) => format!(
                    "{agent} · {}",
                    match step.lifecycle() {
                        StepLifecycle::Turn => "1 turn",
                        StepLifecycle::Stay => "lifecycle stay",
                        StepLifecycle::Until => "lifecycle until",
                    }
                ),
                None => "invalid runner; see workflow validate".into(),
            };
            row.heading = format!("step  {runner}");
            if let Some(timeout) = &step.timeout {
                row.heading.push_str(&format!(" ≤{timeout}"));
            }
            row.summary.clone_from(&step.summary);
            if step.takeover == Some(true) {
                row.details.push("takes over".into());
            }
            if step.share == Some(true) {
                row.details.push("shares the checkout".into());
            }
            if let Some(window) = &step.window {
                row.details.push(format!("window {window}"));
            }
            if let Some(wait) = step.capacity_wait
                && wait != rimz::config::CapacityWait::Forever
            {
                row.details
                    .push(format!("capacity-wait {}", String::from(wait)));
            }
            for edge in live {
                match edge.outcome.as_str() {
                    "deadline" => continue,
                    "cap" => {
                        if let Some(max) = step.max_visits {
                            row.details.push(format!(
                                "{max} visits, then {}",
                                target(name, &edge.target, order)
                            ));
                        }
                        if let Some(max) = &step.max_visits_per_outcome {
                            let reset_on = match max.reset_on() {
                                [] => String::new(),
                                outcomes => format!(" (reset-on {})", outcomes.join(", ")),
                            };
                            row.details.push(format!(
                                "max-visits-per-outcome {}{reset_on}, then {}",
                                max.limit(),
                                target(name, &edge.target, order)
                            ));
                        }
                    }
                    "then" => row.details.push(target(name, &edge.target, order)),
                    _ => row.details.push(edge_text(name, edge, order)),
                }
            }
            row.heading
                .push_str(&format!(" · {}", row.details.join(" · ")));
            row.details.clear();
        }
        row
    }

    fn write(&self, out: &mut impl Write, width: usize) -> std::io::Result<()> {
        let machine = super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme)(self.glyph);
        let prefix = if self.hangs_under.is_some() {
            "├─"
        } else {
            ""
        };
        let first = format!(
            "{}{} ",
            render::paint(render::palette::faint(), prefix),
            render::paint(self.glyph_style, &glyph),
        );
        let heading = format!("{}  {}", self.name, self.heading);
        write_wrapped(out, &first, "│   ", &heading, width, self.style)?;
        if let Some(summary) = &self.summary {
            write_wrapped(
                out,
                "│   ",
                "│   ",
                summary,
                width,
                render::palette::muted(),
            )?;
        }
        for detail in &self.details {
            write_wrapped(out, "│   ", "│   ", detail, width, render::palette::body())?;
        }
        for case in &self.cases {
            let number = case.number.map(|n| n.to_string()).unwrap_or_default();
            let first = if case.mark.is_empty() {
                "│   ".into()
            } else {
                format!("│ {} ", render::paint(case.style, &case.mark))
            };
            let text = format!("{number}  {}  {} {}", case.when, case.outcome, case.target);
            write_wrapped(out, &first, "│      ", text.trim_start(), width, case.style)?;
            for reading in &case.readings {
                write_wrapped(
                    out,
                    "│      ",
                    "│      ",
                    reading,
                    width,
                    render::palette::muted(),
                )?;
            }
        }
        writeln!(out, "│")
    }
}

pub(super) fn case_note(node: &str, edges: &[Edge], order: &[&str]) -> Option<String> {
    let edge = edges.iter().find(|edge| edge.outcome == "unknown")?;
    let action = if edge.target == "@wait" {
        "waits".into()
    } else {
        format!("goes {}", edge_target(node, edge, order))
    };
    Some(format!(
        "the first true case wins; an unread term above it {action}"
    ))
}

fn edge_text(node: &str, edge: &Edge, order: &[&str]) -> String {
    let mark = edge
        .mark()
        .map(|mark| format!(" ({mark})"))
        .unwrap_or_default();
    format!("{} {}{mark}", edge.outcome, edge_target(node, edge, order))
}

pub(super) fn after(edge: &Edge) -> String {
    edge.delay
        .as_ref()
        .map_or_else(String::new, |delay| format!(" after {delay}"))
}

fn edge_target(node: &str, edge: &Edge, order: &[&str]) -> String {
    format!("{}{}", target(node, &edge.target, order), after(edge))
}

fn target(node: &str, target: &str, order: &[&str]) -> String {
    if target.starts_with('@') {
        return target.into();
    }
    let above = order.iter().position(|name| *name == target)
        <= order.iter().position(|name| *name == node);
    format!("{} {target}", if above { "↑" } else { "→" })
}

fn write_wrapped(
    out: &mut impl Write,
    first: &str,
    rest: &str,
    text: &str,
    width: usize,
    style: anstyle::Style,
) -> std::io::Result<()> {
    let prefix_width = [first, rest]
        .map(|prefix| anstream::adapter::strip_str(prefix).to_string().width())
        .into_iter()
        .max()
        .unwrap_or(0);
    let budget = width.saturating_sub(prefix_width).max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for part in text.split(" · ") {
        let joined = if current.is_empty() {
            part.to_owned()
        } else {
            format!("{current} · {part}")
        };
        if unicode_width::UnicodeWidthStr::width(joined.as_str()) <= budget {
            current = joined;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        let mut wrapped = render::wrap_words(part, budget);
        current = wrapped.pop().unwrap_or_default();
        lines.extend(wrapped);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    for (index, line) in lines.iter().enumerate() {
        let prefix = if index == 0 { first } else { rest };
        writeln!(out, "{prefix}{}", render::paint(style, line))?;
    }
    Ok(())
}

pub(super) fn defaults(spec: &WorkflowSpec) -> Vec<&'static str> {
    let mut sentences = Vec::new();
    let edges = spec
        .checks
        .keys()
        .chain(spec.steps.keys())
        .flat_map(|name| spec.edges(name))
        .collect::<Vec<_>>();
    if spec
        .steps
        .values()
        .any(|step| matches!(step.kind(), Some(StepKind::Agent(_))) && step.lifecycle().is_turn())
    {
        sentences.push("A turn step goes on to its next check when its turn ends cleanly or its provider exits.");
    }
    if edges.iter().any(|edge| {
        edge.outcome == "invalid"
            && edge.source == EdgeSource::Default
            && edge.fires != Fireability::Never
    }) {
        sentences.push(
            if spec.route_defaults == rimz::store::workflow::RouteDefaults::Park {
                "A check that cannot run parks at @user."
            } else {
                "An unrouted failure ends the run as failed."
            },
        );
    }
    if spec.workflow.deadline.is_none()
        && edges
            .iter()
            .any(|edge| edge.outcome == "deadline" && edge.source == EdgeSource::Default)
    {
        sentences.push(
            if spec.route_defaults == rimz::store::workflow::RouteDefaults::Park {
                "No deadline is set; a deadline granted to a run parks it at @user."
            } else {
                "No deadline is set; an unrouted deadline granted to a run ends it as failed."
            },
        );
    }
    sentences
}

#[cfg(test)]
mod tests;
