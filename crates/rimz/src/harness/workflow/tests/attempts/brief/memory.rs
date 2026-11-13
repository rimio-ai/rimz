use super::*;

#[test]
fn step_notes_name_kept_memory_only_when_enabled_and_recorded() {
    let mut actual = Vec::new();
    for (enabled, recorded) in [(true, true), (false, true), (true, false)] {
        let text = format!("{STEP}notes={enabled}\n");
        let (_dir, state, record) = setup(&text);
        let relative = crate::store::notes::allocate(
            &state,
            crate::store::notes::Kind::Workflow {
                workflow: record.workflow.clone(),
                run: record.id.clone(),
            },
            &record.scope.path,
            record.created_at,
        )
        .unwrap();
        let mut value = serde_json::to_value(&record).unwrap();
        if recorded {
            value["notes"] = json!(relative);
            let paths = state.workflow(&record.id);
            fs::remove_dir(paths.notes()).unwrap();
            std::os::unix::fs::symlink(
                state.notes_run(crate::disk::paths::Class::AutomationNotes, &relative),
                paths.notes(),
            )
            .unwrap();
        }
        let record: RunRecord = serde_json::from_value(value).unwrap();
        update(&state, &record);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        let note = note(&host);
        let path = state.notes_run(crate::disk::paths::Class::AutomationNotes, &relative);
        let expected = format!(
            "`{}/build-notes.md` is this step's working memory. Only this step's agent writes it, across visits, restarts and its subagents. Write it for someone arriving cold. Note any change you made outside this repository: pushes, PR or issue comments, databases, remote services.",
            path.display()
        );
        actual.push((
            note.contains(&expected),
            note.contains(MORE),
            note.contains("context.json"),
        ));
    }
    assert_eq!(
        actual,
        [
            (true, true, false),
            (false, true, false),
            (false, true, false)
        ]
    );
}

#[test]
fn command_notes_refuse_whatever_their_type() {
    let mut actual = Vec::new();
    for value in ["false", "'wrong'"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nshell='true'\nthen='@done'\nnotes={value}\n"
        );
        match crate::harness::workflow::spec::parse(&text) {
            Err(crate::harness::workflow::SpecErr::Toml { path, source }) => {
                actual.push((path, source.message().contains("unknown field")));
            }
            other => panic!("notes on a command refuses: {other:?}"),
        }
    }
    assert_eq!(actual, vec![("command.build.notes".to_owned(), true); 2]);
}
