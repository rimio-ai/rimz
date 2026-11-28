//! The four kinds of loop task: what `loop add` stores and refuses for each, and how a
//! workflow, agent or team fire launches and ends.

use super::*;

/// A recorded root with a live sidebar, so a scheduled `loop run` opens no room.
fn live_room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "kinds",
    );
    env
}

/// A deployed workflow `name` whose one shell step runs `shell`, in the given scope.
fn deploy_workflow(env: &Env, name: &str, scope: &str, shell: &str) {
    let dir = env.rimz_home().join("workflows").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("workflow.toml"),
        format!(
            "[workflow]\nscope='{scope}'\nstart='step'\n[test.step]\ntimeout='5m'\nshell={shell:?}\non={{pass='@done',fail='@failed'}}\n"
        ),
    )
    .unwrap();
}

/// The `worker` agent profile and the `forge` team, and the `smoke` (worktree) and `whole`
/// (project) workflows.
fn kinds_fixture(env: &Env) {
    write_team_signal_config(env);
    deploy_workflow(env, "smoke", "worktree", "true");
    deploy_workflow(env, "whole", "project", "true");
}

fn stored_rows(env: &Env) -> toml::Table {
    let text = std::fs::read_to_string(loop_config_path(env)).unwrap();
    toml::from_str::<toml::Table>(&text).unwrap()["tasks"]
        .as_table()
        .unwrap()
        .clone()
}

#[test]
fn loop_add_stores_each_kind_as_its_target_and_tail_unchanged() {
    let env = live_room();
    kinds_fixture(&env);
    let adds: [&[&str]; 4] = [
        &[
            "sh-task",
            "--shell",
            "--every",
            "1h",
            "--",
            "sh",
            "-c",
            "echo 'a b'",
        ],
        &["wf-task", "--workflow", "smoke", "--every", "1h"],
        &[
            "agent-task",
            "--agent",
            "worker",
            "--every",
            "1h",
            "--",
            "-p",
            "fix it",
            "--timeout",
            "5m",
        ],
        &[
            "team-task",
            "--team",
            "forge#lane",
            "--every",
            "1h",
            "--",
            "ship it",
        ],
    ];
    for add in adds {
        loop_ok(&env, &[&["loop", "add"][..], add].concat());
    }
    let rows = stored_rows(&env);
    let shape = |name: &str| {
        let row = rows
            .get(name)
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("{name}: {rows:?}"));
        (
            row.get("kind")
                .and_then(|kind| kind.as_str())
                .map(str::to_owned),
            row.get("target")
                .and_then(|target| target.as_str())
                .map(str::to_owned),
            // An empty tail writes no `args`.
            row.get("args")
                .cloned()
                .unwrap_or(toml::Value::Array(Vec::new())),
            row.get("timeout").is_some(),
        )
    };
    let words = |words: &[&str]| toml::Value::try_from(words).unwrap();
    assert_eq!(
        shape("sh-task"),
        (
            Some("shell".to_owned()),
            None,
            words(&["sh", "-c", "echo 'a b'"]),
            true
        )
    );
    assert_eq!(
        shape("wf-task"),
        (
            Some("workflow".to_owned()),
            Some("smoke".to_owned()),
            words(&[]),
            false
        )
    );
    assert_eq!(
        shape("agent-task"),
        (
            Some("agent".to_owned()),
            Some("worker".to_owned()),
            words(&["-p", "fix it", "--timeout", "5m"]),
            false
        )
    );
    assert_eq!(
        shape("team-task"),
        (
            Some("team".to_owned()),
            Some("forge#lane".to_owned()),
            words(&["ship it"]),
            false
        )
    );
    let listed: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "agent-task", "--json"])).unwrap();
    assert_eq!(listed["entry"]["kind"], "agent", "{listed}");
}

/// `loop add` resolves an agent target in the lane its fire would launch into, as the launch
/// does: a bare role of the team that holds the tail's `--channel` lane is that team's role,
/// and outside the lane it stays unknown.
#[test]
fn loop_add_resolves_a_bare_role_in_its_team_lane() {
    let env = live_room();
    kinds_fixture(&env);
    env.install_agent_hooks("claude");
    crate::channel::register_idle_lane_agent(&env, "sess-forge-coder", "forge", Some("forge"));
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "inlane",
            "--agent",
            "reviewer",
            "--every",
            "1h",
            "--",
            "--channel",
            "forge",
            "go",
        ],
    );
    let outside = env
        .rimz()
        .args([
            "loop", "add", "outside", "--agent", "reviewer", "--every", "1h", "--", "go",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&outside.stderr);
    assert!(
        !outside.status.success()
            && stderr.contains("no team, profile, or command named `reviewer`"),
        "{stderr}"
    );

    // A --subagent-profile tail names the subagent profile even where the lane's team has a
    // role of that name, and it takes one profile, never a team.
    crate::common::write_definition(
        &env,
        "subagents",
        "reviewer",
        "description: Reviewer\nagent: claude\ntools: []",
        "",
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "sub-inlane",
            "--agent",
            "reviewer",
            "--every",
            "1h",
            "--",
            "--subagent-profile",
            "--channel",
            "forge",
            "go",
        ],
    );
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "sub-team",
            "--agent",
            "forge",
            "--every",
            "1h",
            "--",
            "--subagent-profile",
            "go",
        ],
    );
    assert!(
        error.contains(
            "loop task `sub-team`: --agent `forge`: --subagent-profile takes one profile name defined under [subagents.profiles], and `forge` is a team\n"
        ),
        "{error}"
    );
}

/// Each tail or flag a task of a launch kind cannot carry is refused at add, naming the task,
/// the kind flag and its target, and the fix; nothing is written.
#[test]
fn loop_add_refuses_what_a_launch_task_cannot_carry() {
    let env = live_room();
    kinds_fixture(&env);
    let cases: [(&[&str], &str); 34] = [
        (
            &["--workflow", "smoke", "--each-worktree", "--", "-w", "x"],
            "loop task `t`: --workflow `smoke`: -w in the tail places the run, and --each-worktree places each fire",
        ),
        (
            &["--workflow", "whole", "--", "-w", "x"],
            "loop task `t`: --workflow `whole`: whole has scope = project, so it runs at the project root; remove -w from the tail",
        ),
        (
            &["--agent", "worker", "--", "-p", "go", "--bg"],
            "loop task `t`: --agent `worker`: `-p --bg` is retired: `--stay false` starts the run that closes at its turn end",
        ),
        (
            &["--workflow", "smoke", "--", "--bogus"],
            "loop task `t`: --workflow `smoke`: the tail does not parse as `rimz workflow run`:",
        ),
        (
            &["--agent", "list"],
            "loop task `t`: --agent `list`: the target reads as a `rimz agents` verb or address, not a launch spec",
        ),
        (
            &["--agent", "worker", "--", "--tmux", "go"],
            "loop task `t`: --agent `worker`: the tail sets the global flag --tmux; a fire launches in its own checkout's room",
        ),
        (&["--agent", "nobody"], "loop task `t`: --agent `nobody`: "),
        (&["--team", "nobody"], "loop task `t`: --team `nobody`: "),
        (
            &["--workflow", "nobody"],
            "loop task `t`: --workflow `nobody`: ",
        ),
        (
            &["--workflow", "./smoke"],
            "loop task `t`: --workflow `./smoke`: `./smoke` is a path; --workflow takes a deployed workflow's name",
        ),
        (
            &["--workflow", "."],
            "loop task `t`: --workflow `.`: `.` is a path; --workflow takes a deployed workflow's name",
        ),
        (
            &["--workflow", ".."],
            "loop task `t`: --workflow `..`: `..` is a path; --workflow takes a deployed workflow's name",
        ),
        (
            &["--agent", "worker", "--", "--detach", "go"],
            "loop task `t`: --agent `worker`: --detach drops the report a launch owes; remove it from the tail",
        ),
        (
            &["--agent", "worker", "--", "-p", "--stdin"],
            "loop task `t`: --agent `worker`: --stdin reads a fire's stdin, which is empty; remove it from the tail",
        ),
        (
            &["--agent", "worker", "--", "--resume"],
            "loop task `t`: --agent `worker`: a bare --resume opens a picker no fire can answer; give the session id; remove it from the tail",
        ),
        (
            &["--team", "forge", "--", "--detach", "go"],
            "loop task `t`: --team `forge`: --detach drops the report a launch owes; remove it from the tail",
        ),
        (
            &["--workflow", "smoke", "--", "--at", "x"],
            "loop task `t`: --workflow `smoke`: --at starts a run by hand at a node, and a task's fire runs from the workflow's start; remove --at\n",
        ),
        (
            &[
                "--workflow",
                "smoke",
                "--",
                "--input",
                r#"{"outcome":"red"}"#,
            ],
            "loop task `t`: --workflow `smoke`: --input hands a run's start an input by hand, and a task's fire gives none; remove --input\n",
        ),
        (
            &["--workflow", "smoke", "--", "--once"],
            "loop task `t`: --workflow `smoke`: --once tries one node by hand, and a task's fire runs the whole workflow; remove --once\n",
        ),
        (
            &["--workflow", "smoke", "--", "--detach"],
            "loop task `t`: --workflow `smoke`: --detach drops the report a task's run owes it; remove --detach\n",
        ),
        (
            &["--workflow", "smoke", "--timeout", "1h"],
            "loop task `t`: --workflow `smoke`: --timeout bounds a shell task's command; remove it: a workflow task's run is bounded by its workflow's deadline\n",
        ),
        (
            &["--agent", "worker", "--timeout", "1h"],
            "loop task `t`: --agent `worker`: --timeout bounds a shell task's command; remove it: an agent task takes `--timeout` in its arguments after `--`\n",
        ),
        (
            &["--team", "forge", "--timeout", "1h"],
            "loop task `t`: --team `forge`: --timeout bounds a shell task's command; remove it: a team task takes `--timeout` in its arguments after `--`\n",
        ),
        (
            &[
                "--agent",
                "worker",
                "--each-worktree",
                "--",
                "-w",
                "x",
                "go",
            ],
            "loop task `t`: --agent `worker`: -w in the tail places the launch, and --each-worktree places each fire",
        ),
        (
            &["--team", "forge#lane", "--each-worktree"],
            "loop task `t`: --team `forge#lane`: a `#lane` on the team places the launch, and --each-worktree places each fire",
        ),
        (
            &["--team", "forge#"],
            "loop task `t`: --team `forge#`: expected a worktree or lane after `#` in `forge#`",
        ),
        (
            &["--team", "forge#lane", "--", "-w", "x"],
            "loop task `t`: --team `forge#lane`: worktree given twice; use either `team#worktree` or `-w/--worktree`",
        ),
        (
            &["--team", "forge#lane", "--", "--channel", "x"],
            "loop task `t`: --team `forge#lane`: --channel cannot be used with team#worktree addressing",
        ),
        (
            &["--workflow", "whole", "--each-worktree"],
            "loop task `t`: --workflow `whole`: whole has scope = project, so it runs at the project root; remove --each-worktree",
        ),
        (
            &["-w", "deps", "--agent", "worker", "--", "-w", "other", "go"],
            "loop task `t`: --agent `worker`: -w `deps` and the launch's worktree `other` differ; keep one\n",
        ),
        (
            &["-w", "deps", "--team", "forge", "--", "-w"],
            "loop task `t`: --team `forge`: -w `deps` and the launch's worktree (a generated name) differ; keep one\n",
        ),
        (
            &["-w", "deps", "--workflow", "whole"],
            "loop task `t`: --workflow `whole`: whole has scope = project, so it runs at the project root; remove -w\n",
        ),
        (
            &[
                "-w",
                "deps",
                "--agent",
                "worker",
                "--",
                "--channel",
                "x",
                "go",
            ],
            "loop task `t`: --agent `worker`: the tail does not parse as `rimz agents`:",
        ),
        (
            &["--", "rimz", "teams", "launch", "forge", "go"],
            "loop task `t`: the command is a `rimz` launch; declare it with --team <TEAM[#LANE]> and put the rest of its arguments after `--`",
        ),
    ];
    for (flags, expected) in cases {
        let mut argv = vec!["loop", "add", "t", "--every", "1h"];
        // `--each-worktree` fans out on a clock already; the rest only need a trigger.
        argv.extend(flags);
        let (_, error) = loop_fail(&env, &argv);
        assert!(error.contains(expected), "{flags:?}: {error}");
    }
    assert!(!loop_config_path(&env).exists(), "nothing was written");
}

/// A machine team whose seat allows a subagent that fails to load in the trusted project is
/// refused at add with the seat's refusal, as a project team is; a healthy machine team adds.
#[test]
fn loop_add_refuses_a_machine_team_whose_seat_allows_a_failed_project_subagent() {
    let env = live_room();
    kinds_fixture(&env);
    for (namespace, name, fields, body) in [
        (
            "subagents",
            "kid",
            "description: Kid\nagent: claude\ntools: []",
            "",
        ),
        (
            "agents",
            "caller",
            "description: Caller\nagent: claude\ntools: []\nsubagents: [kid]",
            "",
        ),
        (
            "teams",
            "crew",
            "leader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: caller, owns: [Build]}",
            "Build.",
        ),
    ] {
        crate::common::write_definition(&env, namespace, name, fields, body);
    }
    env.write_config(
        &env.project_root,
        "[subagents.profiles.kid]\nagent = 'claude'\n",
    );
    loop_ok(&env, &["trust", "grant"]);
    let (_, error) = loop_fail(
        &env,
        &["loop", "add", "t", "--every", "1h", "--team", "crew"],
    );
    assert_eq!(
        error,
        format!(
            "error: loop task `t`: --team `crew`: {}: allows subagent 'kid', which failed to load\n  \
             caused by: {}: [subagents.profiles.kid]: has no non-empty `description:`\n",
            env.agents_home().join("teams/crew.md").display(),
            env.project_root.join(".rimz/config.toml").display(),
        )
    );
    assert!(!loop_config_path(&env).exists(), "nothing was written");
    loop_ok(
        &env,
        &["loop", "add", "f", "--every", "1h", "--team", "forge"],
    );
}

/// A machine team whose layout cell resolves in no machine table fails on its own name, in the
/// trusted project that defines the cell too: `--team` refuses it with its file's error and
/// writes nothing, also when the built-in `peer` or an executable on PATH shares its name, and a
/// healthy agent and team still add.
#[test]
fn loop_add_refuses_a_machine_team_whose_layout_cell_does_not_resolve() {
    let env = live_room();
    kinds_fixture(&env);
    let crew = crate::common::write_definition(
        &env,
        "teams",
        "crew",
        "leader: lead\nstages: [Build]\nlayout: \"lead,proj\"\nroles:\n  - {role: lead, agent: worker, owns: [Build]}",
        "Build.",
    );
    env.write_config(
        &env.project_root,
        "[profiles.proj]\ndescription = 'Proj'\nagent = 'codex'\n",
    );
    loop_ok(&env, &["trust", "grant"]);
    let (_, error) = loop_fail(
        &env,
        &["loop", "add", "t", "--every", "1h", "--team", "crew"],
    );
    assert_eq!(
        error,
        format!(
            "error: loop task `t`: --team `crew`: {}: team `crew` layout token `proj` is not a \
             declared role or valid roleless cell\n",
            crew.display(),
        )
    );
    for name in ["peer", "sh"] {
        let file = crate::common::write_definition(
            &env,
            "teams",
            name,
            "leader: lead\nstages: [Build]\nlayout: \"lead,ghost\"\nroles:\n  - {role: lead, agent: worker, owns: [Build]}",
            "Build.",
        );
        let (_, error) = loop_fail(&env, &["loop", "add", "t", "--every", "1h", "--team", name]);
        assert_eq!(
            error,
            format!(
                "error: loop task `t`: --team `{name}`: {}: team `{name}` layout token `ghost` \
                 is not a declared role or valid roleless cell\n",
                file.display(),
            )
        );
    }
    assert!(!loop_config_path(&env).exists(), "nothing was written");
    loop_ok(
        &env,
        &["loop", "add", "a", "--every", "1h", "--agent", "worker"],
    );
    loop_ok(
        &env,
        &["loop", "add", "f", "--every", "1h", "--team", "forge"],
    );
}

/// While a definition-set failure stands (here a home team named like a home subagent profile),
/// `loop add --team` refuses every team with the set's error and writes nothing, matching launches.
#[test]
fn loop_add_team_refuses_while_definition_set_has_failed() {
    let env = live_room();
    kinds_fixture(&env);
    crate::common::write_definition(
        &env,
        "teams",
        "crew",
        "leader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}",
        "Build.",
    );
    crate::common::write_definition(
        &env,
        "subagents",
        "crew",
        "description: Colliding child\nagent: codex\ntools: [Bash]",
        "Help.",
    );
    for team in ["crew", "forge"] {
        let (_, error) = loop_fail(&env, &["loop", "add", "t", "--every", "1h", "--team", team]);
        assert_eq!(
            error,
            format!(
                "error: loop task `t`: --team `{team}`: {}: team name `crew` is reserved for an \
                 inline profile/command cell; choose another team name\n",
                env.agents_home().display(),
            )
        );
    }
    assert!(!loop_config_path(&env).exists(), "nothing was written");
}

/// A launch child stand-in behind `RIMZ_BIN`: an `agents`, `teams` or `workflow` launch writes
/// what it was started with to `seen` and exits with the code in `code`, printing `boom`;
/// every other `rimz` helper runs the real binary. Every call's argv is appended to
/// `seen.calls`.
#[cfg(unix)]
fn launch_stand_in(env: &Env) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let seen = env.home_root.join("seen");
    let code = env.home_root.join("code");
    let wrapper = env.home_root.join("rimz-launch");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$*\" >> {seen:?}.calls\n\
             case \"$1\" in agents|teams|workflow) ;; *) exec {real:?} \"$@\";; esac\n\
             {{ echo \"argv=$*\"; echo \"cwd=$(pwd -P)\"; echo \"fire=${{RIMZ_LOOP_FIRE-}}\"; \
             echo \"channel=${{RIMZ_CHANNEL-unset}}\"; echo \"agent=${{RIMZ_AGENT_ID-unset}}\"; \
             echo \"stdin=$(cat)\"; }} > {seen:?}\nif [ \"$(cat {code:?})\" = hold ]; then echo $$ > {seen:?}.pid; exec sleep 347; fi\necho boom >&2\nexit \"$(cat {code:?})\"\n",
            real = env.rimz_bin().display().to_string(),
            seen = seen.display().to_string(),
            code = code.display().to_string(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    (wrapper, seen, code)
}

/// An agent or team fire records its launch child's exit at the fire: 0 is `launched` and
/// resets strikes, 125 a neutral budget skip, anything else an `error` strike carrying the
/// child's own error text. The child runs in the checkout with the fire's id and without the
/// room's caller identity or any stdin.
#[cfg(unix)]
#[test]
fn an_agent_or_team_fire_records_its_childs_exit_at_the_fire() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    loop_ok(
        &env,
        &[
            "loop", "add", "solo", "--agent", "worker", "--every", "1h", "--", "go",
        ],
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "crew", "--team", "forge", "--every", "1h", "--", "go",
        ],
    );
    let fire = |task: &str, exit: &str| {
        std::fs::write(&code, exit).unwrap();
        let before = read_loop_run_records(&env).len();
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .env("RIMZ_CHANNEL", "elsewhere")
            .args(["loop", "run", task])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let records = read_loop_run_records(&env);
        assert_eq!(records.len(), before + 1, "one row per fire: {records:?}");
        records.last().unwrap().clone()
    };
    for task in ["solo", "crew"] {
        let failed = fire(task, "7");
        assert_eq!(failed.result, LoopRunResult::Errored, "{failed:?}");
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("boom")),
            "{failed:?}"
        );
        assert_eq!(
            read_loop_strikes(&env).get(&machine_task_key(task)),
            Some(&1)
        );
        let skipped = fire(task, "125");
        assert_eq!(skipped.result, LoopRunResult::BudgetSkipped, "{skipped:?}");
        assert_eq!(
            read_loop_strikes(&env).get(&machine_task_key(task)),
            Some(&1)
        );
        let launched = fire(task, "0");
        assert_eq!(launched.result, LoopRunResult::Launched, "{launched:?}");
        assert_eq!(read_loop_strikes(&env).get(&machine_task_key(task)), None);

        let seen = std::fs::read_to_string(&seen).unwrap();
        let line = |key: &str| {
            seen.lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .unwrap()
                .to_owned()
        };
        let argv = if task == "solo" {
            "agents worker go"
        } else {
            "teams launch forge go"
        };
        assert_eq!(line("argv"), argv);
        assert_eq!(
            std::path::PathBuf::from(line("cwd")),
            canonical(&env.project_root)
        );
        let fire: rimz::ids::FireId = line("fire").parse().unwrap();
        assert_eq!(fire.task(), task_id(&env, task));
        assert_eq!(Some(&fire), launched.fire.as_ref());
        assert_eq!(
            line("channel"),
            "unset",
            "the room's channel is not the launch's"
        );
        assert_eq!(line("agent"), "unset");
        assert_eq!(line("stdin"), "");
    }
}

/// A workflow fire's run is the task's own and its row comes from the run's collection; a
/// hand-started run of the same workflow in the checkout makes the fire a neutral `overlapped`.
#[cfg(unix)]
#[test]
fn a_workflow_fire_publishes_the_tasks_run_and_a_hand_started_run_overlaps_it() {
    let env = live_room();
    let release = env.home_root.join("release");
    deploy_workflow(
        &env,
        "smoke",
        "worktree",
        &format!(
            "while [ ! -e {:?} ]; do sleep 0.05; done",
            release.display().to_string()
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "wf", "--workflow", "smoke", "--every", "1h"],
    );
    let before = run_mark(&env, "wf");
    loop_ok(&env, &["loop", "run", "wf"]);
    std::fs::write(&release, "").unwrap();
    let report = wait_for_loop_report_after(&env, "wf", before);
    assert_eq!(report.result, LoopRunResult::Completed, "{report:?}");
    // The task's own runs are matched by its fire id, so the run is listed as the task's.
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "wf", "--json"])).unwrap();
    let run = report.run_id.as_deref().unwrap();
    assert!(
        shown["workflow_runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == run),
        "{shown}"
    );
    assert!(report.fire.is_some(), "{report:?}");

    std::fs::remove_file(&release).unwrap();
    let hand = env
        .rimz()
        .args(["workflow", "run", "smoke"])
        .output()
        .unwrap();
    assert!(hand.status.success(), "{hand:?}");
    let rows = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "wf"]);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), rows + 1, "{records:?}");
    let overlapped = records.last().unwrap();
    assert_eq!(
        overlapped.result,
        LoopRunResult::Overlapped,
        "{overlapped:?}"
    );
    assert_eq!(read_loop_strikes(&env).get(&machine_task_key("wf")), None);
    std::fs::write(&release, "").unwrap();
}

/// A hand-started run through one link holds a task of another link to the same directory, and
/// the fire names it as started by hand.
#[cfg(unix)]
#[test]
fn a_workflow_fire_sees_a_hand_started_run_through_another_link() {
    let env = live_room();
    let release = env.home_root.join("release");
    deploy_workflow(
        &env,
        "first",
        "worktree",
        &format!(
            "while [ ! -e {:?} ]; do sleep 0.05; done",
            release.display().to_string()
        ),
    );
    let workflows = env.rimz_home().join("workflows");
    std::os::unix::fs::symlink(workflows.join("first"), workflows.join("second")).unwrap();
    loop_ok(
        &env,
        &["loop", "add", "wf", "--workflow", "second", "--every", "1h"],
    );
    let hand = env
        .rimz()
        .args(["workflow", "run", "first"])
        .output()
        .unwrap();
    assert!(hand.status.success(), "{hand:?}");
    let rows = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "wf"]);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), rows + 1, "{records:?}");
    let overlapped = records.last().unwrap();
    assert_eq!(
        overlapped.result,
        LoopRunResult::Overlapped,
        "{overlapped:?}"
    );
    let detail = format!("{overlapped:?}");
    assert!(
        detail.contains("of `first` was started by hand — skipped"),
        "{detail}"
    );
    std::fs::write(&release, "").unwrap();
}

/// A workflow task's steps read the fire's evidence as `RIMZ_LOOP_TRIGGER`, never the run's
/// `--prompt`, and still get it when the task passes no prompt.
#[cfg(unix)]
#[test]
fn a_workflow_fires_steps_read_its_evidence_as_the_trigger() {
    let env = live_room();
    let seen = env.home_root.join("seen");
    deploy_workflow(
        &env,
        "smoke",
        "worktree",
        &format!(
            r#"printf %s "$RIMZ_LOOP_TRIGGER" > {:?}/"$RIMZ_LOOP_TASK""#,
            seen.display().to_string()
        ),
    );
    std::fs::create_dir_all(&seen).unwrap();
    let signal = r#"{"name":"deploy.done","source":"cli","payload":{"env":"prod"}}"#;
    for (task, tail) in [
        ("prompted", &["--", "--prompt", "fix it"][..]),
        ("bare", &[]),
    ] {
        let add = [
            &["loop", "add", task, "--workflow", "smoke"][..],
            &["--signal", "deploy.done", "--repeat"],
            tail,
        ]
        .concat();
        loop_ok(&env, &add);
        let before = run_mark(&env, task);
        loop_ok(&env, &["loop", "run", task, "--signal-json", signal]);
        let report = wait_for_loop_report_after(&env, task, before);
        assert_eq!(report.result, LoopRunResult::Completed, "{report:?}");
        assert_eq!(
            std::fs::read_to_string(seen.join(task)).unwrap(),
            format!("Rule `{task}` fired here. deploy.done · env: prod"),
            "task {task}"
        );
    }
}

/// A fire typed in a pane of another room launches into the task's room: the launch child
/// drops the firing shell's room pin, so it resolves the room from the fire's checkout, as a
/// fire from the timer, which carries no pin, does.
#[cfg(unix)]
#[test]
fn a_fire_from_a_shell_pinned_to_another_room_launches_in_the_tasks_room() {
    let env = live_room();
    let release = env.home_root.join("release");
    deploy_workflow(
        &env,
        "smoke",
        "worktree",
        &format!(
            "while [ ! -e {:?} ]; do sleep 0.05; done",
            release.display().to_string()
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "wf", "--workflow", "smoke", "--every", "1h"],
    );
    let other = env.home_root.join("other");
    env.record(&other);
    let other = canonical(&other);
    let before = run_mark(&env, "wf");
    let output = env
        .rimz()
        .env("RIMZ_PROJECT_ROOT", &other)
        .env(
            "RIMZ_WORKSPACE_ID",
            WorkspaceId::from_project_root(&other).as_str(),
        )
        .args(["loop", "fire", "wf"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let live_runs = |root: &Path| env.state_path_for(root).workflow_runs().unwrap().len();
    // The fire returns once its launch child exits, so the held run is published by now.
    let (tasks, pinned) = (live_runs(&env.project_root), live_runs(&other));
    std::fs::write(&release, "").unwrap();
    assert_eq!(
        (tasks, pinned),
        (1, 0),
        "live runs in the task's room and the pinned one: {output:?}"
    );
    let report = wait_for_loop_report_after(&env, "wf", before);
    assert_eq!(report.result, LoopRunResult::Completed, "{report:?}");
}

/// A workflow fire whose run the trust read refuses at its start (an agent step on an account
/// without hooks) creates no run: an `error` strike carrying the refusal and its fix. A turn
/// step is read as the supervised leader; a session step of a task's run as the layout recipe's
/// leader, with that recipe's own hook verdict.
#[test]
fn a_workflow_fire_refused_on_trust_strikes_with_the_refusal() {
    let env = live_room();
    let _ = std::fs::remove_file(env.agent_config_path("claude"));
    let home = env.agent_config_path("claude");
    std::fs::create_dir_all(home.parent().unwrap()).unwrap();
    let layout = format!(
        "RimZ hooks are missing for claude account `default` at `{}`",
        home.parent().unwrap().display()
    );
    for (task, lifecycle, problem) in [
        (
            "turn",
            "turn",
            "claude hooks are missing, so the supervised turn cannot report completion; \
             run `rimz hooks install claude` (blocks build)",
        ),
        ("session", "session", layout.as_str()),
    ] {
        let dir = env.rimz_home().join("workflows").join(task);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prompt.md"), "Review this\n").unwrap();
        std::fs::write(
            dir.join("workflow.toml"),
            format!(
                "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\n\
                 prompt-file='prompt.md'\nlifecycle='{lifecycle}'\nthen='@done'\n"
            ),
        )
        .unwrap();
        loop_ok(
            &env,
            &["loop", "add", task, "--workflow", task, "--every", "1h"],
        );
        let before = read_loop_run_records(&env).len();
        loop_ok(&env, &["loop", "run", task]);
        let records = read_loop_run_records(&env);
        assert_eq!(records.len(), before + 1, "{records:?}");
        let row = records.last().unwrap();
        assert_eq!(row.result, LoopRunResult::Errored, "{row:?}");
        assert!(
            row.error.as_deref().is_some_and(|error| {
                error.contains("workflow start refused; nothing was created:")
                    && error.contains(problem)
            }),
            "{task}: {row:?}"
        );
        assert!(
            env.store().paths().workflow_runs().unwrap().is_empty(),
            "{row:?}"
        );
    }
}

/// A target written by hand that no longer resolves fails at the fire's launch: an `error`
/// strike carrying the launch's own error text.
#[test]
fn an_unknown_target_written_by_hand_strikes_with_the_launchs_error() {
    let env = live_room();
    write_loop_config(
        &env,
        &format!(
            "[tasks.ghost]\nroot = {:?}\nevery = '1h'\nkind = 'workflow'\ntarget = 'nowhere'\nargs = []\n",
            env.project_root
        ),
    );
    let before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "ghost"]);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), before + 1, "{records:?}");
    let row = records.last().unwrap();
    assert_eq!(row.result, LoopRunResult::Errored, "{row:?}");
    assert!(
        row.error
            .as_deref()
            .is_some_and(|error| error.contains("nowhere")),
        "{row:?}"
    );
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("ghost")),
        Some(&1)
    );
}

/// A launch child that is still running is listed by `loop show` as a launch process and
/// stopped by `loop stop`; the stopped fire is recorded once, neutral.
#[cfg(unix)]
#[test]
fn loop_show_lists_a_running_launch_child_and_loop_stop_ends_it() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    loop_ok(
        &env,
        &[
            "loop", "add", "solo", "--agent", "worker", "--every", "1h", "--", "go",
        ],
    );
    std::fs::write(&code, "hold").unwrap();
    let before = read_loop_run_records(&env).len();
    // The fire waits on its launch child, so it runs beside the test.
    let mut fire = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "fire", "solo"])
        .spawn()
        .unwrap();
    let pid = marked_launch_child(&env, "solo", &seen);
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "solo", "--json"])).unwrap();
    let launches = shown["launches"].as_array().unwrap();
    assert_eq!(launches.len(), 1, "{shown}");
    assert_eq!(launches[0]["what"], "launch process", "{shown}");
    assert_eq!(launches[0]["address"], pid.to_string(), "{shown}");
    assert_eq!(launches[0]["state"], "running", "{shown}");

    let stopped = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "stop", "solo"])
        .output()
        .unwrap();
    assert!(stopped.status.success(), "{stopped:?}");
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while rimz::proc::process_is_live(pid, None) {
        assert!(
            Instant::now() < deadline,
            "the launch child outlived the stop"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // The stop's ladder ends the waiting runner itself.
    fire.wait().unwrap();
    // Let any settle pass that could double-record run first.
    loop_ok(&env, &["loop", "settle"]);
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        before + 1,
        "one row for the fire: {records:?}"
    );
    assert_eq!(
        records.last().unwrap().result,
        LoopRunResult::Canceled,
        "{records:?}"
    );
    assert_eq!(read_loop_strikes(&env).get(&machine_task_key("solo")), None);
}

/// An agent task's fire launches the agent through `rimz agents` in an unfocused tab, stamped
/// with the fire, and records `launched` once placement returns.
#[cfg(unix)]
#[test]
fn an_agent_fire_launches_unfocused_and_stamped_and_records_launched() {
    use crate::common::CommandTimeoutExt;

    let env = live_room();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop", "add", "solo", "--agent", "codex", "--every", "1h", "--", "repair",
        ],
    );
    let trace = env.home_root.join("fire-launch.log");
    let panes = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(panes).unwrap(),
    );
    let before = read_loop_run_records(&env).len();
    let fired = env
        .rimz()
        .args(["--mux", "zellij", "loop", "run", "solo"])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        // The launch child picks its multiplexer from its environment, as any `rimz`
        // does; `--mux` reaches the `loop run` alone.
        .env("ZELLIJ", "0")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .bounded_output()
        .unwrap();
    assert!(
        fired.status.success(),
        "{}",
        String::from_utf8_lossy(&fired.stderr)
    );
    let request = super::row_tests::launched_pane_request(&env, "solo", &trace);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), before + 1, "{records:?}");
    let row = records.last().unwrap();
    assert_eq!(row.result, LoopRunResult::Launched, "{row:?}");
    let fire = row.fire.clone().expect("the row names its fire");
    assert_eq!(request.identity.params.loop_fire.as_ref(), Some(&fire));
    assert_eq!(request.identity.params.loop_task.as_deref(), Some("solo"));
    // The receipt and the fire's logs block name the agent it launched.
    let launched = row.target.as_deref().unwrap_or_default();
    assert!(launched.starts_with('@'), "{row:?}");
    let receipt = String::from_utf8_lossy(&fired.stdout);
    assert!(
        receipt.contains(&format!("launched {launched}")),
        "{receipt}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "solo"]);
    assert!(logs.contains(&format!("launched: {launched}")), "{logs}");

    let trace = std::fs::read_to_string(&trace).unwrap();
    let opened = trace
        .lines()
        .find(|line| line.split('\t').any(|arg| arg == "new-tab"))
        .unwrap_or_else(|| panic!("no tab opened: {trace}"));
    assert!(
        opened.split('\t').any(|arg| arg == "--no-focus"),
        "a fire's tab keeps the user's focus: {trace}"
    );
    assert!(
        !trace
            .split_whitespace()
            .any(|arg| matches!(arg, "go-to-tab" | "focus-pane-id" | "move-focus")),
        "{trace}"
    );
}

/// An agent fire with `-p` returns once its supervised run is published and is recorded as any
/// agent fire is: one `launched` row without a run id, written then. The run's own end, a
/// failure included, changes neither the row nor the task's strikes.
#[cfg(unix)]
#[test]
fn an_agent_print_fire_returns_at_its_run_and_records_its_launch_alone() {
    let env = live_room();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    loop_ok(
        &env,
        &[
            "loop", "add", "print", "--agent", "codex", "--every", "1h", "--", "-p", "fix it",
        ],
    );
    let before = read_loop_run_records(&env).len();
    let fired = env
        .rimz()
        .args(["loop", "fire", "print"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("ZELLIJ", "0")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .output()
        .unwrap();
    assert!(fired.status.success(), "{fired:?}");
    let paths = env.state_path_for(&env.project_root);
    let runs = rimz::harness::run::list(&paths).unwrap();
    assert_eq!(
        runs.len(),
        1,
        "the fire returned at its published run: {runs:?}"
    );
    let run = &runs[0];
    assert!(!run.status.is_terminal(), "{run:?}");
    let fire = run.loop_fire.clone().expect("the run carries the fire");
    assert_eq!(fire.task(), task_id(&env, "print"));
    let launched = read_loop_run_records(&env);
    assert_eq!(
        launched.len(),
        before + 1,
        "one row at the fire: {launched:?}"
    );
    let row = launched.last().unwrap().clone();
    assert_eq!(row.result, LoopRunResult::Launched, "{row:?}");
    assert_eq!(row.run_id, None, "the row names no run: {row:?}");
    assert_eq!(row.fire.as_ref(), Some(&fire), "{row:?}");

    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "print", "--json"])).unwrap();
    let child: u32 = shown["launches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|launch| launch["what"] == "launch process")
        .and_then(|launch| launch["address"].as_str())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("the waiting launch child is listed: {shown}"));
    rimz::harness::run::fail_if_nonterminal(&paths, &env.runtime_paths(), &run.run_id, "boom")
        .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .ok();
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while rimz::proc::process_is_live(child, None) {
        assert!(
            Instant::now() < deadline,
            "the launch child outlived SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    loop_ok(&env, &["loop", "settle"]);
    loop_ok(&env, &["loop", "settle"]);
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        before + 1,
        "the run's end adds no row: {records:?}"
    );
    assert_eq!(records.last(), Some(&row), "{records:?}");
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("print")),
        None,
        "a failed run strikes nothing"
    );
}

/// A team fire launches every member stamped with its fire, records `launched` at the fire,
/// and writes no supervised run.
#[cfg(unix)]
#[test]
fn a_team_fire_stamps_every_member_and_writes_no_supervised_run() {
    let env = live_room();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    crate::common::write_definition(
        &env,
        "agents",
        "codex",
        "description: Codex base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "coderx",
        "description: Coder\nagent: codex\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "pair",
        "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: coderx, owns: [Build]}\n  - {role: reviewer, agent: coderx}",
        "Complete the work.",
    );
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let panes = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(panes).unwrap(),
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "crew", "--team", "pair", "--every", "1h", "--", "go",
        ],
    );
    let trace = env.home_root.join("team-fire.log");
    let before = read_loop_run_records(&env).len();
    let fired = env
        .rimz()
        .args(["loop", "fire", "crew"])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("ZELLIJ", "0")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .output()
        .unwrap();
    assert!(fired.status.success(), "{fired:?}");
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), before + 1, "{records:?}");
    let row = records.last().unwrap();
    assert_eq!(row.result, LoopRunResult::Launched, "{row:?}");
    let fire = row.fire.clone().expect("the row names its fire");

    let trace = std::fs::read_to_string(&trace).unwrap();
    let requests: Vec<_> = trace
        .split("--request-file")
        .skip(1)
        .filter_map(|tail| tail.split('"').find(|part| part.ends_with(".json")))
        .map(|path| path.replace("\\\\", "\\"))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|path| {
            let payload = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{path}: {err}\n{trace}"));
            rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap()
        })
        .collect();
    assert_eq!(requests.len(), 2, "one request per member: {trace}");
    for request in &requests {
        assert_eq!(request.identity.params.loop_fire.as_ref(), Some(&fire));
    }
    // The receipt and the fire's logs block name both members it launched.
    let launched = row.target.as_deref().unwrap_or_default();
    assert_eq!(launched.matches('@').count(), 2, "{row:?}");
    let receipt = String::from_utf8_lossy(&fired.stdout);
    assert!(
        receipt.contains(&format!("launched {launched}")),
        "{receipt}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "crew"]);
    assert!(logs.contains(&format!("launched: {launched}")), "{logs}");
    let paths = env.state_path_for(&env.project_root);
    assert!(rimz::harness::run::list(&paths).unwrap().is_empty());
}

/// A fire whose `loop run` died mid-launch, and whose launch child is gone too, is settled by
/// the pass: with nothing stamped by the fire it is one `error` row, and a strike-out sends the
/// same notice a run report's does.
#[cfg(unix)]
#[test]
fn a_dead_fire_with_nothing_stamped_settles_as_an_error() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    let notify_log = env.project_root.join("loop-disabled-notify.log");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[notifications]\ncommand = '''printf '%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_TITLE\" >> '{}' '''\n",
            notify_log.display()
        ),
    )
    .unwrap();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "solo",
            "--agent",
            "worker",
            "--every",
            "1h",
            "--max-strikes",
            "1",
            "--",
            "go",
        ],
    );
    std::fs::write(&code, "hold").unwrap();
    let before = read_loop_run_records(&env).len();
    let mut runner = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "run", "solo"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let child = marked_launch_child(&env, "solo", &seen);
    runner.kill().unwrap();
    runner.wait().unwrap();
    loop_ok(&env, &["loop", "settle"]);
    assert_eq!(
        read_loop_run_records(&env).len(),
        before,
        "a live launch child keeps the fire open"
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    while rimz::proc::process_is_live(child, None) {
        assert!(
            Instant::now() < deadline,
            "the launch child outlived SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    loop_ok(&env, &["loop", "settle"]);
    loop_ok(&env, &["loop", "settle"]);
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        before + 1,
        "one row for the fire: {records:?}"
    );
    assert_eq!(
        records.last().unwrap().result,
        LoopRunResult::Errored,
        "{records:?}"
    );
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("solo")),
        Some(&1)
    );
    // The handler is spawned detached, so its write trails the settle pass.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let text = std::fs::read_to_string(&notify_log).unwrap_or_default();
        if text.contains("loop_disabled|RimZ: loop solo disabled") {
            assert_eq!(text.lines().count(), 1, "{text}");
            break;
        }
        assert!(Instant::now() < deadline, "notification missing: {text}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn the_tick_settles_a_dead_fire_in_a_room_nothing_else_names() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    loop_ok(
        &env,
        &[
            "loop", "add", "gone", "--agent", "worker", "--every", "1h", "--", "go",
        ],
    );
    std::fs::write(&code, "hold").unwrap();
    let before = read_loop_run_records(&env).len();
    let mut runner = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "run", "gone"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let child = marked_launch_child(&env, "gone", &seen);
    runner.kill().unwrap();
    runner.wait().unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    while rimz::proc::process_is_live(child, None) {
        assert!(
            Instant::now() < deadline,
            "the launch child outlived SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // The row is deleted by hand and no sidebar runs: only the open mark names the room.
    std::fs::write(env.rimz_home().join("loop.toml"), "").unwrap();
    std::fs::remove_file(env.runtime_paths().heartbeat_dir.join("sidebar.kinds.json")).unwrap();
    loop_ok(&env, &["loop", "tick"]);
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while read_loop_run_records(&env).len() == before {
        assert!(
            Instant::now() < deadline,
            "the tick never settled the dead fire"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.last().unwrap().result,
        LoopRunResult::Errored,
        "{records:?}"
    );
}

/// Seeds a room agent stamped with `fire`, alive by its durable rows until `end_agent` ends it.
/// Its session id is bound, not a `launch_` reservation: a stop cancels a never-bound
/// reservation, but with no pane and no owner it can confirm nothing here and writes nothing.
fn seed_stamped_agent(env: &Env, name: &str, fire: &rimz::ids::FireId) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(format!("session_{name}")),
                launch_id: Some(AgentSessionId::from(format!("launch_{name}"))),
                agent_name: name.to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams {
                    loop_fire: Some(fire.clone()),
                    ..Default::default()
                },
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
}

fn end_agent(env: &Env, name: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "rimz.agent-ended",
            &rimz::agents::AgentLifecycleObservation::new(
                Some(format!("session_{name}").into()),
                rimz::agents::LifecycleSignal::Ended,
            ),
        ))
        .unwrap();
}

/// An idle agent a fire launched holds a `skip` task: the next fire is `overlapped` and
/// launches nothing. Under `replace` the next fire waits, and its launch starts only once the
/// old agent has ended.
#[cfg(unix)]
#[test]
fn a_launched_agent_holds_skip_and_replace_launches_only_after_it_ends() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    std::fs::write(&code, "0").unwrap();
    for (task, overlap) in [("held", "skip"), ("swap", "replace")] {
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                task,
                "--agent",
                "worker",
                "--every",
                "1h",
                "--overlap",
                overlap,
                "--",
                "go",
            ],
        );
    }
    let run = |args: &[&str]| {
        let before = read_loop_run_records(&env).len();
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        read_loop_run_records(&env)[before..].to_vec()
    };

    // Each task's first fire launches; its agent row is what that launch left behind.
    let first_fire = |task: &str| {
        let rows = run(&["loop", "run", task]);
        assert_eq!(rows[0].result, LoopRunResult::Launched, "{rows:?}");
        std::fs::remove_file(&seen).unwrap();
        rows[0].fire.clone().unwrap()
    };
    let (held, swap) = (first_fire("held"), first_fire("swap"));
    seed_stamped_agent(&env, "idle", &held);
    let rows = run(&["loop", "run", "held"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, LoopRunResult::Overlapped, "{rows:?}");
    assert!(!seen.exists(), "a held task launches nothing");
    // The overlap names what holds the task as `loop show` lists it, not a fire id.
    let fired = loop_ok(&env, &["loop", "fire", "held"]);
    assert!(
        fired.contains("agent @idle") && !fired.contains("fire_"),
        "{fired}"
    );
    let error = rows[0].error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with("agent @idle") && !error.contains("fire_"),
        "{rows:?}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "held"]);
    assert!(logs.contains("agent @idle"), "{logs}");

    seed_stamped_agent(&env, "old", &swap);
    let rows = run(&["loop", "run", "swap"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, LoopRunResult::Deferred, "{rows:?}");
    assert!(
        rows[0]
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("agent @old")),
        "{rows:?}"
    );
    assert!(!seen.exists(), "the replacement waits for the old launch");
    // The detached stop finds no pane and no owner for the bound fixture agent, so it writes
    // nothing and the durable row stays open.
    let rows = run(&["loop", "run", "swap", "--drain"]);
    assert!(rows.is_empty(), "{rows:?}");
    assert!(!seen.exists(), "the old agent is still alive");

    end_agent(&env, "old");
    let rows = run(&["loop", "run", "swap", "--drain"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, LoopRunResult::Launched, "{rows:?}");
    assert!(seen.exists(), "the replacement launched");
}

/// An `allow` task launches at every trigger while earlier fires' agents are alive, a manual
/// fire included: each fire records `launched`, nothing waits, and `loop stop` reaches the
/// agents of every fire.
#[cfg(unix)]
#[test]
fn an_allow_task_launches_beside_its_alive_agents_and_stop_reaches_them_all() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    std::fs::write(&code, "0").unwrap();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "pile",
            "--agent",
            "worker",
            "--every",
            "1h",
            "--overlap",
            "allow",
            "--",
            "go",
        ],
    );
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "pile", "--json"])).unwrap();
    assert_eq!(shown["entry"]["overlap"], "allow", "{shown}");
    assert!(
        shown["backlog"].is_null(),
        "allow keeps no backlog: {shown}"
    );
    let run = |args: &[&str]| {
        let before = read_loop_run_records(&env).len();
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        read_loop_run_records(&env)[before..].to_vec()
    };
    let mut fires = Vec::new();
    for (args, agent) in [
        (&["loop", "run", "pile"][..], "first"),
        (&["loop", "run", "pile"][..], "second"),
        (&["loop", "fire", "pile"][..], "third"),
    ] {
        let rows = run(args);
        assert_eq!(rows.len(), 1, "{args:?}: {rows:?}");
        assert_eq!(
            rows[0].result,
            LoopRunResult::Launched,
            "{args:?}: {rows:?}"
        );
        assert!(seen.exists(), "{args:?} launched");
        std::fs::remove_file(&seen).unwrap();
        let fire = rows[0].fire.clone().unwrap();
        seed_stamped_agent(&env, agent, &fire);
        fires.push(fire);
    }
    let backlog = env
        .state_path_for(&env.project_root)
        .root
        .join("records/loop-backlog.json");
    assert!(!backlog.exists(), "nothing waits under allow");
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "pile", "--json"])).unwrap();
    let alive: Vec<&str> = shown["launches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|launch| launch["address"].as_str())
        .collect();
    assert_eq!(alive.len(), 3, "every fire's agent is alive: {shown}");
    for (agent, fire) in ["first", "second", "third"].iter().zip(&fires) {
        assert!(
            shown["launches"].as_array().unwrap().iter().any(|launch| {
                launch["address"]
                    .as_str()
                    .is_some_and(|address| address.contains(agent))
                    && launch["fire"] == fire.to_string()
            }),
            "{agent}: {shown}"
        );
    }

    let output = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "stop", "pile"])
        .output()
        .unwrap();
    let calls = stand_in_calls(&seen);
    let mut stopped: Vec<&str> = calls
        .lines()
        .filter_map(|line| line.split_once(" agents stop @").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split(['#', ' ']).next())
        .collect();
    stopped.sort_unstable();
    assert_eq!(stopped, ["first", "second", "third"], "{calls}\n{output:?}");
}

/// `loop add --overlap allow` refuses every shape whose fires cannot run side by side, with the
/// reason true of that shape, and stores nothing.
#[cfg(unix)]
#[test]
fn add_refuses_allow_where_fires_cannot_run_side_by_side() {
    let env = live_room();
    kinds_fixture(&env);
    let side_by_side = "loop task `pile` sets `overlap`; `allow` needs fires that run side by side";
    for (flags, reason) in [
        (
            &["--every", "1h", "--", "true"][..],
            format!("{side_by_side}, and a shell task holds one unfinished run per checkout"),
        ),
        (
            &["--workflow", "smoke", "--every", "1h"][..],
            format!("{side_by_side}, and a workflow task holds one unfinished run per checkout"),
        ),
        (
            &["--team", "forge", "--every", "1h", "--", "go"][..],
            format!("{side_by_side}, and a team task's fires launch the same team into one lane"),
        ),
        (
            &[
                "--agent",
                "worker",
                "--every",
                "1h",
                "--each-worktree",
                "--",
                "go",
            ][..],
            "loop task `pile` sets `overlap`; an each-worktree task takes no overlap policy, \
             since it fires per checkout"
                .to_owned(),
        ),
        (
            &["--agent", "worker", "--in", "1h", "--", "go"][..],
            "loop task `pile` sets `overlap`; a one-shot fires once, so no later fire overlaps it"
                .to_owned(),
        ),
    ] {
        let output = env
            .rimz()
            .args([&["loop", "add", "pile", "--overlap", "allow"][..], flags].concat())
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{flags:?}: {output:?}");
        assert!(error.contains(&reason), "{flags:?}: {error}");
        assert!(
            !error.contains("backlog") && !error.contains("drain"),
            "{error}"
        );
        let listed = loop_ok(&env, &["loop", "list", "--all"]);
        assert!(
            !listed.contains("pile"),
            "{flags:?} stored nothing: {listed}"
        );
    }
}

/// `rimz gc` keeps a consumed booking's retired record while anything still shows it: while
/// the agent its fire launched lives, so `loop show` lists it and `loop stop` finds the task,
/// then while a history row names it, so `loop logs` still prints the fire; once nothing shows
/// it, gc deletes it.
#[cfg(unix)]
#[test]
fn gc_keeps_a_retired_task_record_while_anything_still_shows_it() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, _seen, code) = launch_stand_in(&env);
    std::fs::write(&code, "0").unwrap();
    loop_ok(
        &env,
        &[
            "loop", "add", "booked", "--agent", "worker", "--at", "07:00", "--", "go",
        ],
    );
    let id = task_id(&env, "booked");
    let output = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "run", "booked"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let row = read_loop_run_records(&env).last().cloned().unwrap();
    assert_eq!(row.result, LoopRunResult::Launched, "{row:?}");
    assert!(!read_loop_instances(&env).0.contains_key("booked"));
    seed_stamped_agent(&env, "kept", row.fire.as_ref().unwrap());
    let gc = || {
        let output = env.rimz().args(["gc", "--json"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        task_records(&env).get(&id).cloned()
    };

    // The alive agent alone holds the record: no history row names the task.
    let history = std::fs::read(loop_runs_path(&env)).unwrap();
    std::fs::remove_file(loop_runs_path(&env)).unwrap();
    let record = gc().expect("the alive agent keeps the record");
    assert!(!record.is_live() && !record.marks.is_empty(), "{record:?}");
    std::fs::write(loop_runs_path(&env), &history).unwrap();
    let show = loop_ok(&env, &["loop", "show", "booked"]);
    assert!(show.contains("one-shot fired · launched"), "{show}");
    let launches = show.split_once("\nLAUNCHES").map(|(_, rest)| rest);
    assert!(
        launches.is_some_and(|rest| rest.contains("kept")),
        "the consumed booking lists the agent it launched: {show}"
    );
    let stop = env
        .rimz()
        .args(["loop", "stop", "booked"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&stop.stderr);
    assert!(!stderr.contains("no loop task named"), "{stderr}");

    // The agent ended: the history row alone holds it, and `loop logs` reads the fire.
    end_agent(&env, "kept");
    std::fs::write(loop_runs_path(&env), &history).unwrap();
    assert!(gc().is_some(), "the history row keeps the record");
    let logs = loop_ok(&env, &["loop", "logs", "booked"]);
    assert!(logs.contains("launched"), "{logs}");

    // A worktree whose marker carries the fire alone holds it.
    std::fs::remove_file(loop_runs_path(&env)).unwrap();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let tree = env.home_root.join("fired");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fired",
            tree.to_str().unwrap()
        ]
    ));
    let admin = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(&tree)
        .output()
        .unwrap();
    let marker =
        Path::new(String::from_utf8(admin.stdout).unwrap().trim()).join("rimz-worktree.json");
    let fired = rimz::worktree::WorktreeMarker {
        version: 4,
        name: "fired".into(),
        branch: "fired".into(),
        base_branch: None,
        from_pr: None,
        base_ref: "HEAD".into(),
        repo_root: canonical(&env.project_root),
        worktree_path: canonical(&tree),
        created_at: Timestamp::now(),
        loop_fire: row.fire.clone(),
        external: None,
    };
    std::fs::write(&marker, serde_json::to_vec(&fired).unwrap()).unwrap();
    assert!(gc().is_some(), "the worktree marker keeps the record");

    // That gc may already have swept the tree, which nothing holds and which has landed.
    if marker.exists() {
        std::fs::remove_file(&marker).unwrap();
    }
    assert!(gc().is_none(), "nothing shows the record, so gc deletes it");
}

/// The pid of `task`'s launch child once the stand-in has written it beside `seen` and the
/// fire has marked it: the runner marks the child after the spawn the pid file shows.
fn marked_launch_child(env: &Env, task: &str, seen: &Path) -> u32 {
    let pid_file = seen.with_extension("pid");
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let pid = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        let marked = pid.filter(|pid| {
            task_records(env)
                .named_in(&env.project_root, task)
                .is_some_and(|(_, record)| {
                    record
                        .marks
                        .values()
                        .any(|mark| mark.child.as_ref().is_some_and(|child| child.pid == *pid))
                })
        });
        if let Some(pid) = marked {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the fire never marked its launch child"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The argv of every call the stand-in behind `seen` took, one line each.
fn stand_in_calls(seen: &Path) -> String {
    std::fs::read_to_string(seen.with_extension("calls")).unwrap_or_default()
}

/// A supervised run in the project room carrying `fire`, still running.
fn seed_fire_run(env: &Env, fire: &rimz::ids::FireId) -> RunRecord {
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Running;
    run.loop_fire = Some(fire.clone());
    rimz::harness::run::create(&paths, &run).unwrap();
    run
}

/// A workflow fire whose `rimz workflow run` exited 0 published its run, wherever the run's
/// home is by the time the fire reads it: a run collected out of the live directory before the
/// fire looks (finished, or forgotten as quiet) is still the fire's launch, never an `error`,
/// and its one-off is consumed.
#[cfg(unix)]
#[test]
fn a_workflow_fire_whose_run_left_the_live_directory_first_is_launched() {
    use std::os::unix::fs::PermissionsExt;
    let env = live_room();
    deploy_workflow(&env, "smoke", "worktree", "true");
    deploy_workflow(&env, "loud", "worktree", "echo out; exit 1");
    let live = env
        .state_path_for(&env.project_root)
        .workflow(&rimz::ids::WorkflowRunId::new())
        .home
        .parent()
        .unwrap()
        .to_owned();
    // The launch child returns only once its run's live home is gone.
    let wrapper = env.home_root.join("rimz-collected");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1 $2\" != 'workflow run' ]; then exec {real:?} \"$@\"; fi\n\
             out=$({real:?} \"$@\") || exit $?\nprintf '%s\\n' \"$out\"\n\
             id=$(printf '%s\\n' \"$out\" | head -n 1)\nn=0\n\
             while [ -e {live:?}/\"$id\" ] && [ $n -lt 600 ]; do sleep 0.05; n=$((n+1)); done\n",
            real = env.rimz_bin().display().to_string(),
            live = live.display().to_string(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (task, workflow, result) in [
        ("quiet", "smoke", LoopRunResult::Completed),
        ("loud", "loud", LoopRunResult::Failed),
    ] {
        loop_ok(
            &env,
            &["loop", "add", task, "--workflow", workflow, "--at", "07:00"],
        );
        let before = run_mark(&env, task);
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .args(["loop", "run", task])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let report = wait_for_loop_report_after(&env, task, before.clone());
        assert_eq!(report.result, result, "{report:?}");
        let rows = read_loop_run_records(&env);
        let errored: Vec<_> = rows[before.rows..]
            .iter()
            .filter(|row| row.task == task && row.result == LoopRunResult::Errored)
            .collect();
        assert!(errored.is_empty(), "{task}: {errored:?}");
        assert!(
            !read_loop_instances(&env).0.contains_key(task),
            "{task}: the fire consumed its one-off"
        );
    }
    // A consumed one-off's strikes stay on its retired record, while one is kept.
    let strikes = |task: &str| {
        task_records(&env)
            .named_in(&env.project_root, task)
            .map(|(_, record)| record.strikes)
    };
    assert_eq!(strikes("quiet").unwrap_or_default(), 0);
    assert_eq!(
        strikes("loud"),
        Some(1),
        "only the run's own failure strikes"
    );
}

/// A workflow fire whose child is refused because another task's run of the workflow is live
/// in the checkout records the neutral `overlapped`, never a strike.
#[cfg(unix)]
#[test]
fn a_workflow_fire_refused_by_a_live_run_in_its_checkout_is_overlapped() {
    let env = live_room();
    let release = env.home_root.join("release");
    deploy_workflow(
        &env,
        "smoke",
        "worktree",
        &format!(
            "while [ ! -e {:?} ]; do sleep 0.05; done",
            release.display().to_string()
        ),
    );
    for task in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", task, "--workflow", "smoke", "--every", "1h"],
        );
    }
    loop_ok(&env, &["loop", "run", "first"]);
    let rows = read_loop_run_records(&env).len();
    let holder = rimz::store::workflow::live_runs(&env.state_path_for(&env.project_root))
        .unwrap()
        .into_iter()
        .find(|run| run.task.as_deref() == Some("first"))
        .expect("the first task's run holds the checkout")
        .id;
    loop_ok(&env, &["loop", "run", "second"]);
    let records = read_loop_run_records(&env);
    std::fs::write(&release, "").unwrap();
    assert_eq!(records.len(), rows + 1, "{records:?}");
    let refused = records.last().unwrap();
    assert_eq!(refused.task, "second", "{refused:?}");
    assert_eq!(refused.result, LoopRunResult::Overlapped, "{refused:?}");
    assert_eq!(refused.blocking_run.as_ref(), Some(&holder), "{refused:?}");
    assert_eq!(
        refused.error.as_deref(),
        Some(format!("workflow run {holder} holds the checkout — skipped").as_str()),
    );
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("second")),
        None
    );
}

/// `rimz loop stop` acts on the task it loaded, never on a retired record of the same name
/// another scope left, and each stop it forwards carries `--root <task root>` before its
/// subcommand, so it acts in the task's room whatever room it runs from.
#[cfg(unix)]
#[test]
fn loop_stop_forwards_rooted_stops_to_the_loaded_tasks_launches() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, _code) = launch_stand_in(&env);
    loop_ok(
        &env,
        &[
            "loop", "add", "foo", "--agent", "worker", "--every", "1h", "--", "go",
        ],
    );
    let machine = seed_task(&env, &machine_task_key("foo"));
    let retired = seed_task(&env, &project_task_key(&env.project_root, "foo"));
    edit_task_records(&env, |tasks| {
        tasks.get_mut(retired.as_str()).unwrap()["retired_at"] = json!("2026-01-01T00:00:00Z");
    });
    seed_stamped_agent(&env, "mine", &rimz::ids::FireId::new(&machine));
    seed_stamped_agent(&env, "stale", &rimz::ids::FireId::new(&retired));
    let output = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "stop", "foo"])
        .output()
        .unwrap();
    let calls = stand_in_calls(&seen);
    let stops: Vec<&str> = calls
        .lines()
        .filter(|line| line.contains("agents stop"))
        .collect();
    assert_eq!(stops.len(), 1, "{calls}\n{output:?}");
    let (root, rest) = stops[0]
        .strip_prefix("--root ")
        .and_then(|rest| rest.split_once(" agents stop "))
        .unwrap_or_else(|| panic!("the stop names the task's root first: {calls}"));
    assert_eq!(canonical(Path::new(root)), canonical(&env.project_root));
    assert!(rest.starts_with("@mine"), "{calls}");
}

/// A workflow task's overlap and `replace` see only its own launch: a supervised run left
/// carrying a finished workflow fire neither holds the next fire nor is canceled by it.
#[cfg(unix)]
#[test]
fn a_workflow_fire_is_held_only_by_its_own_run() {
    let env = live_room();
    deploy_workflow(&env, "smoke", "worktree", "true");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "wf",
            "--workflow",
            "smoke",
            "--every",
            "1h",
            "--overlap",
            "replace",
        ],
    );
    let before = run_mark(&env, "wf");
    loop_ok(&env, &["loop", "run", "wf"]);
    let report = wait_for_loop_report_after(&env, "wf", before);
    let side = seed_fire_run(&env, report.fire.as_ref().unwrap());
    let before = run_mark(&env, "wf");
    loop_ok(&env, &["loop", "run", "wf"]);
    let held: Vec<_> = read_loop_run_records(&env)[before.rows..]
        .iter()
        .filter(|row| {
            matches!(
                row.result,
                LoopRunResult::Deferred | LoopRunResult::Overlapped
            )
        })
        .cloned()
        .collect();
    assert!(held.is_empty(), "{held:?}");
    let report = wait_for_loop_report_after(&env, "wf", before);
    assert_eq!(report.result, LoopRunResult::Completed, "{report:?}");
    let paths = env.state_path_for(&env.project_root);
    let side = rimz::harness::run::list(&paths)
        .unwrap()
        .into_iter()
        .find(|run| run.run_id == side.run_id)
        .unwrap();
    assert!(!side.status.is_terminal(), "{side:?}");
}

/// `replace` on an agent task whose `-p` launch child is gone stops the fire's supervised run
/// through a detached `rimz --root <root> agents stop <run>`, and the replacement waits.
#[cfg(unix)]
#[test]
fn replace_stops_a_print_fires_run_once_its_launch_child_is_gone() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    std::fs::write(&code, "0").unwrap();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "swap",
            "--agent",
            "worker",
            "--every",
            "1h",
            "--overlap",
            "replace",
            "--",
            "-p",
            "go",
        ],
    );
    let run = |args: &[&str]| {
        let before = read_loop_run_records(&env).len();
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        read_loop_run_records(&env)[before..].to_vec()
    };
    let rows = run(&["loop", "run", "swap"]);
    assert_eq!(rows[0].result, LoopRunResult::Launched, "{rows:?}");
    let old = seed_fire_run(&env, rows[0].fire.as_ref().unwrap());
    std::fs::remove_file(&seen).unwrap();
    let rows = run(&["loop", "run", "swap"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, LoopRunResult::Deferred, "{rows:?}");
    assert!(!seen.exists(), "the replacement waits for the old run");
    let stop = format!(" agents stop {}", old.run_id);
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let calls = stand_in_calls(&seen);
        if let Some(line) = calls.lines().find(|line| line.ends_with(&stop)) {
            assert!(line.starts_with("--root "), "{calls}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the old run was never stopped: {calls}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `rimz loop show` of a consumed `-p` one-off whose run is live, before the settle pass wrote
/// the fire's row, shows the task and the run it launched.
#[cfg(unix)]
#[test]
fn loop_show_finds_a_consumed_print_one_off_by_its_live_run() {
    let env = live_room();
    kinds_fixture(&env);
    let (wrapper, _seen, code) = launch_stand_in(&env);
    std::fs::write(&code, "0").unwrap();
    loop_ok(
        &env,
        &[
            "loop", "add", "booked", "--agent", "worker", "--at", "07:00", "--", "-p", "go",
        ],
    );
    let output = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "run", "booked"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let fire = read_loop_run_records(&env)
        .last()
        .unwrap()
        .fire
        .clone()
        .unwrap();
    // A `-p` fire's row lands on the settle pass, once its run ends.
    std::fs::remove_file(loop_runs_path(&env)).unwrap();
    let run = seed_fire_run(&env, &fire);
    let show = loop_ok(&env, &["loop", "show", "booked"]);
    let launches = show.split_once("\nLAUNCHES").map(|(_, rest)| rest);
    assert!(
        launches.is_some_and(|rest| rest.contains(run.run_id.as_str())),
        "{show}"
    );
}

/// `loop add -w` places a task of every kind in one RimZ-owned worktree, stored as the row's
/// `worktree` beside an unchanged tail: an agent, team or workflow fire hands it to the launch as
/// one `-w` before the tail, an equal tail `-w` is stored once in the tail, and a shell fire
/// creates the tree and runs its command there.
#[cfg(unix)]
#[test]
fn loop_add_w_places_every_kind_and_each_fire_runs_there() {
    let env = live_room();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    kinds_fixture(&env);
    let (wrapper, seen, code) = launch_stand_in(&env);
    let adds: [&[&str]; 6] = [
        &[
            "sh-task", "-w", "deps", "--every", "1h", "--", "sh", "-c", "pwd -P",
        ],
        // A tail flag that needs `-w` parses beside the loop's own.
        &[
            "trunk-task",
            "--agent",
            "worker",
            "-w",
            "deps",
            "--every",
            "1h",
            "--",
            "--from-trunk",
            "go",
        ],
        &[
            "wf-task",
            "--workflow",
            "smoke",
            "-w",
            "deps",
            "--every",
            "1h",
        ],
        &[
            "agent-task",
            "--agent",
            "worker",
            "-w",
            "deps",
            "--every",
            "1h",
            "--",
            "go",
        ],
        &[
            "team-task",
            "--team",
            "forge",
            "-w",
            "deps",
            "--every",
            "1h",
            "--",
            "go",
        ],
        &[
            "same-task",
            "--agent",
            "worker",
            "--worktree",
            "deps",
            "--every",
            "1h",
            "--",
            "-w",
            "deps",
            "go",
        ],
    ];
    for add in adds {
        loop_ok(&env, &[&["loop", "add"][..], add].concat());
    }
    let rows = stored_rows(&env);
    for name in [
        "sh-task",
        "trunk-task",
        "wf-task",
        "agent-task",
        "team-task",
    ] {
        assert_eq!(
            rows[name].get("worktree").and_then(toml::Value::as_str),
            Some("deps"),
            "{name}: {rows:?}"
        );
    }
    assert_eq!(
        rows["agent-task"]["args"],
        toml::Value::try_from(["go"]).unwrap()
    );
    assert!(rows["same-task"].get("worktree").is_none(), "{rows:?}");

    std::fs::write(&code, "7").unwrap();
    for (task, argv) in [
        ("agent-task", "agents worker -w deps go"),
        ("trunk-task", "agents worker -w deps --from-trunk go"),
        ("team-task", "teams launch forge -w deps go"),
        ("same-task", "agents worker -w deps go"),
        ("wf-task", "workflow run smoke -w deps"),
    ] {
        let output = env
            .rimz()
            .env("RIMZ_BIN", &wrapper)
            .args(["loop", "run", task])
            .output()
            .unwrap();
        assert!(output.status.success(), "{task}: {output:?}");
        let seen = std::fs::read_to_string(&seen).unwrap();
        let line = seen.lines().find_map(|line| line.strip_prefix("argv="));
        assert!(
            line.is_some_and(|line| line == argv || line.starts_with(&format!("{argv} --"))),
            "{task}: {seen}"
        );
    }

    loop_ok(&env, &["loop", "fire", "sh-task"]);
    let report = wait_for_loop_report(&env, "sh-task");
    assert_eq!(report.result, LoopRunResult::Completed, "{report:?}");
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    let tree = owned
        .iter()
        .find(|tree| tree.marker.name == "deps")
        .unwrap_or_else(|| panic!("{owned:?}"));
    // The tree carries the fire that made it, as an agent fire's tree does.
    assert_eq!(tree.marker.loop_fire, report.fire, "{owned:?}");
    // The fire's history keys at the project root, where its conditions and marks live.
    assert_eq!(
        report.checkout.as_deref().map(canonical),
        Some(canonical(&env.project_root)),
        "{report:?}"
    );
    let journal = loop_run_journal(&env, &env.project_root, &report);
    let path = canonical(&tree.marker.worktree_path);
    assert!(
        journal.contains(&format!("{}\n", path.display())),
        "{path:?}: {journal}"
    );
}

/// A shell task's fire never adopts a directory at its worktree's path that RimZ does not own:
/// the fire fails with the reason and asks nothing.
#[test]
fn a_shell_fire_refuses_a_tree_rimz_does_not_own() {
    let env = live_room();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let config = rimz::config::MachineConfig::default();
    let path =
        rimz::worktree::worktree_path(&env.project_root, &config.agents.worktree, "plain").unwrap();
    std::fs::create_dir_all(&path).unwrap();
    loop_ok(
        &env,
        &[
            "loop", "add", "plain", "-w", "plain", "--every", "1h", "--", "true",
        ],
    );
    let _ = env
        .rimz()
        .args(["loop", "run", "plain"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let record = read_loop_run_records(&env)
        .into_iter()
        .rfind(|record| record.task == "plain")
        .expect("the fire records its failure");
    assert_eq!(record.result, LoopRunResult::Errored, "{record:?}");
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("worktree `plain` is not a RimZ-managed worktree")),
        "{record:?}"
    );
}

/// A shell task's fire creates its worktree only after the admission an agent launch's `-w`
/// gets: under an unreadable core config the fire fails with the fix and no tree exists.
#[test]
fn a_shell_fire_creates_no_tree_under_an_unreadable_core_config() {
    let env = live_room();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    loop_ok(
        &env,
        &[
            "loop", "add", "guarded", "-w", "guarded", "--every", "1h", "--", "true",
        ],
    );
    std::fs::write(env.rimz_home().join("config.toml"), "[agents\n").unwrap();
    let _ = env
        .rimz()
        .args(["loop", "run", "guarded"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let record = read_loop_run_records(&env)
        .into_iter()
        .rfind(|record| record.task == "guarded")
        .expect("the fire records its failure");
    assert_eq!(record.result, LoopRunResult::Errored, "{record:?}");
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cannot create a worktree")),
        "{record:?}"
    );
    assert!(
        rimz::worktree::discover_owned(&env.project_root)
            .unwrap()
            .is_empty()
    );
    let config = rimz::config::MachineConfig::default();
    let path = rimz::worktree::worktree_path(&env.project_root, &config.agents.worktree, "guarded")
        .unwrap();
    assert!(!path.exists(), "{path:?}");
}

/// The receipt reads the launch back: the worktree its arguments name (a one-word tail after a
/// bare `-w` is that worktree, with no prompt), whether a prompt was found, a `-p` run's time
/// limit, and for a repeating agent left idle, the note that it holds later fires.
#[test]
fn loop_add_reads_each_launch_back_in_its_receipt() {
    let env = live_room();
    kinds_fixture(&env);
    let receipt = |add: &[&str]| loop_ok(&env, &[&["loop", "add"][..], add].concat());
    let idle = "note: an agent left idle holds later fires; add `-p` or `--stay false` for a run that ends itself, `--overlap replace` to stop the old agent at each fire, or `--overlap allow` to launch beside it";
    let team_idle = "note: an agent left idle holds later fires; add `-p` or `--stay false` for a run that ends itself, or `--overlap replace` to stop the old agent at each fire";
    let trap = receipt(&[
        "trap", "--agent", "worker", "--every", "1h", "--", "-w", "one-word",
    ]);
    assert!(
        trap.lines()
            .any(|line| line == "launch: worktree `one-word` · no prompt"),
        "{trap}"
    );
    assert!(trap.lines().any(|line| line == idle), "{trap}");
    let print = receipt(&[
        "print",
        "--agent",
        "worker",
        "--every",
        "1h",
        "--",
        "-p",
        "--timeout",
        "30m",
        "fix it",
    ]);
    assert!(
        print
            .lines()
            .any(|line| line == "launch: prompt found · time limit 30m"),
        "{print}"
    );
    assert!(!print.contains("note: an agent left idle"), "{print}");
    let placed = receipt(&[
        "placed",
        "--agent",
        "worker",
        "-w",
        "deps",
        "--overlap",
        "replace",
        "--every",
        "1h",
        "--",
        "go",
    ]);
    for line in [
        "action: launches `rimz agents worker -w deps go` in worktree `deps` (created at the first fire if missing)",
        "launch: worktree `deps` · prompt found",
    ] {
        assert!(placed.lines().any(|seen| seen == line), "{line}: {placed}");
    }
    assert!(!placed.contains("note: an agent left idle"), "{placed}");
    let flow = receipt(&["flow", "--workflow", "smoke", "-w", "deps", "--every", "1h"]);
    assert!(
        flow.lines().any(|line| line == "launch: worktree `deps`"),
        "{flow}"
    );
    let crew = receipt(&[
        "crew",
        "--team",
        "forge",
        "--every",
        "1h",
        "--",
        "--timeout",
        "2h",
        "-w",
    ]);
    assert!(
        crew.lines()
            .any(|line| line == "launch: a new worktree each fire · no prompt · time limit 2h"),
        "{crew}"
    );
    assert!(crew.lines().any(|line| line == team_idle), "{crew}");
}

/// An agent task's window terms read the kind its launch runs on when the loop names no
/// `--provider`; a team task resolves no kind and a shell task has none, so each refuses with
/// the flag that names one, at exit 1.
#[test]
fn window_terms_default_to_the_kind_an_agent_task_launches() {
    let env = live_room();
    kinds_fixture(&env);
    env.install_agent_hooks("claude");
    crate::common::wait::publish_claude_5h(&env, 10);
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "guard",
            "--agent",
            "worker",
            "--when",
            "window.5h.left >= 40",
            "--",
            "go",
        ],
    );
    assert!(
        receipt
            .lines()
            .any(|line| line == "windows: claude (from the launch), the room's account"),
        "{receipt}"
    );
    assert_eq!(
        read_loop_instances(&env).0["guard"].provider,
        Some(AgentKind::new_unchecked("claude"))
    );
    // A tier route reads the kind every fire can launch: one whose fallbacks span kinds names
    // none, while a route of one kind defaults like a plain profile.
    crate::common::write_definition(&env, "agents", "codex", "description: Codex base", "Base.");
    for (name, tier) in [("spread", "senior"), ("single", "principal")] {
        crate::common::write_definition(
            &env,
            "agents",
            name,
            &format!("description: Tiered\ntier: {tier}\ntools: [Bash]"),
            "Work.",
        );
    }
    let single = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "single",
            "--agent",
            "single",
            "--when",
            "window.5h.left >= 40",
            "--",
            "go",
        ],
    );
    assert!(
        single
            .lines()
            .any(|line| line == "windows: claude (from the launch), the room's account"),
        "{single}"
    );
    let spread = env
        .rimz()
        .args(["loop", "add", "spread", "--agent", "spread"])
        .args(["--when", "window.5h.left >= 40", "--", "go"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&spread.stderr);
    assert_eq!(spread.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "the launch runs claude and codex; name the provider whose windows to read with \
             --provider <kind>"
        ),
        "{stderr}"
    );
    let refusals: [&[&str]; 2] = [
        &["--team", "forge", "--when", "window.5h.left >= 40"],
        &["--when", "window.7d.left >= 80", "--", "true"],
    ];
    for flags in refusals {
        let output = env
            .rimz()
            .args(["loop", "add", "t"])
            .args(flags)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{flags:?}: {stderr}");
        assert!(
            stderr.contains(
                "a window trigger reads a provider's windows; name it with --provider <kind>"
            ),
            "{flags:?}: {stderr}"
        );
    }
}
