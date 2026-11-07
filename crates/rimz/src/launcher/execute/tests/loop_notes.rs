use super::*;
use crate::disk::paths::Class;
use crate::harness::launch::ReminderSection;

const BODY: &str = "Each run is a fresh agent with no memory of earlier runs.";

fn loop_fixture(enabled: bool) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.fresh.loop_task = Some("review".into());
    fixture.fresh.loop_notes_at = enabled.then(|| "2026-11-07T00:23:36Z".parse().unwrap());
    fixture.fresh.reminder_section = Some(ReminderSection::Loop(BODY.into()));
    fixture
}

#[test]
fn loop_launch_keeps_dated_memory_and_the_completed_section_only_on_its_leader() {
    let mut actual = Vec::new();
    for (supervised, enabled, staged) in [
        (true, true, false),
        (true, false, false),
        (false, true, false),
        (false, false, false),
        (false, true, true),
    ] {
        let mut fixture = loop_fixture(enabled);
        fixture.fresh.layout = crate::harness::spec::parse_layout_spec(
            "claude,claude",
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        fixture.fresh.logins.push("default".parse().unwrap());
        fixture.fresh.prompt_index = Some(1);
        fixture.fresh.leader_index = Some(1);
        if staged {
            fixture.fresh.team = Some((
                "forge".into(),
                crate::config::Team {
                    stages: vec!["Build".into()],
                    ..Default::default()
                },
            ));
        }
        let leader = supervised.then(|| {
            let mut leader = fixture.leader();
            leader.loop_task = fixture.fresh.loop_task.clone();
            leader.completion = RunCompletion::Detached;
            leader
        });
        let store = fixture.fresh.context.store.clone();
        let backend = fake_backend::Backend::default();
        let LaunchOutcome::Opened(opened) =
            open_fresh(fixture.fresh.clone(), leader, &mut Unattended, &backend).unwrap()
        else {
            panic!("loop launch")
        };
        let relative = PathBuf::from("20261107/002336Z-loop-review");
        let folder = store.paths().notes_run(Class::AutomationNotes, &relative);
        let expected_body = if enabled && !staged {
            format!(
                "{BODY}\n\n`{}/review-notes.md` is this run's working memory. Only this run's agent writes it, across restarts and its subagents. Write it for someone arriving cold. Note any change you made outside this repository: pushes, PR or issue comments, databases, remote services.",
                folder.display()
            )
        } else {
            BODY.into()
        };
        let records: Vec<_> = opened
            .identities
            .iter()
            .map(|identity| serde_json::to_value(&identity.launch.record).unwrap())
            .collect();
        let tabs = backend.tabs.lock().unwrap();
        let requests: Vec<_> = tabs[0]
            .panes
            .columns
            .iter()
            .flat_map(|column| &column.panes)
            .map(|pane| {
                let payload = std::fs::read_to_string(pane.argv.last().unwrap()).unwrap();
                crate::harness::launch::decode_exec_request("claude", None, &payload).unwrap()
            })
            .collect();
        actual.push((
            folder.exists(),
            records[0]["notes"].is_null() && records[0]["loop_section"].is_null(),
            records[1]["notes"]
                == if enabled && !staged {
                    serde_json::json!(relative)
                } else {
                    serde_json::Value::Null
                },
            records[1]["loop_section"] == expected_body,
            requests[0].reminder_section.is_none(),
            requests[1].reminder_section == Some(ReminderSection::Loop(expected_body)),
        ));
        if folder.exists() {
            let manifest = crate::store::notes::Manifest::read(&folder).unwrap();
            assert_eq!(manifest.created_at, "2026-11-07T00:23:36Z".parse().unwrap());
            assert_eq!(manifest.checkout, fixture.fresh.launch.cwd);
            assert!(
                matches!(manifest.kind, crate::store::notes::Kind::Loop { task } if task == "review")
            );
            let row = store
                .runtime_projection(crate::RuntimeScope::Audit)
                .unwrap();
            assert!(row.agents.iter().any(|agent| {
                agent
                    .record
                    .as_ref()
                    .and_then(|record| record.notes.as_ref())
                    == Some(&relative)
            }));
        }
    }
    assert_eq!(
        actual,
        [
            (true, true, true, true, true, true),
            (false, true, true, true, true, true),
            (true, true, true, true, true, true),
            (false, true, true, true, true, true),
            (false, true, true, true, true, true),
        ]
    );
}

#[test]
fn verified_loop_retry_reuses_memory_and_does_not_repeat_the_notes_paragraph() {
    let fixture = loop_fixture(true);
    let mut leader = fixture.leader();
    leader.loop_task = fixture.fresh.loop_task.clone();
    leader.completion = RunCompletion::Detached;
    let backend = fake_backend::Backend::default();
    let LaunchOutcome::Opened(opened) = open_fresh(
        fixture.fresh.clone(),
        Some(leader),
        &mut Unattended,
        &backend,
    )
    .unwrap() else {
        panic!("first attempt")
    };
    let first = serde_json::to_value(&opened.identities[0].launch.record).unwrap();
    assert!(
        first["notes"].is_string(),
        "the loop needs kept memory: {first}"
    );
    let run = opened.run.unwrap();
    let run_id = run.record.run_id.clone();
    let retry = run.retry("retry".into(), run_id);
    let PreparedState::Ready(fresh) = retry.state else {
        panic!("retry keeps the settled checkout")
    };
    let LaunchOutcome::Opened(opened) =
        open_fresh(*fresh, retry.leader_run, &mut Unattended, &backend).unwrap()
    else {
        panic!("retry")
    };
    let second = serde_json::to_value(&opened.identities[0].launch.record).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        second["loop_section"]
            .as_str()
            .unwrap()
            .matches("working memory")
            .count(),
        1
    );
    let folders =
        crate::store::notes::enumerate(&fixture.fresh.context.store.paths().automation_notes_dir)
            .unwrap();
    assert_eq!(folders.runs.len(), 1);
}
