use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::Path;

use super::spec::{self, CheckKind, WorkflowSpec};
use crate::config::definitions::LoadedDefinitions;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Edge {
    pub outcome: String,
    pub target: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct NodeRow {
    pub workflow: String,
    pub node: String,
    pub kind: String,
    pub runner: String,
    pub run: Option<String>,
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

pub fn validate(dir: &Path, workflows_home: &Path, definitions: &LoadedDefinitions) -> Validation {
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
    validate_spec(dir, name, &spec, definitions)
}

pub(super) fn validate_spec(
    dir: &Path,
    name: &str,
    spec: &WorkflowSpec,
    definitions: &LoadedDefinitions,
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
        if spec.steps.contains_key(node) {
            report.error(&path, "name is also used by a step");
        }
        if check.kind().is_none() {
            report.error(&path, "set exactly one of run or agent");
        }
        if let Some(CheckKind::Run(run)) = check.kind()
            && let Err(err) = spec::resolve_argv(dir, run)
        {
            report.error(&format!("{path}.run"), err);
        }
        if matches!(check.kind(), Some(CheckKind::Agent(_))) && check.prompt_file.is_none() {
            report.error(
                &format!("{path}.prompt-file"),
                "an agent check requires prompt-file",
            );
        }
        report.agent_fields(
            dir,
            &path,
            check.agent.as_deref(),
            check.prompt_file.as_deref(),
            check.window.as_deref(),
            definitions,
        );
        if let Some(agent) = &check.agent
            && !definitions.failed.contains_key(agent)
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
        report.timeout(&path, check.timeout.as_deref());
        let edges = check.edges();
        if edges.values().any(|target| target == "@wait") && check.timeout.is_none() {
            report.error(
                &format!("{path}.timeout"),
                "a waiting check requires timeout",
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
            if matches!(outcome.as_str(), "cap" | "deadline") {
                report.error(
                    &format!("{path}.on.{outcome}"),
                    "reserved engine outcome; a check routes its own outcomes plus timeout and invalid",
                );
            }
            if let Some(agent) = &check.agent
                && spec
                    .steps
                    .get(target)
                    .is_some_and(|step| &step.agent == agent)
            {
                report.warnings.push(issue(&path, format!("judge and step {target} use the same profile {agent}; independence relies on the prompt")));
            }
        }
        report.rows.push(NodeRow {
            workflow: name.into(),
            node: node.clone(),
            kind: "check".into(),
            runner: match check.kind() {
                Some(CheckKind::Run(_)) => "run",
                _ => "agent",
            }
            .into(),
            run: check.run.clone(),
            agent: check.agent.clone(),
            do_background: do_background(check.agent.as_deref(), definitions).unwrap_or(false),
            edges: resolved_edges(spec, edges, |outcome| check.on.contains_key(outcome)),
        });
    }
    for (node, step) in &spec.steps {
        let path = format!("steps.{node}");
        report.node_name(&path, node);
        report.agent_fields(
            dir,
            &path,
            Some(&step.agent),
            Some(&step.prompt_file),
            step.window.as_deref(),
            definitions,
        );
        report.timeout(&path, step.timeout.as_deref());
        let do_background = do_background(Some(&step.agent), definitions);
        if do_background == Some(false) {
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
        let mut edges = BTreeMap::new();
        if let Some(target) = &step.then {
            edges.insert("then".into(), target.clone());
        }
        for (outcome, target) in [("asked", &step.on.asked), ("cap", &step.on.cap)] {
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
            edges.insert(
                outcome.into(),
                target.clone().unwrap_or_else(|| "@user".into()),
            );
        }
        report.rows.push(NodeRow {
            workflow: name.into(),
            node: node.clone(),
            kind: "step".into(),
            runner: "agent".into(),
            run: None,
            agent: Some(step.agent.clone()),
            do_background: do_background.unwrap_or(false),
            edges: resolved_edges(spec, edges, |outcome| match outcome {
                "then" => true,
                "asked" => step.on.asked.is_some(),
                "cap" => step.on.cap.is_some(),
                _ => false,
            }),
        });
    }
    if !crate::config::MachineConfig::load_lenient()
        .resume
        .auto_continue
    {
        for row in &report.rows {
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
                "cycle through steps has no max-visits bound",
            );
        }
    }
    for (node, check) in &spec.checks {
        if !check.edges().values().any(|target| target == "@wait")
            && cycles_to(spec, node, |name| {
                spec.checks
                    .get(name)
                    .is_some_and(|check| !check.edges().values().any(|target| target == "@wait"))
            })
        {
            report.error(&format!("checks.{node}"), "check-only cycle has no @wait");
        }
    }
    for issue in report.errors.iter_mut().chain(&mut report.warnings) {
        issue.message = format!("{name}: {}", issue.message);
    }
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
        if let Some(prompt) = prompt
            && let Err(err) = spec::confined_file(dir, prompt)
        {
            self.error(&format!("{path}.prompt-file"), err);
        }
        let Some(window) = window else { return };
        if agent.is_none() {
            self.error(&format!("{path}.window"), "window requires an agent");
        }
        if let Err(err) = spec::parse_window(window) {
            self.error(&format!("{path}.window"), err);
        }
    }
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

fn resolved_edges(
    spec: &WorkflowSpec,
    mut edges: BTreeMap<String, String>,
    explicit: impl Fn(&str) -> bool,
) -> Vec<Edge> {
    edges.insert(
        "deadline".into(),
        spec.workflow
            .on
            .deadline
            .clone()
            .unwrap_or_else(|| "@user".into()),
    );
    edges
        .into_iter()
        .map(|(outcome, target)| {
            let is_explicit = if outcome == "deadline" {
                spec.workflow.on.deadline.is_some()
            } else {
                explicit(&outcome)
            };
            Edge {
                outcome,
                target,
                source: if is_explicit { "explicit" } else { "default" }.into(),
            }
        })
        .collect()
}

fn cycles_to(spec: &WorkflowSpec, start: &str, allowed: impl Fn(&str) -> bool) -> bool {
    let mut pending = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let targets = if let Some(check) = spec.checks.get(&node) {
            check.edges().into_values().collect::<Vec<_>>()
        } else if let Some(step) = spec.steps.get(&node) {
            [
                step.then.as_deref(),
                step.on.asked.as_deref(),
                step.on.cap.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect()
        } else {
            continue;
        };
        for target in targets {
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
