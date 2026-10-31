use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::Path;

use super::spec::{self, CheckKind, RESERVED_OUTCOMES, StepKind, WorkflowSpec};
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
    pub do_background: bool,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Serialize)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Validation {
    pub rows: Vec<NodeRow>,
    pub warnings: Vec<Issue>,
    pub errors: Vec<Issue>,
}

pub fn validate(
    dir: &Path,
    workflows_home: &Path,
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
    let confined = canonical.as_ref().is_ok_and(|dir| {
        workflows_home
            .canonicalize()
            .is_ok_and(|home| *dir != home && dir.starts_with(home))
    });
    if !confined {
        report.error(
            "workflow",
            format!(
                "{name}: directory must exist inside {}",
                workflows_home.display()
            ),
        );
        return report;
    }
    let spec = match super::load(dir) {
        Ok(spec) => spec,
        Err(err) => {
            let path = match &err {
                super::SpecErr::Toml { path, .. } => path.as_str(),
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
    let mut report = Validation::default();
    if let Err(err) = crate::harness::schedule::validate_name(name) {
        report.error("workflow", err);
    }
    if !spec.checks.contains_key(&spec.workflow.start) {
        report.error(
            "workflow.start",
            format!("{} must name a check", spec.workflow.start),
        );
    }
    if let Some(deadline) = &spec.workflow.deadline
        && let Err(err) = spec::parse_deadline(deadline)
    {
        report.error("workflow.deadline", err);
    }
    if let Some(target) = &spec.workflow.on.deadline {
        report.target(spec, "workflow.on.deadline", target);
    }
    for (node, check) in &spec.checks {
        let path = format!("checks.{node}");
        report.node_name(&path, node);
        report.summary(&path, check.summary.as_deref());
        if spec.steps.contains_key(node) {
            report.error(&path, "name is also used by a step");
        }
        if check.kind().is_none() {
            report.error(
                &path,
                "set exactly one of run, shell, agent, or cases (nonempty)",
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
            for outcome in check.on.keys() {
                if !matches!(outcome.as_str(), "pass" | "fail" | "timeout" | "invalid") {
                    report.error(
                        &format!("{path}.on.{outcome}"),
                        "a shell check routes only pass, fail, timeout, and invalid",
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
        for wake in check.wake.iter().flatten() {
            if let Err(error) = wake.subscription() {
                report.error(&format!("{path}.wake"), error);
            }
        }
        for (field, present) in [
            ("wake", check.wake.is_some() && condition),
            ("else", check.else_outcome.is_some() && !condition),
            ("provider", check.provider.is_some() && !condition),
            ("account", check.account.is_some() && !condition),
            (
                "prompt-file",
                check.prompt_file.is_some() && !matches!(check.kind(), Some(CheckKind::Agent(_))),
            ),
            (
                "window",
                check.window.is_some() && !matches!(check.kind(), Some(CheckKind::Agent(_))),
            ),
        ] {
            if present {
                report.error(
                    &format!("{path}.{field}"),
                    "field is not supported by this check kind",
                );
            }
        }
        if condition {
            report.condition_fields(&path, check, project_root);
        }
        if let Some(CheckKind::Agent(agent)) = check.kind() {
            report.agent_fields(
                dir,
                &path,
                Some(agent),
                check.prompt_file.as_deref(),
                check.window.as_deref(),
                definitions,
            );
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
        report.timeout(&path, check.timeout.as_deref());
        let edges = spec.edges(node);
        let unknown_waits = condition
            && edges
                .iter()
                .any(|edge| edge.outcome == "unknown" && edge.target == "@wait");
        if edges
            .iter()
            .any(|edge| edge.outcome != "deadline" && edge.target == "@wait")
            && check.timeout.is_none()
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
        for (outcome, target) in &check.on {
            report.target(spec, &format!("{path}.on.{outcome}"), target);
            if outcome == "timeout" && target == "@wait" {
                report.error(
                    &format!("{path}.on.timeout"),
                    "a timed-out check cannot wait on its expired timeout clock",
                );
            }
            if RESERVED_OUTCOMES.contains(&outcome.as_str())
                && !matches!(outcome.as_str(), "timeout" | "invalid")
                && !(outcome == "unknown" && condition)
            {
                report.error(
                    &format!("{path}.on.{outcome}"),
                    "reserved engine outcome; a check routes its own outcomes plus timeout and invalid",
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
                _ => "agent",
            }
            .into(),
            run: check.run.clone(),
            shell: check.shell.clone(),
            agent: check.agent.clone(),
            do_background: do_background(check.agent.as_deref(), definitions).unwrap_or(false),
            edges,
        });
    }
    for (node, step) in &spec.steps {
        let path = format!("steps.{node}");
        report.node_name(&path, node);
        report.summary(&path, step.summary.as_deref());
        if step.kind().is_none() {
            report.error(&path, "set exactly one of agent, run, or shell");
        }
        let agent = matches!(step.kind(), Some(StepKind::Agent(_)));
        let command = matches!(step.kind(), Some(StepKind::Run(_) | StepKind::Shell(_)));
        if agent && step.prompt_file.is_none() {
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
        if command {
            for (field, present, message) in [
                (
                    "takeover",
                    step.takeover.is_some(),
                    "a command step never waits for occupancy, so it has nothing to take over",
                ),
                (
                    "throttle",
                    step.throttle.is_some(),
                    "capacity admission is only for agent steps",
                ),
                (
                    "capacity-wait",
                    step.capacity_wait.is_some(),
                    "capacity admission is only for agent steps",
                ),
                (
                    "on.capacity",
                    step.on.capacity.is_some(),
                    "capacity admission is only for agent steps",
                ),
                (
                    "lifecycle",
                    step.lifecycle.is_some(),
                    "a command step ends when its command exits; lifecycle is for agent steps",
                ),
                (
                    "window",
                    step.window.is_some(),
                    "windows gate provider usage, which a command step does not use",
                ),
                (
                    "on.asked",
                    step.on.asked.is_some(),
                    "a command cannot ask; its outcome goes to prev_attempt",
                ),
                (
                    "prompt-file",
                    step.prompt_file.is_some(),
                    "a command step takes no prompt; it reads context.json",
                ),
            ] {
                if present {
                    report.error(&format!("{path}.{field}"), message);
                }
            }
        }
        let layout = if let Some(StepKind::Agent(agent)) = step.kind() {
            report.prompt_window(
                dir,
                &path,
                step.prompt_file.as_deref(),
                step.window.as_deref(),
            );
            match step_layout(dir, agent, definitions, project_root) {
                Ok(layout) => Some(layout),
                Err(error) => {
                    report.error(&format!("{path}.agent"), error);
                    None
                }
            }
        } else {
            None
        };
        if agent && !step.lifecycle.unwrap_or_default().is_turn() {
            if step.timeout.is_some() {
                report.error(
                    &format!("{path}.timeout"),
                    "a staying step has no step timeout; remove timeout or use lifecycle = 'turn'",
                );
            }
            if step.on.asked.is_some() {
                report.error(
                    &format!("{path}.on.asked"),
                    "a staying step never ends on a question; remove on.asked or use lifecycle = 'turn'",
                );
            }
        }
        report.timeout(&path, step.timeout.as_deref());
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
        if matches!(
            step.lifecycle.unwrap_or_default(),
            StepLifecycle::Turn | StepLifecycle::Until
        ) && do_background == Some(false)
        {
            report.warnings.push(issue(&path, "step adapter has no background reporting; a turn end may finish the attempt while provider background work remains"));
        }
        if !step
            .then
            .as_ref()
            .is_some_and(|target| spec.checks.contains_key(target))
        {
            report.error(
                &format!("{path}.then"),
                "then must name a check, never a step or @done",
            );
        }
        if step.max_visits == Some(0) {
            report.error(&format!("{path}.max-visits"), "must be at least 1");
        }
        if step.max_visits_per_key == Some(0) {
            report.error(&format!("{path}.max-visits-per-key"), "must be at least 1");
        }
        if let (Some(per_key), Some(total)) = (step.max_visits_per_key, step.max_visits)
            && per_key >= total
        {
            report.error(
                &format!("{path}.max-visits-per-key"),
                "must be below max-visits, since the total cap trips first",
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
        for (outcome, target) in [("asked", &step.on.asked), ("cap", &step.on.cap)] {
            if outcome == "asked" && !agent {
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
                _ => "agent",
            }
            .into(),
            run: step.run.clone(),
            shell: step.shell.clone(),
            agent: step.agent.clone(),
            do_background: do_background.unwrap_or(false),
            edges: spec.edges(node),
        });
    }
    if !crate::config::MachineConfig::load_lenient()
        .resume
        .auto_continue
    {
        for row in &report.rows {
            if spec
                .steps
                .get(&row.node)
                .is_some_and(|step| !step.lifecycle.unwrap_or_default().is_turn())
            {
                continue;
            }
            if row.agent.is_some() {
                report.warnings.push(issue(&format!("{}.{}", if row.kind == "step" { "steps" } else { "checks" }, row.node), "auto-continue is off; a provider limit mid-attempt ends the attempt as limited"));
            }
        }
    }
    for (node, step) in &spec.steps {
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
                if step.max_visits_per_key.is_some() {
                    concat!(
                        "cycle through steps has no max-visits bound; ",
                        "a per-key bound does not bound the cycle: ",
                        "a check can name a new key every time; max-visits is needed"
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

fn issue(path: &str, message: impl Display) -> Issue {
    Issue {
        path: path.into(),
        message: format!("{path}: {message}"),
    }
}

impl Validation {
    fn error(&mut self, path: &str, message: impl Display) {
        self.errors.push(issue(path, message));
    }

    fn node_name(&mut self, path: &str, node: &str) {
        if let Err(err) = crate::harness::schedule::validate_name(node) {
            self.error(path, err);
        }
    }

    fn summary(&mut self, path: &str, summary: Option<&str>) {
        if summary.is_some_and(|text| text.trim().is_empty() || text.contains(['\n', '\r'])) {
            self.error(&format!("{path}.summary"), "a summary is one nonempty line");
        }
    }

    fn condition_fields(&mut self, path: &str, check: &spec::Check, project_root: Option<&Path>) {
        use crate::harness::schedule::{condition_stages, when::WhenExpr};
        use crate::utils::time::{DurationUnit, parse_duration_units};

        let mut outcomes = BTreeSet::new();
        if let Some(outcome) = &check.else_outcome {
            if RESERVED_OUTCOMES.contains(&outcome.as_str()) {
                self.error(&format!("{path}.else"), "choose a non-reserved outcome");
            }
            outcomes.insert(outcome.as_str());
        } else {
            self.error(&format!("{path}.else"), "a condition check requires else");
        }
        let mut expressions = Vec::new();
        for (index, case) in check.cases.iter().enumerate() {
            let case_path = format!("{path}.cases.{index}");
            outcomes.insert(case.outcome.as_str());
            if RESERVED_OUTCOMES.contains(&case.outcome.as_str()) {
                self.error(
                    &format!("{case_path}.outcome"),
                    "choose a non-reserved outcome",
                );
            }
            match WhenExpr::parse(&case.when) {
                Ok(expression) => {
                    if expression
                        .terms()
                        .any(|term| term.values.iter().any(|value| value == "unknown"))
                    {
                        self.error(&format!("{case_path}.when"), "unread terms answer the unknown outcome; route on.unknown instead of matching unknown");
                    }
                    expressions.push((index, expression));
                }
                Err(error) => self.error(&format!("{case_path}.when"), error),
            }
            if let Some(hold) = &case.hold {
                if check
                    .on
                    .get(&case.outcome)
                    .is_some_and(|target| target == "@wait")
                {
                    self.error(
                        &format!("{case_path}.for"),
                        "a held case must route away from @wait",
                    );
                }
                match parse_duration_units(
                    hold,
                    &[
                        DurationUnit::Second,
                        DurationUnit::Minute,
                        DurationUnit::Hour,
                        DurationUnit::Day,
                    ],
                ) {
                    Ok(duration) if duration.is_zero() => {
                        self.error(&format!("{case_path}.for"), "for must be nonzero")
                    }
                    Err(error) => self.error(&format!("{case_path}.for"), error),
                    _ => {}
                }
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
        for outcome in check.on.keys() {
            if !outcomes.contains(outcome.as_str())
                && !matches!(outcome.as_str(), "unknown" | "timeout" | "invalid")
            {
                self.error(
                    &format!("{path}.on.{outcome}"),
                    "dead route: name a case outcome, else, unknown, timeout, or invalid",
                );
            }
        }
        let window = expressions.iter().any(|(_, expression)| {
            expression
                .terms()
                .any(|term| term.key.starts_with("window."))
        });
        match (&check.provider, window) {
            (None, true) => {
                self.error(&format!("{path}.provider"), "window terms require provider")
            }
            (Some(_), false) => self.error(
                &format!("{path}.provider"),
                "provider requires a window term",
            ),
            (Some(provider), true)
                if crate::agents::find_definition(provider.as_str()).is_none() =>
            {
                self.error(
                    &format!("{path}.provider"),
                    format!("unknown provider {provider}"),
                )
            }
            _ => {}
        }
        if check.account.is_some() && check.provider.is_none() {
            self.error(&format!("{path}.account"), "account requires provider");
        }
        if !expressions
            .iter()
            .any(|(_, expression)| expression.terms().any(|term| term.key == "team.stage"))
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
            if let Err(error) = WhenExpr::parse_with_stages(&check.cases[index].when, Some(&stages))
            {
                self.error(&format!("{path}.cases.{index}.when"), error);
            }
        }
    }

    fn target(&mut self, spec: &WorkflowSpec, path: &str, target: &str) {
        if !matches!(target, "@wait" | "@done" | "@failed" | "@user")
            && !spec.checks.contains_key(target)
            && !spec.steps.contains_key(target)
        {
            self.error(path, format!("unknown target {target}"));
        }
    }

    fn timeout(&mut self, path: &str, timeout: Option<&str>) {
        use crate::utils::time::{DurationUnit, parse_duration_units};
        if let Some(raw) = timeout
            && let Err(err) = parse_duration_units(
                raw,
                &[
                    DurationUnit::Second,
                    DurationUnit::Minute,
                    DurationUnit::Hour,
                    DurationUnit::Day,
                ],
            )
        {
            self.error(&format!("{path}.timeout"), err);
        }
    }

    fn agent_fields(
        &mut self,
        dir: &Path,
        path: &str,
        agent: Option<&str>,
        prompt: Option<&str>,
        window: Option<&str>,
        definitions: &LoadedDefinitions,
    ) {
        if let Some(agent) = agent {
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
        }
        if window.is_some() && agent.is_none() {
            self.error(&format!("{path}.window"), "window requires an agent");
        }
        self.prompt_window(dir, path, prompt, window);
    }

    fn prompt_window(
        &mut self,
        dir: &Path,
        path: &str,
        prompt: Option<&str>,
        window: Option<&str>,
    ) {
        if let Some(prompt) = prompt
            && let Err(err) = spec::confined_file(dir, prompt)
        {
            self.error(&format!("{path}.prompt-file"), err);
        }
        let Some(window) = window else { return };
        if let Err(err) = spec::parse_window(window) {
            self.error(&format!("{path}.window"), err);
        }
    }
}

fn step_layout(
    dir: &Path,
    agent: &str,
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
    if resolved.team_name.is_some() {
        anyhow::bail!("steps do not support teams; use an agent layout instead");
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
