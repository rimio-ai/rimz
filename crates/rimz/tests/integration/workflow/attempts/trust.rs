//! The start's trust read against the launch it stands for, and the mid-run trust wait on what
//! the start could not see: a generated checkout, a named account, the run's scope root.

use super::*;

const GENERATED: &str = "worktree=''\nadmission.occupancy='share'\n";

/// The checkouts git lists for the room's repository, the main one included.
fn checkouts(room: &Room) -> usize {
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&room.env.project_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

/// Holds the `pr` check until `release` exists, so the test changes trust between the start
/// and the first launch.
fn hold_the_check(room: &Room, release: &std::path::Path) {
    fs::write(
        room.env.rimz_home().join("workflows/review/checks/pr.sh"),
        format!(
            "if grep -q '^  \"node\": \"pr\"' \"$RIMZ_WORKFLOW_DIR/context.json\"; then while [ ! -e '{}' ]; do sleep 0.05; done; printf '{{\"outcome\":\"go\",\"key\":7}}'; else printf '{{\"outcome\":\"done\"}}'; fi\n",
            release.display()
        ),
    )
    .unwrap();
}

/// Adds the codex account `ci` with its hooks installed and trusted and the project trusted,
/// and returns its home.
fn ci_account(room: &Room) -> PathBuf {
    let home = room.env.home_root.join("ci");
    let added = room
        .env
        .rimz()
        .args(["accounts", "add", "codex", "ci", "--home"])
        .arg(&home)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let config = home.join("config.toml");
    rimz::agents::trust_hook_config_for_test(&config, &["Interrupt"]).unwrap();
    let projects: toml::Table = fs::read_to_string(room.env.agent_config_path("codex"))
        .unwrap()
        .parse()
        .unwrap();
    let mut table: toml::Table = fs::read_to_string(&config).unwrap().parse().unwrap();
    table.insert("projects".into(), projects["projects"].clone());
    fs::write(&config, toml::to_string(&table).unwrap()).unwrap();
    home
}

/// Drops every RimZ hook from the account home at `home`, keeping its folder trust.
fn drop_hooks(home: &std::path::Path) {
    let config = home.join("config.toml");
    let table: toml::Table = fs::read_to_string(&config).unwrap().parse().unwrap();
    let kept = toml::Table::from_iter(table.into_iter().filter(|(key, _)| key == "projects"));
    fs::write(&config, toml::to_string(&kept).unwrap()).unwrap();
}

/// A step with a generated worktree has no checkout to judge at the start, so its folder has
/// no verdict there, while its hooks are still read.
#[test]
fn a_generated_worktree_step_starts_and_its_hooks_are_still_read() {
    let room = Room::with_kind(GENERATED, "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    let hooks = room.env.agent_config_path("claude");
    let installed = fs::read(&hooks).unwrap();
    fs::remove_file(&hooks).unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(
        stderr.contains(
            "\n  - claude hooks are missing, so the supervised turn cannot report completion; run `rimz hooks install claude` (blocks build)\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("worktree"), "{stderr}");
    fs::write(&hooks, installed).unwrap();
    let id = room.start();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

/// A generated-worktree step waiting on trust creates no checkout at each recheck.
#[test]
fn a_trust_wait_on_a_generated_worktree_step_leaves_no_checkout_behind() {
    if crate::common::git::git_missing() {
        return;
    }
    let room = Room::with_kind(GENERATED, "claude");
    crate::common::git::init_repo(&room.env.project_root);
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    let release = room.env.project_root.join("release-pr");
    hold_the_check(&room, &release);
    let id = room.start();
    fs::remove_file(room.env.agent_config_path("claude")).unwrap();
    fs::write(&release, "").unwrap();
    until(&room.env, &id, |v| {
        v["record"]["wait"]["outcome"] == "trust"
    });
    assert_eq!(checkouts(&room), 1);
    for count in 2..=3 {
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        let value = until(&room.env, &id, |v| v["record"]["wait"]["count"] == count);
        assert_eq!(value["record"]["wait"]["outcome"], "trust", "{value:#}");
        assert_eq!(checkouts(&room), 1, "{value:#}");
    }
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

/// A step pinned to a named account reads that account's hooks at the start, with the
/// account's own repair.
#[test]
fn a_named_accounts_missing_hooks_refuse_the_start_with_the_accounts_repair() {
    let room = Room::new("account='ci'\n");
    let home = ci_account(&room);
    drop_hooks(&home);
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(
        stderr.contains(&format!(
            "\n  - RimZ hooks are missing for codex account `ci` at `{}`; run `rimz accounts add codex ci` (blocks build)\n",
            home.display()
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("rimz hooks install"), "{stderr}");
}

/// A named account whose hooks break after the start waits on `trust` mid-run with the
/// account's repair, counting no attempt or visit.
#[test]
fn a_named_accounts_hooks_broken_mid_run_wait_on_trust() {
    let room = Room::new("account='ci'\n");
    let home = ci_account(&room);
    let release = room.env.project_root.join("release-pr");
    hold_the_check(&room, &release);
    let id = room.start();
    drop_hooks(&home);
    fs::write(&release, "").unwrap();
    let value = until(&room.env, &id, |v| {
        v["record"]["wait"]["outcome"] == "trust" || v["record"]["state"] != "active"
    });
    assert_eq!(value["record"]["state"], "active", "{value:#}");
    assert_eq!(
        value["record"]["wait"]["detail"],
        format!(
            "RimZ hooks are missing for codex account `ci` at `{}`; run `rimz accounts add codex ci`",
            home.display()
        ),
        "{value:#}"
    );
    assert!(value["record"]["attempt"].is_null(), "{value:#}");
    assert!(value["record"]["prev_attempt"].is_null(), "{value:#}");
    assert!(value["record"]["visits"]["build"].is_null(), "{value:#}");
    assert!(events(&value, "attempt").is_empty(), "{value:#}");
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

/// A step pinned to a named account with no folder decision is told to answer the prompt on
/// that account, since `rimz trust grant` writes only the room's accounts; the decision that
/// answer saves in the account's own home clears the refusal.
#[test]
fn a_named_accounts_undecided_folder_names_the_account_and_its_prompt() {
    if crate::common::git::git_missing() {
        return;
    }
    let room = Room::new("account='ci'\n");
    crate::common::git::init_repo(&room.env.project_root);
    let home = ci_account(&room);
    let config = home.join("config.toml");
    let decided = fs::read_to_string(&config).unwrap();
    let mut table: toml::Table = decided.parse().unwrap();
    table.remove("projects");
    fs::write(&config, toml::to_string(&table).unwrap()).unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    let line = stderr
        .lines()
        .find(|line| line.contains("no trust decision"))
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(
        line.starts_with("  - codex account `ci` has no trust decision")
            && line.ends_with(
                "answer codex's folder-trust prompt there once on account `ci`: run `rimz agents codex --account ci` there (blocks build)"
            ),
        "{stderr}"
    );
    assert!(!stderr.contains("rimz trust grant"), "{stderr}");
    fs::write(&config, decided).unwrap();
    let id = room.start();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

/// A start that launches no agent reads no account or agent config, so a malformed project
/// config refuses only a start with an agent node in scope.
#[test]
fn an_agentless_start_runs_under_a_malformed_project_config() {
    let room = Room::new("");
    let workflows = room.env.rimz_home().join("workflows");
    let review = fs::read_to_string(workflows.join("review/workflow.toml")).unwrap();
    let (head, tail) = review.split_once("[agent.build]").unwrap();
    let scripts = format!(
        "{}{}",
        head.replace("on.go = 'build'", "on.go = 'after'"),
        &tail[tail.find("[script.after]").unwrap()..]
    );
    fs::create_dir_all(workflows.join("plain/checks")).unwrap();
    fs::write(workflows.join("plain/workflow.toml"), scripts).unwrap();
    fs::copy(
        workflows.join("review/checks/pr.sh"),
        workflows.join("plain/checks/pr.sh"),
    )
    .unwrap();
    let config = room.env.project_root.join(".rimz/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[[[ not toml").unwrap();
    let id = success(
        room.command(&["workflow", "run", "plain"])
            .output()
            .unwrap(),
    )
    .lines()
    .next()
    .unwrap()
    .to_owned();
    settled(&room.env, &id, "done");
    let refused = room
        .command(&["workflow", "run", "review", "--prompt", "Review this"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&config.display().to_string()), "{stderr}");
}

/// A `scope = "project"` run launches at the project root, so the start reads Codex's folder
/// trust there, not at the linked worktree it is started from.
#[test]
fn a_project_scope_start_reads_folder_trust_at_the_project_root() {
    if crate::common::git::git_missing() {
        return;
    }
    for root_trusted in [true, false] {
        let room = Room::new("");
        let definition = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &definition,
            fs::read_to_string(&definition)
                .unwrap()
                .replace("scope = 'worktree'", "scope = 'project'"),
        )
        .unwrap();
        crate::common::git::init_repo(&room.env.project_root);
        let linked = room.env.home_root.join("linked");
        let added = std::process::Command::new("git")
            .arg("-C")
            .arg(&room.env.project_root)
            .args(["worktree", "add", "-q", "-b", "linked"])
            .arg(&linked)
            .output()
            .unwrap();
        assert!(added.status.success(), "{added:?}");
        let level = |trusted: bool| if trusted { "trusted" } else { "untrusted" };
        let config = room.env.agent_config_path("codex");
        let mut table: toml::Table = fs::read_to_string(&config).unwrap().parse().unwrap();
        table.insert(
            "projects".into(),
            toml::Value::Table(toml::Table::from_iter([
                (
                    room.env
                        .project_root
                        .canonicalize()
                        .unwrap()
                        .display()
                        .to_string(),
                    toml::Value::Table(toml::Table::from_iter([(
                        "trust_level".to_owned(),
                        level(root_trusted).into(),
                    )])),
                ),
                (
                    linked.canonicalize().unwrap().display().to_string(),
                    toml::Value::Table(toml::Table::from_iter([(
                        "trust_level".to_owned(),
                        level(!root_trusted).into(),
                    )])),
                ),
            ])),
        );
        fs::write(&config, toml::to_string(&table).unwrap()).unwrap();
        let mut command = room.command(&["workflow", "run", "review", "--prompt", "Review this"]);
        let output = command.current_dir(&linked).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if root_trusted {
            assert!(output.status.success(), "{stderr}");
            let id = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap()
                .to_owned();
            success(room.command(&["workflow", "cancel", &id]).output().unwrap());
        } else {
            assert_eq!(output.status.code(), Some(1), "{stderr}");
            assert!(
                stderr.contains("workflow start refused; nothing was created:")
                    && stderr.contains("saved untrusted")
                    && stderr.contains("(blocks build)"),
                "{stderr}"
            );
            assert!(
                room.env.store().paths().workflow_runs().unwrap().is_empty(),
                "{stderr}"
            );
        }
    }
}

/// A trust wait's age and its place under NEEDS YOU come from the wait's first stamp, which
/// rechecks keep, not from the node's entry, which each recheck moves. A run waiting on trust
/// holds no start-queue ticket, so a second start in its scope is admitted beside it.
#[test]
fn a_trust_waits_age_and_order_keep_its_first_stamp_across_rechecks() {
    let room = Room::with_kind("", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    let release = room.env.project_root.join("release-pr");
    hold_the_check(&room, &release);
    let workflows = room.env.rimz_home().join("workflows");
    fs::create_dir_all(workflows.join("other/checks")).unwrap();
    for file in ["workflow.toml", "prompt.md", "checks/pr.sh"] {
        fs::copy(
            workflows.join("review").join(file),
            workflows.join("other").join(file),
        )
        .unwrap();
    }
    let older = room.start();
    let newer = success(
        room.command(&["workflow", "run", "other", "--prompt", "Review this"])
            .output()
            .unwrap(),
    )
    .lines()
    .next()
    .unwrap()
    .to_owned();
    fs::remove_file(room.env.agent_config_path("claude")).unwrap();
    fs::write(&release, "").unwrap();
    // Both launches can meet in the queue, where whichever is admitted second waits `capacity`
    // behind the other's in-flight turn for as long as that turn holds its ticket; the fixture
    // runs no tick, so advances stand in for its rechecks until the wait moves to `trust`.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    for id in [&older, &newer] {
        loop {
            let value = until(&room.env, id, |v| {
                ["trust", "capacity"]
                    .contains(&v["record"]["wait"]["outcome"].as_str().unwrap_or(""))
            });
            if value["record"]["wait"]["outcome"] == "trust" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{value:#}");
            std::thread::sleep(Duration::from_millis(50));
            success(room.command(&["workflow", "advance", id]).output().unwrap());
        }
    }
    let queue = room.env.rimz_home().join("loops/throttle");
    let tickets: Vec<_> = fs::read_dir(&queue)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default();
    assert!(tickets.is_empty(), "{tickets:?}");
    {
        let state = room.env.state_path_for(&room.env.project_root);
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        let paths = state.workflow(&older.parse().unwrap());
        let mut record = records::load(&paths).unwrap();
        record.wait.as_mut().unwrap().first -= Duration::from_secs(3 * 3600);
        records::write(&paths, &record, &guard).unwrap();
    }
    for count in 2..=3 {
        success(
            room.command(&["workflow", "advance", &older])
                .output()
                .unwrap(),
        );
        until(&room.env, &older, |v| v["record"]["wait"]["count"] == count);
    }
    let listed = success(room.command(&["workflow", "list"]).output().unwrap());
    let line = |name: &str| {
        listed
            .lines()
            .position(|line| line.contains(&format!("{name}#")))
            .unwrap_or_else(|| panic!("{name}: {listed}"))
    };
    assert!(line("review") < line("other"), "{listed}");
    assert!(
        listed.lines().nth(line("review")).unwrap().ends_with(" 3h"),
        "{listed}"
    );
    for id in [&older, &newer] {
        success(room.command(&["workflow", "cancel", id]).output().unwrap());
    }
}

/// A second start on a slot a live run holds names that run before any trust read, even when
/// the trust read would refuse it.
#[test]
fn a_held_slot_refuses_a_second_start_before_its_trust_read() {
    let room = Room::with_kind("", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    hold_the_check(&room, &room.env.project_root.join("release-pr"));
    let id = room.start();
    fs::remove_file(room.env.agent_config_path("claude")).unwrap();
    let second = room
        .command(&["workflow", "run", "review", "--prompt", "Review this"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(!second.status.success(), "{stderr}");
    assert!(stderr.contains(&id), "{stderr}");
    assert!(!stderr.contains("workflow start refused"), "{stderr}");
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

/// A cell whose account cannot be resolved lists that error beside the problems the node's
/// other cells found.
#[test]
fn an_unresolvable_cell_login_keeps_the_nodes_other_problems() {
    let room = Room::with_kind("", "claude");
    let machine = room.env.rimz_home().join("config.toml");
    let mut config = fs::read_to_string(&machine).unwrap_or_default();
    config.push_str("\n[accounts.use]\ncodex = 'ghost'\n");
    fs::write(&machine, config).unwrap();
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'claude'", "agent = 'claude,codex'"),
    )
    .unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(stderr.contains("ghost"), "{stderr}");
    assert!(
        stderr.contains("claude has no trust decision and would stop at its trust prompt"),
        "{stderr}"
    );
}
