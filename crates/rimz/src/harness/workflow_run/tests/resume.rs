//! A resume: from the first call that must run, over a fresh copy of the script, with the
//! lines below it kept.

use serde_json::json;

use super::*;

fn resumed(h: &Harness) {
    caught(|| resume(&h.state, &h.room, &h.id, None, None)).expect("the resume ran");
    h.fake.reborn();
}

fn die(h: &Harness, id: &str) {
    h.fake.end(
        id,
        Some(super::super::room::Ending::Failed(
            "the provider crashed".to_owned(),
        )),
    );
    h.advance();
}

#[test]
fn a_resume_is_refused_while_the_run_runs_once_it_ended_done_and_while_its_work_is_alive() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    let refused = |h: &Harness| {
        format!(
            "{:#}",
            resume(&h.state, &h.room, &h.id, None, None).unwrap_err()
        )
    };
    assert_eq!(
        refused(&h),
        format!("workflow run {} has not ended, so it does not resume", h.id)
    );
    h.edit_record(|record| {
        record.open.clear();
        record.ended = Some(crate::store::workflow_run::Ended {
            ending: Ending::Canceled,
            at: record.created_at,
            open: Vec::new(),
        });
    });
    assert_eq!(
        refused(&h),
        format!(
            "workflow run {} still has launch L1 alive; stop it before the run resumes",
            h.id
        )
    );
    h.fake.end("L1", completed("x"));
    h.edit_record(|record| {
        record.ended.as_mut().unwrap().ending = Ending::Done { result: json!(1) };
    });
    assert_eq!(
        refused(&h),
        format!("workflow run {} ended done, so it does not resume", h.id)
    );
}

#[test]
fn a_resume_past_an_edit_runs_the_edited_call_and_keeps_the_answers_before_it() {
    let h = Harness::new();
    let body = |prompt: &str| {
        format!(
            "const one = await shell({{ command: 'echo one' }})\n\
             const two = await agent({{ profile: 'a', prompt: '{prompt}' }})\n\
             const three = await agent({{ profile: 'b', prompt: 'p' }})\n\
             return [one.stdout, two.report, three.report]"
        )
    };
    let dir = h.workflow("flow", &body("v1"));
    h.start(Origin::Path(dir.clone()));
    h.run_hosts();
    h.fake.end("L1", completed("two"));
    h.advance();
    die(&h, "L2");
    assert_eq!(h.failed().1, Some(3));
    std::fs::write(dir.join("workflow.js"), format!("{META}{}", body("v2"))).unwrap();
    resumed(&h);
    assert!(
        h.fake.take_hosts().is_empty(),
        "the command keeps its answer"
    );
    let prompts: Vec<String> = h
        .fake
        .live()
        .iter()
        .map(|launch| launch.object["prompt"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(prompts, ["v2"]);
    drive(&h, world);
    assert_eq!(h.done(), json!(["one\n", "L3 done", "L4 done"]));
    assert_eq!(h.record().files, 2);
    assert_eq!(h.record().launches, 4, "launches made are kept");
}

#[test]
fn a_resume_past_an_open_step_starts_it_again() {
    let h = Harness::new();
    let mut input = h.input(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    input.max_launches = Some(2);
    h.start_with(input);
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    assert_eq!(h.fake.stopped(), ["L1"]);
    resumed(&h);
    assert_eq!(
        h.record().bounds.launches,
        2,
        "a resume given no cap keeps the run's"
    );
    drive(&h, world);
    assert_eq!(h.done(), json!("L2 done"));
}

#[test]
fn a_resume_past_a_caught_failure_hands_the_saved_failure_back() {
    let h = Harness::new();
    h.start(script(
        "let first\ntry { first = (await agent({ profile: 'a', prompt: 'p' })).report } catch (e) { first = e.cause }\n\
         const second = await agent({ profile: 'b', prompt: 'p' })\nreturn [first, second.report]",
    ));
    die(&h, "L1");
    die(&h, "L2");
    assert_eq!(h.failed().1, Some(2));
    resumed(&h);
    let live: Vec<String> = h
        .fake
        .live()
        .into_iter()
        .map(|launch| launch.object["profile"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(live, ["b"]);
    drive(&h, world);
    assert_eq!(h.done(), json!(["died", "L3 done"]));
}

#[test]
fn a_resume_past_a_null_in_a_pipeline_keeps_the_null() {
    let h = Harness::new();
    h.start(script(
        "const lanes = await pipeline(['x', 'y'], lane => agent({ profile: lane, prompt: lane }))\n\
         const last = await agent({ profile: 'z', prompt: 'z' })\n\
         return [lanes.map(lane => lane && lane.report), last.report]",
    ));
    die(&h, "L1");
    h.fake.end("L2", completed("y done"));
    h.advance();
    die(&h, "L3");
    assert_eq!(h.failed().1, Some(3));
    resumed(&h);
    drive(&h, world);
    assert_eq!(h.done(), json!([[null, "y done"], "L4 done"]));
}

#[test]
fn a_resume_past_an_edit_inside_a_workflow_call_whose_timeout_was_caught_runs_the_call_again() {
    let h = Harness::new();
    let dir = h.workflow(
        "flow",
        "let called\ntry { called = await workflow({ source: 'path', value: 'child', timeout: '1m' }) } catch (e) { called = e.cause }\n\
         const last = await agent({ profile: 'z', prompt: 'z' })\nreturn [called, last.report]",
    );
    let child = |command: &str| {
        format!(
            "{META}await shell({{ command: '{command}' }})\nreturn (await agent({{ profile: 'c', prompt: 'c' }})).report"
        )
    };
    std::fs::create_dir_all(dir.join("child")).unwrap();
    std::fs::write(dir.join("child/workflow.js"), child("echo v1")).unwrap();
    h.start(Origin::Path(dir.clone()));
    h.run_hosts();
    assert_eq!(h.fake.live().len(), 1);
    h.fake.later(jiff::SignedDuration::from_mins(2));
    h.advance();
    die(&h, "L2");
    assert_eq!(h.failed().1, Some(4));
    std::fs::write(dir.join("child/workflow.js"), child("echo v2")).unwrap();
    resumed(&h);
    drive(&h, world);
    assert_eq!(h.done(), json!(["L3 done", "L4 done"]));
    let commands: Vec<String> = h
        .lines()
        .into_iter()
        .filter(|line| line.block == "shell")
        .map(|line| line.summary)
        .collect();
    assert_eq!(commands, ["echo v2"]);
}

#[test]
fn a_resume_with_a_workflow_call_open_at_the_end_keeps_its_finished_childrens_answers() {
    let h = Harness::new();
    h.start(script(
        "return await workflow({ source: 'script', value: \"await shell({ command: 'echo a' })\\nreturn (await agent({ profile: 'c', prompt: 'c' })).report\" })",
    ));
    h.run_hosts();
    assert_eq!(h.fake.live().len(), 1);
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    assert_eq!(h.record().ended.unwrap().open, [1, 3]);
    resumed(&h);
    assert!(
        h.fake.take_hosts().is_empty(),
        "the command under the call keeps its answer"
    );
    drive(&h, world);
    assert_eq!(h.done(), json!("L2 done"));
}

#[test]
fn a_resume_after_a_crash_between_sealing_and_its_publish_seals_again() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    die(&h, "L1");
    h.fake.crash_at("now", h.fake.calls("now") + 1, false);
    assert!(caught(|| resume(&h.state, &h.room, &h.id, None, None)).is_none());
    h.fake.reborn();
    let paths = h.state.workflow_run(&h.id);
    let generations = || {
        let mut names: Vec<String> = std::fs::read_dir(paths.home.join("files"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    assert!(!paths.files(2).exists() && h.record().files == 1);
    assert_eq!(
        generations().len(),
        2,
        "the run's copy and the staging the crash left"
    );
    resumed(&h);
    assert_eq!(h.record().files, 2);
    assert_eq!(generations(), ["2"]);
    drive(&h, world);
    assert_eq!(h.done(), json!("L2 done"));
}

#[test]
fn a_resume_runs_the_workflow_a_call_past_the_cut_names_now() {
    let h = Harness::new();
    let old = h.workflow("old", "return 'old'");
    let new = h.workflow("new", "return 'new'");
    h.fake.install("old", &old);
    h.fake.install("new", &new);
    std::fs::write(h.checkout.join("mode"), "old").unwrap();
    h.start(script(
        "const a = agent({ profile: 'a', prompt: 'p' })\n\
         const s = await shell({ command: 'cat mode' })\n\
         const w = await workflow({ source: 'name', value: s.stdout })\n\
         return [w, (await a).report]",
    ));
    h.run_hosts();
    assert_eq!(h.record().open.keys().copied().collect::<Vec<_>>(), [1]);
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    std::fs::write(h.checkout.join("mode"), "new").unwrap();
    resumed(&h);
    drive(&h, world);
    assert_eq!(h.done(), json!(["new", "L2 done"]));
}

#[test]
fn a_command_whose_stop_has_not_landed_keeps_it_owed_and_refuses_a_resume() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo out' })).stdout",
    ));
    let (_, host) = h.fake.take_hosts().remove(0);
    h.fake.linger(&host);
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    assert_eq!(
        h.record().owed,
        [crate::store::workflow_run::Stop::Process {
            process: host.clone()
        }]
    );
    let refused = resume(&h.state, &h.room, &h.id, None, None).unwrap_err();
    assert!(
        format!("{refused:#}").contains("still has a command alive"),
        "{refused:#}"
    );
    h.fake.kill_process(&host);
    h.advance();
    assert!(h.record().owed.is_empty());
}

#[test]
fn a_resume_whose_cleanup_fails_after_its_publish_returns_its_status_and_advances() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 0500, so the fixture cannot make the cleanup fail.
    }
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    die(&h, "L1");
    let held = h
        .state
        .workflow_run(&h.id)
        .home
        .join("files/.9.left.staging/held");
    std::fs::create_dir_all(&held).unwrap();
    std::fs::write(held.join("file"), "").unwrap();
    let mode = |mode: u32| {
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(0o500);
    let resumed = resume(&h.state, &h.room, &h.id, None, None);
    mode(0o700);
    let status = resumed.expect("the resume published, so it returns its status");
    assert_eq!(status.ended, None);
    let live: Vec<String> = h.fake.live().into_iter().map(|launch| launch.id).collect();
    assert_eq!(live, ["L2"]);
}

#[test]
fn a_resume_that_cannot_take_the_lock_leaves_no_staging() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    die(&h, "L1");
    let lock = h.state.workspace_lock.clone();
    h.fake.hook("now", move || {
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        true
    });
    let refused = resume(&h.state, &h.room, &h.id, None, None);
    std::fs::remove_dir(&h.state.workspace_lock).unwrap();
    assert!(refused.is_err());
    let names: Vec<String> = std::fs::read_dir(h.state.workflow_run(&h.id).home.join("files"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["1"]);
}

#[test]
fn a_resume_removes_the_large_results_a_crashed_resume_left_in_its_log_generation() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    die(&h, "L1");
    let results = h.state.workflow_run(&h.id).results();
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("2.7.json"), "\"left\"").unwrap();
    resumed(&h);
    assert!(!results.join("2.7.json").exists());
}
