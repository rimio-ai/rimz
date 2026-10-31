use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::*;
use crate::cli::usage::{UsageError, shell_command};
use rimz::store::workflow::{RunAddress, ScopeKind};

enum Target {
    Id(WorkflowRunId),
    Address(RunAddress),
    Workflow(String),
}

pub(super) struct Located {
    pub(super) state: rimz::StatePaths,
    pub(super) paths: paths::WorkflowPaths,
    pub(super) record: RunRecord,
}

pub(super) enum Selection {
    Run(Box<Located>),
    Workflow(Vec<Located>),
}

pub(super) fn caller(globals: &super::super::GlobalFlags) -> Option<rimz::ResolvedWorkspace> {
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok()?;
    let recorded = paths::StatePaths::for_project_root(&workspace.project_root)
        .ok()
        .is_some_and(|state| rimz::workspace::record::read(&state.workspace_record).is_ok());
    if globals.root.is_none()
        && workspace.root_class == rimz::workspace::RootClass::Directory
        && !recorded
    {
        return None;
    }
    Some(workspace)
}

pub(super) fn all() -> Result<Vec<Located>> {
    let rooms: BTreeMap<_, _> = paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .map(|(state, _)| (state.root.clone(), state))
        .collect();
    let mut runs = Vec::new();
    for state in rooms.into_values() {
        for record in records::list(&state)? {
            runs.push(Located {
                paths: state.workflow(&record.id),
                state: state.clone(),
                record,
            });
        }
    }
    Ok(runs)
}

pub(super) fn command(
    verb: &str,
    targets: &[&str],
    root: Option<&Path>,
    at: Option<&str>,
    extend: &[String],
    flags: &[&str],
) -> String {
    let prefix = root.map_or_else(
        || "rimz".into(),
        |root| {
            let root = render::home_relative_path(root);
            if let Some(tail) = root.strip_prefix("~/") {
                // Keep tilde expansion outside the quoted path tail.
                format!("rimz --root ~/{}", shell_command([tail]))
            } else if root == "~" {
                "rimz --root ~".into()
            } else {
                shell_command(["rimz", "--root", &root])
            }
        },
    );
    let at_args = match at {
        Some(node) if node.starts_with('-') => vec![format!("--at={node}")],
        Some(node) => vec!["--at".into(), node.into()],
        None => Vec::new(),
    };
    let flags = flags
        .iter()
        .copied()
        .chain(at_args.iter().map(String::as_str))
        .chain(extend.iter().flat_map(|grant| ["--extend", grant.as_str()]));
    let args = ["workflow", verb].into_iter();
    let command = if targets.iter().any(|target| target.starts_with('-')) {
        shell_command(
            args.chain(flags)
                .chain(["--"])
                .chain(targets.iter().copied()),
        )
    } else {
        shell_command(args.chain(targets.iter().copied()).chain(flags))
    };
    format!("{prefix} {command}")
}

pub(super) fn run_command(
    record: &RunRecord,
    verb: &str,
    globals: &super::super::GlobalFlags,
    at: Option<&str>,
    extend: &[String],
    flags: &[&str],
) -> String {
    RunCommands::new(record, globals).command(verb, at, extend, flags)
}

pub(super) struct RunCommands<'a> {
    record: &'a RunRecord,
    target: String,
    root: Option<PathBuf>,
}

impl<'a> RunCommands<'a> {
    pub(super) fn new(record: &'a RunRecord, globals: &super::super::GlobalFlags) -> Self {
        let mut context = globals.clone();
        context.root = None;
        let elsewhere =
            caller(&context).is_none_or(|workspace| workspace.project_root != record.project_root);
        context.root = elsewhere.then(|| record.project_root.clone());
        let address = record.address().to_string();
        let target = match select(&address, "show", &context, None, &[]) {
            Ok(Selection::Run(run)) if run.record.id == record.id => address,
            _ => record.id.to_string(),
        };
        Self {
            record,
            target,
            root: context.root,
        }
    }

    pub(super) fn command(
        &self,
        verb: &str,
        at: Option<&str>,
        extend: &[String],
        flags: &[&str],
    ) -> String {
        let target = if verb != "show" && self.record.state.is_terminal() {
            self.record.id.as_str()
        } else {
            &self.target
        };
        command(verb, &[target], self.root.as_deref(), at, extend, flags)
    }
}

impl Target {
    fn parse(raw: &str) -> Result<Self> {
        if let Ok(id) = raw.parse::<WorkflowRunId>() {
            return Ok(Self::Id(id));
        }
        if !raw.contains('#') {
            return Ok(Self::Workflow(raw.into()));
        }
        let address = raw.parse().map_err(|_| {
            UsageError::new(
                "run address must be <workflow>#<lane>; workflow and lane must be nonempty".into(),
            )
        })?;
        Ok(Self::Address(address))
    }
}

pub(super) fn names_workflow(raw: &str) -> bool {
    matches!(Target::parse(raw), Ok(Target::Workflow(_)))
}

fn candidate_target(record: &RunRecord, runs: &[Located]) -> String {
    let address = record.address();
    if runs.iter().any(|other| {
        other.record.id != record.id
            && other.record.project_root == record.project_root
            && other.record.address() == address
    }) {
        record.id.to_string()
    } else {
        address.to_string()
    }
}

pub(super) fn select(
    raw: &str,
    verb: &str,
    globals: &super::super::GlobalFlags,
    at: Option<&str>,
    extend: &[String],
) -> Result<Selection> {
    let target = Target::parse(raw)?;
    if let Target::Id(id) = target {
        let (state, paths) = locate(&id)?;
        let record = records::load(&paths)?;
        return Ok(Selection::Run(Box::new(Located {
            state,
            paths,
            record,
        })));
    }
    let workspace = caller(globals);
    let root = workspace
        .as_ref()
        .map(|workspace| workspace.project_root.as_path());
    let name = match &target {
        Target::Address(address) => &address.workflow,
        Target::Workflow(name) => name,
        Target::Id(_) => unreachable!("ids returned above"),
    };
    let mut runs = all()?;
    runs.retain(|run| run.record.workflow == *name);
    let mut components = Path::new(name).components();
    let definition = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && paths::workflows_dir().join(name).is_dir();
    let known = !runs.is_empty() || definition;
    let local_workflow = runs
        .iter()
        .any(|run| Some(run.record.project_root.as_path()) == root);
    let live_addresses = runs
        .iter()
        .filter(|run| !run.record.state.is_terminal())
        .filter(|run| !local_workflow || Some(run.record.project_root.as_path()) == root)
        .map(|run| run.record.address().to_string())
        .collect::<BTreeSet<_>>();
    runs.retain(|run| match &target {
        Target::Address(address) => address.matches(&run.record.address()),
        _ => true,
    });
    if verb != "show"
        || matches!(target, Target::Workflow(_))
        || runs.iter().any(|run| !run.record.state.is_terminal())
    {
        runs.retain(|run| !run.record.state.is_terminal());
    }
    if runs
        .iter()
        .any(|run| Some(run.record.project_root.as_path()) == root)
    {
        runs.retain(|run| Some(run.record.project_root.as_path()) == root);
    }
    let roots = runs
        .iter()
        .map(|run| &run.record.project_root)
        .collect::<BTreeSet<_>>();
    if roots.len() > 1 {
        let commands = runs
            .iter()
            .map(|run| {
                command(
                    verb,
                    &[&candidate_target(&run.record, &runs)],
                    Some(&run.record.project_root),
                    at,
                    extend,
                    &[],
                )
            })
            .collect::<BTreeSet<_>>();
        bail!(
            "ambiguous workflow run {raw}:\n{}",
            commands.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    if matches!(target, Target::Workflow(_)) && verb == "show" {
        if !known {
            bail!("unknown workflow {name}");
        }
        return Ok(Selection::Workflow(runs));
    }
    if verb == "show" && runs.iter().all(|run| run.record.state.is_terminal()) {
        runs.sort_by_key(|run| std::cmp::Reverse(run.record.terminal_at));
        runs.truncate(1);
    }
    if matches!(target, Target::Workflow(_))
        && let Some(workspace) = &workspace
    {
        let local = |run: &Located| {
            run.record.scope.path == workspace.worktree_root
                || (run.record.scope.kind == ScopeKind::Project
                    && run.record.project_root == workspace.project_root)
        };
        if runs.iter().any(local) {
            runs.retain(local);
        }
    }
    if runs.len() > 1 {
        let mut context = globals.clone();
        context.root = None;
        let caller_root = caller(&context).map(|workspace| workspace.project_root);
        let choices = runs
            .iter()
            .map(|run| {
                let target = candidate_target(&run.record, &runs);
                let root = (caller_root.as_ref() != Some(&run.record.project_root))
                    .then_some(run.record.project_root.as_path());
                format!(
                    "  {} {}\n    {}",
                    run.record.id,
                    run.record.scope.path.display(),
                    command(verb, &[&target], root, at, extend, &[])
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!("ambiguous workflow run {raw}:\n{choices}");
    }
    if let Some(run) = runs.pop() {
        return Ok(Selection::Run(Box::new(run)));
    }
    let miss = match &target {
        Target::Address(address) => format!("no {} run in {}", address.workflow, address.lane),
        _ => format!("{name} has no live runs"),
    };
    let hint = if live_addresses.is_empty() {
        format!("{name} has no live runs")
    } else {
        format!(
            "live runs: {}",
            live_addresses.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    if miss == hint {
        bail!("{miss}");
    }
    bail!("{miss}\n{hint}")
}
