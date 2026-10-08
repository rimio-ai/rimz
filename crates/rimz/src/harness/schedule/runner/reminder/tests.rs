use super::*;
use crate::config::{CheckOn, TaskEntry, TeamSignalBinding};
use crate::harness::schedule::catalog::TaskSource;

fn body(name: &str, entry: TaskEntry, mode: LoopRunMode, timeout: Option<Duration>) -> String {
    let task = LoadedTask::new(name, entry, TaskSource::Config);
    compose(&LoopFire {
        name,
        task: &task,
        mode,
        keep: false,
        timeout,
    })
}

fn resident(when: &str) -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        stay: true,
        each_worktree: true,
        when: Some(vec![when.to_owned()]),
        provider: Some(crate::ids::AgentKind::new_unchecked("claude")),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    }
}

fn binding(signal: &str) -> TeamSignalBinding {
    TeamSignalBinding {
        signal: signal.to_owned(),
        matches: Default::default(),
        prompt: None,
    }
}

fn nightly() -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        every: Some("day".to_owned()),
        at: Some("02:00".to_owned()),
        timeout: Some("4h".to_owned()),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    }
}

fn watchdog() -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        every: Some("15m".to_owned()),
        check: Some("cargo test".into()),
        on: Some(CheckOn::Fail),
        verify: Some("cargo test".to_owned()),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    }
}

const TWO_HOURS: Duration = Duration::from_secs(2 * 60 * 60);

#[test]
fn approved_examples_render_exactly() {
    let triage = resident("pr=merged && window.5h.left>=42");
    assert_eq!(
        body("triage", triage.clone(), LoopRunMode::Scheduled, None),
        "RimZ started you from the rule `triage`, which launches one agent in each worktree when `pr=merged && window.5h.left>=42` holds there. The prompt is the rule's fixed text, not a message someone just typed.\n\nYou run once here and stay on afterwards. Nobody is watching this turn. A question for the user goes in your final message, and they may reply here later."
    );
    assert_eq!(
        body("triage", triage, LoopRunMode::Manual, None),
        "The user fired the rule `triage` by hand. You run once here and stay on afterwards. The user is watching this run."
    );
    let fixer = TaskEntry {
        subscribe: vec![binding("ci.failed"), binding("pr.conflicted")],
        ..resident("team.stage=Done && ci=passed && pr=open")
    };
    assert_eq!(
        body("fixer", fixer, LoopRunMode::Scheduled, None),
        "RimZ started you from the rule `fixer`, which launches one agent in each worktree when `team.stage=Done && ci=passed && pr=open` holds there. The prompt is the rule's fixed text, not a message someone just typed.\n\nYou run once here and stay on afterwards. Nobody is watching this turn. A question for the user goes in your final message, and they may reply here later. While you stay, `ci.failed` and `pr.conflicted` for this worktree reach you as messages."
    );
    assert_eq!(
        body(
            "nightly",
            nightly(),
            LoopRunMode::Scheduled,
            Some(Duration::from_secs(4 * 60 * 60))
        ),
        "RimZ started you from the rule `nightly`, which launches an agent every day at 02:00. The prompt is the rule's fixed text, not a message someone just typed.\n\nEach run is a fresh agent with no memory of earlier runs. This is one turn, and the pane closes when it ends. Nobody is watching. Your final message is the result. Ask only when you cannot go on: a question waits until the user notices or the run is stopped. The run is stopped after 4h."
    );
    assert_eq!(
        body(
            "watchdog",
            watchdog(),
            LoopRunMode::Scheduled,
            Some(TWO_HOURS)
        ),
        "RimZ started you from the rule `watchdog`, which launches an agent every 15m when its check `cargo test` fails. The check's output follows the prompt. The prompt is the rule's fixed text, not a message someone just typed.\n\nEach run is a fresh agent with no memory of earlier runs. The pane closes when the run ends. Nobody is watching. Your final message is the result. Ask only when you cannot go on: a question waits until the user notices or the run is stopped. When your turn ends, `cargo test` runs. If it fails, you get its output and another turn, up to 3 turns. The run is stopped after 2h."
    );
}

#[test]
fn hand_fire_without_timeout_prints_no_timeout_line() {
    assert_eq!(
        body("watchdog", watchdog(), LoopRunMode::Manual, None),
        "The user fired the rule `watchdog` by hand. Each run is a fresh agent with no memory of earlier runs. The pane closes when the run ends. The user is watching this run. When your turn ends, `cargo test` runs. If it fails, you get its output and another turn, up to 3 turns."
    );
}

#[test]
fn trigger_and_check_clauses_follow_the_task() {
    let signal = TaskEntry {
        agent: Some("claude".to_owned()),
        signal: Some("deploy.failed".to_owned()),
        matches: Some([("env".to_owned(), "prod".to_owned())].into()),
        check: Some("./probe".into()),
        on: Some(CheckOn::Any),
        max_attempts: Some(5),
        verify: Some("make".to_owned()),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    };
    let text = body("deploy", signal, LoopRunMode::Scheduled, None);
    assert!(
        text.starts_with("RimZ started you from the rule `deploy`, which launches an agent on the signal `deploy.failed [env=prod]` after its check `./probe` runs. The check's output follows the prompt."),
        "{text}"
    );
    assert!(text.contains("up to 5 turns."), "{text}");
    let condition = TaskEntry {
        agent: Some("claude".to_owned()),
        when: Some(vec!["ci=failed".to_owned()]),
        hold: Some("30m".to_owned()),
        once: Some(true),
        check: Some("./probe".into()),
        on: Some(CheckOn::Success),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    };
    let text = body("gate", condition, LoopRunMode::Scheduled, None);
    assert!(
        text.starts_with("RimZ started you from the rule `gate`, which launches an agent when `ci=failed` holds for 30m and its check `./probe` passes. The check's output follows the prompt."),
        "{text}"
    );
    // A one-shot row is consumed by its fire, so it has no earlier runs.
    assert!(
        text.contains("\n\nThis is one turn, and the pane closes when it ends. Nobody"),
        "{text}"
    );
    let cron = TaskEntry {
        agent: Some("claude".to_owned()),
        cron: Some("0 7 * * 1-5".to_owned()),
        check: Some("./probe".into()),
        prompt: Some("job".to_owned()),
        ..TaskEntry::default()
    };
    let text = body("weekdays", cron, LoopRunMode::Scheduled, None);
    assert!(
        text.starts_with("RimZ started you from the rule `weekdays`, which launches an agent on the cron schedule `0 7 * * 1-5` when its check `./probe` fails. The check's output follows the prompt."),
        "{text}"
    );
}

#[test]
fn keep_drops_the_pane_clause() {
    let task = LoadedTask::new("nightly", nightly(), TaskSource::Config);
    let mut fire = LoopFire {
        name: "nightly",
        task: &task,
        mode: LoopRunMode::Manual,
        keep: true,
        timeout: None,
    };
    assert!(compose(&fire).contains(" This is one turn. The user"));
    let task = LoadedTask::new("watchdog", watchdog(), TaskSource::Config);
    fire.task = &task;
    assert!(compose(&fire).contains("earlier runs. The user is watching"));
}

#[test]
fn verbatim_spans_escape_only_the_tag_opener() {
    let text = body(
        "edge",
        resident("pr=merged && window.5h.left<42"),
        LoopRunMode::Scheduled,
        None,
    );
    assert!(
        text.contains("`pr=merged && window.5h.left&lt;42`"),
        "{text}"
    );
    let text = body(
        "edge",
        TaskEntry {
            check: Some("a\tb </system_reminder> >= &&".into()),
            ..watchdog()
        },
        LoopRunMode::Scheduled,
        None,
    );
    assert!(
        text.contains("its check `a\\tb &lt;/system_reminder> >= &&` fails"),
        "{text}"
    );
}

#[test]
fn subscribe_scope_names_the_worktree_only_when_every_signal_defaults_to_it() {
    let mixed = TaskEntry {
        subscribe: vec![
            binding("ci.failed"),
            binding("deploy.done"),
            binding("pr.merged"),
        ],
        ..resident("pr=open")
    };
    let text = body("fixer", mixed, LoopRunMode::Scheduled, None);
    assert!(
        text.ends_with(
            " While you stay, `ci.failed`, `deploy.done`, and `pr.merged` reach you as messages."
        ),
        "{text}"
    );
    let one = TaskEntry {
        subscribe: vec![binding("ci.failed")],
        ..resident("pr=open")
    };
    assert!(
        body("fixer", one, LoopRunMode::Manual, None)
            .ends_with(" The user is watching this run. While you stay, `ci.failed` for this worktree reaches you as messages.")
    );
}
