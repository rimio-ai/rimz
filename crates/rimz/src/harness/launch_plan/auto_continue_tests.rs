use super::idle_compact::{compile_seat, settings};
use super::*;

const KEY: &str = "autoContinueAtUsageLimit";

fn machine(auto_continue: bool) -> crate::config::MachineConfig {
    let mut machine = crate::config::MachineConfig::default();
    machine.resume.auto_continue = auto_continue;
    machine
}

fn native_auto_continue(plan: &LaunchPlan) -> Option<bool> {
    settings(plan).and_then(|value| value[KEY].as_bool())
}

fn every_action(args: Vec<String>) -> [(&'static str, ExecAction); 4] {
    [
        ("launch", action_with_args("launch", args.clone())),
        (
            "pick",
            ExecAction::Pick {
                extra_args: args.clone(),
            },
        ),
        ("resume", action_with_args("resume", args.clone())),
        ("fork", action_with_args("fork", args)),
    ]
}

#[test]
fn rimz_auto_continue_turns_claudes_off_and_keeps_every_other_setting() {
    let project = tempfile::tempdir().unwrap();
    let machine = machine(true);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for explicit in [None, Some(true), Some(false)] {
        let mut original = serde_json::json!({"theme": "dark"});
        if let Some(value) = explicit {
            original[KEY] = value.into();
        }
        for file in [false, true] {
            let path = project.path().join("settings.json");
            std::fs::write(&path, original.to_string()).unwrap();
            let args = vec![
                "--settings".into(),
                if file {
                    path.display().to_string()
                } else {
                    original.to_string()
                },
            ];
            for (action, exec) in every_action(args) {
                let plan = compile_seat(project.path(), &request("claude", exec), &machine, None);
                let merged = settings(&plan).unwrap();
                assert_eq!(merged["theme"], "dark", "{action}");
                assert_eq!(merged["idleCompaction"], false, "{action}");
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    original.to_string()
                );
                actual.push((action, explicit, file, merged[KEY].as_bool()));
                expected.push((action, explicit, file, Some(explicit.unwrap_or(false))));
            }
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn claudes_auto_continue_is_left_alone_when_rimz_does_not_resume() {
    let project = tempfile::tempdir().unwrap();
    let headless = || {
        let mut request = request("claude", action_with_args("launch", Vec::new()));
        request.headless = Some(crate::agents::HeadlessRequest {
            schema_open: false,
            schema: crate::harness::headless::judge::LOOP_ANSWER_SCHEMA.into(),
            schema_file: "/check/schema.json".into(),
            verdict_file: "/check/verdict.json".into(),
        });
        request
    };
    let launch = || request("claude", action_with_args("launch", Vec::new()));
    let actual = [
        ("rimz off", launch(), false),
        ("headless", headless(), true),
        ("rimz on", launch(), true),
    ]
    .map(|(label, request, on)| {
        let plan = compile_seat(project.path(), &request, &machine(on), None);
        (label, native_auto_continue(&plan))
    });
    assert_eq!(
        actual,
        [
            ("rimz off", None),
            ("headless", None),
            ("rimz on", Some(false))
        ]
    );
    for kind in ["codex", "pi"] {
        let request = request(kind, action_with_args("launch", Vec::new()));
        assert_eq!(
            compile_seat(project.path(), &request, &machine(true), None)
                .process()
                .provider_argv,
            compile_seat(project.path(), &request, &machine(false), None)
                .process()
                .provider_argv,
            "{kind}"
        );
    }
}
