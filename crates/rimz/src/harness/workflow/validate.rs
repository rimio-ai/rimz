use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::Path;

use super::spec::{self, CheckKind, Invoke, Issue, RESERVED_OUTCOMES, StepKind, Wake, issue};
use super::{Definition, Target};
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
                | super::SpecErr::ReservedNode { path }
                | super::SpecErr::DuplicateNode { path, .. } => path.as_str(),
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
    spec: &Definition,
    definitions: &LoadedDefinitions,
    project_root: Option<&Path>,
) -> Validation {
    let mut report = Validation::default();
    if let Err(err) = crate::harness::name::validate_name(name) {
        report.error("workflow", err);
    }
    let header = &spec.authored().workflow;
    report.summary("workflow.summary", header.summary.as_deref());
    report.parses("workflow.deadline", header.deadline.as_deref(), |raw| {
        spec::parse_deadline(raw).map(|_| ())
    });
    if let Some(limit) = &header.limit {
        if limit.max_parallel.is_none() && limit.stagger.is_none() {
            report.error("workflow.limit", "set max-parallel or stagger");
        }
        report.limit("workflow.limit", limit);
    }
    if !spec
        .authored()
        .checks
        .contains_key(&spec.authored().workflow.start)
        && !spec
            .authored()
            .steps
            .contains_key(&spec.authored().workflow.start)
    {
        report.error(
            "workflow.start",
            format!(
                "{} must name a check or a step",
                spec.authored().workflow.start
            ),
        );
    }
    if let Some(target) = &spec.authored().workflow.on.deadline {
        report.target(spec, "workflow.on.deadline", target);
    }
    if let Some(spec::OnBreak::Target(target)) = &spec.authored().workflow.on_break
        && !matches!(target.as_str(), "@done" | "@failed")
    {
        report.error(
            "workflow.on-break",
            "use @failed, @done, or { agent = \"<profile>\" }",
        );
    }
    for (node, check) in &spec.authored().checks {
        let path = format!("{}.{node}", check.table.word());
        report.node_name(&path, node);
        if check.kind().is_none() {
            report.error(&path, "set exactly one of run or shell");
        }
        report.check_values(&path, check);
        let invoke = match check.kind() {
            Some(CheckKind::Test(invoke) | CheckKind::Script(invoke)) => Some(invoke),
            _ => None,
        };
        report.invoke(dir, &path, invoke);
        if matches!(check.kind(), Some(CheckKind::Test(_))) {
            for outcome in ["pass", "fail"] {
                if !check.on.contains_key(outcome) {
                    report.error(
                        &format!("{path}.on.{outcome}"),
                        "a test check requires both on.pass and on.fail",
                    );
                }
            }
        }
        if matches!(check.kind(), Some(CheckKind::Judge(_))) && check.prompt_file.is_none() {
            report.error(
                &format!("{path}.prompt-file"),
                "a judge check requires prompt-file",
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
        if let Some(CheckKind::Judge(agent)) = check.kind() {
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
                .any(|edge| edge.outcome == "unknown" && edge.target == Target::Wait);
        if edges
            .iter()
            .any(|edge| edge.outcome != "deadline" && edge.target == Target::Wait)
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
            report.target(spec, &format!("{path}.on.{outcome}"), route);
            if outcome == "timeout" && route == "@wait" {
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
            kind: check.table.word().into(),
            runner: match check.kind() {
                Some(CheckKind::Test(Invoke::Run(_)) | CheckKind::Script(Invoke::Run(_))) => "run",
                Some(CheckKind::Test(Invoke::Shell(_)) | CheckKind::Script(Invoke::Shell(_))) => {
                    "shell"
                }
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
    for (node, step) in &spec.authored().steps {
        let at_break = spec.is_break(node);
        let path = if at_break {
            "workflow.on-break".into()
        } else {
            format!("{}.{node}", step.table.word())
        };
        if !at_break {
            report.node_name(&path, node);
        }
        if step.kind().is_none() {
            report.error(&path, "set exactly one of run or shell");
        }
        let agent = matches!(step.kind(), Some(StepKind::Agent(_)));
        let command = matches!(step.kind(), Some(StepKind::Command(_)));
        let message = matches!(step.kind(), Some(StepKind::Message(_)));
        report.step_values(&path, step);
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
        let invoke = match step.kind() {
            Some(StepKind::Command(invoke)) => Some(invoke),
            _ => None,
        };
        report.invoke(dir, &path, invoke);
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
                .any(|target| !spec.authored().checks.contains_key(target))
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
                .filter_map(|node| spec.authored().checks.get(node))
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
                    && edge.target.as_str() == pair[1]
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
            kind: step.table.word().into(),
            runner: match step.kind() {
                Some(StepKind::Command(Invoke::Run(_))) => "run",
                Some(StepKind::Command(Invoke::Shell(_))) => "shell",
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
                        && matches!(
                            edge.outcome.as_str(),
                            "timeout" | "invalid" | "undelivered" | "cap" | "deadline"
                        )
                })
                .map(move |edge| {
                    if edge.outcome == "deadline" {
                        "workflow.on.deadline".into()
                    } else {
                        format!("{}.{}.on.{}", row.kind, row.node, edge.outcome)
                    }
                })
        })
        .collect::<BTreeSet<_>>();
    for path in missing {
        report.error(&path, "a fireable failure edge needs a route; route it to a node, @done, or @failed, or set [workflow] on-break");
    }
    if !crate::config::MachineConfig::load_lenient()
        .resume
        .auto_continue
    {
        for row in &report.rows {
            if row.agent.is_some() {
                let message = if row.kind == "agent" {
                    "auto-continue is off; a step paused on a provider limit waits for you"
                } else if row
                    .agent
                    .as_deref()
                    .and_then(|agent| {
                        crate::harness::spec::resolve_profile(agent, &definitions.agent_profiles)
                            .ok()
                    })
                    .is_some_and(|profile| profile.kind.as_str() == "codex")
                {
                    "auto-continue is off; a failed Codex judge retries once, then follows on.invalid"
                } else {
                    "auto-continue is off; a provider limit mid-attempt ends the attempt as limited"
                };
                let path = if spec.is_break(&row.node) {
                    "workflow.on-break".into()
                } else {
                    format!("{}.{}", row.kind, row.node)
                };
                report.warnings.push(issue(&path, message));
            }
        }
    }
    for (node, step) in &spec.authored().steps {
        if step.max_visits.is_none()
            && cycles_to(spec, node, |name| {
                !spec
                    .authored()
                    .steps
                    .get(name)
                    .is_some_and(|step| step.max_visits.is_some())
            })
        {
            report.error(
                &format!("{}.{node}", step.table.word()),
                "cycle through steps has no max-visits bound",
            );
        }
    }
    for (node, check) in &spec.authored().checks {
        if !spec
            .edges(node)
            .iter()
            .any(|edge| edge.outcome != "deadline" && edge.target == Target::Wait)
            && cycles_to(spec, node, |name| {
                spec.authored().checks.contains_key(name)
                    && !spec
                        .edges(name)
                        .iter()
                        .any(|edge| edge.outcome != "deadline" && edge.target == Target::Wait)
            })
        {
            report.error(
                &format!("{}.{node}", check.table.word()),
                "check-only cycle has no @wait",
            );
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

    /// Refuse an optional value its parser rejects, at the value's own path.
    fn parses<T: ?Sized, E: Display>(
        &mut self,
        path: &str,
        value: Option<&T>,
        parse: impl FnOnce(&T) -> Result<(), E>,
    ) {
        if let Some(error) = value.and_then(|value| parse(value).err()) {
            self.error(path, error);
        }
    }

    fn summary(&mut self, path: &str, summary: Option<&str>) {
        self.parses(path, summary, |text| {
            if text.trim().is_empty() || text.contains(['\n', '\r']) {
                Err("a summary is one nonempty line")
            } else {
                Ok(())
            }
        });
    }

    fn duration(&mut self, path: &str, raw: Option<&str>) {
        self.parses(path, raw, |raw| {
            crate::harness::duration::duration(raw).map(|_| ())
        });
    }

    /// A nonzero duration, as a hold or a stagger must be.
    fn nonzero(&mut self, path: &str, raw: Option<&str>, zero: &str) {
        self.parses(path, raw, |raw| {
            match crate::harness::duration::duration(raw) {
                Ok(duration) if duration.is_zero() => Err(zero.to_owned()),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        });
    }

    fn limit(&mut self, path: &str, limit: &crate::harness::throttle::Limit) {
        if limit.max_parallel == Some(0) {
            self.error(&format!("{path}.max-parallel"), "must be at least 1");
        }
        self.nonzero(
            &format!("{path}.stagger"),
            limit.stagger.as_deref(),
            "must be greater than zero",
        );
    }

    /// A test, script, or command's one invocation: an argv that resolves, or shell text.
    fn invoke(&mut self, dir: &Path, path: &str, invoke: Option<Invoke<'_>>) {
        match invoke {
            Some(Invoke::Run(run)) => {
                if let Err(error) = spec::resolve_argv(dir, run) {
                    self.error(&format!("{path}.run"), error);
                }
            }
            Some(Invoke::Shell(shell)) if shell.trim().is_empty() => {
                self.error(&format!("{path}.shell"), "set a nonempty shell command");
            }
            Some(Invoke::Shell(_)) | None => {}
        }
    }

    fn wakes(
        &mut self,
        path: &str,
        wakes: Option<&Vec<Wake>>,
        parse: fn(&Wake) -> Result<spec::Subscription, super::SpecErr>,
    ) {
        for (index, wake) in wakes.into_iter().flatten().enumerate() {
            if let Err(error) = parse(wake) {
                self.error(&format!("{path}.{index}"), error);
            }
        }
    }

    /// The values a check's own fields must hold, beyond what its kind table admits.
    fn check_values(&mut self, path: &str, check: &spec::Check) {
        let condition = matches!(check.kind(), Some(CheckKind::Condition));
        let sleep = check.is_sleep();
        self.summary(&format!("{path}.summary"), check.summary.as_deref());
        self.duration(&format!("{path}.timeout"), check.timeout.as_deref());
        self.parses(
            &format!("{path}.admission.window"),
            check.window.as_deref(),
            |raw| spec::parse_window(raw).map(|_| ()),
        );
        self.wakes(
            &format!("{path}.wake"),
            check.wake.as_ref(),
            Wake::subscription,
        );
        for (index, wake) in check.wake.iter().flatten().enumerate() {
            if wake.prompt().is_some() {
                self.error(
                    &format!("{path}.wake.{index}.prompt"),
                    "a prompt is only for an agent's subscribe binding",
                );
            }
        }
        let outcomes = check
            .cases
            .iter()
            .map(|case| case.outcome.as_str())
            .chain(check.else_outcome.as_deref())
            .collect::<BTreeSet<_>>();
        for outcome in check.on.keys() {
            let engine = RESERVED_OUTCOMES.contains(&outcome.as_str())
                && !matches!(outcome.as_str(), "timeout" | "invalid")
                && !(condition && outcome == "unknown");
            let dead = condition
                && !outcomes.contains(outcome.as_str())
                && !matches!(outcome.as_str(), "unknown" | "timeout" | "invalid");
            if engine || dead {
                self.error(
                    &format!("{path}.on.{outcome}"),
                    "dead route: this check cannot answer that outcome",
                );
            }
        }
        for (index, case) in check.cases.iter().enumerate() {
            let case_path = format!("{path}.cases.{index}.for");
            self.nonzero(&case_path, case.hold.as_deref(), "for must be nonzero");
            if case.hold.is_some() && check.on.get(&case.outcome).is_some_and(|to| to == "@wait") {
                self.error(&case_path, "a held case must route away from @wait");
            }
        }
        if sleep {
            self.parses(&format!("{path}.for"), check.hold.as_deref(), |raw| {
                let hold = crate::harness::duration::duration(raw).map_err(|e| e.to_string())?;
                if hold.is_zero() {
                    return Err("for must be nonzero".to_owned());
                }
                if check.when.is_none() {
                    return Err("for requires when".into());
                }
                if check
                    .sleep
                    .as_deref()
                    .and_then(|sleep| crate::harness::duration::duration(sleep).ok())
                    .is_some_and(|sleep| hold >= sleep)
                {
                    return Err("for must be shorter than sleep".into());
                }
                Ok(())
            });
        }
        if condition || sleep {
            self.parses(
                &format!("{path}.provider"),
                check.provider.as_ref(),
                |provider| {
                    crate::agents::find_definition(provider.as_str())
                        .map(|_| ())
                        .ok_or_else(|| format!("unknown provider {provider}"))
                },
            );
            let window = check
                .condition_cases()
                .iter()
                .filter_map(|case| crate::harness::when::WhenExpr::parse(&case.when).ok())
                .any(|expr| expr.terms().any(|term| term.key.starts_with("window.")));
            if check.provider.is_some() && !window {
                self.error(
                    &format!("{path}.provider"),
                    "provider requires a window term",
                );
            }
            if check.account.is_some() && check.provider.is_none() {
                self.error(&format!("{path}.account"), "account requires provider");
            }
        }
    }

    /// The values a step's own fields must hold, beyond what its kind table admits.
    fn step_values(&mut self, path: &str, step: &spec::Step) {
        let agent = matches!(step.kind(), Some(StepKind::Agent(_)));
        let lifecycle = step.lifecycle();
        self.summary(&format!("{path}.summary"), step.summary.as_deref());
        self.duration(&format!("{path}.timeout"), step.timeout.as_deref());
        if step.max_visits == Some(0) {
            self.error(&format!("{path}.max-visits"), "must be at least 1");
        }
        self.parses(
            &format!("{path}.admission.window"),
            step.window.as_deref(),
            |raw| spec::parse_window(raw).map(|_| ()),
        );
        if let Some(limit) = &step.limit {
            self.limit(&format!("{path}.admission"), limit);
        }
        self.parses(&format!("{path}.budget"), step.budget.as_deref(), |raw| {
            raw.parse::<crate::harness::budget::BudgetSpec>()
                .map(|_| ())
        });
        self.parses(
            &format!("{path}.worktree"),
            step.worktree.as_deref(),
            |name| {
                if name.is_empty() {
                    return Ok(());
                }
                crate::worktree::parse_requested_name(name).map(|_| ())
            },
        );
        if step
            .session
            .as_ref()
            .is_some_and(|session| session.as_str().trim().is_empty())
        {
            self.error(
                &format!("{path}.session"),
                "set a nonempty agent session id",
            );
        }
        if step.mode == Some(crate::agents::PermissionMode::Plan) {
            self.error(
                &format!("{path}.mode"),
                "plan mode cannot do a step's work; use auto, ask, or yolo",
            );
        }
        if !agent {
            return;
        }
        if step.keep.is_some() && lifecycle == StepLifecycle::Stay {
            self.error(
                &format!("{path}.keep"),
                "keep is only for turn and until agent steps",
            );
        }
        if step.subscribe.is_some() && lifecycle.is_turn() {
            self.error(
                &format!("{path}.subscribe"),
                "only a staying agent step receives signals; a turn ends where they would land",
            );
        }
        self.wakes(
            &format!("{path}.subscribe"),
            step.subscribe.as_ref(),
            Wake::step_subscription,
        );
        if step.timeout.is_some() && lifecycle == StepLifecycle::Stay {
            self.error(
                &format!("{path}.timeout"),
                "a staying step has no step timeout; remove timeout or use lifecycle = \"turn\"",
            );
        }
        if step.on.asked.is_some() && !lifecycle.is_turn() {
            self.error(
                &format!("{path}.on.asked"),
                "a staying step never ends on a question; remove on.asked or use lifecycle = \"turn\"",
            );
        }
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
        if !check.on.contains_key("unknown") {
            self.error(
                &format!("{path}.on.unknown"),
                "a condition check requires on.unknown; route it to @wait to keep waiting while a term is unknown",
            );
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

    fn target(&mut self, spec: &Definition, path: &str, target: &str) {
        if !matches!(target, "@wait" | "@done" | "@failed")
            && !spec.authored().checks.contains_key(target)
            && !spec.authored().steps.contains_key(target)
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
fn message_then(report: &mut Validation, spec: &Definition, path: &str, then: &[String]) {
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

fn cycles_to(spec: &Definition, start: &str, allowed: impl Fn(&str) -> bool) -> bool {
    let mut pending = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        for edge in spec.resolved_edges(&node).unwrap_or_default() {
            if edge.outcome == "deadline" {
                continue;
            }
            let target = edge.target.as_str().to_owned();
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
