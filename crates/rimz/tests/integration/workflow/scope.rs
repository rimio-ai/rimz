use super::*;

#[test]
fn project_scope_is_unique_across_checkouts_and_reports_the_override() {
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
    for args in [
        vec!["workflow", "run", "review"],
        vec!["workflow", "run", "review", "-w", "one"],
    ] {
        let duplicate = command(&env, &args);
        assert_eq!(duplicate.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&duplicate.stderr).contains(&id));
    }
    success(command(&env, &["workflow", "cancel", &id]));
    let output = command(&env, &["workflow", "run", "review", "-w", "one"]);
    let notice = format!(
        "note: review has scope = project; the run is at {}, not {}",
        env.project_root.display(),
        checkout.display()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&notice),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = success(output);
    let id = receipt.lines().next().unwrap();
    let value = until(&env, id, |v| v["record"]["wait"].is_object());
    assert_eq!(value["record"]["scope"]["path"], json!(env.project_root));
    assert!(success(command(&env, &["workflow", "show", id])).contains("scope project"));
    success(command(&env, &["workflow", "cancel", id]));
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
    success(command(&env, &["workflow", "cancel", id]));
}

#[test]
fn script_canceled_and_limited_are_routes_not_engine_diagnostics() {
    let (env, _) = fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.canceled='next'\n[checks.next]\nrun='sh checks/pr.sh'\non.limited='@done'\n"
        ),
        "if test -f \"$RIMZ_WORKFLOW_DIR/called\"; then printf '{\"outcome\":\"limited\"}'; else touch \"$RIMZ_WORKFLOW_DIR/called\"; printf '{\"outcome\":\"canceled\"}'; fi",
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
