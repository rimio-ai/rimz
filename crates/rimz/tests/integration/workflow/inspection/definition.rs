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
