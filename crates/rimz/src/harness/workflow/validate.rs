use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::Path;

use super::spec::{self, CheckKind, Issue, RESERVED_OUTCOMES, StepKind, WorkflowSpec, issue};
use super::{Edge, EdgeSource};
use crate::config::definitions::LoadedDefinitions;
use crate::store::workflow::StepLifecycle;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NodeRow {
    pub workflow: String,
    pub node: String,
    pub summary: Option<String>,
    pub kind: String,
    pub runner: String,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub agent: Option<String>,
    pub message: Option<String>,
    pub do_background: bool,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Default, Serialize)]
pub struct Validation {
    pub rows: Vec<NodeRow>,
    pub warnings: Vec<Issue>,
    pub errors: Vec<Issue>,
}

pub fn validate(
    dir: &Path,
    definitions: &LoadedDefinitions,
    project_root: Option<&Path>,
) -> Validation {
    let mut report = Validation::default();
    let canonical = dir.canonicalize();
    let Some(name) = canonical
        .as_deref()
        .unwrap_or(dir)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        report.error(
            "workflow",
            format!("{}: directory needs a workflow name", dir.display()),
        );
        return report;
    };
    let spec = match super::load(dir) {
        Ok(spec) => spec,
        Err(err) => {
            let path = match &err {
                super::SpecErr::Toml { path, .. }
                | super::SpecErr::ReservedTarget { path, .. }
                | super::SpecErr::ReservedNode { path } => path.as_str(),
                _ => "workflow",
            };
            report.error(path, format!("{name}: {err}"));
            return report;
        }
    };
    validate_spec(dir, name, &spec, definitions, project_root)
}

pub(super) fn validate_spec(
    dir: &Path,
    name: &str,
    spec: &WorkflowSpec,
    definitions: &LoadedDefinitions,
    project_root: Option<&Path>,
) -> Validation {
    let mut report = Validation {
        warnings: spec.ignored.clone(),
        ..Default::default()
    };
    if let Err(err) = crate::harness::name::validate_name(name) {
        report.error("workflow", err);
    }
    if !spec.checks.contains_key(&spec.workflow.start)
        && !spec.steps.contains_key(&spec.workflow.start)
    {
        report.error(
            "workflow.start",
            format!("{} must name a check or a step", spec.workflow.start),
        );
    }
    if let Some(target) = &spec.workflow.on.deadline {
        report.target(spec, "workflow.on.deadline", target);
    }
    if let Some(spec::OnBreak::Target(target)) = &spec.workflow.on_break
        && !matches!(target.as_str(), "@done" | "@failed")
    {
        report.error(
            "workflow.on-break",
            "use @failed, @done, or { agent = \"<profile>\" }",
        );
    }
    for (node, check) in &spec.checks {
        let path = format!("checks.{node}");
        report.node_name(&path, node);
        if spec.steps.contains_key(node) {
            report.error(&path, "name is also used by a step");
        }
        if check.kind().is_none() {
            report.error(
                &path,
                "set exactly one of run, shell, agent, sleep, or cases (nonempty)",
            );
        }
        if let Some(CheckKind::Run(run)) = check.kind()
            && let Err(err) = spec::resolve_argv(dir, run)
        {
            report.error(&format!("{path}.run"), err);
        }
        if let Some(CheckKind::Shell(shell)) = check.kind() {
            if shell.trim().is_empty() {
                report.error(&format!("{path}.shell"), "set a nonempty shell command");
            }
            for outcome in ["pass", "fail"] {
                if !check.on.contains_key(outcome) {
                    report.error(
                        &format!("{path}.on.{outcome}"),
                        "a shell check requires both on.pass and on.fail",
                    );
                }
            }
        }
        if matches!(check.kind(), Some(CheckKind::Agent(_))) && check.prompt_file.is_none() {
            report.error(
                &format!("{path}.prompt-file"),
                "an agent check requires prompt-file",
            );
        }
        let condition = matches!(check.kind(), Some(CheckKind::Condition));
        let sleep = check.is_sleep();
        if condition {
            report.condition_fields(&path, check, project_root);
        }
        if sleep {
            if let Some(raw) = &check.sleep {
                match crate::harness::duration::duration(raw) {
                    Ok(duration) if duration.is_zero() => {
                        report.error(&format!("{path}.sleep"), "sleep must be nonzero")
                    }
                    Err(error) => report.error(&format!("{path}.sleep"), error),
                    Ok(_) => {}
                }
            }
            match check.then.as_deref() {
                None | Some("@wait") => report.error(
                    &format!("{path}.then"),
                    "a sleep requires then routing away from @wait",
                ),
                Some(target) => report.target(spec, &format!("{path}.then"), target),
            }
            report.condition_terms(&path, check, project_root);
        }
        if let Some(CheckKind::Agent(agent)) = check.kind() {
            if check.summary.is_none() {
                report.error(
                    &format!("{path}.summary"),
                    "a judge check requires a one-line summary of what it reviews; add summary = \"...\"",
                );
            }
            report.agent_fields(dir, &path, agent, check.prompt_file.as_deref(), definitions);
            if !definitions.failed.contains_key(agent)
                && let Ok(profile) =
                    crate::harness::spec::resolve_profile(agent, &definitions.agent_profiles)
                && let Some(adapter) = crate::agents::find_definition(profile.kind.as_str())
                && adapter.spec().launch.headless.is_none()
            {
                report.error(
                    &format!("{path}.agent"),
                    format!("{agent} has no headless launch for judge checks"),
                );
            }
        }
        let edges = spec.edges(node);
        let unknown_waits = condition
            && edges
                .iter()
                .any(|edge| edge.outcome == "unknown" && edge.target == "@wait");
        if edges
            .iter()
            .any(|edge| edge.outcome != "deadline" && edge.target == "@wait")
            && check.timeout.is_none()
            && !sleep
        {
            report.error(
                &format!("{path}.timeout"),
                if unknown_waits {
                    "a waiting check requires timeout; set timeout or route on.unknown away from @wait (and any other waiting outcomes)"
                } else {
                    "a waiting check requires timeout"
                },
            );
        }
        for (outcome, route) in &check.on {
            report.target(spec, &format!("{path}.on.{outcome}"), route.target());
            if outcome == "timeout" && route.target() == "@wait" {
                report.error(
                    &format!("{path}.on.timeout"),
                    "a timed-out check cannot wait on its expired timeout clock",
                );
            }
        }
        report.rows.push(NodeRow {
            workflow: name.into(),
            node: node.clone(),
            summary: check.summary.clone(),
            kind: "check".into(),
            runner: match check.kind() {
                Some(CheckKind::Run(_)) => "run",
                Some(CheckKind::Shell(_)) => "shell",
                Some(CheckKind::Condition) => "condition",
                Some(CheckKind::Sleep) => "sleep",
                _ => "agent",
            }
            .into(),
            run: check.run.clone(),
            shell: check.shell.clone(),
            agent: check.agent.clone(),
            message: None,
            do_background: do_background(check.agent.as_deref(), definitions).unwrap_or(false),
            edges,
        });
    }
    let answerable = spec
        .checks
        .keys()
        .flat_map(|node| spec.edges(node))
        .map(|edge| edge.outcome)
        .filter(|outcome| !RESERVED_OUTCOMES.contains(&outcome.as_str()))
        .collect::<BTreeSet<_>>();
    for (node, step) in &spec.steps {
        let at_break = spec.is_break(node);
        let path = if at_break {
            "workflow.on-break".into()
        } else {
            format!("steps.{node}")
        };
        if !at_break {
            report.node_name(&path, node);
        }
        if let Some(cap) = &step.max_visits_per_outcome {
            let cap_path = format!("{path}.max-visits-per-outcome");
            if !cap.reset_on().is_empty() && step.max_visits.is_none() {
                report.error(
                    &format!("{cap_path}.reset-on"),
                    "reset-on requires max-visits",
                );
            }
            for outcome in cap.reset_on() {
                if !answerable.contains(outcome) {
                    report.error(
                        &format!("{cap_path}.reset-on"),
                        format!("no check answers outcome {outcome}"),
                    );
                }
            }
            if cap.reset_on().is_empty() && spec.workflow.start != *node {
                let inbound = spec
                    .checks
                    .keys()
                    .chain(spec.steps.keys())
                    .flat_map(|source| spec.edges(source))
                    .filter(|edge| edge.target == *node)
                    .map(|edge| edge.outcome)
                    .collect::<BTreeSet<_>>();
                if inbound.len() == 1 && !inbound.contains("then") {
                    report.warnings.push(issue(
                        &cap_path,
                        concat!(
                            "without reset-on, a step reached by only one outcome repeats ",
                            "max-visits unless a check answers keys"
                        ),
                    ));
                }
            }
        }
        if step.kind().is_none() {
            report.error(&path, "set exactly one of agent, run, shell, or message");
        }
        let agent = matches!(step.kind(), Some(StepKind::Agent(_)));
        let command = matches!(step.kind(), Some(StepKind::Run(_) | StepKind::Shell(_)));
        let message = matches!(step.kind(), Some(StepKind::Message(_)));
        if let Some(file) = &step.system_prompt_file
            && let Err(error) = spec::confined_file(dir, file)
        {
            report.error(&format!("{path}.system-prompt-file"), error);
        }
        if agent && !at_break && step.prompt_file.is_none() {
            report.error(
                &format!("{path}.prompt-file"),
                "an agent step requires prompt-file",
            );
        }
        if command && step.timeout.is_none() {
            report.error(
                &format!("{path}.timeout"),
                "a run or shell step requires timeout, so a hung command cannot hold the run",
            );
        }
        if let Some(StepKind::Run(run)) = step.kind()
            && let Err(error) = spec::resolve_argv(dir, run)
        {
            report.error(&format!("{path}.run"), error);
        }
        if let Some(StepKind::Shell(shell)) = step.kind()
            && shell.trim().is_empty()
        {
            report.error(&format!("{path}.shell"), "set a nonempty shell command");
        }
        if let Some(StepKind::Message(target)) = step.kind() {
            if let Err(error) = message_target(target) {
                report.error(&format!("{path}.message"), error);
            }
            report.prompt(dir, &path, step.prompt_file.as_deref());
            message_then(&mut report, spec, &path, &step.then);
        }
        let layout = if let Some(StepKind::Agent(agent)) = step.kind() {
            report.prompt(dir, &path, step.prompt_file.as_deref());
            match step_layout(dir, agent, step.lifecycle(), definitions, project_root) {
                Ok(layout) => Some(layout),
                Err(error) => {
                    report.error(&format!("{path}.agent"), error);
                    None
                }
            }
        } else {
            None
        };
        let do_background = layout.as_ref().and_then(|layout| {
            let leader = crate::harness::spec::prompt_leader(layout, None).ok()?;
            let cell = layout.agent_cells().nth(leader)?;
            let adapter = crate::agents::find_definition(&cell.kind)?;
            Some(
                adapter
                    .concern_coverage(crate::agents::IntegrationConcern::BackgroundParking)
                    .is_wired(),
            )
        });
        if matches!(step.lifecycle(), StepLifecycle::Turn | StepLifecycle::Until)
            && do_background == Some(false)
        {
            report.warnings.push(issue(&path, "step adapter has no background reporting; a turn end may finish the attempt while provider background work remains"));
        }
        if !message && step.then.is_empty() {
            report.error(
                &format!("{path}.then"),
                "then must name a check or a nonempty list of checks",
            );
        }
        let ends_run = agent
            && step.lifecycle() != StepLifecycle::Until
            && matches!(step.then.as_slice(), [target] if matches!(target.as_str(), "@done" | "@failed"));
        if !at_break
            && !message
            && !ends_run
            && step
                .then
                .iter()
                .any(|target| !spec.checks.contains_key(target))
        {
            report.error(
                &format!("{path}.then"),
                if agent && step.lifecycle() != StepLifecycle::Until {
                    if step.then.len() > 1
                        && step
                            .then
                            .iter()
                            .any(|target| target == "@done" || target == "@failed")
                    {
                        "then lists only checks; @done or @failed stands alone as then"
                    } else {
                        "then must name a check, @done, or @failed, never a step"
                    }
                } else {
                    "then must name a check, never a step or @done"
                },
            );
        }
        if !message && step.lifecycle() != StepLifecycle::Until && step.then.len() > 1 {
            report.warnings.push(issue(
                &format!("{path}.then"),
                concat!(
                    "only an until step keeps its agent open across the list; ",
                    "enter the first check and let its routes decide the rest"
                ),
            ));
        }
        if step.lifecycle() == StepLifecycle::Until
            && step
                .then
                .iter()
                .filter_map(|node| spec.checks.get(node))
                .any(|check| check.is_sleep())
        {
            report.error(
                &format!("{path}.then"),
                "an until step cannot use a sleep as its turn-end check; use lifecycle = \"turn\"",
            );
        }
        for pair in step.then.windows(2).filter(|_| !message) {
            if !spec.edges(&pair[0]).iter().any(|edge| {
                edge.outcome != "deadline"
                    && edge.target == pair[1]
                    && edge.fires != super::Fireability::Never
            }) {
                report.error(
                    &format!("{path}.then"),
                    format!(
                        "{} requires a direct route to the next listed check {}",
                        pair[0], pair[1]
                    ),
                );
            }
        }
        if step.takeover == Some(true) && step.share == Some(true) {
            report.error(
                &format!("{path}.share"),
                "choose takeover or share: takeover stops an idle occupant; share launches beside it",
            );
        }
        if agent && let Some(target) = &step.on.capacity {
            report.target(spec, &format!("{path}.on.capacity"), target);
            if matches!(target.as_str(), "@user" | "@wait") {
                report.error(
                    &format!("{path}.on.capacity"),
                    "capacity give-up cannot park or wait; use @failed, @done, or a node",
                );
            }
        }
        for (outcome, target) in [
            ("asked", &step.on.asked),
            ("cap", &step.on.cap),
            ("failed", &step.on.failed),
            ("invalid", &step.on.invalid),
            ("undelivered", &step.on.undelivered),
        ] {
            if (matches!(outcome, "asked" | "failed" | "invalid") && !agent)
                || (outcome == "undelivered" && !message)
            {
                continue;
            }
            if let Some(target) = target {
                report.target(spec, &format!("{path}.on.{outcome}"), target);
                if target == "@wait" {
                    report.error(&format!("{path}.on.{outcome}"), "only checks can wait");
                }
                if outcome == "cap" && target == node {
                    report.error(
                        &format!("{path}.on.cap"),
                        "a capped step cannot target itself",
                    );
                }
            }
        }
        report.rows.push(NodeRow {
            workflow: name.into(),
            node: node.clone(),
            summary: step.summary.clone(),
            kind: "step".into(),
            runner: match step.kind() {
                Some(StepKind::Run(_)) => "run",
                Some(StepKind::Shell(_)) => "shell",
                Some(StepKind::Message(_)) => "message",
                Some(StepKind::Agent(_)) | None => "agent",
            }
            .into(),
            run: step.run.clone(),
            shell: step.shell.clone(),
            agent: step.agent.clone(),
            message: step.message.clone(),
            do_background: do_background.unwrap_or(false),
            edges: spec.edges(node),
        });
    }
    let missing = report
        .rows
        .iter()
        .flat_map(|row| {
            row.edges
                .iter()
                .filter(|edge| {
                    edge.fires == super::Fireability::Yes
                        && edge.source == EdgeSource::Default
                        && !spec.declared_default(&row.node, &edge.outcome)
                        && matches!(
                            edge.outcome.as_str(),
                            "timeout" | "invalid" | "undelivered" | "cap" | "deadline"
                        )
                })
                .map(move |edge| {
                    if edge.outcome == "deadline" {
                        "workflow.on.deadline".into()
                    } else {
                        let kind = if row.kind == "check" {
                            "checks"
                        } else {
                            "steps"
                        };
                        format!("{kind}.{}.on.{}", row.node, edge.outcome)
                    }
                })
        })
        .collect::<BTreeSet<_>>();
    if !missing.is_empty()
        && let Some(issue) = spec
            .ignored
            .iter()
            .find(|issue| issue.path.starts_with("workflow.on-break"))
    {
        report.error(&issue.path, "invalid on-break is needed by a failure edge; fix it or route every failure explicitly");
    }
    for path in missing {
        report.error(&path, "a fireable failure edge needs a route; route it to a node, @done, or @failed, or set [workflow] on-break");
    }
    if !crate::config::MachineConfig::load_lenient()
        .resume
        .auto_continue
    {
        for row in &report.rows {
            if row.agent.is_some() {
                let (kind, message) = if row.kind == "step" {
                    (
                        "steps",
                        "auto-continue is off; a step paused on a provider limit waits for you",
                    )
                } else if row
                    .agent
                    .as_deref()
                    .and_then(|agent| {
                        crate::harness::spec::resolve_profile(agent, &definitions.agent_profiles)
                            .ok()
                    })
                    .is_some_and(|profile| profile.kind.as_str() == "codex")
                {
                    (
                        "checks",
                        "auto-continue is off; a failed Codex judge retries once, then follows on.invalid",
                    )
                } else {
                    (
                        "checks",
                        "auto-continue is off; a provider limit mid-attempt ends the attempt as limited",
                    )
                };
                let path = if spec.is_break(&row.node) {
                    "workflow.on-break".into()
                } else {
                    format!("{kind}.{}", row.node)
                };
                report.warnings.push(issue(&path, message));
            }
        }
    }
    for (node, step) in &spec.steps {
        if let Some(visits) = step.max_visits
            && never_waits(step)
            && cycles_to(spec, node, |name| {
                spec.steps.get(name).is_some_and(never_waits)
                    || (spec.checks.contains_key(name)
                        && !spec
                            .edges(name)
                            .iter()
                            .any(|edge| edge.outcome != "deadline" && edge.target == "@wait"))
            })
        {
            report.warnings.push(issue(
                &format!("steps.{node}"),
                format!(
                    "cycle through command or message steps never waits and can spend all \
                     {visits} visits in one advance; route it through a check that can wait \
                     (@wait) if that is not intended"
                ),
            ));
        }
        if step.max_visits.is_none()
            && cycles_to(spec, node, |name| {
                !spec
                    .steps
                    .get(name)
                    .is_some_and(|step| step.max_visits.is_some())
            })
        {
            report.error(
                &format!("steps.{node}"),
                if step.max_visits_per_outcome.is_some() {
                    concat!(
                        "cycle through steps has no max-visits bound; ",
                        "a per-outcome bound does not bound the cycle: ",
                        "a check can name a new outcome or key every time; max-visits is needed"
                    )
                } else {
                    "cycle through steps has no max-visits bound"
                },
            );
        }
    }
    for node in spec.checks.keys() {
        if !spec
            .edges(node)
            .iter()
            .any(|edge| edge.outcome != "deadline" && edge.target == "@wait")
            && cycles_to(spec, node, |name| {
                spec.checks.contains_key(name)
                    && !spec
                        .edges(name)
                        .iter()
                        .any(|edge| edge.outcome != "deadline" && edge.target == "@wait")
            })
        {
            report.error(&format!("checks.{node}"), "check-only cycle has no @wait");
        }
    }
    for issue in report.errors.iter_mut().chain(&mut report.warnings) {
        issue.message = format!("{name}: {}", issue.message);
    }
    let walk = spec.walk();
    let order = walk
        .nodes
        .into_iter()
        .map(|node| node.node)
        .chain(walk.unreachable)
        .enumerate()
        .map(|(index, node)| (node, index))
        .collect::<BTreeMap<_, _>>();
    report.rows.sort_by_key(|row| order[&row.node]);
    report
}

impl Validation {
    fn error(&mut self, path: &str, message: impl Display) {
        self.errors.push(issue(path, message));
    }

    fn node_name(&mut self, path: &str, node: &str) {
        if let Err(err) = crate::harness::name::validate_name(node) {
            self.error(path, err);
        }
    }

    fn condition_fields(&mut self, path: &str, check: &spec::Check, project_root: Option<&Path>) {
        let mut outcomes = BTreeSet::new();
        if let Some(outcome) = &check.else_outcome {
            if RESERVED_OUTCOMES.contains(&outcome.as_str()) {
                self.error(&format!("{path}.else"), "choose a non-reserved outcome");
            }
            outcomes.insert(outcome.as_str());
        } else {
            self.error(&format!("{path}.else"), "a condition check requires else");
        }
        for (index, case) in check.cases.iter().enumerate() {
            let case_path = format!("{path}.cases.{index}");
            outcomes.insert(case.outcome.as_str());
            if RESERVED_OUTCOMES.contains(&case.outcome.as_str()) {
                self.error(
                    &format!("{case_path}.outcome"),
                    "choose a non-reserved outcome",
                );
            }
        }
        for outcome in &outcomes {
            if !check.on.contains_key(*outcome) {
                self.error(
                    &format!("{path}.on.{outcome}"),
                    "route every case and else outcome in on",
                );
            }
        }
        self.condition_terms(path, check, project_root);
    }

    fn condition_terms(&mut self, path: &str, check: &spec::Check, project_root: Option<&Path>) {
        use crate::harness::when::{WhenExpr, condition::condition_stages};

        let cases = check.condition_cases();
        let when_path = |index| {
            if check.is_sleep() {
                format!("{path}.when")
            } else {
                format!("{path}.cases.{index}.when")
            }
        };
        let mut expressions = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            match WhenExpr::parse(&case.when) {
                Ok(expression) => expressions.push((index, expression)),
                Err(error) => self.error(&when_path(index), error),
            }
        }
        let window = expressions.iter().any(|(_, expression)| {
            expression
                .terms()
                .any(|term| term.key.starts_with("window."))
        });
        if check.provider.is_none() && window {
            self.error(&format!("{path}.provider"), "window terms require provider");
        }
        if !expressions
            .iter()
            .any(|(_, expression)| expression.terms().any(|term| term.team_stage().is_some()))
        {
            return;
        }
        let Some(root) = project_root else {
            self.warnings.push(issue(
                path,
                "stage names were not checked: validate from the project's workspace",
            ));
            return;
        };
        let stages = match condition_stages(root) {
            Ok(stages) => stages,
            Err(error) => {
                self.error(path, error);
                return;
            }
        };
        for (index, _) in expressions {
            if let Err(error) = WhenExpr::parse_with_stages(&cases[index].when, Some(&stages)) {
                self.error(&when_path(index), error);
            }
        }
    }

    fn target(&mut self, spec: &WorkflowSpec, path: &str, target: &str) {
        if !matches!(target, "@wait" | "@done" | "@failed")
            && !spec.checks.contains_key(target)
            && !spec.steps.contains_key(target)
        {
            self.error(path, format!("unknown target {target}"));
        }
    }

    fn agent_fields(
        &mut self,
        dir: &Path,
        path: &str,
        agent: &str,
        prompt: Option<&str>,
        definitions: &LoadedDefinitions,
    ) {
        if definitions.failed.contains_key(agent) {
            self.error(
                &format!("{path}.agent"),
                format!(
                    "broken definition for {agent}: {:?}",
                    definitions.failed[agent]
                ),
            );
        } else if let Err(err) =
            crate::harness::spec::resolve_profile(agent, &definitions.agent_profiles)
        {
            self.error(&format!("{path}.agent"), err);
        }
        self.prompt(dir, path, prompt);
    }

    fn prompt(&mut self, dir: &Path, path: &str, prompt: Option<&str>) {
        if let Some(prompt) = prompt
            && let Err(err) = spec::confined_file(dir, prompt)
        {
            self.error(&format!("{path}.prompt-file"), err);
        }
    }
}

/// Command and message steps finish within one advance.
fn never_waits(step: &spec::Step) -> bool {
    matches!(
        step.kind(),
        Some(StepKind::Run(_) | StepKind::Shell(_) | StepKind::Message(_))
    )
}

/// A message step names one living agent: `@handle` or `@handle#channel`, never a person.
fn message_target(target: &str) -> Result<(), String> {
    if !target.starts_with('@') || target.contains(':') {
        return Err(format!(
            "message must be an agent address such as @planner or @planner#feat-x, not {target}"
        ));
    }
    let (selector, _) =
        crate::address::parse_selector(target).map_err(|error| error.to_string())?;
    match selector.as_str() {
        "user" => Err("@user is not an agent; a message step hands work to an agent".into()),
        "me" => Err("@me names the caller, and a workflow run has none; name the agent".into()),
        _ => Ok(()),
    }
}

/// A message step's `then` names exactly one target: any node, `@done`, or `@failed`.
fn message_then(report: &mut Validation, spec: &WorkflowSpec, path: &str, then: &[String]) {
    let path = format!("{path}.then");
    let [target] = then else {
        report.error(
            &path,
            "a message step's then names exactly one node, @done, or @failed",
        );
        return;
    };
    if matches!(target.as_str(), "@wait" | "@user") {
        report.error(
            &path,
            "a message step's then names a node, @done, or @failed",
        );
        return;
    }
    report.target(spec, &path, target);
}

fn step_layout(
    dir: &Path,
    agent: &str,
    lifecycle: StepLifecycle,
    definitions: &LoadedDefinitions,
    project_root: Option<&Path>,
) -> anyhow::Result<crate::harness::spec::LayoutSpec> {
    for cell in crate::harness::spec::parse_layout_structure(agent)?.cells() {
        if let Some(paths) = definitions.failed.get(cell) {
            anyhow::bail!("broken definition for {cell}: {paths:?}");
        }
    }
    let mut machine = (*crate::config::MachineConfig::load_lenient()).clone();
    machine
        .agents
        .profiles
        .clone_from(&definitions.agent_profiles);
    machine.agents.teams.clone_from(&definitions.teams);
    let effective = crate::config::effective::load(&machine, project_root.unwrap_or(dir))?;
    let resolved = crate::harness::plan::resolve_launch(
        &effective,
        crate::config::effective::ProfileScope::Agents,
        &machine.agents.commands,
        Some(agent),
        None,
    )?;
    // A stay step holds its attempt until the leader's session ends and judges no turn end,
    // the one thing a team has no answer for.
    if resolved.team_name.is_some() && lifecycle != StepLifecycle::Stay {
        anyhow::bail!("a team runs only in a stay step; set lifecycle = \"stay\"");
    }
    if resolved.layout.agent_cells().next().is_none() {
        anyhow::bail!("step layout needs a prompt leader; add an agent cell");
    }
    crate::harness::spec::prompt_leader(&resolved.layout, None)
        .map_err(|error| anyhow::anyhow!("{error}; choose an unambiguous prompt leader"))?;
    Ok(resolved.layout)
}

/// `None` when the profile does not resolve; `agent_fields` reports that error.
fn do_background(agent: Option<&str>, definitions: &LoadedDefinitions) -> Option<bool> {
    let profile =
        crate::harness::spec::resolve_profile(agent?, &definitions.agent_profiles).ok()?;
    let adapter = crate::agents::find_definition(profile.kind.as_str())?;
    Some(
        adapter
            .concern_coverage(crate::agents::IntegrationConcern::BackgroundParking)
            .is_wired(),
    )
}

fn cycles_to(spec: &WorkflowSpec, start: &str, allowed: impl Fn(&str) -> bool) -> bool {
    let mut pending = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        for edge in spec.resolved_edges(&node).unwrap_or_default() {
            if edge.outcome == "deadline"
                || (spec.steps.contains_key(&node) && edge.source != EdgeSource::Explicit)
            {
                continue;
            }
            let target = edge.target;
            if !allowed(&target) {
                continue;
            }
            if target == start {
                return true;
            }
            pending.push(target);
        }
    }
    false
}
