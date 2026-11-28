use super::*;

#[test]
fn project_scope_is_unique_across_checkouts_and_refuses_a_named_worktree() {
    let (env, _) = fixture(
        &format!("[workflow]\nscope='project'\nstart='pr'\n{WAIT}"),
        PENDING,
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    success(command(&env, &["worktree", "new", "one"]));
    let checkout = rimz::worktree::discover_owned(&env.project_root)
        .unwrap()
        .remove(0)
        .marker
        .worktree_path;
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    assert_eq!(
        value["record"]["scope"],
        json!({"kind":"project", "path":env.project_root})
    );
    let duplicate = command(&env, &["workflow", "run", "review"]);
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&duplicate.stderr).contains("already has run review#main for"),
        "{duplicate:?}"
    );
    success(command(&env, &["workflow", "cancel", &id]));
    let output = command(&env, &["workflow", "run", "review", "-w", "one"]);
    assert_eq!(output.status.code(), Some(1));
    let refusal = format!(
        "review has scope = project, so it runs at {}; start it without -w",
        env.project_root.display()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&refusal),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = env.state_path_for(&env.project_root);
    assert!(rimz::store::workflow::live_runs(&state).unwrap().is_empty());
    assert!(
        rimz::store::workflow::find(&state, &id.parse().unwrap())
            .unwrap()
            .unwrap()
            .finished
    );
    let receipt = success(
        env.rimz()
            .current_dir(checkout)
            .args(["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    assert_eq!(
        show(&env, id)["record"]["scope"]["path"],
        json!(env.project_root)
    );
    assert!(success(command(&env, &["workflow", "show", id])).starts_with("review#main"));
    success(command(&env, &["workflow", "cancel", id]));
}

#[test]
fn a_script_canceled_outcome_is_a_route_not_an_engine_diagnostic() {
    let (env, _) = fixture(
        &format!(
            "{HEADER}[script.pr]\ntimeout='365d'\nrun='sh checks/pr.sh'\non.canceled='next'\n[script.next]\ntimeout='365d'\nrun='sh checks/pr.sh'\non.ready='@done'\n"
        ),
        "if test -f \"$RIMZ_WORKFLOW_DIR/called\"; then printf '{\"outcome\":\"ready\"}'; else touch \"$RIMZ_WORKFLOW_DIR/called\"; printf '{\"outcome\":\"canceled\"}'; fi",
    );
    let id = start(&env);
    let value = settled(&env, &id, "done");
    let stats = assists_once(&env, "workflow_ends", 1);
    assert_eq!(stats["rollup"]["workflow_routes"], 2, "{stats}");
    assert!(judges::workflow_diagnostics(&env, "workflow_rerun").is_empty());
    assert!(
        events(&value, "evaluation")
            .iter()
            .all(|row| row["origin"] == "check")
    );
}

/// `workflow run -w` creates a missing RimZ-owned worktree the way an agent launch's `-w` does,
/// runs there, and reuses the tree on the next run; a directory of that name RimZ does not own
/// refuses without a terminal to confirm entering it.
#[test]
fn a_named_worktree_is_created_when_missing_and_reused_after() {
    let (env, _) = fixture(
        &format!("[workflow]\nscope='worktree'\nstart='pr'\n{WAIT}"),
        PENDING,
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    // A run under a loop fire stamps the tree it creates with that fire, as a launch does.
    let fire = rimz::ids::FireId::new(&rimz::ids::TaskId::new());
    let receipt = success(
        env.rimz()
            .env("RIMZ_LOOP_FIRE", fire.to_string())
            .args(["workflow", "run", "review", "-w", "fresh"])
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    assert_eq!(owned.len(), 1, "{owned:?}");
    let tree = &owned[0].marker;
    assert_eq!(tree.name, "fresh");
    assert_eq!(tree.loop_fire.as_ref(), Some(&fire), "{tree:?}");
    let tree_path = tree.worktree_path.canonicalize().unwrap();
    assert_eq!(show(&env, id)["record"]["scope"]["path"], json!(tree_path));
    success(command(&env, &["workflow", "cancel", id]));
    let again = success(command(&env, &["workflow", "run", "review", "-w", "fresh"]));
    let again = again.lines().next().unwrap();
    assert_eq!(
        show(&env, again)["record"]["scope"]["path"],
        json!(tree_path)
    );
    assert_eq!(
        rimz::worktree::discover_owned(&env.project_root)
            .unwrap()
            .len(),
        1
    );
    success(command(&env, &["workflow", "cancel", again]));

    let unowned = tree.worktree_path.with_file_name("plain");
    std::fs::create_dir_all(&unowned).unwrap();
    let output = command(&env, &["workflow", "run", "review", "-w", "plain"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "{} is not a linked worktree of",
            unowned.display()
        )),
        "{stderr}"
    );

    // From a checkout of another repository, `-w` asks before creating there, and without a
    // terminal it refuses with nothing made.
    let other = env.home_root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&other)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let room = env.project_root.canonicalize().unwrap();
    let output = env
        .rimz()
        .current_dir(&other)
        .env(
            rimz::workspace::ENV_WORKSPACE_ID,
            rimz::WorkspaceId::from_project_root(&room).as_str(),
        )
        .env(rimz::workspace::ENV_PROJECT_ROOT, &room)
        .stdin(std::process::Stdio::null())
        .args(["workflow", "run", "review", "-w", "cross"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("current git root:"), "{stderr}");
    assert!(
        rimz::worktree::discover_owned(&other).unwrap().is_empty(),
        "{stderr}"
    );
    let state = env.state_path_for(&env.project_root);
    assert!(
        rimz::store::workflow::live_runs(&state)
            .unwrap()
            .iter()
            .all(|run| !run.scope.path.ends_with("cross")),
        "{stderr}"
    );
}
