use super::*;

/// Each notice's key and its room-entry line for a project file holding `text`.
fn notices(text: &str) -> Vec<(String, String)> {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".rimz")).unwrap();
    std::fs::write(root.path().join(".rimz/config.toml"), text).unwrap();
    project_key_notices(root.path())
        .into_iter()
        .map(|notice| (notice.key().to_owned(), notice.to_string()))
        .collect()
}

fn unknown(key: &str) -> (String, String) {
    (
        key.to_owned(),
        format!("project config ignores unknown key `{key}`; remove or rename it"),
    )
}

fn pointer() -> (String, String) {
    (
        "agents.profiles".to_owned(),
        "project config ignores `agents.profiles`, which is not a project table; \
         declare project profiles as `[profiles.<name>]`"
            .to_owned(),
    )
}

fn machine_only(key: &str) -> (String, String) {
    (
        key.to_owned(),
        format!("project config ignores machine-only key `{key}`; set it in ~/.rimz/config.toml"),
    )
}

#[test]
fn a_machine_spelled_profile_table_names_the_project_spelling_and_a_typo_is_unknown() {
    let found = notices(
        "[agents.profiles.proj2]\nagent = \"claude\"\n\n\
         [profilez.typo]\nagent = \"claude\"\n\n\
         [profiles.good]\nagent = \"claude\"\n",
    );
    assert_eq!(found, vec![pointer(), unknown("profilez")]);
}

#[test]
fn nested_unknown_keys_are_reported_by_their_dotted_path_in_both_agents_shapes() {
    let table = notices(
        r#"
[agents.teams.crew]
layot = "lead"

[[agents.teams.crew.roles]]
role = "lead"
profile = "lead"
modle = "opus"

[lsp.servers.ra]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
extensons = ["rs"]
"#,
    );
    let entries = notices("[[agents]]\nname = \"build\"\nlanch_command = \"make\"\n");
    assert_eq!(
        [table, entries],
        [
            vec![
                unknown("agents.teams.crew.layot"),
                unknown("agents.teams.crew.roles.0.modle"),
                unknown("lsp.servers.ra.extensons"),
            ],
            vec![unknown("agents.0.lanch_command")],
        ]
    );
}

#[test]
fn keys_each_project_reader_reads_draw_no_notice() {
    // `sentinel` proves the check ran over each file.
    let table = notices(
        r#"
sentinel = 1

[agents]
env-reminder = true

[agents.teams.crew]
leader = "lead"
layout = "lead"
stages = ["Plan"]
scratch-files = ["tmp/"]
consensus = []

[[agents.teams.crew.roles]]
role = "lead"
profile = "lead"
owns = ["Plan"]
model = "opus"
effort = "high"
budget = "5"
auto-compact = "100000"

[profiles.lead]
agent = "claude"
description = "leads"
any-row-key = "the row check owns it"

[subagents.profiles.helper]
agent = "claude"
description = "helps"

[lsp.servers.ra]
kind = "rust-analyzer"
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { check = { command = "clippy" } }
editor-check-on-save = true
wait-timeout = "30s"
memory-estimate = "2G"

[packs]
expect = ["crew/core"]

[tasks.nightly]
command = "make"
every = "1d"

[[hooks]]
event = "session_start"
command = "echo hi"

[env]
FOO = "bar"

[accounts]
claude = "work"
"#,
    );
    let entries = notices(
        r#"
sentinel = 1

[[agents]]
name = "build"
launch_command = "make"
env = { FOO = "bar" }
"#,
    );
    assert_eq!(
        [table, entries],
        [vec![unknown("sentinel")], vec![unknown("sentinel")]]
    );
}

#[test]
fn keys_only_the_machine_config_reads_name_it_and_others_are_unknown() {
    let found = notices(
        r#"
[throttle]
max-parallel-agents = 1

[keys.bind.normal]
'Alt+x' = { run = "rimz pane zoom" }

[agents]
placement = "pane"

[nonsense]
x = 1
"#,
    );
    assert_eq!(
        found,
        vec![
            machine_only("agents.placement"),
            machine_only("keys"),
            unknown("nonsense"),
            machine_only("throttle"),
        ]
    );
}

#[test]
fn keys_another_check_reports_draw_no_second_notice() {
    let owned = notices(
        r#"
sentinel = 1

[tiers]
senior = ["claude"]

[models.claude]
best = "claude-opus-4-1"

[profiles.lead]
agent = "claude"
descripton = "a row typo the row check owns"

[agents.teams.crew]
layout = "lead"

[[agents.teams.crew.roles]]
role = "lead"
profile = "lead"
tier = "senior"

[tasks.nightly]
command = "make"
every = "1d"
bogus = 1

[lsp]
reserve-percent = 5
"#,
    );
    let refused = notices("[layout]\nfoo = 1\n\n[profilez.typo]\nagent = \"claude\"\n");
    let untyped =
        notices("[agents]\nenv-reminder = \"yes\"\n\n[profilez.typo]\nagent = \"claude\"\n");
    // `packs::expectation_warnings` reports a `packs` that is not a table itself.
    let packs = notices("packs = \"crew/core\"\n\n[profilez.typo]\nagent = \"claude\"\n");
    assert_eq!(
        [owned, refused, untyped, packs],
        [
            vec![unknown("sentinel")],
            Vec::new(),
            vec![unknown("profilez")],
            vec![unknown("profilez")]
        ]
    );
}

#[test]
fn a_mistyped_known_value_silences_only_the_keys_inside_it() {
    let found = notices(
        "[agents]\nenv-reminder = \"yes\"\n\n[agents.profiles.demo]\nagent = \"claude\"\n\n\
         [throttle]\nmax-parallel-agents = 1\n",
    );
    assert_eq!(found, vec![pointer(), machine_only("throttle")]);
}

#[test]
fn key_text_is_escaped_and_unambiguous() {
    let found =
        notices("\"a\\u001b]0;x\\u0007b\" = 1\n\"agents.fooz\" = 1\n\n[agents]\nfooz = 2\n");
    assert_eq!(
        found,
        vec![
            unknown(r#""a\u001B]0;x\u0007b""#),
            unknown("agents.fooz"),
            unknown(r#""agents.fooz""#),
        ]
    );
}
