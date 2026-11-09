//! Workflow run targets: what `rimz workflow` commands select, and the copyable command that
//! names a run through that same selection.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rimz::disk::paths;
use rimz::ids::WorkflowRunId;
use rimz::store::workflow::{self as records, RunAddress, RunRecord, ScopeKind};

use crate::cli::GlobalFlags;
use crate::cli::render;
use crate::cli::usage::{UsageError, shell_command};

enum Target {
    Id(WorkflowRunId),
    Address(RunAddress),
    Workflow(String),
}

pub(in crate::cli) struct Located {
    pub(in crate::cli) state: rimz::StatePaths,
    pub(in crate::cli) paths: paths::WorkflowPaths,
    pub(in crate::cli) record: RunRecord,
}

pub(in crate::cli) enum Selection {
    Run(Box<Located>),
    Workflow(Vec<Located>),
}

enum Picked {
    Run(usize),
    Workflow(Vec<usize>),
}

pub(in crate::cli) fn caller(globals: &GlobalFlags) -> Option<rimz::ResolvedWorkspace> {
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

pub(in crate::cli) fn locate(
    id: &WorkflowRunId,
) -> Result<(rimz::StatePaths, paths::WorkflowPaths)> {
    paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .find(|(state, run)| state.workflow(id).home == run.home)
        .with_context(|| format!("unknown workflow run {id}"))
}

pub(in crate::cli) fn all() -> Result<Vec<Located>> {
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

pub(in crate::cli) fn command(
    verb: &str,
    targets: &[&str],
    root: Option<&Path>,
    at: Option<&str>,
    extend: &[String],
    flags: &[&str],
) -> String {
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
    format!("{} {command}", rimz_prefix(root))
}

/// `rimz`, with `--root` when the command must reach another room.
pub(in crate::cli) fn rimz_prefix(root: Option<&Path>) -> String {
    root.map_or_else(
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
    )
}

pub(in crate::cli) fn run_command(
    record: &RunRecord,
    verb: &str,
    globals: &GlobalFlags,
    at: Option<&str>,
    extend: &[String],
    flags: &[&str],
) -> String {
    RunCommands::new(record, globals).command(verb, at, extend, flags)
}

/// Names many runs for copyable commands against one load of every room's runs, and one caller
/// resolution per room the runs live in.
pub(in crate::cli) struct RunTargets {
    globals: GlobalFlags,
    runs: Option<Vec<Located>>,
    home: Option<rimz::ResolvedWorkspace>,
    rooms: RefCell<BTreeMap<PathBuf, Option<rimz::ResolvedWorkspace>>>,
}

impl RunTargets {
    pub(in crate::cli) fn load(globals: &GlobalFlags) -> Self {
        let mut globals = globals.clone();
        globals.root = None;
        Self {
            home: caller(&globals),
            runs: all().ok(),
            globals,
            rooms: RefCell::default(),
        }
    }

    pub(in crate::cli) fn commands<'a>(&self, record: &'a RunRecord) -> RunCommands<'a> {
        let home = self.home.as_ref().map(|workspace| &workspace.project_root);
        let elsewhere = home != Some(&record.project_root);
        let mut context = self.globals.clone();
        context.root = elsewhere.then(|| record.project_root.clone());
        let address = record.address().to_string();
        let selected = self.runs.as_deref().and_then(|runs| {
            let workspace = match &context.root {
                None => self.home.clone(),
                Some(root) => self
                    .rooms
                    .borrow_mut()
                    .entry(root.clone())
                    .or_insert_with(|| caller(&context))
                    .clone(),
            };
            let target = Target::parse(&address).ok()?;
            let home = || home.cloned();
            match pick(
                &target,
                &address,
                "show",
                workspace.as_ref(),
                home,
                runs,
                None,
                &[],
            ) {
                Ok(Picked::Run(index)) => Some(&runs[index].record.id),
                _ => None,
            }
        });
        let target = if selected == Some(&record.id) {
            address
        } else {
            record.id.to_string()
        };
        RunCommands {
            record,
            target,
            root: context.root,
        }
    }
}

pub(in crate::cli) struct RunCommands<'a> {
    record: &'a RunRecord,
    target: String,
    root: Option<PathBuf>,
}

impl<'a> RunCommands<'a> {
    pub(in crate::cli) fn new(record: &'a RunRecord, globals: &GlobalFlags) -> Self {
        RunTargets::load(globals).commands(record)
    }

    /// What `workflow show` selects this run by: its address, or its id when the address
    /// would select another run.
    pub(in crate::cli) fn target(&self) -> &str {
        &self.target
    }

    pub(in crate::cli) fn command(
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

pub(in crate::cli) fn names_workflow(raw: &str) -> bool {
    matches!(Target::parse(raw), Ok(Target::Workflow(_)))
}

fn candidate_target(record: &RunRecord, runs: &[&Located]) -> String {
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

pub(in crate::cli) fn select(
    raw: &str,
    verb: &str,
    globals: &GlobalFlags,
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
    let home = || {
        let mut context = globals.clone();
        context.root = None;
        caller(&context).map(|workspace| workspace.project_root)
    };
    let mut runs = all()?;
    let picked = pick(
        &target,
        raw,
        verb,
        workspace.as_ref(),
        home,
        &runs,
        at,
        extend,
    )?;
    Ok(match picked {
        Picked::Run(index) => Selection::Run(Box::new(runs.swap_remove(index))),
        Picked::Workflow(indices) => {
            let keep = indices.into_iter().collect::<BTreeSet<_>>();
            let mut index = 0;
            runs.retain(|_| {
                index += 1;
                keep.contains(&(index - 1))
            });
            Selection::Workflow(runs)
        }
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "select's inputs, with the home scan and caller lookup passed in for batch naming"
)]
fn pick(
    target: &Target,
    raw: &str,
    verb: &str,
    workspace: Option<&rimz::ResolvedWorkspace>,
    home: impl FnOnce() -> Option<PathBuf>,
    all: &[Located],
    at: Option<&str>,
    extend: &[String],
) -> Result<Picked> {
    let root = workspace.map(|workspace| workspace.project_root.as_path());
    let name = match target {
        Target::Address(address) => &address.workflow,
        Target::Workflow(name) => name,
        Target::Id(_) => unreachable!("select returns ids before picking"),
    };
    let mut runs = all
        .iter()
        .filter(|run| run.record.workflow == *name)
        .collect::<Vec<_>>();
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
    runs.retain(|run| match target {
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
    let index_of = |run: &Located| {
        all.iter()
            .position(|other| std::ptr::eq(other, run))
            .expect("every picked run is borrowed from all")
    };
    if matches!(target, Target::Workflow(_)) && verb == "show" {
        if !known {
            bail!("unknown workflow {name}");
        }
        return Ok(Picked::Workflow(runs.into_iter().map(index_of).collect()));
    }
    if verb == "show" && runs.iter().all(|run| run.record.state.is_terminal()) {
        runs.sort_by_key(|run| std::cmp::Reverse(run.record.terminal_at));
        runs.truncate(1);
    }
    if matches!(target, Target::Workflow(_))
        && let Some(workspace) = workspace
    {
        let local = |run: &&Located| {
            run.record.scope.path == workspace.worktree_root
                || (run.record.scope.kind == ScopeKind::Project
                    && run.record.project_root == workspace.project_root)
        };
        if runs.iter().any(local) {
            runs.retain(local);
        }
    }
    if runs.len() > 1 {
        let caller_root = home();
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
        return Ok(Picked::Run(index_of(run)));
    }
    let miss = match target {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(root: &str) -> rimz::ResolvedWorkspace {
        let root = PathBuf::from(root);
        rimz::ResolvedWorkspace {
            workspace_id: rimz::ids::WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: root.clone(),
            worktree_branch: None,
            session_name: format!("rimz-{}", root.display()),
            mux_hint: None,
        }
    }

    fn run(root: &str, lane: &str, state: &str, second: i64) -> Located {
        let at = jiff::Timestamp::from_second(second).unwrap();
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version": 1, "id": WorkflowRunId::new(), "workflow": "review", "task": "sweep",
            "spec_dir": "/spec", "spec_digest": "digest",
            "scope": { "kind": "worktree", "path": format!("{root}/{lane}") },
            "project_root": root, "created_at": at, "state": state, "node": "gate",
            "entered_at": at, "terminal_at": (state != "active").then_some(at),
            "visits": {}, "visit_grant": 0, "deadline_fired": false, "next_seq": 1,
        }))
        .unwrap();
        let state =
            rimz::StatePaths::for_project_root_under(Path::new(root), Path::new("/rimz-home"))
                .unwrap();
        Located {
            paths: state.workflow(&record.id),
            state,
            record,
        }
    }

    #[test]
    fn batch_targets_select_their_own_runs() {
        let runs = vec![
            run("/repo", "one", "done", 10),
            run("/repo", "one", "done", 20),
            run("/repo", "two", "active", 5),
            run("/repo", "two", "done", 30),
            run("/other", "three", "done", 40),
        ];
        let globals = GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let targets = RunTargets {
            globals,
            runs: Some(runs),
            home: Some(workspace("/repo")),
            rooms: RefCell::new(BTreeMap::from([(
                PathBuf::from("/other"),
                Some(workspace("/other")),
            )])),
        };
        let all = targets.runs.as_deref().unwrap();
        let mut by_id = Vec::new();
        for located in all {
            let commands = targets.commands(&located.record);
            let target = commands.target();
            let root = located.record.project_root.to_str().unwrap();
            let selected = match Target::parse(target).unwrap() {
                Target::Id(id) => {
                    by_id.push(located.record.scope.path.clone());
                    id
                }
                parsed => {
                    let picked = pick(
                        &parsed,
                        target,
                        "show",
                        Some(&workspace(root)),
                        || Some("/repo".into()),
                        all,
                        None,
                        &[],
                    )
                    .unwrap();
                    let Picked::Run(index) = picked else {
                        panic!("{target} names one run");
                    };
                    all[index].record.id.clone()
                }
            };
            assert_eq!(selected, located.record.id, "{target}");
            let command = commands.command("show", None, &[], &[]);
            assert_eq!(command.contains("--root"), root == "/other", "{command}");
            assert!(
                command.ends_with(&format!("workflow show {}", shell_command([target]))),
                "{command}"
            );
        }
        assert_eq!(
            by_id,
            [PathBuf::from("/repo/one"), PathBuf::from("/repo/two")]
        );
    }
}
