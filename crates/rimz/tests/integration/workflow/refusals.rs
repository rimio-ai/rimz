use super::*;

#[test]
fn validate_refuses_an_unknown_field_at_its_dotted_path_in_text_and_json() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "validate", "review"]);
    assert!(
        !output.status.success(),
        "an unknown field refuses: {output:?}"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("script.pr.future") && text.contains("unknown field"),
        "{text}"
    );
    assert!(!text.contains("ignored"), "{text}");
    let output = command(&env, &["workflow", "validate", "review", "--json"]);
    assert!(!output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["script.pr.future"]
    );
    assert_eq!(value["warnings"], json!([]));
}

#[test]
fn validate_prints_a_toml_excerpt_with_its_caret_on_a_line_of_its_own() {
    let (env, _) = fixture("[workflow\nscope = 'worktree'\n", PENDING);
    let output = command(&env, &["workflow", "validate", "review"]);
    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with('|') && line.trim_end().ends_with('^')),
        "{stdout}"
    );
    assert!(!stdout.contains(r"\n"), "{stdout}");
}

#[test]
fn run_refuses_an_unknown_field_and_starts_no_run() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "run", "review"]);
    assert!(
        !output.status.success(),
        "an unknown field refuses a run: {output:?}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("script.pr.future") && stderr.contains("unknown field"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "no receipt for a refused run");
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert_eq!(listed, json!([]));
}

#[test]
fn reread_of_an_invalid_definition_refuses_at_its_dotted_path_and_keeps_the_run() {
    let text = format!(
        "{HEADER}{}",
        WAIT.replace("[script.pr]", "[script.pr]\nwake=['review.ready']")
    );
    let (env, dir) = fixture(&text, PENDING);
    let id = start(&env);
    let before = until(&env, &id, |v| {
        v["record"]["wait"].is_object() && v["record"]["attempt"].is_null()
    });
    fs::write(
        dir.join("workflow.toml"),
        text.replace("timeout = '1h'", "timeout = 'soon'\nfuture = true"),
    )
    .unwrap();
    fs::write(dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    let output = command(&env, &["workflow", "advance", &id]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("script.pr.future") && stderr.contains("unknown field"),
        "{stderr}"
    );
    let value = show(&env, &id);
    assert_eq!(
        value["record"], before["record"],
        "the refused reread writes nothing"
    );
    assert_eq!(value["journal"], before["journal"]);
    assert!(value.get("warnings").is_none(), "{value}");
    let shown = success(command(&env, &["workflow", "show", &id]));
    assert!(
        shown.contains("\nwaits on its definition: ") && shown.contains("script.pr.future"),
        "the text view names the refusal: {shown}"
    );
    let canonical = dir.canonicalize().unwrap();
    assert!(
        shown.contains(&format!(
            "check with rimz workflow validate {}",
            canonical.display()
        )),
        "{shown}"
    );
    let listed = success(command(&env, &["workflow", "list"]));
    assert!(
        listed.contains("waits on its definition: ") && listed.contains("script.pr.future"),
        "{listed}"
    );
    fs::write(dir.join("workflow.toml"), "not a valid spec").unwrap();
    let broken = show(&env, &id);
    assert!(broken["record"].is_object() && broken["journal"].is_array());
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    settled(&env, &id, "done");
}

#[test]
fn clean_runs_carry_no_warning_surfaces() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    assert!(value.get("warnings").is_none(), "{value}");
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert!(listed[0].get("warnings").is_none());
    let text = success(command(&env, &["workflow", "list"]));
    assert!(!text.contains("ignored"), "{text}");
    success(command(&env, &["workflow", "cancel", &id]));
}

/// `workflow show` prints a live run whose definition admission refuses from its record, the
/// first error and the validate command, and exits 0, whether the definition is missing, does
/// not parse, or parses with a value admission refuses; `workflow list` names the refusal ahead
/// of a recorded capacity wait.
#[test]
fn a_refused_definition_shows_the_run_from_its_record_and_lists_ahead_of_capacity() {
    let text = format!("{HEADER}{WAIT}");
    let (env, dir) = fixture(&text, PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| {
        v["record"]["wait"].is_object() && v["record"]["attempt"].is_null()
    });
    let record = Path::new(value["dir"].as_str().unwrap())
        .parent()
        .unwrap()
        .join("record.json");
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut held: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    held["wait"]["outcome"] = json!("capacity");
    held["wait"]["detail"] = json!("waiting for provider capacity");
    rimz::disk::atomic::write_temp_then_rename_cache(&record, &held).unwrap();
    drop(guard);
    let validate = format!(
        "check with rimz workflow validate {}",
        dir.canonicalize().unwrap().display()
    );
    let spec = dir.join("workflow.toml");
    for (edit, heading, error) in [
        (
            Some(text.replace("timeout = '1h'", "timeout = 'tomorrow'")),
            "its definition is refused",
            "tomorrow",
        ),
        (
            Some("not a valid spec".into()),
            "its definition does not parse",
            "",
        ),
        (None, "its definition cannot be read", ""),
    ] {
        match &edit {
            Some(edit) => fs::write(&spec, edit).unwrap(),
            None => fs::remove_file(&spec).unwrap(),
        }
        let output = command(&env, &["workflow", "show", &id]);
        assert!(output.status.success(), "{heading}: {output:?}");
        let shown = String::from_utf8(output.stdout).unwrap();
        assert!(
            shown.contains(heading)
                && shown.contains("\nwaits on its definition: ")
                && shown.contains(error)
                && shown.contains(&validate),
            "{heading}: {shown}"
        );
        let listed = success(command(&env, &["workflow", "list"]));
        let row = listed.lines().find(|line| line.contains("#main"));
        assert!(
            row.is_some_and(|row| row.contains("waits on its definition: ")),
            "{heading}: {listed}"
        );
    }
    fs::write(&spec, &text).unwrap();
    success(command(&env, &["workflow", "cancel", &id]));
}
