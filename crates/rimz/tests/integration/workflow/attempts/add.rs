use super::*;

fn git(root: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn loop_add_workflow_writes_a_task_that_fires_per_owned_checkout() {
    let room = Room::new("");
    git(&room.env.project_root, &["init", "-q", "-b", "main"]);
    git(
        &room.env.project_root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    fs::write(
        room.env.rimz_home().join("workflows/review/workflow.toml"),
        format!("{HEADER}{WAIT}"),
    )
    .unwrap();
    fs::write(
        room.env.rimz_home().join("workflows/review/checks/pr.sh"),
        PENDING,
    )
    .unwrap();
    for lane in ["one", "two"] {
        success(room.command(&["worktree", "new", lane]).output().unwrap());
    }
    let receipt = success(
        room.command(&[
            "loop",
            "add",
            "review",
            "--workflow",
            "review",
            "--every",
            "1h",
            "--each-worktree",
            "--label",
            "keep CI green",
            "--budget-per-day",
            "5",
            "--surplus",
            "2x",
        ])
        .output()
        .unwrap(),
    );
    assert!(
        receipt.contains("action: starts workflow review"),
        "{receipt}"
    );
    let stored: toml::Table =
        toml::from_str(&fs::read_to_string(room.env.rimz_home().join("loop.toml")).unwrap())
            .unwrap();
    let task = stored["tasks"]["review"].as_table().unwrap();
    assert_eq!(task["workflow"].as_str(), Some("review"), "{task:?}");
    assert_eq!(task["every"].as_str(), Some("1h"), "{task:?}");
    assert_eq!(task["each-worktree"].as_bool(), Some(true), "{task:?}");
    assert_eq!(task["label"].as_str(), Some("keep CI green"), "{task:?}");
    assert_eq!(task["budget-per-day"].as_str(), Some("$5.00"), "{task:?}");
    assert_eq!(task["surplus"].as_str(), Some("2x"), "{task:?}");
    for field in ["agent", "wait", "check", "stay", "timeout", "budget"] {
        assert!(!task.contains_key(field), "{field}: {task:?}");
    }
    let owned = rimz::worktree::discover_owned(&room.env.project_root).unwrap();
    let checkout = &owned[0].marker.worktree_path;
    success(
        room.command(&["loop", "run", "review", "--cwd"])
            .arg(checkout)
            .output()
            .unwrap(),
    );
    let runs = every_run(&room.env.state_path_for(&room.env.project_root));
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].task.as_deref(), Some("review"));
    assert_eq!(
        runs[0].scope.path.canonicalize().unwrap(),
        checkout.canonicalize().unwrap()
    );
    success(
        room.command(&["workflow", "cancel", runs[0].id.as_str()])
            .output()
            .unwrap(),
    );
}

#[test]
fn loop_add_workflow_refuses_every_flag_the_fire_would_ignore() {
    let room = Room::new("");
    let config = room.env.rimz_home().join("loop.toml");
    fs::write(&config, "# untouched\n").unwrap();
    let steps = "[agent.<name>";
    let checks = "[test.<name>]";
    let profile = "agent` profile";
    let spend = "set `budget` on the workflow's `[agent.<name>]`";
    let window = "no provider window";
    let fanout = "fan-out needs --when, --every, or --cron";
    let clocked: &[(&[&str], &str, &str)] = &[
        (&["--stay"], "--stay", "agent steps"),
        (
            &["--takeover"],
            "--takeover",
            "[agent.<name>.admission] occupancy",
        ),
        (&["--subscribe", "ci.done"], "--subscribe", "signals"),
        (&["--check", "true"], "--check", checks),
        (&["--check-timeout", "1m"], "--check-timeout", checks),
        (&["--judge", "claude"], "--judge", checks),
        (&["--judge-prompt", "q"], "--judge-prompt", checks),
        (
            &["--judge-prompt-file", "q.md"],
            "--judge-prompt-file",
            checks,
        ),
        (&["--judge-recheck", "1m"], "--judge-recheck", checks),
        (&["--judge-timeout", "1m"], "--judge-timeout", checks),
        (&["--on", "fail"], "--on", checks),
        (&["--verify", "true"], "--verify", checks),
        (&["--max-attempts", "2"], "--max-attempts", checks),
        (
            &["--verify", "true", "--max-attempts", "2"],
            "--max-attempts",
            checks,
        ),
        (&["--throttle", "off"], "--throttle", steps),
        (&["--capacity-wait", "1h"], "--capacity-wait", steps),
        (&["--timeout", "1h"], "--timeout", steps),
        (&["--mode", "auto"], "--mode", profile),
        (&["--effort", "high"], "--effort", profile),
        (&["--fast"], "--fast", profile),
        (&["--no-fast"], "--no-fast", profile),
        (&["--account", "work"], "--account", profile),
        (
            &["--system-prompt-file", "system.md"],
            "--system-prompt-file",
            profile,
        ),
        (&["--budget", "5"], "--budget", spend),
        (
            &["--budget", "5", "--budget-per-day", "5"],
            "--budget",
            spend,
        ),
        (&["--worktree", "lane"], "--worktree", "--root <worktree>"),
    ];
    let triggered: &[(&[&str], &str, &str)] = &[
        (&["--after-reset", "5h"], "--after-reset", window),
        (&["--when", "window.5h.left<50"], "window.", window),
        (
            &["--each-worktree", "--signal", "ci.done"],
            "--signal",
            fanout,
        ),
        (&["--each-worktree", "--in", "5m"], "--in", fanout),
        (&["--each-worktree", "--at", "12:00"], "--at", fanout),
        (
            &["--each-worktree", "--when", "pr=open", "--once"],
            "--once",
            fanout,
        ),
    ];
    let cases = clocked
        .iter()
        .map(|(flags, flag, fix)| ([&["--every", "1h"][..], flags].concat(), flag, fix))
        .chain(
            triggered
                .iter()
                .map(|(flags, flag, fix)| (flags.to_vec(), flag, fix)),
        );
    for (flags, flag, fix) in cases {
        let output = room
            .command(&["loop", "add", "nightly", "--workflow", "review"])
            .args(&flags)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{flags:?}: {stderr}");
        assert!(
            stderr.contains("--workflow") && stderr.contains(*flag) && stderr.contains(*fix),
            "{flags:?}: {stderr}"
        );
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "# untouched\n",
            "{flags:?}"
        );
    }
    // A workflow task follows its overlap policy like any repeating task.
    success(
        room.command(&["loop", "add", "nightly", "--workflow", "review"])
            .args([
                "--every",
                "1h",
                "--overlap",
                "queue",
                "--overlap-limit",
                "2",
            ])
            .output()
            .unwrap(),
    );
}

#[test]
fn loop_add_workflow_refuses_definitions_workflow_run_refuses_with_its_errors() {
    let room = Room::new("");
    let broken = room.env.rimz_home().join("workflows/broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(
        broken.join("workflow.toml"),
        "[workflow]\nscope='worktree'\nstart='missing'\n",
    )
    .unwrap();
    for name in ["absent", "../review", "broken"] {
        let run = room.command(&["workflow", "run", name]).output().unwrap();
        let add = room
            .command(&[
                "loop",
                "add",
                "nightly",
                "--workflow",
                name,
                "--every",
                "1h",
            ])
            .output()
            .unwrap();
        let run_error = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success(), "{name}: {run_error}");
        assert_eq!(add.status.code(), Some(1), "{name}");
        assert_eq!(
            String::from_utf8_lossy(&add.stderr),
            run_error,
            "{name}: loop add prints workflow run's refusal"
        );
    }
    assert!(!room.env.rimz_home().join("loop.toml").exists());
}
