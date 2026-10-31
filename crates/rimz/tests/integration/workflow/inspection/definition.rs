use super::*;

const GRAPH: &str = r#"
[workflow]
summary = 'carry the change to green'
scope = 'worktree'
start = 'begin'
[checks.begin]
shell = 'true'
on.pass = 'sweep'
on.fail = '@user'
[steps.sweep]
shell = 'echo sweep'
timeout = '1h'
then = 'watch'
[checks.watch]
summary = 'wait for the forge'
cases = [{ when = ['ci=passed'], outcome = 'green', for = '3m' }]
else = 'red'
on.green = '@done'
on.red = 'fix'
timeout = '3d'
[steps.fix]
agent = 'claude'
prompt-file = 'prompt.md'
then = 'watch'
timeout = '2h'
takeover = true
max-visits = 4
max-visits-per-key = 2
capacity-wait = '6h'
on.asked = 'watch'
[checks.unused]
shell = 'false'
on.pass = '@done'
on.fail = '@failed'
"#;

fn graph() -> (Env, PathBuf) {
    super::super::fixture(GRAPH, "")
}

#[test]
fn definition_draws_walk_hanging_limits_cases_defaults_and_runs() {
    let (env, _) = graph();
    seed(&env, &env.project_root, &env.project_root, "parked");
    let text = success(command(&env, &["workflow", "show", "review"]));
    for fragment in [
        "review · carry the change to green",
        "one run per worktree · no loop task starts it; by hand: rimz workflow run review -w '<worktree>'",
        "○ begin",
        "○ sweep",
        "○ watch",
        "├─○ fix",
        "wait for the forge",
        "1  ci=passed for 3m",
        "green @done",
        "none true",
        "red → fix",
        "waits ≤3d, then @user",
        "4 visits, then @user",
        "2 visits per key, then @user",
        "takes over",
        "capacity-wait 6h",
        "only when a loop task starts the run",
        "asked ↑ watch",
        "A step goes on to its next check however its turn ends.",
        "A check that cannot run parks at @user.",
        "No deadline is set; a deadline granted to a run parks it at @user.",
        "one node in full: rimz workflow show review begin",
        "source ~/",
        "unreachable from begin",
        "○ unused",
        "NEEDS YOU",
        "review#project",
    ] {
        assert!(text.contains(fragment), "missing {fragment:?}:\n{text}");
    }
    let positions = ["○ begin", "○ sweep", "○ watch", "├─○ fix", "○ unused"]
        .map(|name| text.find(name).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(!text.contains("never:"), "{text}");
    assert!(!text.contains("invalid →"), "{text}");
    assert!(!text.contains("waits ≤1h"), "{text}");
}

#[test]
fn definition_hints_parse_hyphen_leading_workflow_and_node_names() {
    let results = [
        ("review", "-begin"),
        ("-review", "begin"),
        ("-review", "-begin"),
    ]
    .map(|(name, node)| {
        let (env, dir) = graph();
        fs::write(dir.join("workflow.toml"), GRAPH.replace("begin", node)).unwrap();
        fs::rename(&dir, dir.with_file_name(name)).unwrap();
        let text = success(command(&env, &["workflow", "show", "--", name]));
        let node_hint = text
            .lines()
            .find_map(|line| line.strip_prefix("one node in full: "))
            .unwrap()
            .split_once(" · source ")
            .unwrap()
            .0;
        let node_output = copied(&env, node_hint, false);
        let run_hint = text
            .lines()
            .find_map(|line| line.split_once("by hand: "))
            .unwrap()
            .1
            .replace("<worktree>", "missing");
        let run_output = copied(&env, &run_hint, false);
        (
            node_output.status.code(),
            run_output.status.code(),
            String::from_utf8_lossy(&run_output.stderr)
                .contains("rimz worktrees require a git repository"),
            text,
            String::from_utf8_lossy(&node_output.stderr).into_owned(),
            String::from_utf8_lossy(&run_output.stderr).into_owned(),
        )
    });
    assert!(
        results
            .iter()
            .all(|(node, run, worktree, ..)| *node == Some(0) && *run == Some(1) && *worktree),
        "{results:#?}"
    );
}

#[test]
fn the_by_hand_hint_starts_in_the_selected_room() {
    let (env, _) = super::super::fixture(
        "[workflow]\nscope='project'\nstart='gate'\n\
         [checks.gate]\nshell='true'\non={pass='@done',fail='@failed'}\n",
        "",
    );
    let other = env.home_root.join("other/project");
    env.record(&other);
    let text = success(command(
        &env,
        &[
            "--root",
            other.to_str().unwrap(),
            "workflow",
            "show",
            "review",
        ],
    ));
    let hint = text
        .lines()
        .find_map(|line| line.split_once("by hand: "))
        .unwrap()
        .1;
    success(copied(&env, hint, false));
    let output = command(
        &env,
        &[
            "--root",
            other.to_str().unwrap(),
            "workflow",
            "show",
            "review#project",
            "--json",
        ],
    );
    assert!(output.status.success(), "{text}\n{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["record"]["project_root"], json!(other), "{value:#}");
}

#[test]
fn definition_names_loop_triggers_and_sealed_or_granted_deadlines() {
    let (env, dir) = graph();
    fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.carry]\nworkflow='review'\nroot={:?}\nevery='1h'\n",
            env.project_root
        ),
    )
    .unwrap();
    fs::write(
        dir.join("workflow.toml"),
        GRAPH.replace("start = 'begin'", "start = 'begin'\non.deadline = 'unused'"),
    )
    .unwrap();
    let text = success(command(&env, &["workflow", "show", "review"]));
    assert!(
        text.contains("started by loop task carry: every 1h"),
        "{text}"
    );
    assert!(
        text.contains(
            "when a deadline granted to a run passes → unused (a live attempt is canceled)"
        ),
        "{text}"
    );
    assert!(!text.contains("unreachable"), "{text}");
    fs::write(
        dir.join("workflow.toml"),
        GRAPH.replace("start = 'begin'", "start = 'begin'\ndeadline = '2h'"),
    )
    .unwrap();
    let text = success(command(&env, &["workflow", "show", "review"]));
    assert!(
        text.contains("deadline 2h → @user (a live attempt is canceled)"),
        "{text}"
    );
    assert!(!text.contains("No deadline is set"), "{text}");
}

#[test]
fn missing_or_broken_definition_keeps_the_runs_visible_and_json_unchanged() {
    let (env, dir) = graph();
    let run = seed(&env, &env.project_root, &env.project_root, "parked");
    for broken in [false, true] {
        if broken {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("workflow.toml"), "broken = [").unwrap();
        } else {
            fs::remove_dir_all(&dir).unwrap();
        }
        let text = success(command(&env, &["workflow", "show", "review"]));
        assert!(text.contains("definition unavailable"), "{text}");
        assert!(
            text.lines()
                .next()
                .unwrap()
                .contains("rimz workflow validate review"),
            "{text}"
        );
        assert!(text.contains("NEEDS YOU"), "{text}");
        assert_eq!(json_show(&env, "review")[0]["id"], json!(run.id));
    }
}

const NODE_GRAPH: &str = r#"
[workflow]
scope = 'project'
start = 'judge'
[checks.judge]
summary = 'review every routing fact'
agent = 'claude'
prompt-file = 'prompt.md'
window = '5h.left>=25'
wake = [{ signal = 'ci.*', match = { path = '/checkout' } }]
provider = 'claude'
account = 'default'
timeout = '8h'
on.ok = 'fix'
on.invalid = '@failed'
[checks.watch]
cases = [{ when = ['ci=passed', 'pr=open'], outcome = 'green', for = '3m', key = 'lint' }]
else = 'waiting'
on.green = '@done'
on.waiting = '@wait'
[checks.script]
run = 'sh checks/pr.sh'
wake = ['pr.*']
on.ok = '@done'
[checks.command]
shell = "printf 'a\\nb\\n'"
on.pass = '@done'
on.fail = '@failed'
[steps.fix]
summary = 'one bounded round'
agent = 'claude,nvim'
prompt-file = 'prompt.md'
lifecycle = 'turn'
takeover = false
window = '7d.left>=10'
throttle = 'off'
capacity-wait = '6h'
then = 'judge'
max-visits = 4
max-visits-per-key = 2
timeout = '2h'
on.asked = 'watch'
on.cap = '@user'
on.capacity = '@failed'
"#;

#[test]
fn node_view_exposes_check_step_condition_and_command_facts() {
    let (env, _) = super::super::fixture(NODE_GRAPH, "");
    for (node, facts) in [
        (
            "judge",
            vec![
                "judge · check in review · agent",
                "review every routing fact",
                "reached from:",
                "fix by then",
                "wakes on:",
                "ci.*",
                "path",
                "/checkout",
                "minute tick",
                "agent:",
                "claude",
                "prompt-file:",
                "prompt.md",
                "window:",
                "5h.left>=25",
                "provider:",
                "account:",
                "default",
                "timeout:",
                "8h",
                "ok → fix (explicit)",
                "invalid → @failed (explicit, never: a judge retries once, then parks at @user)",
                "deadline → @user (default, only with a deadline)",
            ],
        ),
        (
            "fix",
            vec![
                "fix · step in review · agent",
                "one bounded round",
                "judge by ok",
                "agent:",
                "claude,nvim",
                "prompt-file:",
                "prompt.md",
                "lifecycle:",
                "turn",
                "takeover:",
                "false",
                "window:",
                "7d.left>=10",
                "throttle:",
                "off",
                "capacity-wait:",
                "6h",
                "max-visits:",
                "4",
                "max-visits-per-key:",
                "2",
                "timeout:",
                "2h",
                "then → judge (explicit)",
                "asked → watch (explicit)",
                "cap → @user (explicit)",
                "capacity → @failed (explicit, never: throttle is off)",
            ],
        ),
        (
            "watch",
            vec![
                "watch · check in review · condition",
                "fix by asked",
                "wakes on:",
                "ci.*",
                "pr.*",
                "minute tick",
                "cases:",
                "ci=passed && pr=open",
                "for 3m",
                "key",
                "\"lint\"",
                "green → @done",
                "none true",
                "waiting → @wait",
                "timeout → @user (default, never: no timeout)",
                "unknown → @wait (default)",
            ],
        ),
        (
            "script",
            vec!["run:", "sh checks/pr.sh", "pr.*", "ok → @done (explicit)"],
        ),
        (
            "command",
            vec!["shell:", "printf 'a\\nb\\n'", "pass → @done (explicit)"],
        ),
    ] {
        let text = success(command(&env, &["workflow", "show", "review", node]));
        for fact in facts {
            assert!(text.contains(fact), "{node} missing {fact:?}:\n{text}");
        }
    }
}

#[test]
fn condition_case_note_agrees_in_definition_and_node_views() {
    let (env, dir) = graph();
    let results = [
        (None, "waits"),
        (Some("@wait"), "waits"),
        (Some("@user"), "goes @user"),
        (Some("@done"), "goes @done"),
        (Some("@failed"), "goes @failed"),
        (Some("begin"), "goes ↑ begin"),
        (Some("watch"), "goes ↑ watch"),
        (Some("fix"), "goes → fix"),
    ]
    .map(|(target, action)| {
        let spec = target.map_or_else(
            || GRAPH.to_owned(),
            |target| {
                GRAPH.replace(
                    "on.green = '@done'",
                    &format!("on.green = '@done'\non.unknown = '{target}'"),
                )
            },
        );
        fs::write(dir.join("workflow.toml"), spec).unwrap();
        let note = format!("the first true case wins; an unread term above it {action}");
        let texts = [
            success(command(&env, &["workflow", "show", "review"])),
            success(command(&env, &["workflow", "show", "review", "watch"])),
        ];
        let found = texts.each_ref().map(|text| text.contains(&note));
        let notes = texts.map(|text| {
            text.lines()
                .filter(|line| line.contains("the first true case wins"))
                .collect::<Vec<_>>()
                .join("\n")
        });
        (target, found, notes)
    });
    assert!(
        results.iter().all(|(_, found, _)| *found == [true, true]),
        "{results:#?}"
    );
}

#[test]
fn node_view_refuses_run_targets_with_the_definition_form() {
    let (env, _) = graph();
    let run = seed(&env, &env.project_root, &env.project_root, "parked");
    for target in ["review#project", run.id.as_str()] {
        let output = command(&env, &["workflow", "show", target, "fix"]);
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(2), "{text}");
        assert!(text.contains("show <workflow> <node>"), "{text}");
    }
}

#[test]
fn node_view_conflicts_with_json_and_history_in_clap() {
    let (env, _) = graph();
    for flag in ["--json", "--history"] {
        let output = command(&env, &["workflow", "show", "review", "fix", flag]);
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(2), "{text}");
        assert!(text.contains("cannot be used with"), "{text}");
        assert!(text.contains(flag), "{text}");
    }
}

#[test]
fn node_view_missing_definition_and_unknown_node_name_the_fix() {
    let (env, dir) = graph();
    let text = refusal(command(&env, &["workflow", "show", "review", "absent"]));
    for fact in [
        "unknown node absent",
        "begin",
        "fix",
        "sweep",
        "unused",
        "watch",
    ] {
        assert!(text.contains(fact), "{text}");
    }
    for broken in [false, true] {
        if broken {
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("workflow.toml"), "broken = [").unwrap();
        } else {
            fs::remove_dir_all(&dir).unwrap();
        }
        let text = refusal(command(&env, &["workflow", "show", "review", "fix"]));
        assert!(text.contains("definition unavailable"), "{text}");
        assert!(text.contains("rimz workflow validate review"), "{text}");
    }
}
