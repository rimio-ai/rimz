use super::*;

#[test]
fn automation_notes_follow_workflow_cells_descendants_and_replayed_records() {
    let project = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(project.path(), project.path()).unwrap();
    let runtime = RuntimePaths::for_state_under(&state, project.path());
    let machine: crate::config::MachineConfig =
        toml::from_str("[agents]\nallow-routine-rimz=false\nruntime-env=false").unwrap();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    effective.teams.0.insert(
        "forge".into(),
        toml::from_str("leader='coder'\n[[roles]]\nrole='coder'\nprofile='claude'\nowns=['Build']")
            .unwrap(),
    );
    let id = crate::ids::WorkflowRunId::new();
    let stamp = "2026-11-07T00:23:36Z".parse().unwrap();
    let relative = crate::store::notes::allocate(
        &state,
        crate::store::notes::Kind::Workflow {
            workflow: "review".into(),
            run: id.clone(),
        },
        project.path(),
        stamp,
    )
    .unwrap();
    let kept = state
        .notes_run(crate::disk::paths::Class::AutomationNotes, &relative)
        .canonicalize()
        .unwrap();
    let team_relative = crate::store::notes::allocate(
        &state,
        crate::store::notes::Kind::Team {
            team: "forge".into(),
            worktree: None,
        },
        project.path(),
        stamp,
    )
    .unwrap();
    let team_run = state
        .notes_run(crate::disk::paths::Class::TeamNotes, &team_relative)
        .canonicalize()
        .unwrap();
    std::fs::write(team_run.join("blackboard.md"), "Stage: Build\n").unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":crate::store::workflow::VERSION,"id":id,"workflow":"review","spec_dir":project.path(),
        "spec_digest":"test","project_root":project.path(),"notes":relative,
        "scope":{"kind":"worktree","path":project.path()},"state":"active","node":"build",
        "created_at":stamp,"entered_at":stamp,"deadline_fired":false,
        "next_seq":1,"visits":{},"visit_grant":0,
    }))
    .unwrap();
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    crate::store::workflow::initialize(&state, &record, b"spec", &guard).unwrap();
    drop(guard);
    let mut parent = crate::testkit::agent_state("claude", "parent", stamp);
    parent.record = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "model":null,"effort":null,"agent":null,"notes":relative,
        }))
        .unwrap(),
    ));
    let mut actual = Vec::new();
    for source in [
        "workflow",
        "subagent",
        "peer",
        "team-peer",
        "replay",
        "foreign",
        "fork",
    ] {
        for scoped in [false, true] {
            let request = notes_request(source, &parent, &id, project.path());
            let link = project.path().join(".rimz/team");
            if source == "team-peer" {
                std::os::unix::fs::symlink(&team_run, &link).unwrap();
            }
            let plan = compile(LaunchPlanInputs {
                request: &request,
                cwd: project.path(),
                project_root: project.path(),
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                machine: &machine,
                effective: Some(&effective),
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                isolation: if scoped {
                    Isolation::Scoped
                } else {
                    Isolation::Host
                },
                bwrap: scoped.then_some(Path::new("/bin/bwrap")),
                agents: Ok(std::slice::from_ref(&parent)),
                ambient_env: &BTreeMap::new(),
                agent_shell: None,
                hibernate: false,
                recorded_system_prompt_digest: None,
            })
            .unwrap();
            let argv = &plan.process().provider_argv;
            let dirs = argv
                .windows(2)
                .find(|args| args[0] == "--settings")
                .map(|args| {
                    serde_json::from_str::<serde_json::Value>(&args[1]).unwrap()["permissions"]
                        ["additionalDirectories"]
                        .clone()
                })
                .unwrap_or_default();
            let recorded =
                serde_json::to_value(&plan.request.identity.params.record).unwrap()["notes"]
                    .clone();
            let reachable = plan.sandbox.as_ref().is_none_or(|sandbox| {
                sandbox.plan.mounts.iter().any(|mount| {
                    matches!(mount, crate::sandbox::Mount::Bind { source, target }
                    if kept.starts_with(target) && source == target)
                })
            });
            actual.push((source, scoped, dirs, recorded, reachable));
            if source == "team-peer" {
                assert_eq!(
                    super::super::super::scratch::run_dir(&state, project.path()),
                    Some(team_run.clone())
                );
                let reminder = &plan.process().reminder;
                assert!(
                    reminder.contains("blackboard.md") && reminder.contains("[Build]"),
                    "{reminder}"
                );
                std::fs::remove_file(link).unwrap();
            }
        }
    }
    let expected: Vec<_> = [
        "workflow",
        "subagent",
        "peer",
        "team-peer",
        "replay",
        "foreign",
        "fork",
    ]
    .into_iter()
    .flat_map(|source| {
        [false, true].map(|scoped| {
            let allowed = !matches!(source, "foreign" | "fork");
            let recorded = match source {
                "foreign" => serde_json::json!(project.path()),
                "fork" => serde_json::Value::Null,
                _ => serde_json::json!(relative),
            };
            (
                source,
                scoped,
                if source == "team-peer" {
                    serde_json::json!([kept, team_run])
                } else if allowed {
                    serde_json::json!([kept])
                } else {
                    serde_json::Value::Null
                },
                recorded,
                true,
            )
        })
    })
    .collect();
    assert_eq!(actual, expected);
}

fn notes_request(
    source: &str,
    parent: &crate::agents::AgentState,
    run: &crate::ids::WorkflowRunId,
    project: &Path,
) -> ExecRequest {
    let mut request = request("claude", action_with_args("launch", vec![]));
    match source {
        "workflow" => {
            request.identity.params.workflow = Some(crate::agents::WorkflowAttemptRef {
                run: run.clone(),
                seq: 1,
                role: crate::agents::WorkflowAttemptRole::Step,
            });
        }
        "subagent" => {
            request.subagent = true;
            request.identity.params.parent_agent_id = Some(parent.agent_id.clone());
            request.identity.params.parent_agent_kind = Some(parent.kind.clone());
        }
        "peer" | "team-peer" => {
            request.identity.params.launched_by = Some(Box::new(crate::agents::LaunchedBy {
                kind: parent.kind.clone(),
                agent_id: parent.agent_id.clone(),
            }));
            if source == "team-peer" {
                request.identity.params.team = Some("forge".into());
                request.identity.params.role = Some("coder".into());
            }
        }
        "foreign" => {
            request.identity.params.record = Some(Box::new(
                serde_json::from_value(serde_json::json!({
                    "model": null, "effort": null, "agent": null, "notes": project,
                }))
                .unwrap(),
            ));
        }
        _ => request.identity.params.record = parent.record.clone(),
    }
    if source == "fork" {
        request.action = ExecAction::Fork {
            session_id: "parent".into(),
            extra_args: vec![],
        };
    }
    request
}
