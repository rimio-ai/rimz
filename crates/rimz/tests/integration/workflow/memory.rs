use super::*;
use rimz::disk::paths::Class;

pub(super) fn history_path(env: &Env, id: &str) -> PathBuf {
    let state = env.state_path_for(&env.project_root);
    let record = rimz::store::workflow::load(&state.workflow(&id.parse().unwrap())).unwrap();
    state
        .notes_run(Class::AutomationNotes, record.notes.as_ref().unwrap())
        .join("history.txt")
}

pub(super) fn assert_history_copy(env: &Env, id: &str) {
    let path = history_path(env, id);
    let expected = success(command(env, &["workflow", "show", id, "--history"]));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let actual = fs::read_to_string(&path);
        if actual.as_ref().is_ok_and(|actual| actual == &expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}: {actual:?}\nexpected:\n{expected}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn terminal_runs_keep_history_and_overwrite_an_existing_copy() {
    let mut actual = Vec::new();
    for target in ["@done", "@failed"] {
        let spec = format!(
            "{HEADER}[script.pr]\nrun='sh checks/pr.sh'\non.pending='@wait'\non.ok='{target}'\nwake=['memory.ready']\ntimeout='1h'\n"
        );
        let (env, dir) = fixture(&spec, PENDING);
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        let path = history_path(&env, &id);
        fs::write(&path, "previous end").unwrap();
        fs::write(
            dir.join("checks/pr.sh"),
            "printf '%s\\n' '{\"outcome\":\"ok\"}'\n",
        )
        .unwrap();
        success(command(&env, &["events", "emit", "memory.ready"]));
        settled(&env, &id, if target == "@done" { "done" } else { "failed" });
        success(command(&env, &["workflow", "advance", &id]));
        let expected = success(command(&env, &["workflow", "show", &id, "--history"]));
        actual.push(fs::read_to_string(path).is_ok_and(|text| text == expected));
    }
    assert_eq!(actual, [true, true]);
}

#[test]
fn cancel_keeps_history_but_runs_without_notes_do_not_write_a_copy() {
    for without_notes in [true, false] {
        let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        let kept = history_path(&env, &id);
        if without_notes {
            let state = env.state_path_for(&env.project_root);
            let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            let paths = state.workflow(&id.parse().unwrap());
            let mut record = rimz::store::workflow::load(&paths).unwrap();
            record.notes = None;
            rimz::store::workflow::write(&paths, &record, &guard).unwrap();
        }
        success(command(&env, &["workflow", "cancel", &id]));
        settled(&env, &id, "canceled");
        if without_notes {
            assert!(
                !kept.exists(),
                "runs without notes must not write {}",
                kept.display()
            );
        } else {
            assert_history_copy(&env, &id);
        }
    }
}

#[test]
fn history_copy_failures_warn_without_failing_committed_transitions() {
    for verb in ["advance", "cancel"] {
        let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
        let id = start(&env);
        until(&env, &id, |value| value["record"]["wait"].is_object());
        let history = history_path(&env, &id);
        fs::create_dir(&history).unwrap();
        if verb == "advance" {
            fs::write(
                dir.join("checks/pr.sh"),
                "printf '%s\\n' '{\"outcome\":\"done\"}'\n",
            )
            .unwrap();
            let state = env.state_path_for(&env.project_root);
            let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            let paths = state.workflow(&id.parse().unwrap());
            let mut record = rimz::store::workflow::load(&paths).unwrap();
            record.wait.as_mut().unwrap().recheck_at = None;
            record.recheck_at = None;
            rimz::store::workflow::write(&paths, &record, &guard).unwrap();
        }
        let output = command(&env, &["workflow", verb, &id]);
        assert!(
            output.status.success(),
            "{verb} committed but refused: {output:?}"
        );
        let warning = String::from_utf8_lossy(&output.stderr);
        assert!(
            warning.contains("warning:") && warning.contains("history") && warning.contains(&id),
            "{output:?}"
        );
        if verb == "cancel" {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("canceled"),
                "{output:?}"
            );
        }
        settled(
            &env,
            &id,
            if verb == "cancel" { "canceled" } else { "done" },
        );
        fs::remove_dir(&history).unwrap();
        success(command(&env, &["workflow", "advance", &id]));
        assert_history_copy(&env, &id);
    }
}

#[test]
fn gc_backfills_history_before_removing_a_terminal_home_and_keeps_it_on_failure() {
    let mut actual = Vec::new();
    for fail in [false, true] {
        let (env, _) = fixture(
            &format!("{HEADER}[script.pr]\nrun='sh checks/pr.sh'\non.ok='@done'\n"),
            "printf '%s\\n' '{\"outcome\":\"ok\"}'\n",
        );
        let id = start(&env);
        settled(&env, &id, "done");
        let history = history_path(&env, &id);
        let expected = success(command(&env, &["workflow", "show", &id, "--history"]));
        let _ = fs::remove_file(&history);
        if fail {
            fs::create_dir(&history).unwrap();
        }
        let state = env.state_path_for(&env.project_root);
        let paths = state.workflow(&id.parse().unwrap());
        fs::File::open(&paths.record)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
        let output = env.rimz().args(["gc", "--json"]).output().unwrap();
        success(output);
        actual.push((
            paths.home.exists(),
            fail || fs::read_to_string(history).is_ok_and(|text| text == expected),
        ));
    }
    assert_eq!(actual, [(false, true), (true, true)]);
}
