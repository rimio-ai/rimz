use std::time::Duration;

use super::*;

fn rewake_without_own_run(linked: bool) -> (bool, bool) {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let id = crate::WorkspaceId::from_project_root(&checkout);
    let state = crate::StatePaths::under(id.clone(), &temp.path().join("home")).unwrap();
    let runtime = crate::RuntimePaths::for_state_under(&state, temp.path());
    let store = Store::open(state, runtime).unwrap();
    let workspace = ResolvedWorkspace {
        workspace_id: id,
        project_root: checkout.clone(),
        cwd_project_root: None,
        root_class: crate::workspace::RootClass::Directory,
        worktree_root: checkout.clone(),
        worktree_branch: None,
        session_name: "test".into(),
        mux_hint: None,
    };
    let team: Team = toml::from_str(
        "stages = ['Build']\n[[roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Build']",
    )
    .unwrap();
    if linked {
        super::super::team_notes::register(
            &store,
            &checkout,
            "mill",
            &team,
            super::super::team_notes::Launch::Fresh,
        )
        .unwrap();
        std::fs::write(
            super::super::scratch::board_path(&checkout),
            "Stage: Build (@coder)\n",
        )
        .unwrap();
    }
    let now = Timestamp::now();
    let mut member = crate::testkit::agent_state("claude", "coder", now);
    member.role = Some("coder".into());
    let mut called = false;
    let result = with_rewake(
        &workspace,
        &store,
        "forge",
        &team,
        &member,
        &[],
        &checkout,
        None,
        now,
        RewakeCause::Registration,
        |_, _| {
            called = true;
            Ok(())
        },
    )
    .unwrap();
    (matches!(result, Rewake::Closed), called)
}

/// A return's PROMPT stands in while it names the member's launch at its epoch and is pending,
/// or was delivered on this return: since the resume began, or for a registration within the
/// delivery window, so a member registering again long after its return is woken.
#[test]
fn a_return_prompt_stands_in_while_pending_or_delivered_on_this_return() {
    use crate::store::message::{DeliveryGate, ReturningMember};
    let since = Timestamp::now();
    let window = MessageBody::Prompt.delivery_window();
    let recent = since - window / 2;
    let stale = since - window - Duration::from_secs(1);
    let after = since + Duration::from_secs(1);
    let resume = RewakeCause::Resume {
        since,
        last_prompt_at: None,
    };
    let mut member = crate::testkit::agent_state("claude", "reviewer", since);
    member.launch_id = Some("launch-reviewer".into());
    member.epoch = 1;
    let record = |status, delivered_at, launch: &str, epoch| {
        let mut record = MessageRecord::new_for_card(
            crate::WorkspaceId::from_project_root(Path::new("/ws")),
            AgentKind::new_unchecked("claude"),
            "leader".into(),
            Some("coder".into()),
            "say hi".into(),
            DeliveryGate::Done,
        );
        record.team_return = vec![ReturningMember {
            launch_id: launch.into(),
            epoch,
        }];
        record.status = status;
        record.delivered_at = delivered_at;
        record
    };
    let cases = [
        (
            record(MessageStatus::Queued, None, "launch-reviewer", 1),
            true,
            true,
        ),
        (
            record(MessageStatus::Sent, None, "launch-reviewer", 1),
            true,
            true,
        ),
        (
            record(MessageStatus::Delivered, Some(after), "launch-reviewer", 1),
            true,
            true,
        ),
        (
            record(MessageStatus::Delivered, Some(recent), "launch-reviewer", 1),
            false,
            true,
        ),
        (
            record(MessageStatus::Delivered, Some(stale), "launch-reviewer", 1),
            false,
            false,
        ),
        (
            record(MessageStatus::Canceled, None, "launch-reviewer", 1),
            false,
            false,
        ),
        (
            record(MessageStatus::Errored, None, "launch-reviewer", 1),
            false,
            false,
        ),
        (
            record(MessageStatus::Queued, None, "launch-other", 1),
            false,
            false,
        ),
        (
            record(MessageStatus::Queued, None, "launch-reviewer", 0),
            false,
            false,
        ),
    ];
    for (record, by_resume, by_registration) in cases {
        for (cause, expected) in [
            (resume, by_resume),
            (RewakeCause::Registration, by_registration),
        ] {
            let records = std::slice::from_ref(&record);
            assert_eq!(
                (
                    cause.return_prompted(records, &[], &member, since),
                    cause.return_prompted(&[], records, &member, since)
                ),
                (expected, expected),
                "{:?} delivered {:?} to {:?}",
                record.status,
                record.delivered_at,
                record.team_return
            );
        }
    }
}

#[test]
fn rewake_skips_an_unlinked_board() {
    assert_eq!(rewake_without_own_run(false), (true, false));
}

#[test]
fn rewake_skips_another_teams_run() {
    assert_eq!(
        rewake_without_own_run(true),
        (true, false),
        "another team's board must not wake this member"
    );
}

#[test]
fn stage_strip_matches_exact_names_and_includes_terminal() {
    let stages = vec!["Plan".into(), "Plan review".into()];
    assert_eq!(
        stage_strip(&stages, Some("Plan review")),
        "Plan → [Plan review] → Done"
    );
    assert_eq!(
        stage_strip(&stages, Some("Done")),
        "Plan → Plan review → [Done]"
    );
    assert_eq!(
        stage_strip(&stages, Some("Plan (delta)")),
        "Plan → Plan review → Done"
    );
}

#[test]
fn stage_notices_are_prose_for_handoff_refire_and_recovery() {
    assert_eq!(
        stage_open_body(
            Some("Plan"),
            "Implement",
            "planner",
            None,
            Some("plan ready"),
            false,
            None
        ),
        "@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from .rimz/team/blackboard.md.\n\nNote: plan ready"
    );
    assert_eq!(
        stage_open_body(
            None,
            "Explore",
            "planner",
            None,
            Some("opened"),
            false,
            None
        ),
        "@planner opened the stage Explore. Explore is yours: pick it up from .rimz/team/blackboard.md.\n\nNote: opened"
    );
    assert_eq!(
        stage_open_body(
            Some("Plan"),
            "Implement",
            "planner",
            Some("round-binder"),
            None,
            false,
            Some("lead")
        ),
        "@planner (@round-binder) flipped the stage Plan -> Implement. Implement is yours: pick it up from .rimz/team/blackboard.md.\n\nYour report goes in your stage file and anything for the user to @lead; end the turn with the flip and no pane text."
    );
    assert_eq!(
        stage_open_body(
            Some("Implement"),
            "Implement",
            "user",
            None,
            Some("retry"),
            false,
            None
        ),
        "@user re-opened Implement. It is still yours: pick it up from .rimz/team/blackboard.md.\n\nNote: retry"
    );
    assert_eq!(
        stage_open_body(
            Some("Implement"),
            "Implement",
            "rimz",
            None,
            None,
            true,
            None
        ),
        "The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread .rimz/team/blackboard.md and continue from where it stops."
    );
}

#[test]
fn hand_off_is_a_member_leaving_its_own_stage_for_another_owner() {
    let team: Team = toml::from_str(
        r#"
        [[roles]]
        role = "coder"
        profile = "claude"
        owns = ["Build", "Polish"]
        [[roles]]
        role = "reviewer"
        profile = "claude"
        owns = ["Review"]
        "#,
    )
    .unwrap();
    let agent = crate::testkit::agent_state("claude", "coder", Timestamp::UNIX_EPOCH);
    let coder = Flipper::Member {
        role: "coder",
        agent: &agent,
        pane: None,
    };
    for (by, from, owner, expected) in [
        (&coder, Some("Build"), Some("reviewer"), true),
        (&coder, Some("Build"), None, true),
        (&coder, Some("Build"), Some("coder"), false),
        (&coder, Some("Review"), Some("coder"), false),
        (&coder, None, Some("reviewer"), false),
        (&Flipper::User, Some("Build"), Some("reviewer"), false),
    ] {
        assert_eq!(
            hands_off(by, &team, from, owner),
            expected,
            "{} {from:?} -> {owner:?}",
            by.name()
        );
    }
}

#[test]
fn uncommitted_paths_list_changes_and_untracked_files() {
    let outside = tempfile::tempdir().unwrap();
    assert!(uncommitted_paths(outside.path()).unwrap().is_empty());
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(repo.path())
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.path().join("moved.rs"), "fn a() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    std::fs::create_dir_all(repo.path().join(".rimz")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.path().join(".rimz/team")).unwrap();
    std::fs::write(
        repo.path().join(".rimz/team/blackboard.md"),
        "Stage: Build\n",
    )
    .unwrap();
    assert!(uncommitted_paths(repo.path()).unwrap().is_empty());
    git(&["mv", "moved.rs", "renamed.rs"]);
    std::fs::create_dir(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/new file.rs"), "").unwrap();
    assert_eq!(
        uncommitted_paths(repo.path()).unwrap(),
        ["renamed.rs", "src/new file.rs"]
    );
}

#[test]
fn rewake_skips_a_member_whose_stage_notice_is_queued_or_already_delivered() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let member = crate::testkit::agent_state("codex", "sess-coder", now);
    let other = crate::testkit::agent_state("codex", "sess-reviewer", now);
    let window = MessageBody::Prompt.delivery_window();
    let record = |to: &AgentState, sender: MessageSender, delivered_at: Option<Timestamp>| {
        let mut record = MessageRecord::new(
            crate::ids::WorkspaceId::from_project_root(Path::new("/tmp/rimz-rewake")),
            to,
            "stage".to_owned(),
            DeliveryGate::Done,
        );
        record.sender = sender;
        if let Some(at) = delivered_at {
            record.status = MessageStatus::Delivered;
            record.delivered_at = Some(at);
        }
        record
    };
    let stage = MessageSender::Harness {
        notice: HarnessNotice::Stage,
    };
    let wait = MessageSender::Harness {
        notice: HarnessNotice::Wait,
    };
    let queued = record(&member, stage.clone(), None);
    let just_delivered = record(&member, stage.clone(), Some(now - Duration::from_secs(1)));
    let long_delivered = record(&member, stage.clone(), Some(now - window * 2));
    let resume = RewakeCause::Resume {
        since: now - Duration::from_secs(2),
        last_prompt_at: None,
    };
    let before_attempt = record(&member, stage.clone(), Some(now - Duration::from_secs(3)));
    let not_stage = [
        record(&member, wait.clone(), None),
        record(&member, MessageSender::Human, None),
        record(&other, stage.clone(), None),
    ];
    let delivered_not_stage = [
        record(&member, wait, Some(now)),
        record(&member, MessageSender::Human, Some(now)),
        record(&other, stage, Some(now)),
    ];
    for (cause, live, history, expected) in [
        (resume, &[queued.clone()][..], &[][..], true),
        (resume, &[][..], std::slice::from_ref(&just_delivered), true),
        (
            resume,
            &[][..],
            std::slice::from_ref(&before_attempt),
            false,
        ),
        (RewakeCause::Registration, &[queued][..], &[][..], true),
        (
            RewakeCause::Registration,
            &[][..],
            std::slice::from_ref(&just_delivered),
            true,
        ),
        (
            RewakeCause::Registration,
            &[][..],
            std::slice::from_ref(&long_delivered),
            false,
        ),
        (resume, &not_stage[..], &delivered_not_stage[..], false),
        (
            RewakeCause::Registration,
            &not_stage[..],
            &delivered_not_stage[..],
            false,
        ),
    ] {
        assert_eq!(
            cause.already_woken(live, history, &member, now),
            expected,
            "{cause:?} live={} history={}",
            live.len(),
            history.len()
        );
    }
}

#[test]
fn a_member_prompted_since_its_resume_or_registration_is_not_rewoken() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let window = MessageBody::Prompt.delivery_window();
    let copy = now - Duration::from_secs(60);
    let mut member = crate::testkit::agent_state("codex", "sess-coder", now);
    let resume = |last_prompt_at| RewakeCause::Resume {
        since: copy,
        last_prompt_at,
    };
    let (early, late) = (now - window / 2, now - window / 4);
    let cases = [
        (resume(Some(Some(copy))), Some(copy), None, false),
        (resume(Some(Some(copy))), Some(now), None, true),
        (resume(Some(None)), Some(copy), None, true),
        (resume(Some(None)), None, None, false),
        (resume(None), Some(now), None, false),
        (RewakeCause::Registration, Some(early), None, true),
        (
            RewakeCause::Registration,
            Some(now - window * 2),
            None,
            false,
        ),
        (RewakeCause::Registration, None, None, false),
        // A prompt that predates the member's newest Stage notice cannot stand in for it.
        (RewakeCause::Registration, Some(early), Some(late), false),
        (RewakeCause::Registration, Some(late), Some(late), false),
        (RewakeCause::Registration, Some(late), Some(early), true),
    ];
    let actual = cases.map(|(cause, last_prompt_at, newest_notice, _)| {
        member.last_prompt_at = last_prompt_at;
        cause.prompted(&member, newest_notice, now)
    });
    assert_eq!(actual, cases.map(|(_, _, _, expected)| expected));

    let mut withdrawn = MessageRecord::new(
        crate::ids::WorkspaceId::from_project_root(Path::new("/tmp/rimz-rewake")),
        &member,
        "stage".to_owned(),
        DeliveryGate::Done,
    )
    .with_premises(vec![Premise::NotPromptedSince {
        last_prompt_at: None,
    }]);
    withdrawn.sender = MessageSender::Harness {
        notice: HarnessNotice::Stage,
    };
    withdrawn.status = MessageStatus::Canceled;
    withdrawn.updated_at = now - window / 2;
    let mut stale = withdrawn.clone();
    stale.updated_at = now - window * 2;
    let mut unpremised = withdrawn.clone();
    unpremised.premises.clear();
    assert_eq!(
        [&withdrawn, &stale, &unpremised].map(|record| {
            RewakeCause::Registration.already_woken(&[], std::slice::from_ref(record), &member, now)
        }),
        [true, false, false]
    );
}

#[test]
fn a_flip_withdraws_the_ended_visits_messages_and_a_return_does_not_revive_them() {
    let temp = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(temp.path());
    let state = crate::StatePaths::under(id.clone(), &temp.path().join("home")).unwrap();
    let runtime = crate::RuntimePaths::for_state_under(&state, temp.path());
    let store = Store::open(state, runtime).unwrap();
    let checkout = temp.path().join("checkout");
    let first = Timestamp::from_second(1_000_000).unwrap();
    let visit = |stage: &str, started_at: Timestamp| Premise::StageVisit {
        checkout: checkout.clone(),
        stage: stage.to_owned(),
        started_at: Some(started_at),
    };
    let queue = |premise: Premise| {
        let member = crate::testkit::agent_state("codex", "sess-coder", first);
        let record =
            MessageRecord::new(id.clone(), &member, "stage".to_owned(), DeliveryGate::Done)
                .with_premises(vec![premise]);
        store.queue_message(&record, "test").unwrap();
        record.message_id
    };
    let build = queue(visit("Build", first));
    let elsewhere = queue(Premise::StageVisit {
        checkout: temp.path().join("other"),
        stage: "Build".to_owned(),
        started_at: Some(first),
    });
    let live = || {
        store
            .list_messages()
            .unwrap()
            .into_iter()
            .map(|record| record.message_id)
            .collect::<Vec<_>>()
    };
    withdraw_ended_visits(&store, "test", Some(&visit("Build", first)));
    assert_eq!(live(), [build.clone(), elsewhere.clone()]);
    withdraw_ended_visits(
        &store,
        "test",
        Some(&visit("Review", first + Duration::from_secs(5))),
    );
    assert_eq!(live(), std::slice::from_ref(&elsewhere));
    let review = queue(visit("Review", first + Duration::from_secs(5)));
    withdraw_ended_visits(
        &store,
        "test",
        Some(&visit("Build", first + Duration::from_secs(9))),
    );
    assert_eq!(live(), [elsewhere]);
    let ended = store
        .list_message_history()
        .unwrap()
        .into_iter()
        .map(|record| (record.message_id, record.status, record.last_error))
        .collect::<Vec<_>>();
    assert_eq!(
        ended,
        [
            (
                build,
                MessageStatus::Canceled,
                Some("stage Build ended before delivery".to_owned())
            ),
            (
                review,
                MessageStatus::Canceled,
                Some("stage Review ended before delivery".to_owned())
            ),
        ]
    );
}

#[test]
fn stage_notices_carry_the_channel_rule_for_a_non_leader_owner() {
    assert_eq!(
        stage_open_body(
            Some("Plan"),
            "Implement",
            "planner",
            None,
            Some("plan ready"),
            false,
            Some("planner")
        ),
        "@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from .rimz/team/blackboard.md.\n\nNote: plan ready\n\nYour report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text."
    );
    assert_eq!(
        stage_open_body(
            Some("Implement"),
            "Implement",
            "rimz",
            None,
            None,
            true,
            Some("planner")
        ),
        "The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread .rimz/team/blackboard.md and continue from where it stops.\n\nYour report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text."
    );
}
