//! The command-line loop row: what `loop add` stores, what a fire runs, and how it ends.

use super::*;

fn stored_rows(env: &Env) -> toml::Table {
    let text = std::fs::read_to_string(loop_config_path(env)).unwrap();
    toml::from_str::<toml::Table>(&text).unwrap()["tasks"]
        .as_table()
        .unwrap()
        .clone()
}

/// A recorded root with a live sidebar, so a scheduled `loop run` opens no room.
fn live_room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "rows",
    );
    env
}

fn run_and_report(env: &Env, name: &str) -> LoopRunRecord {
    let before = run_mark(env, name);
    loop_ok(env, &["loop", "run", name]);
    wait_for_loop_report_after(env, name, before)
}

#[test]
fn a_when_add_stores_the_trigger_command_timeout_and_generated_name() {
    let env = Env::new();
    env.record(&env.project_root);
    let command = ["sh", "-c", "echo 'a b'"];
    let argv = [
        &["loop", "add", "--when", "pr[x]=merged", "--"][..],
        &command,
    ]
    .concat();
    let receipt = loop_ok(&env, &argv);

    // A one-shot lives in the room's instance store, not in `loop.toml`.
    assert!(!loop_config_path(&env).exists());
    let rows: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&std::fs::read(loop_instances_path(&env)).unwrap()).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (name, row) = rows.iter().next().unwrap();
    assert!(name.starts_with("loop-"), "{name}");
    assert!(receipt.contains(name.as_str()), "{receipt}");
    let row = row.as_object().unwrap();
    assert_eq!(
        row.keys().map(String::as_str).collect::<Vec<_>>(),
        ["args", "kind", "root", "timeout", "when"],
        "{row:?}"
    );
    assert_eq!(row["kind"], "shell");
    assert_eq!(row["args"], json!(command));
    assert_eq!(row["timeout"], "2h");
    assert_eq!(row["when"], json!(["pr[x]=merged"]));

    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", name, "--json"])).unwrap();
    assert_eq!(
        &shown["entry"],
        &serde_json::Value::Object(row.clone()),
        "{shown}"
    );
}

#[cfg(unix)]
#[test]
fn an_argv_word_with_space_quote_dollar_and_glob_reaches_the_process_unchanged() {
    let env = live_room();
    let word = "a b'\"$HOME *.rs";
    let seen = env.home_root.join("seen");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "words",
            "--cron",
            "0 0 1 1 *",
            "--timeout",
            "1m",
            "--",
            "sh",
            "-c",
            r#"printf %s "$1" > "$2""#,
            "sh",
            word,
            seen.to_str().unwrap(),
        ],
    );
    let before = run_mark(&env, "words");
    loop_ok(&env, &["loop", "run", "words"]);
    assert_eq!(
        wait_for_loop_report_after(&env, "words", before).result,
        LoopRunResult::Completed
    );
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), word);
}

#[cfg(unix)]
#[test]
fn exit_zero_ends_done_and_a_failure_or_timeout_ends_failed_with_a_strike() {
    let env = live_room();
    for (name, timeout, command) in [
        ("passes", "1m", vec!["true"]),
        ("fails", "1m", vec!["sh", "-c", "echo gone-wrong; exit 3"]),
        ("hangs", "1s", vec!["sleep", "347"]),
    ] {
        let argv = [
            &[
                "loop",
                "add",
                name,
                "--cron",
                "0 * * * *",
                "--timeout",
                timeout,
                "--",
            ][..],
            &command,
        ]
        .concat();
        loop_ok(&env, &argv);
    }
    let results = ["passes", "fails", "hangs"].map(|name| {
        let report = run_and_report(&env, name);
        (name, report.result, report.exit_label())
    });
    assert_eq!(
        results,
        [
            ("passes", LoopRunResult::Completed, Some("0".to_owned())),
            ("fails", LoopRunResult::Failed, Some("3".to_owned())),
            ("hangs", LoopRunResult::Failed, Some("timeout".to_owned())),
        ]
    );
    for verb in ["show", "logs"] {
        let shown = env.rimz().args(["loop", verb, "fails"]).output().unwrap();
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(
            shown.contains("exit 3") && !shown.contains("exit 1"),
            "{verb}: the command's own exit code: {shown}"
        );
        assert!(
            shown.contains("command: fail · exit 3 · "),
            "{verb}: the command's journal line: {shown}"
        );
        assert_eq!(
            (
                shown.matches("│ gone-wrong").count(),
                shown.matches("  output: ").count()
            ),
            (1, 1),
            "{verb}: the command's output once: {shown}"
        );
    }
    let strikes = read_loop_strikes(&env);
    assert_eq!(strikes.get(&machine_task_key("passes")), None);
    assert_eq!(strikes.get(&machine_task_key("fails")), Some(&1));
    assert_eq!(strikes.get(&machine_task_key("hangs")), Some(&1));
}

#[cfg(unix)]
#[test]
fn once_per_checkout_does_not_refire_after_exit_zero_and_does_after_a_failure() {
    let env = live_room();
    let code = env.home_root.join("code");
    let fired = env.home_root.join("fired");
    std::fs::write(&code, "1").unwrap();
    let (fired_arg, code_arg) = (fired.to_str().unwrap(), code.to_str().unwrap());
    let add = |flag: &[&str]| {
        let argv = [
            &[
                "loop",
                "add",
                "once",
                "--cron",
                "0 * * * *",
                "--timeout",
                "1m",
            ][..],
            flag,
            &[
                "--",
                "sh",
                "-c",
                r#"echo fired >> "$1"; exit "$(cat "$2")""#,
            ],
            &["sh", fired_arg, code_arg],
        ]
        .concat();
        loop_ok(&env, &argv);
    };
    add(&["--once-per-checkout"]);
    assert_eq!(
        stored_rows(&env)["once"].get("once-per-checkout"),
        Some(&toml::Value::Boolean(true))
    );
    // A one-shot's row is gone after its fire, so the flag is refused and nothing is written.
    let (_, stderr) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "spent",
            "--at",
            "5m",
            "--once-per-checkout",
            "--",
            "true",
        ],
    );
    assert!(stderr.contains("cannot be used on a one-shot"), "{stderr}");
    assert!(!stored_rows(&env).contains_key("spent"));
    assert!(!loop_instances_path(&env).exists());
    let fires = || {
        std::fs::read_to_string(&fired)
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert_eq!(run_and_report(&env, "once").result, LoopRunResult::Failed);
    assert_eq!(run_and_report(&env, "once").result, LoopRunResult::Failed);
    assert_eq!(fires(), 2, "a failed fire is not remembered");
    std::fs::write(&code, "0").unwrap();
    assert_eq!(
        run_and_report(&env, "once").result,
        LoopRunResult::Completed
    );
    // A remembered checkout starts no run and writes no row; the decision is synchronous.
    let before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "once"]);
    assert_eq!(read_loop_run_records(&env).len(), before);
    assert_eq!(fires(), 3, "an exit-0 fire is remembered for its checkout");
    add(&[]);
    assert_eq!(
        stored_rows(&env)["once"].get("once-per-checkout"),
        None,
        "a re-add without the flag drops it"
    );
}

#[test]
fn a_strict_row_refusal_stands_alone_in_each_of_the_three_stores() {
    let env = live_room();
    let fired = env.home_root.join("fired");
    write_loop_config(
        &env,
        &format!(
            "[tasks.old-machine]\nroot = {root:?}\ncron = '0 * * * *'\nagent = 'codex'\nprompt = 'fix'\n\
             [tasks.every-machine]\nroot = {root:?}\nevery = '1h'\ntimeout = '1m'\nargs = ['true']\n\
             [tasks.signal-machine]\nroot = {root:?}\nsignal = 'ci.failed'\nrepeat = true\ntimeout = '1m'\nargs = ['true']\n\
             [tasks.good]\nroot = {root:?}\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['touch', {fired:?}]\n",
            root = env.project_root,
            fired = fired.display().to_string()
        ),
    );
    std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
    std::fs::write(
        env.project_root.join(".rimz/config.toml"),
        "[tasks.old-project]\ncron = '0 * * * *'\ncheck = 'true'\n\
         [tasks.every-project]\nevery = '1h'\ntimeout = '1m'\nargs = ['true']\n\
         [tasks.signal-project]\nsignal = 'ci.failed'\nrepeat = true\ntimeout = '1m'\nargs = ['true']\n\
         [tasks.shared]\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['true']\n",
    )
    .unwrap();
    grant_project_trust(&env);
    let instances = loop_instances_path(&env);
    std::fs::create_dir_all(instances.parent().unwrap()).unwrap();
    let old_instance = json!({"root": env.project_root, "every": "1h", "stay": true});
    let shell = |trigger: &str, value: &str| json!({"root": env.project_root, trigger: value, "timeout": "1m", "args": ["true"]});
    std::fs::write(
        &instances,
        serde_json::to_vec(&json!({
            "old-instance": old_instance,
            "every-instance": shell("every", "1h"),
            "signal-instance": shell("signal", "ci.failed"),
        }))
        .unwrap(),
    )
    .unwrap();

    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.status.success(), "{text}");
    for name in ["old-machine", "old-project", "old-instance"] {
        assert!(
            text.contains(&format!("loop task `{name}` is not a row this RimZ takes"))
                && text.contains("delete the task and re-add it with `rimz loop add`"),
            "{name}: {text}"
        );
    }
    // A row on `every`, or on `signal` outside a team binding, is refused alone, and the
    // refusal names the trigger that takes its place.
    for store in ["machine", "project", "instance"] {
        for (trigger, detail) in [
            (
                "every",
                "a task no longer fires on `every`: write the interval as a `cron` expression \
                 (`*/30 * * * *` for every 30 minutes)",
            ),
            (
                "signal",
                "a task no longer fires on `signal`: fire on a state with `when`, or have a \
                 workflow `subscribe` to the signal",
            ),
        ] {
            let name = format!("{trigger}-{store}");
            assert!(
                text.contains(&format!(
                    "loop task `{name}` is not a row this RimZ takes ({detail}); delete the task \
                     and re-add it with `rimz loop add`"
                )),
                "{name}: {text}"
            );
        }
    }
    assert!(text.contains("good") && text.contains("shared"), "{text}");
    let shown = loop_ok(&env, &["loop", "show", "old-machine"]);
    let headline = shown.lines().next().unwrap_or_default();
    assert!(
        headline.starts_with(
            "old-machine · one-off · invalid: loop task `old-machine` is not a row this RimZ takes"
        ) && headline.contains("delete the task and re-add it with `rimz loop add`"),
        "{shown}"
    );
    for output in [&text, &shown] {
        assert!(
            !output.contains("firing time") && !output.contains("invalid trigger"),
            "no trigger fix for a refused row: {output}"
        );
    }
    assert_eq!(
        shown.matches("is not a row this RimZ takes").count(),
        1,
        "the refusal once: {shown}"
    );

    assert_eq!(
        run_and_report(&env, "good").result,
        LoopRunResult::Completed
    );
    assert!(
        fired.exists(),
        "the valid row beside a refused one still fires"
    );
    loop_ok(
        &env,
        &["loop", "add", "fresh", "--cron", "0 * * * *", "--", "true"],
    );
    let rows = stored_rows(&env);
    assert!(
        rows.contains_key("fresh")
            && rows.contains_key("old-machine")
            && rows["every-machine"]
                .get("every")
                .and_then(toml::Value::as_str)
                == Some("1h")
            && rows["signal-machine"]
                .get("signal")
                .and_then(toml::Value::as_str)
                == Some("ci.failed"),
        "{rows:?}"
    );
    assert_eq!(
        rows["old-machine"]
            .get("agent")
            .and_then(toml::Value::as_str),
        Some("codex"),
        "a refused row keeps its values through a rewrite"
    );
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&instances).unwrap()).unwrap();
    assert_eq!(kept["old-instance"], old_instance);
    assert_eq!(kept["every-instance"], shell("every", "1h"));
    assert_eq!(kept["signal-instance"], shell("signal", "ci.failed"));
}

#[test]
fn a_mistyped_project_row_refuses_alone_beside_trust_and_its_siblings() {
    let env = live_room();
    std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
    std::fs::write(
        env.project_root.join(".rimz/config.toml"),
        "[tasks.typo]\ncron = '0 * * * *'\ntimeout = '1m'\ncommand = 'cargo test'\n\
         [tasks.shared]\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['true']\n",
    )
    .unwrap();
    let status = loop_ok(&env, &["trust", "status"]);
    assert!(status.contains("untrusted"), "{status}");
    grant_project_trust(&env);
    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.status.success(), "{text}");
    assert!(text.contains("shared"), "{text}");
    assert!(text.contains("loop task `typo`"), "{text}");
    loop_ok(&env, &["loop", "enable", "shared"]);
    assert_eq!(
        run_and_report(&env, "shared").result,
        LoopRunResult::Completed
    );
}

/// A row stored with the retired `fresh-worktree` key refuses alone as an unknown key, naming
/// the re-add, while the row beside it still loads and fires.
#[test]
fn a_stored_fresh_worktree_row_refuses_alone_beside_its_neighbour() {
    let env = live_room();
    let fired = env.home_root.join("fired");
    write_loop_config(
        &env,
        &format!(
            "[tasks.nightly]\nroot = {root:?}\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['true']\n\
             fresh-worktree = 'night'\n\
             [tasks.good]\nroot = {root:?}\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['touch', {fired:?}]\n",
            root = env.project_root,
            fired = fired.display().to_string()
        ),
    );
    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.status.success(), "{text}");
    assert!(
        text.contains(
            "loop task `nightly` is not a row this RimZ takes (unknown field `fresh-worktree`"
        ) && text.contains("delete the task and re-add it with `rimz loop add`"),
        "{text}"
    );
    assert!(text.contains("good"), "{text}");
    assert_eq!(
        run_and_report(&env, "good").result,
        LoopRunResult::Completed
    );
    assert!(fired.exists(), "the neighbour of a refused row still fires");
}

/// A row with an `at` key, and one with an `every` key, each list as a refused row while the
/// row beside them still loads and fires.
#[test]
fn clock_rows_outside_fire_at_every_and_cron_list_as_invalid_beside_a_valid_row() {
    let env = live_room();
    let fired = env.home_root.join("fired");
    write_loop_config(
        &env,
        &format!(
            "[tasks.morning]\nroot = {root:?}\nat = '07:00'\ntimeout = '1m'\nargs = ['true']\n\
             [tasks.weekdays]\nroot = {root:?}\nevery = 'weekday'\ntimeout = '1m'\nargs = ['true']\n\
             [tasks.good]\nroot = {root:?}\ncron = '0 * * * *'\ntimeout = '1m'\nargs = ['touch', {fired:?}]\n",
            root = env.project_root,
            fired = fired.display().to_string()
        ),
    );
    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.status.success(), "{text}");
    assert!(
        text.contains("loop task `morning` is not a row this RimZ takes (unknown field `at`"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("weekdays")
                && line.contains(
                    "invalid: loop task `weekdays` is not a row this RimZ takes (a task no longer \
                     fires on `every`"
                )),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("good") && !line.contains("invalid")),
        "{text}"
    );
    assert_eq!(
        run_and_report(&env, "good").result,
        LoopRunResult::Completed
    );
    assert!(
        fired.exists(),
        "the neighbour of an invalid row still fires"
    );
}

/// The exec request of the agent pane a launch under a fire opened, from its traced layout.
#[cfg(unix)]
pub(super) fn launched_pane_request(
    env: &Env,
    task: &str,
    trace: &Path,
) -> rimz::harness::launch::ExecRequest {
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let text = loop {
        let text = std::fs::read_to_string(trace).unwrap_or_default();
        if text.contains("layout-json\t") {
            break text;
        }
        if Instant::now() >= deadline {
            let logs = env.rimz().args(["loop", "logs", task]).output().unwrap();
            panic!(
                "no launch was traced: {text}\n{}{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let layout: String = serde_json::from_str(
        text.lines()
            .find_map(|line| line.strip_prefix("layout-json\t"))
            .unwrap(),
    )
    .unwrap();
    let layout: kdl::KdlDocument = layout.parse().unwrap();
    let argv = request_file_args(&layout).expect("agent pane args");
    let request = argv
        .windows(2)
        .find(|pair| pair[0] == "--request-file")
        .unwrap();
    let payload = std::fs::read_to_string(&request[1]).unwrap();
    rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap()
}

#[cfg(unix)]
fn request_file_args(document: &kdl::KdlDocument) -> Option<Vec<String>> {
    document.nodes().iter().find_map(|node| {
        let children = node.children()?;
        if node.name().value() == "pane"
            && let Some(args) = children.get("args")
        {
            let args = args
                .entries()
                .iter()
                .map(|entry| entry.value().as_string().unwrap().to_owned())
                .collect::<Vec<_>>();
            if args.iter().any(|arg| arg == "--request-file") {
                return Some(args);
            }
        }
        request_file_args(children)
    })
}

/// Agent `a` books `rimzd`, whose command launches a codex agent; a firer at the chain limit
/// runs the fire with the launch `flags`. The launch carries `a`'s chain and none of the
/// firer's ancestry, and a booking the launched agent makes carries `[launched, a]`.
#[cfg(unix)]
fn a_fired_launch_carries_the_booking_chain(flags: &[&str]) -> rimz::harness::launch::ExecRequest {
    use crate::common::CommandTimeoutExt;

    let env = live_room();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    seed_agent_launch(&env, &env.project_root, "a", Default::default(), None);
    seed_agent_launch(
        &env,
        &env.project_root,
        "firer",
        rimz::agents::LaunchParams {
            launch_depth: Some(3),
            ..Default::default()
        },
        None,
    );
    let root = env.project_root.to_str().unwrap();
    let rimz_bin = env.rimz_bin();
    let mut book = vec![
        "--root",
        root,
        "loop",
        "add",
        "rimzd",
        "--cron",
        "0 * * * *",
        "--timeout",
        "30s",
        "--",
        "sh",
        "-c",
        r#"exec "$@""#,
        "sh",
    ];
    book.extend([
        rimz_bin.to_str().unwrap(),
        "--mux",
        "zellij",
        "agents",
        "codex",
    ]);
    book.extend(flags);
    book.push("repair");
    let booked = calling_loop(&env, "launch_a").args(&book).output().unwrap();
    assert!(
        booked.status.success(),
        "{}",
        String::from_utf8_lossy(&booked.stderr)
    );
    let trace = env.home_root.join("fire-launch.log");
    let panes = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(panes).unwrap(),
    );
    let fired = calling_loop(&env, "launch_firer")
        .args(["--mux", "zellij", "loop", "run", "rimzd"])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
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
    let request = launched_pane_request(&env, "rimzd", &trace);
    let params = serde_json::to_value(&request.identity.params).unwrap();
    assert_eq!(
        params["loop_booked_by"],
        json!([{"kind": "claude", "agent_id": "launch_a"}]),
        "{params}"
    );
    assert!(
        request.identity.params.launched_by.is_none()
            && request.identity.params.launch_depth.is_none(),
        "the firer's ancestry stays out of the fire's launch: {params}"
    );

    let launched = request.identity.launch_id.clone().unwrap();
    let rebooked = env
        .rimz()
        .args([
            "--root",
            root,
            "loop",
            "add",
            "after",
            "--cron",
            "0 * * * *",
            "--",
            "true",
        ])
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_ID", &launched)
        .output()
        .unwrap();
    assert!(
        rebooked.status.success(),
        "{}",
        String::from_utf8_lossy(&rebooked.stderr)
    );
    let chain = stored_rows(&env)["after"]["booked-by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["agent_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(chain, [launched.as_str(), "launch_a"], "the chain travels");
    request
}

/// A script's room launch under a fire launches and returns, as from a shell, stamped with the
/// fire: a plain, a detached and a `--bg` launch alike, none a supervised run.
#[cfg(unix)]
#[test]
fn a_script_launch_under_a_fire_launches_and_returns_stamped_without_a_run() {
    for flags in [&[][..], &["--detach"], &["--bg"]] {
        let request = a_fired_launch_carries_the_booking_chain(flags);
        let params = &request.identity.params;
        assert_eq!(params.loop_task.as_deref(), Some("rimzd"), "{flags:?}");
        assert!(
            params.loop_fire.is_some(),
            "{flags:?}: the launch carries its fire"
        );
        assert!(request.run_id.is_none(), "{flags:?}: no supervised run");
    }
}

#[cfg(unix)]
#[test]
fn loop_stop_ends_a_running_command_fire_and_records_it_canceled() {
    let env = live_room();
    let pid = env.home_root.join("pid");
    let command = format!("echo $$ > {:?}; exec sleep 347", pid.display().to_string());
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stopme",
            "--cron",
            "0 * * * *",
            "--timeout",
            "5m",
            "--",
            "sh",
            "-c",
            &command,
        ],
    );
    let before = run_mark(&env, "stopme");
    loop_ok(&env, &["loop", "fire", "stopme"]);
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let pid: u32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "the fire's command never started"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let stopped = loop_ok(&env, &["loop", "stop", "stopme"]);
    assert!(stopped.contains("loop `stopme`: stopped"), "{stopped}");
    assert_eq!(
        wait_for_loop_report_after(&env, "stopme", before).result,
        LoopRunResult::Canceled
    );
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while rimz::proc::process_is_live(pid, None) {
        assert!(
            Instant::now() < deadline,
            "the fire's command outlived the stop"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A fire whose command made a foreground `-p` launch: the run carries the fire's id, and
/// `rimz loop stop` cancels it with the fire. Any other end of the fire leaves the run to its
/// own rules, as a run started from a shell.
#[cfg(unix)]
#[test]
fn loop_stop_cancels_a_print_launch_a_fire_made() {
    let env = live_room();
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
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
            .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        command
    };
    let rimz_bin = env.rimz_bin();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "spawn",
            "--cron",
            "*/15 * * * *",
            "--timeout",
            "10m",
            "--",
            "sh",
            "-c",
            r#"exec "$@""#,
            "sh",
            rimz_bin.to_str().unwrap(),
            "--mux",
            "zellij",
            "agents",
            "codex",
            "-p",
            "fix it",
        ],
    );
    let before = run_mark(&env, "spawn");
    let fired = command().args(["loop", "fire", "spawn"]).output().unwrap();
    assert!(fired.status.success(), "{fired:?}");
    let paths = env.state_path_for(&env.project_root);
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let run = loop {
        let records = rimz::harness::run::list(&paths).unwrap_or_default();
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            // The trace mux opens no provider process, so supply its startup hook.
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "loop-spawn-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                env.drain_hooks();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the launch's Running run: {}",
            String::from_utf8_lossy(
                &env.rimz()
                    .args(["loop", "logs", "spawn"])
                    .output()
                    .unwrap()
                    .stdout
            )
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(run.loop_task.as_deref(), Some("spawn"), "{run:?}");
    let fire = run
        .loop_fire
        .clone()
        .expect("the launch names the fire that owns it");
    let snapshot = env.store().snapshot().unwrap();
    let row = snapshot
        .agents
        .iter()
        .find(|agent| agent.name.as_deref() == run.agent_name.as_deref())
        .expect("the launch's agent row");
    assert_eq!(
        row.loop_task.as_deref(),
        Some("spawn"),
        "the blocking launch's agent row names its task"
    );
    assert_eq!(row.loop_fire.as_ref(), Some(&fire), "and carries its fire");
    let stopped = command().args(["loop", "stop", "spawn"]).output().unwrap();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert_eq!(
        wait_for_loop_report_after(&env, "spawn", before).result,
        LoopRunResult::Canceled
    );
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let status = rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status;
        if status == RunStatus::Canceled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the fire's launch was not canceled by loop stop: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
