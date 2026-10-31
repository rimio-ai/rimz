use std::io::Write;

use rimz::config::GlyphRole;
use rimz::harness::schedule::when::WhenExpr;
use rimz::harness::workflow::{CheckKind, Edge, EdgeSource, Fireability, StepKind, WorkflowSpec};
use rimz::store::workflow::StepLifecycle;

use super::render;

pub(super) struct Rail {
    pub(super) rows: Vec<Row>,
    pub(super) unreachable: Vec<Row>,
    start: String,
}

pub(super) struct Row {
    pub(super) name: String,
    pub(super) hangs_under: Option<String>,
    pub(super) glyph: GlyphRole,
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
            glyph: GlyphRole::PipelineFuture,
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
                None => "invalid runner; see workflow validate".into(),
            };
            row.heading = format!("check  {runner}");
            row.summary.clone_from(&check.summary);
            if let Some(timeout) = &check.timeout
                && let Some(edge) = live.iter().find(|edge| edge.outcome == "timeout")
            {
                row.heading.push_str(&format!(
                    " · waits ≤{timeout}, then {}",
                    target(name, &edge.target, order)
                ));
            }
            if let Some(window) = &check.window {
                row.heading.push_str(&format!(" · window {window}"));
            }
            if !check.cases.is_empty() {
                if let Some(edge) = live.iter().find(|edge| edge.outcome == "unknown") {
                    let action = if edge.target == "@wait" {
                        "waits".into()
                    } else {
                        format!("goes {}", target(name, &edge.target, order))
                    };
                    row.details.push(format!(
                        "the first true case wins; an unread term above it {action}"
                    ));
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
                            target: target(name, &edge.target, order),
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
                        target: target(name, &edge.target, order),
                        readings: Vec::new(),
                    });
                }
            }
            let mut routes = Vec::new();
            for edge in live {
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
                    match step.lifecycle.unwrap_or_default() {
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
                        if let Some(max) = step.max_visits_per_key {
                            row.details.push(format!(
                                "{max} visits per key, then {}",
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
        let heading = format!("{prefix}{glyph} {}  {}", self.name, self.heading);
        write_wrapped(out, "", "│   ", &heading, width, self.style)?;
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
            let text = format!(
                "{} {number}  {}  {} {}",
                case.mark, case.when, case.outcome, case.target
            );
            write_wrapped(out, "│   ", "│      ", text.trim_start(), width, case.style)?;
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

fn edge_text(node: &str, edge: &Edge, order: &[&str]) -> String {
    let mark = edge
        .mark()
        .map(|mark| format!(" ({mark})"))
        .unwrap_or_default();
    format!(
        "{} {}{mark}",
        edge.outcome,
        target(node, &edge.target, order)
    )
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
    let budget = width
        .saturating_sub(unicode_width::UnicodeWidthStr::width(rest))
        .max(1);
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
    if spec.steps.values().any(|step| {
        matches!(step.kind(), Some(StepKind::Agent(_)))
            && step.lifecycle.unwrap_or_default().is_turn()
    }) {
        sentences.push("A step goes on to its next check however its turn ends.");
    }
    if edges.iter().any(|edge| {
        edge.outcome == "invalid"
            && edge.source == EdgeSource::Default
            && edge.fires != Fireability::Never
    }) {
        sentences.push("A check that cannot run parks at @user.");
    }
    if spec.workflow.deadline.is_none()
        && edges
            .iter()
            .any(|edge| edge.outcome == "deadline" && edge.source == EdgeSource::Default)
    {
        sentences.push("No deadline is set; a deadline granted to a run parks it at @user.");
    }
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: &str = r#"
    [workflow]
    scope = 'worktree'
    start = 'begin'
    [checks.begin]
    shell = 'true'
    on.pass = 'sweep'
    on.fail = '@user'
    [steps.sweep]
    shell = 'echo sweep'
    timeout = '1h'
    then = 'watch'
    [checks.watch]
    cases = [{ when = ['ci=passed'], outcome = 'green' }]
    else = 'red'
    on.green = 'notes'
    on.red = 'fix'
    timeout = '3d'
    [steps.fix]
    shell = 'echo fix'
    timeout = '1h'
    then = 'watch'
    max-visits = 4
    [checks.notes]
    shell = 'true'
    on.pass = 'triage'
    on.fail = '@done'
    [steps.triage]
    shell = 'echo triage'
    timeout = '1h'
    then = 'ruled'
    max-visits = 2
    [checks.ruled]
    shell = 'true'
    on.pass = 'triage'
    on.fail = '@done'
    "#;

    #[test]
    fn turn_end_footer_only_describes_turn_agent_steps() {
        for (runner, turn_end) in [
            ("shell='true'", false),
            ("agent='claude'\nlifecycle='stay'", false),
            ("agent='claude'\nlifecycle='until'", false),
            ("agent='claude'", true),
        ] {
            let spec: WorkflowSpec = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='gate'\n\
                 [checks.gate]\nshell='true'\non.pass='work'\n\
                 [steps.work]\nthen='gate'\n{runner}\n"
            ))
            .unwrap();
            assert_eq!(
                defaults(&spec)
                    .contains(&"A step goes on to its next check however its turn ends."),
                turn_end,
                "{runner}"
            );
        }
    }

    #[test]
    fn limit_routes_use_the_same_direction_marks_as_other_edges() {
        let spec: WorkflowSpec = toml::from_str(
            &LAND
                .replace("timeout = '3d'", "timeout = '3d'\non.timeout = 'fix'")
                .replace("max-visits = 4", "max-visits = 4\non.cap = 'notes'"),
        )
        .unwrap();
        let rail = Rail::definition(&spec);
        let watch = rail.rows.iter().find(|row| row.name == "watch").unwrap();
        let fix = rail.rows.iter().find(|row| row.name == "fix").unwrap();
        assert!(
            watch.heading.contains("waits ≤3d, then → fix"),
            "{}",
            watch.heading
        );
        assert!(
            fix.heading.contains("4 visits, then → notes"),
            "{}",
            fix.heading
        );
    }

    #[test]
    fn multiline_commands_stay_on_one_rail_line() {
        let headings = ["run", "shell"].map(|runner| {
            let spec: WorkflowSpec = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='gate'\n\
                 [checks.gate]\n{runner}=\"\"\"echo first\necho second\"\"\"\non.pass='work'\n\
                 [steps.work]\n{runner}=\"\"\"echo first\necho second\"\"\"\nthen='gate'\n"
            ))
            .unwrap();
            Rail::definition(&spec)
                .rows
                .into_iter()
                .map(|row| row.heading)
                .collect::<Vec<_>>()
        });
        assert!(
            headings
                .iter()
                .flatten()
                .all(|heading| heading.contains("echo first; echo second")
                    && !heading.contains('\n')),
            "{headings:?}"
        );
    }

    #[test]
    fn land_rail_agrees_with_validate_and_arrows_follow_the_walk() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("land");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("workflow.toml"), LAND).unwrap();
        let spec = rimz::harness::workflow::load(&dir).unwrap();
        let report =
            rimz::harness::workflow::validate(&dir, home.path(), &Default::default(), None);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let mut out = anstream::StripStream::new(Vec::new());
        let rail = Rail::definition(&spec);
        rail.write(&mut out, 140).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(text.contains("├─○ fix"), "{text}");
        assert!(!text.contains("├─○ sweep"), "{text}");
        assert!(!text.contains("├─○ triage"), "{text}");
        let footer = defaults(&spec).join("\n");
        let walk = spec.walk();
        for (index, row) in report.rows.iter().enumerate() {
            let mut out = anstream::StripStream::new(Vec::new());
            rail.rows
                .iter()
                .find(|rail| rail.name == row.node)
                .unwrap()
                .write(&mut out, 140)
                .unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(text.contains(&format!("○ {}", row.node)), "{text}");
            for edge in &row.edges {
                match edge.outcome.as_str() {
                    "deadline" => assert!(footer.contains("deadline")),
                    "invalid" => assert!(footer.contains("cannot run")),
                    "unknown" => assert!(text.contains("an unread term above it waits")),
                    "timeout" if edge.fires != Fireability::Never => {
                        assert!(text.contains(&format!("waits ≤3d, then {}", edge.target)));
                    }
                    "cap" if edge.fires != Fireability::Never => {
                        assert!(text.contains(&format!("visits, then {}", edge.target)));
                    }
                    _ if edge.fires == Fireability::Never => {
                        assert!(!text.contains(&format!("{} → {}", edge.outcome, edge.target)));
                    }
                    _ => {
                        let position = walk.nodes.iter().position(|node| node.node == edge.target);
                        let target = match position {
                            Some(position) if position <= index => format!("↑ {}", edge.target),
                            Some(_) => format!("→ {}", edge.target),
                            None => edge.target.clone(),
                        };
                        assert!(text.contains(&target), "missing {target}:\n{text}");
                    }
                }
            }
        }
        for width in [80, 140] {
            let mut out = anstream::StripStream::new(Vec::new());
            Rail::definition(&spec).write(&mut out, width).unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(
                text.lines()
                    .all(|line| unicode_width::UnicodeWidthStr::width(line) <= width)
            );
        }
    }
}
