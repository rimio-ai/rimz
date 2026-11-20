//! Shared folder- and hook-trust launch verdicts.

use crate::agents::FolderTrust;
use anyhow::Result;

#[derive(Debug, PartialEq, Eq)]
pub struct TrustNotice(String);

impl std::fmt::Display for TrustNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const AGENT_RELAUNCH: &str = "launch from a directory the account already trusts: on `rimz agents`, `--cwd <DIR>` chooses one and dropping `--cwd` uses your own checkout";

#[derive(Clone, Copy)]
pub enum Unattended {
    Team,
    Workflow,
    Subagent,
    Agent,
}

impl std::fmt::Display for Unattended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Team => "team",
            Self::Workflow => "workflow",
            Self::Subagent => "subagent resume",
            Self::Agent => "agent-started",
        })
    }
}

pub fn preflight_hooks(
    health: Result<(), crate::agents::BirthLoginErr>,
    unattended: impl FnOnce() -> Option<Unattended>,
    fix_command: impl FnOnce() -> String,
    noticed: &mut std::collections::HashSet<crate::ids::LoginKey>,
) -> Result<Option<TrustNotice>> {
    let Err(error) = health else {
        return Ok(None);
    };
    let crate::agents::BirthLoginErr::HooksUntrusted {
        kind,
        name,
        choice: Some(choice),
        ..
    } = error
    else {
        return Err(error.into());
    };
    if let Some(context) = unattended() {
        let fix_command = fix_command();
        anyhow::bail!(
            "{context} launch refused: {kind} account `{name}` has untrusted RimZ hooks, and only the user can trust them.\nIf this blocks your task, ask the user to run `{fix_command}` once and {choice}."
        );
    }
    if noticed.insert(crate::ids::LoginKey {
        kind: kind.clone(),
        name: name.clone(),
    }) {
        let provider = crate::agents::find_definition(kind.as_str())
            .map_or(kind.as_str(), |adapter| adapter.spec().display_name);
        return Ok(Some(TrustNotice(format!(
            "rimz: {kind} account `{name}` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent."
        ))));
    }
    Ok(None)
}

pub fn preflight(
    kind: &str,
    cwd: &std::path::Path,
    trust: Option<FolderTrust>,
    unattended: Option<Unattended>,
) -> Result<Option<TrustNotice>> {
    let message = match trust {
        Some(FolderTrust::Restricted { key }) => {
            if matches!(unattended, Some(Unattended::Agent)) {
                anyhow::bail!(
                    "agent-started launch refused: {kind} folder `{}` is saved untrusted; ask the user to trust the folder in Codex; {AGENT_RELAUNCH}; RimZ leaves the saved decision alone",
                    key.display()
                );
            }
            let message = format!(
                "{kind} folder `{}` is saved untrusted; it will stop at its Folder access prompt on every launch; {}",
                key.display(),
                FolderTrust::RESTRICTED_FIX
            );
            if let Some(context) = unattended {
                anyhow::bail!(
                    "{context} launch refused: {message}; RimZ leaves the saved decision alone"
                );
            }
            message
        }
        Some(FolderTrust::Undecided(gap)) => {
            let fix =
                if crate::disk::paths::is_under_root(cwd, &crate::disk::paths::empty_dir_root()) {
                    gap.empty_dir_fix(kind)
                } else {
                    gap.fix(kind, "rimz trust grant")
                };
            if matches!(unattended, Some(Unattended::Agent)) {
                anyhow::bail!(
                    "agent-started launch refused: {kind} has no trust decision for `{}`; the pane would stop at its folder-trust prompt with nobody watching; ask the user to {}; {AGENT_RELAUNCH}",
                    gap.key.display(),
                    fix
                );
            }
            let message = format!(
                "{kind} has no trust decision for `{}`; it will stop at its folder-trust prompt in the pane; {}",
                gap.key.display(),
                fix
            );
            if matches!(unattended, Some(Unattended::Subagent)) {
                anyhow::bail!("subagent resume refused: {message}");
            }
            message
        }
        Some(FolderTrust::Decided) | None => return Ok(None),
    };
    Ok(Some(TrustNotice(format!("rimz: {message}"))))
}

/// One problem a workflow launch's trust read found, with its fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrustProblem {
    text: String,
    /// Whether the user clears it in place (hooks, folder trust), so a run waits on it; the
    /// rest (the home directory as checkout, an unsupported lifecycle, a missing account home)
    /// end the attempt.
    clearable: bool,
}

/// A workflow launch refused on trust, with every problem the read found.
#[derive(Debug)]
pub struct TrustRefusal(pub(super) Vec<TrustProblem>);

impl TrustRefusal {
    /// The problems with their fixes, as a trust wait's detail.
    pub fn detail(&self) -> String {
        let texts: Vec<_> = self.0.iter().map(|problem| problem.text.as_str()).collect();
        texts.join("; ")
    }

    pub fn clearable(&self) -> bool {
        self.0.iter().all(|problem| problem.clearable)
    }
}

impl std::fmt::Display for TrustRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workflow launch refused: {}", self.detail())
    }
}

impl std::error::Error for TrustRefusal {}

/// The launch recipe that judges a workflow cell: the supervised leader, or the layout recipe
/// (a companion, or a leader that is a loop task's or a team's session).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrustRecipe {
    Supervised,
    Layout,
}

/// The trust read of one workflow cell under `recipe`, from the typed sources its launch
/// reads. `folder` is the directory the cell starts in with the recipe's project hint, or
/// `None` when that directory does not exist yet. An undecided folder refuses the supervised
/// leader only: the layout recipe launches past it with a notice.
pub(super) fn workflow_problems(
    recipe: TrustRecipe,
    adapter: &crate::agents::AgentDefinition,
    login: &crate::agents::ProviderLogin,
    folder: Option<(&std::path::Path, Option<&std::path::Path>)>,
    hook_fix: impl FnOnce() -> String,
) -> Vec<TrustProblem> {
    use crate::agents::{HookPreflightErr, LaunchDirRefusal};
    let kind = adapter.spec().kind;
    let env = login.env(&crate::agents::ambient_env());
    let mut problems = Vec::new();
    match recipe {
        TrustRecipe::Supervised => {
            // A named account's hooks live in its own home, which only its own repair fixes.
            let account = if login.is_default() {
                None
            } else {
                layout_hook_problem(login.health(&crate::agents::ambient_env()), hook_fix)
            };
            let account_failed = account.is_some();
            problems.extend(account);
            let mut push = |clearable, text| problems.push(TrustProblem { text, clearable });
            match crate::agents::preflight_hooks(
                adapter,
                &env,
                crate::agents::TurnLifecycleNeed::Wired,
            ) {
                Ok(()) => {}
                Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => push(
                    false,
                    format!(
                        "{kind} cannot be supervised: a verified executable turn-lifecycle \
                         signal is required; {reason}"
                    ),
                ),
                Err(_) if account_failed => {}
                Err(HookPreflightErr::HooksMissing) => push(
                    true,
                    format!(
                        "{kind} hooks are missing, so the supervised turn cannot report \
                         completion; run `rimz hooks install {kind}`"
                    ),
                ),
                Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => push(
                    true,
                    format!("{kind} hooks are installed but not trusted ({hooks}); {fix}"),
                ),
                Err(HookPreflightErr::HooksDisabled { reason, fix }) => {
                    push(
                        true,
                        format!("{kind} cannot be supervised: {reason}; {fix}"),
                    );
                }
            }
            if let Some((cwd, repo_root)) = folder {
                match crate::agents::preflight_launch_dir(
                    adapter,
                    cwd,
                    repo_root,
                    &env,
                    crate::agents::LaunchDirMode::Supervised,
                ) {
                    Ok(()) => {}
                    Err(error @ LaunchDirRefusal::Home { .. }) => {
                        push(false, format!("{error}; {WORKFLOW_HOME_FIX}"));
                    }
                    // `rimz trust grant` writes only the room's accounts, so a named account's
                    // decision is saved by answering the prompt on that account.
                    Err(LaunchDirRefusal::Undecided { kind, dir, key, .. })
                        if !login.is_default() =>
                    {
                        let name = login.name();
                        let error = LaunchDirRefusal::Undecided {
                            kind,
                            account: Some(name.clone()),
                            dir,
                            key,
                            fix: format!(
                                "answer {kind}'s folder-trust prompt there once on account \
                                 `{name}`: run `rimz agents {kind} --account {name}` there"
                            ),
                        };
                        push(true, error.to_string());
                    }
                    Err(error) => push(true, error.to_string()),
                }
            }
        }
        TrustRecipe::Layout => {
            problems.extend(layout_hook_problem(
                login.health(&crate::agents::ambient_env()),
                hook_fix,
            ));
            let mut push = |clearable, text| problems.push(TrustProblem { text, clearable });
            if let Some((cwd, hint)) = folder
                && let Some(FolderTrust::Restricted { key }) = adapter.folder_trust(cwd, hint, &env)
            {
                push(
                    true,
                    format!(
                        "{kind} folder `{}` is saved untrusted; it will stop at its Folder \
                         access prompt on every launch; {}; RimZ leaves the saved decision alone",
                        key.display(),
                        FolderTrust::RESTRICTED_FIX
                    ),
                );
            }
        }
    }
    problems
}

/// The layout recipe's hook verdict for a workflow cell, from its account's health: a hook
/// problem keeps its own fix and the user clears it; any other account problem ends the attempt.
fn layout_hook_problem(
    health: Result<(), crate::agents::BirthLoginErr>,
    hook_fix: impl FnOnce() -> String,
) -> Option<TrustProblem> {
    use crate::agents::BirthLoginErr;
    let (clearable, text) = match health.err()? {
        BirthLoginErr::HooksUntrusted {
            kind,
            name,
            choice: Some(choice),
            ..
        } => (
            true,
            format!(
                "{kind} account `{name}` has untrusted RimZ hooks, and only the user can trust \
                 them: run `{}` once and {choice}",
                hook_fix()
            ),
        ),
        error @ (BirthLoginErr::HooksMissing { .. }
        | BirthLoginErr::HooksDisabled { .. }
        | BirthLoginErr::HooksUntrusted { .. }) => (true, error.to_string()),
        error => (false, error.to_string()),
    };
    Some(TrustProblem { text, clearable })
}

/// What refuses a workflow start: each distinct problem the trust read finds for an agent step
/// or judge's launch, in the order found, with the nodes it blocks. A node whose launch cannot
/// be resolved lists that error. `scope` is the run's checkout, which may not exist yet; with
/// `tried` (a try's node), only that node and the judges of its rest check are read.
pub fn workflow_start_problems(
    workspace: &crate::ResolvedWorkspace,
    store: &crate::store::Store,
    machine: &crate::config::MachineConfig,
    definition: &crate::harness::workflow::Definition,
    scope: &std::path::Path,
    tried: Option<&str>,
    task_run: bool,
) -> Result<Vec<(String, Vec<String>)>> {
    use crate::harness::workflow::{Kind, Target};
    let tried = tried.map(|at| {
        let rest = definition.step(at).or_else(|| definition.check(at));
        std::iter::once(at)
            .chain(
                rest.into_iter()
                    .flat_map(|node| node.rest_check().iter().map(Target::as_str)),
            )
            .collect::<Vec<_>>()
    });
    let steps = definition
        .steps()
        .filter_map(|(name, node)| match node.kind() {
            Some(Kind::Agent(profile)) => Some((name, profile, Some(node))),
            _ => None,
        });
    let judges = definition
        .checks()
        .filter_map(|(name, node)| match node.kind() {
            Some(Kind::Judge(profile)) => Some((name, profile, None)),
            _ => None,
        });
    let nodes: Vec<_> = steps
        .chain(judges)
        .filter(|(name, ..)| {
            tried
                .as_ref()
                .is_none_or(|tried| tried.contains(&name.as_str()))
        })
        .collect();
    // A start that launches no agent reads no account or agent config.
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let logins = crate::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        machine,
    )?;
    let effective = crate::config::effective::load(machine, &workspace.project_root)?;
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for (name, profile, step) in nodes {
        let launch = NodeLaunch {
            profile,
            account: step.and_then(|step| step.account()),
            worktree: step
                .and_then(|step| step.worktree())
                .map(|name| {
                    (!name.is_empty())
                        .then(|| {
                            crate::worktree::worktree_path(
                                &workspace.project_root,
                                &machine.agents.worktree,
                                name,
                            )
                        })
                        .transpose()
                })
                .transpose()?,
            // A judge lives until it answers, as a session does.
            session: step.is_none_or(|step| !step.lifecycle().is_turn()),
            task_run,
        };
        let problems = node_problems(
            workspace, store, machine, &effective, &logins, scope, &launch,
        )
        .unwrap_or_else(|error| {
            vec![TrustProblem {
                text: format!("{error:#}"),
                clearable: false,
            }]
        });
        for problem in problems {
            match found.iter_mut().find(|(text, _)| *text == problem.text) {
                Some((_, nodes)) => nodes.push(name.clone()),
                None => found.push((problem.text, vec![name.clone()])),
            }
        }
    }
    Ok(found)
}

/// One node's launch as the start reads it.
struct NodeLaunch<'a> {
    profile: &'a str,
    account: Option<&'a crate::ids::LoginName>,
    /// The step's own worktree: its path, or `None` for a generated name, which has no
    /// checkout until the launch makes one.
    worktree: Option<Option<std::path::PathBuf>>,
    session: bool,
    task_run: bool,
}

/// The trust read of every cell `launch` would open, by the recipe the workflow host's launch
/// takes for it (`launcher::resident_leader`), with the folder each recipe judges; a folder
/// that does not exist yet has no verdict.
fn node_problems(
    workspace: &crate::ResolvedWorkspace,
    store: &crate::store::Store,
    machine: &crate::config::MachineConfig,
    effective: &crate::config::effective::LaunchAgents,
    logins: &crate::agents::RoomAccounts,
    scope: &std::path::Path,
    launch: &NodeLaunch<'_>,
) -> Result<Vec<TrustProblem>> {
    let overrides = super::LaunchOverrides {
        login: launch.account.cloned(),
        ..Default::default()
    };
    let finalized = super::resolve_finalized_layout(
        None,
        machine,
        effective,
        crate::config::effective::ProfileScope::Agents,
        Some(launch.profile),
        None,
        &overrides,
        None,
        None,
        None,
        false,
        Some((store.runtime_paths(), store.paths())),
    )?;
    let team = finalized.resolved.team_name.is_some();
    let layout_recipe = launch.session && (launch.task_run || team);
    if team && !layout_recipe {
        anyhow::bail!("a run-bearing layout cannot launch a team; use an agent layout instead");
    }
    let layout = finalized.resolved.layout;
    let leader = if layout_recipe {
        None
    } else {
        Some(crate::harness::spec::prompt_leader(&layout, None)?)
    };
    let leader_cwd = match &launch.worktree {
        Some(own) => own.as_deref(),
        None => Some(scope),
    };
    let leader_folder = match leader_cwd {
        Some(cwd) if cwd.is_dir() => {
            let repo_root = workspace.at_launch_dir(cwd)?.cwd_project_root;
            Some((cwd.to_path_buf(), repo_root))
        }
        _ => None,
    };
    // The layout recipe judges the room's root for a step with its own worktree, where the
    // detached host reads it.
    let cell_root = if launch.worktree.is_some() {
        &workspace.project_root
    } else {
        scope
    };
    let cell_folder = if cell_root.is_dir() {
        let at = workspace.at_launch_dir(cell_root)?;
        Some((at.worktree_root, at.cwd_project_root))
    } else {
        None
    };
    let mut problems = Vec::new();
    for (index, cell) in layout.agent_cells().enumerate() {
        let adapter = crate::agents::find_definition(cell.kind.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", cell.kind))?;
        let login = match cell
            .launch
            .login
            .clone()
            .map_or(
                crate::store::writer::LaunchLogin::RoomDefault,
                crate::store::writer::LaunchLogin::Pinned,
            )
            .resolve(&cell.kind, logins, &machine.accounts)
        {
            Ok(login) => login,
            Err(error) => {
                problems.push(TrustProblem {
                    text: error.to_string(),
                    clearable: false,
                });
                continue;
            }
        };
        let (recipe, folder) = if leader == Some(index) {
            (TrustRecipe::Supervised, &leader_folder)
        } else {
            (TrustRecipe::Layout, &cell_folder)
        };
        problems.extend(workflow_problems(
            recipe,
            adapter,
            &login,
            folder
                .as_ref()
                .map(|(cwd, hint)| (cwd.as_path(), hint.as_deref())),
            || {
                crate::config::definitions::hook_trust_fix(
                    &cell.kind,
                    cell.launch.profile.as_deref(),
                    None,
                )
            },
        ));
    }
    Ok(problems)
}

/// How a workflow moves its agents off the home directory.
pub(super) const WORKFLOW_HOME_FIX: &str = "start the workflow from a project directory";

#[cfg(test)]
mod tests {

    fn untrusted_hooks(choice: Option<&'static str>) -> crate::agents::BirthLoginErr {
        crate::agents::BirthLoginErr::HooksUntrusted {
            kind: crate::ids::AgentKind::new_unchecked("codex"),
            name: "team-1".parse().unwrap(),
            home: Box::new("/private/codex-home".into()),
            env_key: Some("CODEX_HOME"),
            hooks: "SessionStart, UserPromptSubmit".into(),
            fix: "run /hooks inside codex and trust the RimZ hooks".into(),
            choice,
        }
    }

    #[test]
    fn hook_trust_verdict_depends_on_the_command() {
        for (kind, provider, choice) in [
            ("hermes", "Hermes", "answer y to each RimZ hook prompt"),
            ("codex", "Codex", "choose \"Trust all and continue\""),
        ] {
            for unattended in [
                None,
                Some(super::Unattended::Team),
                Some(super::Unattended::Subagent),
                Some(super::Unattended::Agent),
            ] {
                let mut error = untrusted_hooks(Some(choice));
                if let crate::agents::BirthLoginErr::HooksUntrusted {
                    kind: agent_kind, ..
                } = &mut error
                {
                    *agent_kind = crate::ids::AgentKind::new_unchecked(kind);
                }
                let result = super::preflight_hooks(
                    Err(error),
                    || unattended,
                    || {
                        assert!(unattended.is_some(), "watched launches need no fix lookup");
                        "rimz agents sol".into()
                    },
                    &mut Default::default(),
                );
                if let Some(context) = unattended {
                    let text = result
                        .expect_err("an unwatched trust prompt must refuse")
                        .to_string();
                    for expected in [
                        context.to_string().as_str(),
                        "only the user can trust them",
                        "ask the user to run `rimz agents sol` once",
                        choice,
                    ] {
                        assert!(text.contains(expected), "{text}");
                    }
                    for excluded in [
                        "/private/codex-home",
                        "SessionStart",
                        "UserPromptSubmit",
                        "CODEX_HOME",
                    ] {
                        assert!(!text.contains(excluded), "{text}");
                    }
                } else {
                    assert!(result.is_ok(), "{result:?}");
                    assert_eq!(
                        result.unwrap().unwrap().to_string(),
                        format!(
                            "rimz: {kind} account `team-1` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent."
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn hook_notice_is_once_per_account_per_command() {
        let mut notices = Vec::new();
        let mut noticed = Default::default();
        for name in ["team-1", "team-1", "team-2"] {
            let mut error = untrusted_hooks(Some("choose \"Trust all and continue\""));
            if let crate::agents::BirthLoginErr::HooksUntrusted { name: account, .. } = &mut error {
                *account = name.parse().unwrap();
            }
            let notice = super::preflight_hooks(
                Err(error),
                || None,
                || panic!("watched launches need no fix lookup"),
                &mut noticed,
            )
            .unwrap();
            notices.extend(notice);
        }
        let text = notices
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("`team-1`"));
        assert!(text.contains("`team-2`"));
    }

    #[test]
    fn hook_trust_without_a_pane_choice_and_other_errors_pass_through() {
        super::preflight_hooks(
            Ok(()),
            || panic!("healthy launches need no caller lookup"),
            || panic!("healthy launches need no fix lookup"),
            &mut Default::default(),
        )
        .unwrap();
        for unattended in [None, Some(super::Unattended::Agent)] {
            let error = untrusted_hooks(None);
            let expected = error.to_string();
            let result = super::preflight_hooks(
                Err(error),
                || unattended,
                || panic!("non-opted-in providers keep their original error"),
                &mut Default::default(),
            );
            assert_eq!(result.unwrap_err().to_string(), expected);
        }
        let error = crate::agents::BirthLoginErr::MissingHome {
            kind: crate::ids::AgentKind::new_unchecked("codex"),
            name: crate::ids::LoginName::default_login(),
            home: "/missing".into(),
        };
        let expected = error.to_string();
        assert_eq!(
            super::preflight_hooks(
                Err(error),
                || panic!("other health errors need no caller lookup"),
                || panic!("other health errors need no fix lookup"),
                &mut Default::default(),
            )
            .unwrap_err()
            .to_string(),
            expected
        );
    }

    #[test]
    fn every_workflow_hook_refusal_keeps_its_fix_and_is_cleared_in_place() {
        use crate::agents::BirthLoginErr;
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let name = crate::ids::LoginName::default_login();
        for error in [
            BirthLoginErr::HooksMissing {
                kind: kind.clone(),
                name: name.clone(),
                home: "/codex".into(),
            },
            BirthLoginErr::HooksDisabled {
                kind,
                name,
                reason: "hooks disabled".into(),
                fix: "enable hooks".into(),
            },
            untrusted_hooks(None),
        ] {
            let expected = super::TrustProblem {
                text: error.to_string(),
                clearable: true,
            };
            let problem = super::layout_hook_problem(Err(error), || {
                panic!("the original error carries its fix")
            });
            assert_eq!(problem, Some(expected));
        }
        let problem = super::layout_hook_problem(
            Err(untrusted_hooks(Some("choose \"Trust all and continue\""))),
            || "rimz hooks trust codex".into(),
        )
        .unwrap();
        assert!(problem.clearable);
        assert_eq!(
            problem.text,
            "codex account `team-1` has untrusted RimZ hooks, and only the user can trust them: \
             run `rimz hooks trust codex` once and choose \"Trust all and continue\""
        );
        assert_eq!(super::layout_hook_problem(Ok(()), String::new), None);
    }

    #[test]
    fn restricted_launch_verdict_depends_on_the_command() {
        for unattended in [
            None,
            Some(super::Unattended::Team),
            Some(super::Unattended::Subagent),
            Some(super::Unattended::Agent),
        ] {
            let result = super::preflight(
                "codex",
                std::path::Path::new("/repo"),
                Some(crate::agents::FolderTrust::Restricted {
                    key: "/repo".into(),
                }),
                unattended,
            );
            assert_eq!(result.is_err(), unattended.is_some());
            if let Err(error) = result {
                let text = error.to_string();
                assert!(text.contains(&unattended.unwrap().to_string()));
                assert!(text.contains("saved untrusted"));
                if matches!(unattended, Some(super::Unattended::Agent)) {
                    assert!(text.contains("ask the user to trust the folder in Codex"));
                    assert!(!text.contains("in the pane"));
                } else {
                    assert!(text.contains("Open restricted"));
                }
                assert!(!text.contains("rimz trust grant"));
            }
        }
    }

    #[test]
    fn empty_dir_undecided_fix_is_machine_wide() {
        if std::env::var_os("RIMZ_TEST_TRUST_LINKED_HOME").is_none() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real");
            let link = dir.path().join("link");
            std::fs::create_dir_all(real.join("empty/child")).unwrap();
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launcher::trust::tests::empty_dir_undecided_fix_is_machine_wide",
                    "--nocapture",
                ])
                .env("RIMZ_UNIT_TEST_RIMZ_HOME", &link)
                .env("RIMZ_TEST_TRUST_LINKED_HOME", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let cwd = crate::disk::paths::empty_dir_root().join("child");
        let resolved = cwd.canonicalize().unwrap();
        for cwd in [&cwd, &resolved] {
            for unattended in [
                None,
                Some(super::Unattended::Subagent),
                Some(super::Unattended::Agent),
            ] {
                let gap = crate::agents::FolderTrustGap {
                    key: cwd.to_path_buf(),
                    path: "/config".into(),
                    grant: Ok(crate::agents::FolderTrustPreview {
                        original: None,
                        candidate: String::new(),
                    }),
                };
                let result = super::preflight(
                    "claude",
                    cwd,
                    Some(crate::agents::FolderTrust::Undecided(gap)),
                    unattended,
                );
                assert_eq!(result.is_err(), unattended.is_some());
                let text = match result {
                    Ok(notice) => notice.unwrap().to_string(),
                    Err(error) => error.to_string(),
                };
                assert!(
                    text.contains("run `rimz trust grant --empty-dir`"),
                    "{text}"
                );
                assert!(!text.contains("--agents"), "{text}");
                assert!(!text.contains(" in `"), "{text}");
            }
        }
    }

    #[test]
    fn agent_undecided_refusal_names_the_key_and_both_relaunch_exits() {
        let gap = crate::agents::FolderTrustGap {
            key: "/target".into(),
            path: "/config".into(),
            grant: Ok(crate::agents::FolderTrustPreview {
                original: None,
                candidate: String::new(),
            }),
        };
        let error = super::preflight(
            "claude",
            std::path::Path::new("/target"),
            Some(crate::agents::FolderTrust::Undecided(gap)),
            Some(super::Unattended::Agent),
        )
        .expect_err("agent caller must not leave an unwatched prompt");
        let text = error.to_string();
        for expected in [
            "claude",
            "/target",
            "nobody watching",
            "ask the user to run `rimz trust grant --agents claude`",
            "--cwd <DIR>",
            "dropping `--cwd` uses your own checkout",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn agent_restricted_refusal_leaves_the_saved_decision_alone() {
        let error = super::preflight(
            "codex",
            std::path::Path::new("/target"),
            Some(crate::agents::FolderTrust::Restricted {
                key: "/target".into(),
            }),
            Some(super::Unattended::Agent),
        )
        .unwrap_err();
        let text = error.to_string();
        for expected in [
            "/target",
            "ask the user to trust the folder in Codex",
            "--cwd <DIR>",
            "RimZ leaves the saved decision alone",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("rimz trust grant"));
        assert!(!text.contains("in the pane"));
    }
}
