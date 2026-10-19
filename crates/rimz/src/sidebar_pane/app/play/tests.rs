use super::*;

fn pilot() -> Scenario {
    Scenario::parse("pilot", include_str!("../../../../scenarios/pilot.scn")).unwrap()
}

fn text_at(column: &mut Column, scenario: &Scenario, millis: u64) -> String {
    column
        .run_until(scenario, Duration::from_millis(millis), |_| Ok(()))
        .unwrap();
    column.capture().unwrap().text(None)
}

fn before(text: &str, first: &str, second: &str) -> bool {
    text.find(first).unwrap() < text.find(second).unwrap()
}

#[test]
fn cost_beat_key_frame_waits_for_the_money_roll() {
    let scenario = Scenario::parse("money", "scenario money length 4s\nagent @a claude\n0s @a runs \"work\"\n0s @a set cost $1\n1s @a set cost $5").unwrap();
    let mut keys = Vec::new();
    Column::new(&scenario, None, None)
        .unwrap()
        .run_until(&scenario, Duration::from_secs(3), |frame| {
            if frame.key && frame.beat == 2 {
                keys.push(frame.text(None));
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert!(
        keys[0].contains("$5.00"),
        "settled cost is the key frame: {}",
        keys[0]
    );
}

#[test]
fn unread_ui_beat_is_painted_before_at_and_key_capture() {
    let scenario = Scenario::parse("unread", "scenario unread length 3s\nagent @a claude\nagent @b codex\n0s @a done\n0s @b done\n0s ui select @a\n0s ui key u\n1s ui key m").unwrap();
    let mut column = Column::new(&scenario, None, None).unwrap();
    let opening = text_at(&mut column, &scenario, 0);
    assert!(opening.contains("@a#main") && opening.contains("@b#main"));
    let mut keys = Vec::new();
    column
        .run_until(&scenario, Duration::from_secs(1), |frame| {
            if frame.key && frame.beat == 4 {
                keys.push(frame.text(None));
            }
            Ok(())
        })
        .unwrap();
    let at = column.capture().unwrap().text(None);
    assert_ne!(
        at, opening,
        "--at the UI beat must include its unread toggle"
    );
    assert!(
        !at.contains("@a#main") && at.contains("@b#main"),
        "the read card leaves the unread filter: {at}"
    );
    column
        .run_until(&scenario, Duration::from_millis(1_500), |frame| {
            if frame.key && frame.beat == 4 {
                keys.push(frame.text(None));
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert!(
        !keys[0].contains("@a#main") && keys[0].contains("@b#main"),
        "the settled key includes the unread toggle: {}",
        keys[0]
    );
}

#[test]
fn filmstrip_reexport_removes_only_previous_export_files() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "0000.txt",
        "0017.ansi",
        "0017.png",
        "10000.ansi",
        "index.jsonl",
        "notes.txt",
    ] {
        std::fs::write(dir.path().join(name), "old").unwrap();
    }
    Filmstrip::new(dir.path()).unwrap();
    for name in ["0000.txt", "0017.ansi", "0017.png", "10000.ansi"] {
        assert!(!dir.path().join(name).exists(), "stale export {name}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("index.jsonl")).unwrap(),
        ""
    );
    assert!(dir.path().join("notes.txt").exists());
}

#[test]
fn an_offscreen_ui_beat_is_recorded_even_when_the_body_does_not_change() {
    let scenario = Scenario::parse(
        "missing-ui",
        "scenario missing-ui length 3s pets off\nagent @a claude\nagent @b codex\n0s @b runs \"work\"\n1s ui key w\n2s ui select @a",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Filmstrip::new(dir.path()).unwrap();
    Column::new(&scenario, None, None)
        .unwrap()
        .run_until(&scenario, Duration::from_millis(2_999), |frame| {
            writer.push(frame)
        })
        .unwrap();
    let index = std::fs::read_to_string(dir.path().join("index.jsonl")).unwrap();
    assert!(index.lines().any(|line| {
        let frame: serde_json::Value = serde_json::from_str(line).unwrap();
        frame["label"]
            .as_str()
            .unwrap()
            .contains("@a not on screen")
    }));
}

#[test]
fn ui_beats_toggle_a_single_cards_delegation_through_mouse_geometry() {
    let scenario = Scenario::parse("one", "scenario one length 5s\nagent @a claude\n0s @a runs \"work\"\n0s @a spawns @child codex \"nested work\"\n1s ui expand @a\n2s ui collapse @a\n3s ui expand @a").unwrap();
    let mut column = Column::new(&scenario, None, None).unwrap();
    assert!(text_at(&mut column, &scenario, 1_100).contains("nested work"));
    let collapsed = text_at(&mut column, &scenario, 2_100);
    assert!(!collapsed.contains("nested work"), "{collapsed}");
    assert!(text_at(&mut column, &scenario, 3_100).contains("nested work"));
}

#[test]
fn pilot_holds_then_reorders_at_the_loop_freeze_deadline() {
    let scenario = pilot();
    let mut column = Column::new(&scenario, None, None).unwrap();
    let before_ask = text_at(&mut column, &scenario, 12_000);
    assert!(before(&before_ask, "@planner#main", "@coder#main"));
    let freeze = column
        .state
        .ui
        .freeze
        .as_ref()
        .expect("selection arms the product freeze");
    let deadline =
        (freeze.deadline_ms(false) - scenario.at(Duration::ZERO).now.as_millisecond()) as u64;
    assert!(
        deadline > 12_500 && deadline <= 17_000,
        "deadline {deadline}"
    );
    let during = text_at(&mut column, &scenario, deadline - 1);
    assert!(before(&during, "@planner#main", "@coder#main"));
    let after = text_at(&mut column, &scenario, deadline + 100);
    assert!(before(&after, "@coder#main", "@planner#main"));
}

#[test]
fn pilot_renders_nested_results_countdown_expansion_and_root_unread() {
    let scenario = pilot();
    let mut column = Column::new(&scenario, None, None).unwrap();
    let children = text_at(&mut column, &scenario, 6_500);
    assert!(
        children.contains("codex")
            && children.contains("claude")
            && children.contains("renderer reviewed"),
        "{children}"
    );
    let first = text_at(&mut column, &scenario, 11_000);
    let second = text_at(&mut column, &scenario, 12_000);
    assert!(first.contains("wakes in 1m"), "{first}");
    assert!(second.contains("wakes now"), "{second}");
    text_at(&mut column, &scenario, 17_600);
    let selected = column
        .state
        .ui
        .visible_roster(&column.state.current)
        .row(column.state.ui.selected_index.unwrap())
        .unwrap()
        .as_agent()
        .unwrap();
    assert_eq!(selected.handle.as_deref(), Some("coder"));
    text_at(&mut column, &scenario, 21_100);
    let coder = column
        .state
        .current
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find(|row| {
            row.as_agent()
                .is_some_and(|card| card.handle.as_deref() == Some("coder"))
        })
        .unwrap();
    assert!(coder.unread, "root finish opens an unread episode");
}

#[test]
fn wraps_and_seeks_rebuild_instead_of_carrying_loop_state() {
    let scenario = pilot();
    let mut column = Column::new(&scenario, None, None).unwrap();
    let opening = text_at(&mut column, &scenario, 0);
    text_at(&mut column, &scenario, 21_100);
    column.seek(&scenario, Duration::ZERO).unwrap();
    assert_eq!(column.capture().unwrap().text(None), opening);
    let mut first = Vec::new();
    column
        .run_until(
            &scenario,
            scenario.length - Duration::from_millis(1),
            |frame| {
                first.push((frame.t_ms, frame.text(None)));
                Ok(())
            },
        )
        .unwrap();
    column.seek(&scenario, Duration::ZERO).unwrap();
    let mut second = Vec::new();
    column
        .run_until(
            &scenario,
            scenario.length - Duration::from_millis(1),
            |frame| {
                second.push((frame.t_ms, frame.text(None)));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(first, second);
}

#[test]
fn pilot_filmstrip_is_a_reviewable_index_and_key_frames() {
    let scenario = pilot();
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Filmstrip::new(dir.path()).unwrap();
    Column::new(&scenario, None, None)
        .unwrap()
        .run_until(
            &scenario,
            scenario.length - Duration::from_millis(1),
            |frame| writer.push(frame),
        )
        .unwrap();
    let index = std::fs::read_to_string(dir.path().join("index.jsonl")).unwrap();
    assert!(index.lines().any(|line| {
        let frame: serde_json::Value = serde_json::from_str(line).unwrap();
        (17_000..17_500).contains(&frame["t_ms"].as_u64().unwrap()) && frame["key"] == true
    }));
    let mut review = index.clone();
    for row in index.lines() {
        let row: serde_json::Value = serde_json::from_str(row).unwrap();
        if row["key"] == true {
            review.push_str(&format!(
                "\n== {}ms: {} ==\n",
                row["t_ms"],
                row["label"].as_str().unwrap()
            ));
            review.push_str(
                &std::fs::read_to_string(dir.path().join(row["text"].as_str().unwrap())).unwrap(),
            );
        }
    }
    let review = review
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(review);
}

#[test]
fn motion_emits_each_paint_and_still_frames_keep_money_easing() {
    let scenario = Scenario::parse("money", "scenario money length 4s\nagent @a claude\n0s @a runs \"work\"\n0s @a set cost $1\n1s @a set cost $5").unwrap();
    let mut column = Column::new(&scenario, None, None).unwrap();
    let start = text_at(&mut column, &scenario, 1_000);
    let end = text_at(&mut column, &scenario, 2_500);
    assert_ne!(start, end);
    assert!(
        end.contains("$5.00"),
        "money easing settles while cosmetic motion is held: {end}"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Filmstrip::new(dir.path()).unwrap();
    Column::new(
        &scenario,
        None,
        Some(Duration::ZERO..Duration::from_secs(1)),
    )
    .unwrap()
    .run_until(&scenario, Duration::from_secs(1), |frame| {
        writer.push(frame)
    })
    .unwrap();
    let count = std::fs::read_to_string(dir.path().join("index.jsonl"))
        .unwrap()
        .lines()
        .count();
    assert!(count >= 10, "motion paints on the refresh grid: {count}");
}
