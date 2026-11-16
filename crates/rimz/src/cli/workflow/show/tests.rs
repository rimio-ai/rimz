use super::*;

#[test]
fn a_run_whose_home_moved_after_it_was_located_shows_as_its_id_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
        "spec_dir":dir.path(),"project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},"state":"done","node":"@done",
        "created_at":now,"entered_at":now,"terminal_at":now,"visits":{},"visit_grant":0,
        "deadline_fired":false,"next_seq":1,
    }))
    .unwrap();
    let id = record.id.clone();
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(&state, &record, &guard).unwrap();
    let located = resolve::Located {
        state: state.clone(),
        paths: state.workflow(&id),
        record,
    };
    assert!(moved_to_finished(&located).unwrap().is_none());

    let moved = records::finish(&state, &id, Some(b"review done\n"), &guard).unwrap();
    assert_eq!(moved, records::Finished::Moved);
    let other = moved_to_finished(&located).unwrap().expect("moved home");
    let by_id = records::find(&state, &id).unwrap().unwrap();
    assert!(other.found.finished && by_id.finished);
    assert_eq!(other.found.paths.home, by_id.paths.home);
    assert_eq!(other.found.paths.home, state.finished_workflow(&id).home);
    assert_eq!(other.found.class, by_id.class);
    assert!(matches!(other.found.class, records::RunClass::Current(_)));

    // A moved record that does not decode still shows as a finished run.
    std::fs::write(&other.found.paths.record, "[]").unwrap();
    let other = moved_to_finished(&located).unwrap().expect("moved home");
    assert!(other.found.finished);
    assert_eq!(other.found.class, records::RunClass::NotObject);

    // A home in neither directory is an error, not the live shape.
    std::fs::remove_dir_all(&other.found.paths.home).unwrap();
    let error = moved_to_finished(&located).err().expect("gone run");
    assert_eq!(
        error.to_string(),
        format!("workflow run {id} is gone: neither its live nor its finished home holds it")
    );
}

#[test]
fn now_line_uses_the_open_case_hold_instead_of_restarting_it() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
        "spec_dir":dir.path(),"project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},"state":"parked","node":"gate",
        "created_at":now,"entered_at":now,"visits":{},"visit_grant":0,
        "deadline_fired":false,"next_seq":1,
        "wait":{"node":"gate","outcome":"hold","case":0,
            "first":now-jiff::SignedDuration::from_mins(1),"last":now,"count":1},
    }))
    .unwrap();
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='gate'\n[condition.gate]\ntimeout='365d'\nelse='pending'\non={ready='@done',pending='@wait',unknown='@wait'}\ncases=[{when=['team.stage=Done'],outcome='ready',for='2m'}]\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    std::fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let paths = state.workflow(&record.id);
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let view = View {
        record: &record,
        paths: &paths,
        spec: &spec,
        journal: &[],
        now,
        commands: resolve::RunCommands::new(&record, &globals),
    };
    let mut output = Vec::new();
    view.now_line(&state, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.contains("now gate")
            && output.contains("hold")
            && output.contains("1m remaining")
            && output.contains("team.stage=Done"),
        "{output}"
    );
}

#[test]
fn a_hold_under_unknown_reads_unknown_without_a_forecast_until_true_again() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let first = now - jiff::SignedDuration::from_mins(3);
    let mut record: RunRecord = serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
        "spec_dir":dir.path(),"project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},"state":"active","node":"gate",
        "created_at":first,"entered_at":first,"visits":{},"visit_grant":0,
        "deadline_fired":false,"next_seq":1,
        "wait":{"node":"gate","outcome":"hold","case":0,"detail":"ready",
            "unknown":"team.stage (no board)","first":first,"last":now,"count":2},
    }))
    .unwrap();
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='gate'\n[condition.gate]\ntimeout='365d'\nelse='pending'\non={ready='@done',pending='@wait',unknown='@wait'}\ncases=[{when=['team.stage=Done'],outcome='ready',for='2m'}]\n").unwrap();
    let paths = state.workflow(&record.id);
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let present = |record: &RunRecord| {
        let view = View {
            record,
            paths: &paths,
            spec: &spec,
            journal: &[],
            now,
            commands: resolve::RunCommands::new(record, &globals),
        };
        let phrase = view.presentation(None).unwrap();
        (phrase.state, phrase.forecast)
    };
    let (state_line, forecast) = present(&record);
    assert!(
        state_line.contains("unknown: team.stage (no board)")
            && !state_line.contains("holding")
            && !forecast.contains("then @done"),
        "{state_line}{forecast}"
    );
    record.wait.as_mut().unwrap().unknown = None;
    let (state_line, forecast) = present(&record);
    assert!(
        state_line.starts_with("holding ready at gate 2m of 2m")
            && forecast.starts_with(", then @done at "),
        "{state_line}{forecast}"
    );
}

#[test]
fn now_line_reads_a_sleep_a_step_enters_next_instead_of_reporting_it_slept() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
        "spec_dir":dir.path(),"project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},"state":"active","node":"build",
        "created_at":now,"entered_at":now-jiff::SignedDuration::from_mins(10),"visits":{},
        "visit_grant":0,"deadline_fired":false,"next_seq":1,
    }))
    .unwrap();
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='p.md'\nlifecycle='turn'\nthen='nap'\n[sleep.nap]\nsleep='5m'\nwhen=['team.stage=Done']\nthen='@done'\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    std::fs::write(
        dir.path().join(".rimz/team/blackboard.md"),
        "Stage: Review\n",
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let view = View {
        record: &record,
        paths: &paths,
        spec: &spec,
        journal: &[],
        now,
        commands: resolve::RunCommands::new(&record, &globals),
    };
    let mut output = Vec::new();
    view.now_line(&state, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.contains("now nap")
            && output.contains("team.stage=Review")
            && !output.contains("slept"),
        "{output}"
    );
}

const GATE: &str = "[workflow]\nscope='worktree'\nstart='gate'\n[condition.gate]\ntimeout='1h'\ncases=[{when=['ci=pending'],outcome='pending'}]\non={pending='@wait',pass='future',fail='build'}\n[test.future]\ntimeout='365d'\nshell='true'\non={pass='@done',fail='@failed'}\n[command.build]\ntimeout='365d'\nshell='true'\nthen='gate'\n";

fn output(with_you: bool) -> (String, String) {
    output_of(GATE, with_you)
}

fn output_of(definition: &str, with_you: bool) -> (String, String) {
    let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
        "spec_dir":"/spec","project_root":"/repo",
        "scope":{"kind":"worktree","path":"/repo/lane"},
        "created_at":Timestamp::UNIX_EPOCH,"entered_at":Timestamp::UNIX_EPOCH,
        "state":"active","node":"gate","visits":{"build":1},
        "visit_grant":0,"deadline_fired":false,"next_seq":3,
        "wait":{"node":"gate","outcome":"pending","first":Timestamp::UNIX_EPOCH,
            "last":Timestamp::UNIX_EPOCH,"count":2},
        "held":with_you.then(|| serde_json::json!({"entered_at":Timestamp::UNIX_EPOCH,
            "attempt":{"kind":"agent","lifecycle":"session","seq":2,"node":"build",
                "started_at":Timestamp::UNIX_EPOCH,"host":{"pid":1,"start":null},
                "run":{"run_id":rimz::ids::RunId::new(),"handle":"@pet"}}})),
    }))
    .unwrap();
    let spec = toml::from_str(definition).unwrap();
    let journal: Vec<JournalEntry> = serde_json::from_value(serde_json::json!([
        {"event":"attempt","node":"build","seq":1,"visit":1,"outcome":"ended",
            "started_at":Timestamp::UNIX_EPOCH,"ended_at":Timestamp::UNIX_EPOCH,
            "at":Timestamp::UNIX_EPOCH,"digests":{}},
        {"event":"evaluation","node":"gate","seq":2,"outcome":"pending",
            "object":{"outcome":"pending","case":0,"readings":{"ci":"pending"}},
            "started_at":now,"ended_at":now,"at":now,"digests":{}},
    ]))
    .unwrap();
    let paths = paths::StatePaths::for_project_root(&record.project_root)
        .unwrap()
        .workflow(&record.id);
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Always,
    };
    let view = View {
        record: &record,
        paths: &paths,
        spec: &spec,
        journal: &journal,
        now,
        commands: resolve::RunCommands::new(&record, &globals),
    };
    let mut headline = Vec::new();
    let mut rail = Vec::new();
    view.headline(&mut headline, None, None).unwrap();
    view.rail(&mut rail).unwrap();
    (
        String::from_utf8(headline).unwrap(),
        String::from_utf8(rail).unwrap(),
    )
}

#[test]
fn an_asking_or_held_judge_names_its_pane_and_its_running_timeout() {
    let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='pr'\n[judge.pr]\nagent='claude'\nsummary='Reviews'\nprompt-file='p.md'\ntimeout='1h'\non.ok='@done'\n").unwrap();
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let mut lines = Vec::new();
    for (state, stop) in [
        ("active", None),
        ("active", Some("ask")),
        ("active", Some("hold")),
        ("failed", Some("ask")),
    ] {
        let mut attempt = serde_json::json!({
            "kind":"judge", "lifecycle":"session", "seq":1, "node":"pr",
            "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
            "run":{"run_id":rimz::ids::RunId::new(), "handle":"@fox"},
        });
        match stop {
            Some("ask") => {
                attempt["asking"] = serde_json::json!({"since":Timestamp::UNIX_EPOCH,
                    "text":"Which base?", "kind":"question", "handle":"@fox"});
            }
            Some(_) => {
                attempt["held_since"] = serde_json::json!(Timestamp::UNIX_EPOCH);
                attempt["held_on"] = serde_json::json!({"since":Timestamp::UNIX_EPOCH,
                    "handle":"@fox", "stop":"error", "class":"paused_rate_limit"});
            }
            None => {}
        }
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),
            "workflow":"review",
            "project_root":"/repo","scope":{"kind":"worktree","path":"/repo"},
            "created_at":Timestamp::UNIX_EPOCH,"entered_at":Timestamp::UNIX_EPOCH,
            "state":state,"node":"pr","visits":{},"visit_grant":0,
            "deadline_fired":false,"next_seq":2,"attempt":attempt,
        }))
        .unwrap();
        let paths = paths::StatePaths::for_project_root(&record.project_root)
            .unwrap()
            .workflow(&record.id);
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal: &[],
            now,
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut headline = Vec::new();
        view.headline(&mut headline, None, None).unwrap();
        let text = String::from_utf8(headline).unwrap();
        lines.push((
            text.contains("the check timeout keeps running"),
            text.contains("pr is held 2m") && text.contains("@fox: rate limit"),
            text.contains("checking pr in @fox"),
        ));
    }
    assert_eq!(
        lines,
        [
            (false, false, true),
            (true, false, false),
            (true, true, false),
            (false, false, false),
        ]
    );
}

/// A held session's rest-check judge asks or is held: the headline names the judge's node
/// and agent, not the session's.
#[test]
fn a_rest_check_judges_ask_or_hold_names_the_judge_not_the_session() {
    let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='p.md'\nlifecycle='session'\nrest-check=['review']\nthen='@done'\n[judge.review]\nagent='claude'\nsummary='Reviews'\nprompt-file='p.md'\ntimeout='1h'\non.ok='@done'\n").unwrap();
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let session = serde_json::json!({
        "kind":"agent", "lifecycle":"session", "seq":1, "node":"build",
        "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
        "run":{"run_id":rimz::ids::RunId::new(), "handle":"@pet"},
    });
    let headlines = ["ask", "hold"].map(|stop| {
        let mut judge = serde_json::json!({
            "kind":"judge", "lifecycle":"session", "seq":2, "node":"review",
            "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
            "run":{"run_id":rimz::ids::RunId::new(), "handle":"@fox"},
        });
        if stop == "ask" {
            judge["asking"] = serde_json::json!({"since":Timestamp::UNIX_EPOCH,
                "text":"Which base?", "kind":"question", "handle":"@fox"});
        } else {
            judge["held_since"] = serde_json::json!(Timestamp::UNIX_EPOCH);
            judge["held_on"] = serde_json::json!({"since":Timestamp::UNIX_EPOCH,
                "stop":"error", "class":"paused_rate_limit"});
        }
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),
            "workflow":"review",
            "project_root":"/repo","scope":{"kind":"worktree","path":"/repo"},
            "created_at":Timestamp::UNIX_EPOCH,"entered_at":Timestamp::UNIX_EPOCH,
            "state":"active","node":"review","visits":{},"visit_grant":0,
            "deadline_fired":false,"next_seq":3,"attempt":judge,
            "held":{"attempt":session, "entered_at":Timestamp::UNIX_EPOCH},
        }))
        .unwrap();
        let paths = paths::StatePaths::for_project_root(&record.project_root)
            .unwrap()
            .workflow(&record.id);
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal: &[],
            now,
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut headline = Vec::new();
        view.headline(&mut headline, None, None).unwrap();
        let text = String::from_utf8(headline).unwrap();
        text.lines().nth(1).unwrap_or_default().to_owned()
    });
    let [asking, held] = &headlines;
    assert!(
        asking.contains("review is asking you") && asking.contains("· @fox (question)"),
        "{headlines:#?}"
    );
    assert!(
        held.contains("review is held 2m") && held.contains("· @fox: rate limit"),
        "{headlines:#?}"
    );
}

#[test]
fn headline_separates_state_context_and_forecast_tones() {
    let (waiting, _) = output(false);
    let (with_you, _) = output(true);
    let style = render::status::workflow(RunState::Active, false).bold();
    let clock = render::workflow_clock(
        Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(1),
        Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2),
    );
    assert_eq!(
        [
            waiting.contains(&render::paint(style, "☾ waiting at gate 2m: pending")),
            waiting.contains(&render::paint(
                render::palette::muted(),
                &format!(" · gives up {clock}, then @failed")
            )),
            with_you.contains(&render::paint(style, "⢿ build is with you")),
            with_you.contains(&render::paint(
                render::palette::body(),
                " · @pet · checking gate: pending"
            )),
        ],
        [true; 4],
        "{waiting}\n{with_you}"
    );
}

#[test]
fn vertical_rail_distinguishes_reached_current_future_and_winning_case() {
    let (_, rail) = output(false);
    let (_, held) = output(true);
    let current = render::status::workflow(RunState::Active, false).bold();
    assert_eq!(
        [
            rail.contains(&format!(
                "{} {}build",
                render::paint(render::palette::good(), "●"),
                render::palette::body().render()
            )),
            rail.contains(&format!(
                "{} {}gate",
                render::paint(current, "☾"),
                current.render()
            )),
            rail.contains(&render::paint(render::palette::muted(), "◇")),
            rail.contains(&format!(
                "│ {} ",
                render::paint(render::palette::cool().bold(), "▸")
            )),
            held.contains(&format!(
                "{} {}build",
                render::paint(current, "⢿"),
                current.render()
            )),
            held.contains(&format!(
                "{} {}gate",
                render::paint(current, "⢿"),
                current.render()
            )),
        ],
        [true; 6],
        "{rail}\n{held}"
    );
}

#[test]
fn a_terminal_case_draws_in_its_ending_tone() {
    let definition = GATE
        .replace(
            "outcome='pending'}]",
            "outcome='pending'},{when=['ci=passed'],outcome='ready'}]",
        )
        .replace("pending='@wait'", "pending='@done',ready='@done'");
    let (_, rail) = output_of(&definition, false);
    let line = |outcome: &str| {
        rail.lines()
            .find(|line| line.contains(&format!(" {outcome} @")))
            .unwrap_or_default()
            .to_owned()
    };
    let painted = |style: anstyle::Style, text: &str| render::paint(style, text);
    assert_eq!(
        (line("pending"), line("ready")),
        (
            format!(
                "│ {} {}",
                painted(render::palette::good().bold(), "▸"),
                painted(
                    render::palette::good().bold(),
                    "1  ci=pending  pending @done"
                )
            ),
            format!(
                "│   {}",
                painted(render::palette::good(), "2  ci=passed  ready @done")
            ),
        ),
    );
}
