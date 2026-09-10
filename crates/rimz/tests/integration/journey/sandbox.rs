//! A real team consumes peer messages and subagent scratch across mount views.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use rimz::config::Isolation;

use crate::common::{CommandTimeoutExt, Env, path_with_front, write_hook_firing_agent};

#[test]
fn tmux_sandbox_team_consumes_message_and_subagent_shared_tmp() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping sandbox team journey");
        return;
    }
    if let Err(err) = rimz::sandbox::preflight(Isolation::Sandbox) {
        eprintln!("skipping sandbox team journey: {err}");
        return;
    }
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.install_agent_hooks("claude");
    for name in ["visible", "hidden"] {
        let dir = env.home_root.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).expect("skill directory");
        std::fs::write(dir.join("SKILL.md"), name).expect("skill content");
    }
    let agent_bin = write_hook_firing_agent(&env, "claude");
    let shim = agent_bin.join("claude");
    let mut body = std::fs::read_to_string(&shim).expect("read hook-firing shim");
    body = body.replace("sess-hook-agent", "sandbox-$$");
    let tail = body
        .rfind("feed '{\"hook_event_name\":\"Stop\"")
        .expect("hook-firing shim completion hook");
    body.truncate(tail);
    body.push_str(
        r#"set -eu
test "$TMPDIR" = /tmp
test ! -e "$RIMZ_TEST_HOST_TMP_FILE"
test ! -e "$HOME/.agents/skills/hidden"
test -r "$HOME/.agents/skills/visible/SKILL.md"
case "$*" in
    *sandbox-child-task*)
        test "$(cat /tmp/parent-file)" = parent-to-child
        printf '%s\n' child-to-room > /tmp/child-file
        feed '{"hook_event_name":"Stop","session_id":"'"$session"'","last_assistant_message":"scratch written"}'
        exit 0
        ;;
esac
feed '{"hook_event_name":"Stop","session_id":"'"$session"'","last_assistant_message":"ready"}'
case "$RIMZ_AGENT_ROLE" in
    parent)
        printf '%s\n' parent-to-child > /tmp/parent-file
        "$rimz" --mux tmux subagents worker-child sandbox-child-task --timeout 1m > /tmp/child-launch 2>&1
        while [ ! -s /tmp/child-file ] || [ ! -e /tmp/other-ready ]; do sleep 0.1; done
        child_text=$(cat /tmp/child-file)
        test "$child_text" = child-to-room
        "$rimz" --mux tmux message @other "sandbox-handoff:$child_text" > /tmp/message-send 2>&1
        while [ ! -s /tmp/other-consumed ]; do sleep 0.1; done
        test "$(cat /tmp/other-consumed)" = "$child_text"
        printf '%s\n' "parent-read:$child_text" > /tmp/journey-complete
        ;;
    other)
        printf '%s\n' "$$" > /tmp/other-generation
        touch /tmp/other-ready
        while IFS= read -r line; do
            case "$line" in
                *sandbox-handoff:child-to-room*)
                    child_text=$(cat /tmp/child-file)
                    test "$child_text" = child-to-room
                    printf '%s\n' "$child_text" > /tmp/other-consumed
                    break
                    ;;
            esac
        done
        ;;
esac
while IFS= read -r line; do :; done
"#,
    );
    std::fs::write(&shim, body).expect("write sandbox journey agent");

    let host_tmp = tempfile::NamedTempFile::new_in("/tmp").expect("host-only tmp sentinel");
    let config = toml::to_string(&serde_json::json!({
        "agents": [{
            "name": "claude",
            "env": {
                "PATH": path_with_front(&agent_bin).to_str().expect("agent PATH"),
                "RIMZ_TEST_HOST_TMP_FILE": host_tmp.path().to_str().expect("sentinel path"),
            },
        }],
    }))
    .expect("serialize agent environment");
    env.write_config(&env.project_root, &config);
    env.rimz()
        .args(["trust", "grant"])
        .assert_success_within_timeout("trust sandbox journey agent environment");
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("create machine config directory");
    std::fs::write(
        config_dir.join("agents.toml"),
        r#"[agents]
isolation = "sandbox"
[agents.profiles.worker]
agent = "claude"
skills = ["hidden:off"]
[subagents.profiles.worker-child]
agent = "claude"
skills = ["hidden:off"]
[agents.teams.duo]
layout = "parent+other"
[[agents.teams.duo.roles]]
role = "parent"
profile = "worker"
[[agents.teams.duo.roles]]
role = "other"
profile = "worker"
"#,
    )
    .expect("write sandbox team config");

    env.rimz()
        .env("PATH", path_with_front(&agent_bin))
        .args(["--mux", "tmux", "start", "--no-attach"])
        .assert_success_within_timeout("start sandbox team room");
    env.rimz()
        .env("PATH", path_with_front(&agent_bin))
        .args(["--mux", "tmux", "teams", "duo", "--bg"])
        .assert_success_within_timeout("launch sandbox team");

    let store = env.store();
    let scratch = &store.paths().scratch_dir;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let completed = std::fs::read_to_string(scratch.join("journey-complete"));
        if matches!(completed.as_deref(), Ok("parent-read:child-to-room\n")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sandbox consumers did not finish: completion={completed:?}, child launch={:?}, message send={:?}, teammate read={:?}",
            std::fs::read_to_string(scratch.join("child-launch")),
            std::fs::read_to_string(scratch.join("message-send")),
            std::fs::read_to_string(scratch.join("other-consumed")),
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read_to_string(scratch.join("other-consumed")).expect("teammate consumed message"),
        "child-to-room\n",
    );
    assert!(host_tmp.path().exists(), "host tmp remains untouched");
    let generation =
        std::fs::read_to_string(scratch.join("other-generation")).expect("first generation");
    env.rimz()
        .env("PATH", path_with_front(&agent_bin))
        .args(["--mux", "tmux", "agents", "restart", "@other"])
        .assert_success_within_timeout("restart sandboxed teammate");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let next = std::fs::read_to_string(scratch.join("other-generation"));
        if next
            .as_ref()
            .is_ok_and(|next| !next.is_empty() && next != &generation)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restarted teammate never became ready: {next:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read_to_string(scratch.join("child-file")).expect("scratch survives restart"),
        "child-to-room\n"
    );
    env.rimz()
        .args(["--mux", "tmux", "reset", "--no-start", "--yes"])
        .assert_success_within_timeout("reset sandbox room");
    assert!(!scratch.exists(), "reset removes shared scratch");
}
