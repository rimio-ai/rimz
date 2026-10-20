use super::*;
use rimz::config::{
    Profile,
    tiers::{ModelTier, TierProvenance},
};
use rimz::harness::spec::{SpecCandidate, SpecKind};

fn catalog(word: &str) -> UnknownSpec {
    let mut profile: Profile = toml::from_str("agent = 'claude'\nmodel = 'opus'").unwrap();
    profile.model_tier = Some(TierProvenance {
        tier: ModelTier::Senior,
        fell_back: false,
    });
    UnknownSpec {
        word: word.into(),
        candidates: vec![
            SpecCandidate {
                name: "build".into(),
                kind: SpecKind::Command,
            },
            SpecCandidate {
                name: "perf".into(),
                kind: SpecKind::Profile(Box::new(profile)),
            },
            SpecCandidate {
                name: "peer".into(),
                kind: SpecKind::Team(Some("claude,codex".into())),
            },
            SpecCandidate {
                name: "claude".into(),
                kind: SpecKind::Kind,
            },
            SpecCandidate {
                name: "codex".into(),
                kind: SpecKind::Kind,
            },
            SpecCandidate {
                name: "term".into(),
                kind: SpecKind::Kind,
            },
        ],
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn agents_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".rimz")
}

fn report(message: String) -> String {
    let mut output = anstream::StripStream::new(Vec::new());
    super::super::write_report(&mut output, &anyhow::anyhow!(message)).unwrap();
    String::from_utf8(output.into_inner()).unwrap()
}

#[test]
fn close_match_block_matches_the_scene_contract() {
    assert_eq!(
        report(agents_message(
            &catalog("per"),
            true,
            &argv(&["rimz", "agents", "per"]),
            &agents_home()
        )),
        "error: no team, profile, or command named `per`\n  did you mean:\n    rimz agents perf     profile · claude · opus (senior)\n    rimz agents peer     team · claude,codex\n"
    );
}

#[test]
fn verb_typo_matches_the_scene_contract() {
    assert_eq!(
        report(agents_message(
            &catalog("lsit"),
            true,
            &argv(&["rimz", "agents", "lsit"]),
            &agents_home()
        )),
        "error: no verb, team, profile, or command named `lsit`\n  did you mean: rimz agents list\n"
    );
}

#[test]
fn no_match_block_matches_the_scene_contract() {
    assert_eq!(
        report(agents_message(
            &catalog("xyz"),
            true,
            &argv(&["rimz", "agents", "xyz"]),
            &agents_home()
        )),
        "error: no team, profile, or command named `xyz`\n  teams:        peer\n  profiles:     build, perf\n  kinds:        claude, codex, term\n                (each also as <kind>-plan, -ask, -auto, -yolo, -fast where supported)\n  details:      rimz agents profiles · rimz teams list\n  new team:     ~/.rimz/teams/xyz.md\n  new profile:  ~/.rimz/agents/xyz.md\n"
    );
}

#[test]
fn suggested_commands_preserve_and_quote_the_invocation() {
    let message = agents_message(
        &catalog("per"),
        true,
        &argv(&["rimz", "agents", "per", "fix it"]),
        &agents_home(),
    );
    assert!(message.contains("rimz agents perf 'fix it'"), "{message}");
    let message = agents_message(
        &catalog("per"),
        true,
        &argv(&["/opt/rimz", "--color", "never", "agents", "per", "fix it"]),
        &agents_home(),
    );
    assert!(
        message.contains("/opt/rimz --color never agents perf 'fix it'"),
        "{message}"
    );
}

#[test]
fn suggested_commands_only_replace_the_launch_word() {
    for (scene, word, name, invocation, expected) in [
        (
            "agents",
            "agents",
            "agent",
            ["rimz", "agents", "agents", "fix it"],
            "rimz agents agent 'fix it'",
        ),
        (
            "agents",
            "rimz",
            "peer",
            ["rimz", "agents", "rimz", "fix it"],
            "rimz agents peer 'fix it'",
        ),
        (
            "teams",
            "teams",
            "team",
            ["rimz", "teams", "teams", "fix it"],
            "rimz teams team 'fix it'",
        ),
    ] {
        assert_eq!(
            corrected_command(scene, word, name, &argv(&invocation)),
            expected
        );
    }
}

#[test]
fn explicit_launch_excludes_verbs() {
    let message = agents_message(
        &catalog("lsit"),
        false,
        &argv(&["rimz", "agents", "launch", "lsit"]),
        &agents_home(),
    );
    assert!(!message.contains("did you mean:"), "{message}");
    assert!(
        message.starts_with("no team, profile, or command named `lsit`"),
        "{message}"
    );
}

#[test]
fn agents_scene_recognizes_both_layout_error_shapes() {
    for error in [
        anyhow::Error::from(LayoutErr::UnknownSpec(Box::new(catalog("per")))),
        anyhow::Error::from(rimz::harness::plan::ResolveLaunchError::Layout(
            LayoutErr::UnknownSpec(Box::new(catalog("per"))),
        )),
    ] {
        assert_eq!(
            unknown_spec(&error).map(|unknown| unknown.word.as_str()),
            Some("per")
        );
    }
}
