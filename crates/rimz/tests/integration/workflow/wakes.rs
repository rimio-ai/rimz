use super::*;

pub(super) fn wake(env: &Env, name: &str, payload: Value) {
    let signal = json!({"name":name, "payload":payload, "source":"forge"});
    success(command(
        env,
        &["workflow", "advance", "--wake-signal", &signal.to_string()],
    ));
}

fn assert_untouched(env: &Env, id: &str, name: &str, payload: Value) {
    let before = show(env, id);
    wake(env, name, payload);
    let after = show(env, id);
    assert_eq!(
        after["journal"], before["journal"],
        "unsubscribed wake: {name}"
    );
    assert_eq!(
        after["record"], before["record"],
        "unsubscribed wake: {name}"
    );
}

fn assert_woken(env: &Env, id: &str, name: &str, payload: Value) {
    let before = show(env, id);
    wake(env, name, payload);
    let after = until(env, id, |value| {
        events(value, "evaluation").len() > events(&before, "evaluation").len()
            && value["record"]["attempt"].is_null()
    });
    assert_eq!(
        events(&after, "wake").len(),
        events(&before, "wake").len() + 1,
        "{after}"
    );
    assert_eq!(events(&after, "wake").last().unwrap()["signal"], name);
}

#[test]
fn checks_without_subscriptions_keep_their_backoff_and_wait_count() {
    for check in [
        WAIT.to_owned(),
        "[checks.pr]\nprovider='claude'\nelse='pending'\ntimeout='1h'\non={ready='@done',pending='@wait'}\n[[checks.pr.cases]]\nwhen=['window.5h.left>=40']\noutcome='ready'\n".into(),
        "[checks.pr]\nelse='pending'\ntimeout='1h'\non={ready='@done',pending='@wait'}\n[[checks.pr.cases]]\nwhen=['pr[main]=merged || ci[main]=passed || pr.queue[main]=queued']\noutcome='ready'\n".into(),
    ] {
        let (env, _) = fixture(&format!("{HEADER}{check}"), PENDING);
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        assert_untouched(&env, &id, "review.ready", json!({}));
        assert_untouched(&env, &id, "pr.opened", json!({"path":env.project_root}));
    }
}

#[test]
fn invalid_wakes_refuse_validation_and_run_with_the_same_message() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}wake=['agent.idle']\n"), PENDING);
    let validation = command(&env, &["workflow", "validate", "--json"]);
    let report: Value = serde_json::from_slice(&validation.stdout).unwrap();
    let errors = report["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{report}");
    assert_eq!(errors[0]["path"], "checks.pr.wake");
    let run = command(&env, &["workflow", "run", "review"]);
    assert!(!run.status.success());
    assert!(
        String::from_utf8_lossy(&run.stderr).contains(errors[0]["message"].as_str().unwrap()),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn subscriptions_of_other_checks_do_not_wake_the_current_node() {
    let (env, _) = fixture(
        &format!(
            "{HEADER}{WAIT}[checks.other]\nrun='true'\non.ok='@done'\nwake=['review.ready']\n"
        ),
        PENDING,
    );
    let id = start(&env);
    until(&env, &id, |value| value["record"]["wait"].is_object());
    assert_untouched(&env, &id, "review.ready", json!({}));
}

#[test]
fn declared_forge_wakes_filter_checkout_and_ignore_sibling_skips() {
    for (name, sibling) in [("ci.failed", "ci.passed"), ("pr.opened", "pr.closed")] {
        for scope in ["worktree", "project"] {
            let header = HEADER.replace("worktree", scope);
            let (env, _) = fixture(&format!("{header}{WAIT}wake=['{name}']\n"), PENDING);
            let id = start(&env);
            until(&env, &id, |value| value["record"]["wait"].is_object());
            assert_untouched(&env, &id, name, json!({"path":"/another/checkout"}));
            assert_untouched(&env, &id, name, json!({}));
            assert_untouched(&env, &id, sibling, json!({"path":env.project_root}));
            assert_woken(&env, &id, name, json!({"path":env.project_root}));
        }
    }
}

#[test]
fn explicit_wake_filters_override_checkout_defaults_and_require_present_keys() {
    for (matches, wrong, matching) in [
        ("path='*'", json!({}), json!({"path":"/another/checkout"})),
        (
            "branch='main'",
            json!({"branch":"other"}),
            json!({"branch":"main"}),
        ),
        (
            "path='/chosen/checkout'",
            json!({"path":"/other"}),
            json!({"path":"/chosen/checkout"}),
        ),
    ] {
        let (env, _) = fixture(
            &format!("{HEADER}{WAIT}wake=[{{signal='ci.failed',match={{{matches}}}}}]\n"),
            PENDING,
        );
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        assert_untouched(&env, &id, "ci.failed", wrong);
        assert_untouched(&env, &id, "ci.passed", matching.clone());
        assert_woken(&env, &id, "ci.failed", matching);
    }
}

#[test]
fn condition_forge_wakes_follow_only_the_current_checks_checkout_terms() {
    let check = "[checks.pr]\nelse='pending'\ntimeout='1h'\non={ready='@done',pending='@wait'}\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n";
    let (env, _) = fixture(&format!("{HEADER}{check}"), "");
    let id = start(&env);
    until(&env, &id, |value| value["record"]["wait"].is_object());
    assert_untouched(&env, &id, "ci.failed", json!({"path":env.project_root}));
    assert_untouched(&env, &id, "pr.opened", json!({"path":"/another/checkout"}));
    assert_woken(&env, &id, "pr.opened", json!({"path":env.project_root}));
}

#[test]
fn stage_wakes_follow_the_current_archive_link_for_declared_and_derived_subscriptions() {
    use std::os::unix::fs::symlink;
    let condition = "[checks.pr]\nelse='pending'\ntimeout='1h'\non={ready='@done',pending='@wait'}\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\n";
    for check in [format!("{WAIT}wake=['team.stage']\n"), condition.into()] {
        let (env, _) = fixture(&format!("{HEADER}{check}"), PENDING);
        let archive = env.state_path_for(&env.project_root).team_notes_dir;
        let first = archive.join("first");
        let second = archive.join("second");
        for dir in [&first, &second] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("blackboard.md"), "# Board\n\nStage: Plan\n").unwrap();
        }
        let first_board = first.canonicalize().unwrap().join("blackboard.md");
        let second_board = second.canonicalize().unwrap().join("blackboard.md");
        let link = env.project_root.join(".rimz/team");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&first, &link).unwrap();
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        assert_untouched(&env, &id, "team.stage", json!({"board":second_board}));
        assert_untouched(&env, &id, "team.stage", json!({}));
        assert_woken(&env, &id, "team.stage", json!({"board":first_board}));
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();
        assert_untouched(&env, &id, "team.stage", json!({"board":first_board}));
        assert_woken(&env, &id, "team.stage", json!({"board":second_board}));
    }
}

#[test]
fn explicit_stage_filters_override_the_runs_board_scope() {
    for key in ["team", "instance", "board"] {
        let (env, _) = fixture(
            &format!("{HEADER}{WAIT}wake=[{{signal='team.stage',match={{{key}='chosen'}}}}]\n"),
            PENDING,
        );
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        assert_untouched(&env, &id, "team.stage", json!({(key):"other"}));
        assert_woken(&env, &id, "team.stage", json!({(key):"chosen"}));
    }
}
