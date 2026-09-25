use super::*;
use crate::config::{CommandsConfig, SkillName};

#[test]
fn isolation_defaults_inherit_but_roles_cannot_set_them() {
    let root = fixture();
    definition(
        root.path(),
        "agents/admin.md",
        "agent: claude\nisolation: host\ntools: [Bash]",
        "Admin.",
    );
    definition(root.path(), "agents/child.md", "agent: admin", "Child.");
    definition(
        root.path(),
        "agents/boxed.md",
        "agent: admin\nisolation: sandbox",
        "Boxed.",
    );
    let loaded = clean(root.path());
    for (name, expected) in [("admin", "host"), ("child", "host"), ("boxed", "sandbox")] {
        let value = serde_json::to_value(&loaded.agent_profiles.0[name]).unwrap();
        assert_eq!(value["isolation"], expected);
    }
    assert!(
        serde_saphyr::from_str::<frontmatter::RoleFrontmatter>("agent: admin\nisolation: host")
            .is_err()
    );
}

fn team_fixture() -> tempfile::TempDir {
    let root = fixture();
    definition(
        root.path(),
        "agents/worker.md",
        "model: opus\ntools: [Bash, Skill, AskUserQuestion]\nskills: [work]\nsubagents: [general]",
        "Craft.\n\n${traits}",
    );
    root
}

fn team_definition(root: &Path, fields: &str, roles: &str, body: &str) {
    write(
        root,
        "teams/probe.md",
        &format!("---\n{fields}\nroles:\n{roles}\n---\n{body}"),
    );
}

const TEAM_ROLES: &str = "  - agent: worker\n    role: lead\n    owns: [Plan, Implement]\n  - agent: worker\n    role: judge\n    owns: [Review]";
const TEAM_STAGES: &str = "leader: lead\nstages: [Plan, Implement, Review]";

#[test]
fn team_seats_materialize_launch_settings_and_route_questions_to_leader() {
    let root = team_fixture();
    team_definition(
        root.path(),
        TEAM_STAGES,
        TEAM_ROLES,
        "Pipeline. @lead @judge @all @rimz @codex ${literal}",
    );
    let loaded = clean(root.path());
    let team = &loaded.teams.0["probe"];
    assert!(team.staged());
    assert!(team.scratch_files.is_none());
    assert!(team.consensus_file.is_none());
    assert_eq!(team.leader.as_deref(), Some("lead"));
    assert_eq!(team.owner_of("Review"), Some("judge"));
    assert_eq!(
        team.roles[0].flip_compact,
        Some(crate::config::FlipCompact::Threshold(
            crate::store::message::AutoCompact::Tokens(120_000)
        ))
    );
    assert_eq!(
        team.roles[1].flip_compact,
        Some(crate::config::FlipCompact::Threshold(
            crate::store::message::AutoCompact::Tokens(180_000)
        ))
    );
    for role in &team.roles {
        let profile = &loaded.agent_profiles.0[&role.profile];
        assert_eq!(profile.agent, "claude");
        assert_eq!(profile.model.as_deref(), Some("opus"));
        assert_eq!(profile.auto_compact.as_deref(), Some("258k"));
        assert_eq!(
            profile
                .skills
                .as_ref()
                .unwrap()
                .iter()
                .map(SkillName::as_str)
                .collect::<Vec<_>>(),
            ["work", "reflect"]
        );
        assert_eq!(profile.subagents.as_ref().unwrap(), &["general"]);
        assert_eq!(texts(profile), ["Craft."]);
        assert_eq!(
            profile.args.as_ref().unwrap().contains("AskUserQuestion"),
            role.role == "lead"
        );
        assert_eq!(
            loaded.sources.agent_profiles[&role.profile],
            root.path().join("teams/probe.md")
        );
        let row = loaded
            .rows
            .iter()
            .find(|row| row.name == role.profile)
            .unwrap();
        assert_eq!(row.namespace, "team");
        assert_eq!(row.team.as_deref(), Some("probe"));
        assert_eq!(row.role.as_deref(), Some(role.role.as_str()));
        assert_eq!(row.owns, role.owns);
        assert_eq!(row.signals, role.signals);
    }
    assert_eq!(
        loaded.sources.team("probe"),
        Some(root.path().join("teams/probe.md").as_path())
    );
    assert_eq!(
        team.append_system_prompt_files,
        [PromptSource::Text {
            origin: root.path().join("teams/probe.md"),
            text: "Pipeline. @lead @judge @all @rimz @codex ${literal}".to_owned()
        }]
    );
}

#[test]
fn team_overlays_keep_ancestor_crafts_and_move_the_base() {
    let root = team_fixture();
    definition(
        root.path(),
        "agents/parent.md",
        "model: opus\ntools: [Bash, Skill, AskUserQuestion]\ntraits: [parent]\nskills: [work]",
        "Parent. ${traits}",
    );
    definition(
        root.path(),
        "agents/worker.md",
        "agent: parent\ntraits: [own]\nbudget: 2\nmodel-reminder: false",
        "Child. ${traits}",
    );
    for name in ["parent", "own", "role", "team"] {
        write(root.path(), &format!("traits/{name}.md"), name);
    }
    team_definition(
        root.path(),
        &format!("{TEAM_STAGES}\nname: rig\nlayout: lead+judge\ntraits: [team, own]"),
        &format!(
            "{TEAM_ROLES}\n    model: astra\n    mode: yolo\n    effort: medium\n    budget: 3/day\n    model-reminder: true\n    auto-compact: 500k\n    traits: [role, own]\n    skills: [replacement, reflect]\n    subagents: []\n    flip-compact: 'OFF'"
        ),
        "Pipeline.",
    );
    let loaded = clean(root.path());
    let lead = &loaded.agent_profiles.0["rig.lead"];
    let judge = &loaded.agent_profiles.0["rig.judge"];
    assert_eq!(texts(lead), ["Parent. parent", "Child. own\n\nteam"]);
    assert_eq!(
        texts(judge),
        ["Parent. parent", "Child. own\n\nrole\n\nteam"]
    );
    assert_eq!(judge.agent, "codex");
    assert_eq!(judge.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(judge.mode, Some(crate::agents::PermissionMode::Yolo));
    assert_eq!(judge.effort.as_deref(), Some("medium"));
    assert_eq!(judge.budget.as_deref(), Some("3/day"));
    assert_eq!(judge.model_reminder, Some(true));
    assert_eq!(judge.auto_compact.as_deref(), Some("500k"));
    assert_eq!(judge.subagents, Some(Vec::new()));
    assert_eq!(
        judge
            .skills
            .as_ref()
            .unwrap()
            .iter()
            .map(SkillName::as_str)
            .collect::<Vec<_>>(),
        ["replacement", "reflect"]
    );
    assert!(
        judge
            .args
            .as_ref()
            .unwrap()
            .contains("tools.experimental_request_user_input.enabled=false")
    );
    assert_eq!(
        loaded.teams.0["rig"].roles[1].flip_compact,
        Some(crate::config::FlipCompact::Off)
    );
    assert_eq!(lead.budget.as_deref(), Some("2"));
    assert_eq!(lead.model_reminder, Some(false));
    assert_eq!(loaded.teams.0["rig"].layout.as_deref(), Some("lead+judge"));
}

#[test]
fn team_skill_clear_and_custom_model_keep_the_original_kind() {
    let root = team_fixture();
    team_definition(
        root.path(),
        TEAM_STAGES,
        &format!("{TEAM_ROLES}\n    model: custom\n    skills: []\n    flip-compact: 70%"),
        "Pipeline.",
    );
    let loaded = clean(root.path());
    let judge = &loaded.agent_profiles.0["probe.judge"];
    assert_eq!(judge.agent, "claude");
    assert_eq!(judge.model.as_deref(), Some("custom"));
    assert_eq!(judge.skills, Some(Vec::new()));
    assert_eq!(
        loaded.teams.0["probe"].roles[1].flip_compact,
        Some(crate::config::FlipCompact::Threshold(
            crate::store::message::AutoCompact::Percent(70)
        ))
    );
}

#[test]
fn team_signal_bindings_preserve_matches_and_trim_prompts() {
    let root = team_fixture();
    team_definition(
        root.path(),
        TEAM_STAGES,
        &format!(
            "{TEAM_ROLES}\n    signals:\n      - ci.failed\n      - signal: 'pr.*'\n        match: {{branch: feat-x}}\n        prompt: ' Read it. '\n      - signal: agent.idle\n        match: {{handle: lead}}\n      - signal: agent.*\n        match: {{session: session-id}}"
        ),
        "Pipeline.",
    );
    let loaded = clean(root.path());
    let roles = &loaded.teams.0["probe"].roles;
    assert!(roles[0].signals.is_empty());
    let signals = &roles[1].signals;
    assert_eq!(signals.len(), 4);
    assert_eq!(signals[0].signal, "ci.failed");
    assert_eq!(signals[1].matches["branch"], "feat-x");
    assert_eq!(signals[1].prompt.as_deref(), Some("Read it."));
    assert_eq!(signals[2].matches["handle"], "lead");
    assert_eq!(signals[3].matches["session"], "session-id");
}

#[test]
fn invalid_teams_publish_neither_roster_nor_seats() {
    let cases = [
        (
            "leader: lead",
            TEAM_ROLES.to_owned(),
            "Pipeline.",
            "declares no `stages:`",
        ),
        (
            "stages: [Plan, Implement, Review]",
            TEAM_ROLES.to_owned(),
            "Pipeline.",
            "no `leader:`",
        ),
        (
            "leader: nobody\nstages: [Plan, Implement, Review]",
            TEAM_ROLES.to_owned(),
            "Pipeline.",
            "not a declared role",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES.replace("owns: [Review]", "owns: [Unknown]"),
            "Pipeline.",
            "unknown stage",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES.replace("owns: [Review]", "owns: [Implement]"),
            "Pipeline.",
            "owned by both",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES.replace("owns: [Review]", "owns: []"),
            "Pipeline.",
            "has no owner",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES
                .replace("[Plan, Implement]", "[Implement, Review]")
                .replace("owns: [Review]", "owns: [Plan]"),
            "Pipeline.",
            "producer of a stage never referees",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES.replace("role: judge", "role: lead"),
            "Pipeline.",
            "twice",
        ),
        (
            TEAM_STAGES,
            TEAM_ROLES.to_owned(),
            "Pipeline. @missing",
            "undeclared handle",
        ),
        (TEAM_STAGES, TEAM_ROLES.to_owned(), "", "has no body"),
        (TEAM_STAGES, String::new(), "Pipeline.", "lists no roles"),
        (
            TEAM_STAGES,
            TEAM_ROLES.replace("agent: worker", "agent: general"),
            "Pipeline.",
            "agents only",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    meka: old"),
            "Pipeline.",
            "its model decides the runtime",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    mystery: true"),
            "Pipeline.",
            "unknown field",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    flip-compact: soon"),
            "Pipeline.",
            "flip-compact: soon",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    subagents: [nosuch]"),
            "Pipeline.",
            "unknown subagent",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    tools: [Agent, Bash]"),
            "Pipeline.",
            "lists the Agent tool",
        ),
        (
            TEAM_STAGES,
            format!("{TEAM_ROLES}\n    traits: [missing]"),
            "Pipeline.",
            "cannot read trait",
        ),
    ];
    for (fields, roles, body, needle) in cases {
        let root = team_fixture();
        team_definition(root.path(), fields, &roles, body);
        error(root.path(), needle);
        let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
        assert!(loaded.teams.0.is_empty(), "{needle}");
        assert!(
            !loaded
                .agent_profiles
                .0
                .keys()
                .any(|name| name.starts_with("probe.")),
            "{needle}"
        );
        assert!(
            !loaded.rows.iter().any(|row| row.team.is_some()),
            "{needle}"
        );
    }
}

#[test]
fn malformed_signals_fail_before_the_team_can_be_published() {
    for (binding, needle) in [
        ("[cifailed]", "rimz reads one event name or one family"),
        (
            "[ci.failed.extra]",
            "rimz reads one event name or one family",
        ),
        ("[{signal: agent.idle}]", "no handle or session match"),
        (
            "[{signal: ci.failed, matches: {a: b}}]",
            "malformed frontmatter",
        ),
        ("[{signal: ci.failed, match: [x]}]", "malformed frontmatter"),
        (
            "[{signal: ci.failed, match: {branch: ' '}}]",
            "non-empty string values",
        ),
        ("[{signal: ci.failed, prompt: ' '}]", "empty `prompt:`"),
        ("[]", "non-empty list"),
        ("null", "non-empty list"),
        ("", "non-empty list"),
        ("{}", "malformed frontmatter"),
        ("ci.failed", "malformed frontmatter"),
    ] {
        let root = team_fixture();
        team_definition(
            root.path(),
            TEAM_STAGES,
            &format!("{TEAM_ROLES}\n    signals: {binding}"),
            "Pipeline.",
        );
        error(root.path(), needle);
        assert!(
            load(root.path(), SkillCheck::Skip, &CommandsConfig::default())
                .errors
                .iter()
                .any(|error| error.path == root.path().join("teams/probe.md"))
        );
        assert!(
            load(root.path(), SkillCheck::Skip, &CommandsConfig::default())
                .teams
                .0
                .is_empty()
        );
    }
}

#[test]
fn flip_compact_accepts_yaml_off_and_rejects_other_shapes() {
    for value in ["off", "false", "'OFF'", "120000", "120k", "70%"] {
        let root = team_fixture();
        team_definition(
            root.path(),
            TEAM_STAGES,
            &format!("{TEAM_ROLES}\n    flip-compact: {value}"),
            "Pipeline.",
        );
        let loaded = clean(root.path());
        if matches!(value, "off" | "false" | "'OFF'") {
            assert_eq!(
                loaded.teams.0["probe"].roles[1].flip_compact,
                Some(crate::config::FlipCompact::Off)
            );
        }
    }
    for value in ["true", "soon", "[]", "{}", "null", "-1"] {
        let root = team_fixture();
        team_definition(
            root.path(),
            TEAM_STAGES,
            &format!("{TEAM_ROLES}\n    flip-compact: {value}"),
            "Pipeline.",
        );
        error(
            root.path(),
            "a token count such as '120k', a percentage such as '70%', or 'off'",
        );
    }
}

#[test]
fn unsupported_preset_fields_fail_at_the_definition() {
    for (kind, field, value) in [
        ("pi", "auto-compact", "200k"),
        ("amp", "effort", "high"),
        ("droid", "model", "custom"),
    ] {
        let root = fixture();
        definition(
            root.path(),
            "agents/parent.md",
            &format!("agent: {kind}\n{field}: {value}"),
            "",
        );
        definition(root.path(), "agents/child.md", "agent: parent", "");
        let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
        let expected =
            crate::agents::PresetErr::UnsupportedField { agent: kind, field }.to_string();
        assert!(
            loaded
                .errors
                .iter()
                .any(|error| error.path == root.path().join("agents/parent.md")
                    && error.message == expected)
        );
        assert!(!loaded.agent_profiles.0.contains_key("parent"));
        assert!(!loaded.agent_profiles.0.contains_key("child"));
        error(root.path(), "follows `parent`, which failed to load");
    }
}

#[test]
fn duplicate_team_names_remove_all_seats_and_sources() {
    let root = team_fixture();
    team_definition(root.path(), TEAM_STAGES, TEAM_ROLES, "Pipeline.");
    write(
        root.path(),
        "teams/duplicate.md",
        &format!("---\nname: probe\n{TEAM_STAGES}\nroles:\n{TEAM_ROLES}\n---\nPipeline."),
    );
    error(root.path(), "declared twice");
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert!(loaded.teams.0.is_empty());
    assert!(loaded.sources.team("probe").is_none());
    let paths = BTreeSet::from([
        root.path().join("teams/duplicate.md"),
        root.path().join("teams/probe.md"),
    ]);
    for name in ["probe", "probe.lead", "probe.judge"] {
        assert_eq!(loaded.failed[name], paths);
    }
    assert!(
        !loaded
            .agent_profiles
            .0
            .keys()
            .any(|name| name.starts_with("probe."))
    );
    assert!(
        !loaded
            .sources
            .agent_profiles
            .keys()
            .any(|name| name.starts_with("probe."))
    );
}

#[test]
fn failed_teams_record_names_and_roles_and_name_each_bad_seat() {
    let root = team_fixture();
    team_definition(
        root.path(),
        "name: renamed\nleader: lead\nstages: [Plan, Implement, Review]",
        &TEAM_ROLES.replace("agent: worker", "agent: missing"),
        "Pipeline.",
    );
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    let path = root.path().join("teams/probe.md");
    for name in ["renamed", "renamed.lead", "renamed.judge"] {
        assert_eq!(loaded.failed[name], BTreeSet::from([path.clone()]));
    }
    for handle in ["lead", "judge"] {
        assert!(loaded.errors.iter().any(|error| error.path == path && error.message == format!("team 'renamed' role '{handle}' selects unknown or failed agent 'missing'; team roles can select definitions from agents only")));
    }
    write(
        root.path(),
        "teams/probe.md",
        "---\nroles: [\n---\nPipeline.",
    );
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert_eq!(loaded.failed["probe"], BTreeSet::from([path]));
}

#[test]
fn duplicate_agent_names_record_both_sources_in_one_namespace() {
    let root = fixture();
    for file in ["first", "second"] {
        definition(
            root.path(),
            &format!("agents/{file}.md"),
            "name: duplicate\nagent: pi",
            "",
        );
    }
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert!(!loaded.agent_profiles.0.contains_key("duplicate"));
    assert_eq!(
        loaded.failed["duplicate"],
        BTreeSet::from([
            root.path().join("agents/first.md"),
            root.path().join("agents/second.md"),
        ])
    );
}

#[test]
fn team_skills_check_reflect_and_the_overridden_runtime() {
    let root = team_fixture();
    for name in ["work", "reflect"] {
        write(
            root.path(),
            &format!("skills/{name}/SKILL.md"),
            "---\nname: probe\n---\nSkill.",
        );
    }
    write(
        root.path(),
        "skills/work/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    team_definition(
        root.path(),
        TEAM_STAGES,
        &format!("{TEAM_ROLES}\n    model: astra"),
        "Pipeline.",
    );
    let loaded = load_checked(root.path());
    assert!(loaded.agent_profiles.0.contains_key("worker"));
    assert!(loaded.teams.0.is_empty());
    assert_eq!(loaded.errors.len(), 1);
    assert_eq!(loaded.errors[0].path, root.path().join("teams/probe.md"));
    assert!(
        loaded.errors[0].message.contains("user-only"),
        "{:?}",
        loaded.errors
    );
    std::fs::remove_file(root.path().join("skills/work/agents/openai.yaml")).unwrap();
    std::fs::remove_file(root.path().join("skills/reflect/SKILL.md")).unwrap();
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 2);
    assert!(
        loaded
            .errors
            .iter()
            .all(|error| error.message.contains("reflect"))
    );
    assert!(loaded.teams.0.is_empty());
}

#[test]
fn even_a_bodyless_seat_needs_its_runtime_base() {
    let root = team_fixture();
    definition(root.path(), "agents/worker.md", "agent: pi", "");
    std::fs::remove_file(root.path().join("agents/pi.md")).unwrap();
    team_definition(root.path(), TEAM_STAGES, TEAM_ROLES, "Pipeline.");
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert!(!loaded.agent_profiles.0.contains_key("worker"));
    assert!(loaded.teams.0.is_empty());
    let base = root.path().join("agents/pi.md");
    assert!(loaded.errors.iter().any(|error| {
        error.path.ends_with("agents/worker.md")
            && error.message.contains("kind base is missing")
            && error.message.contains(&base.display().to_string())
    }));
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for kind in ["claude", "codex", "pi"] {
        write(
            root.path(),
            &format!("agents/{kind}.md"),
            "---\ndescription: Foundation.\n---\nBase.",
        );
    }
    root
}

/// Checks skills with `HOME` at `root`, so provider roots stay inside the fixture.
fn load_checked(root: &Path) -> LoadedDefinitions {
    let env = BTreeMap::from([("HOME".to_owned(), root.display().to_string())]);
    load(
        root,
        SkillCheck::Check {
            env: &env,
            library: &root.join("skills"),
        },
        &CommandsConfig::default(),
    )
}

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn definition(root: &Path, name: &str, fields: &str, body: &str) {
    write(
        root,
        name,
        &format!("---\ndescription: Probe.\n{fields}\n---\n{body}"),
    );
}

fn clean(root: &Path) -> LoadedDefinitions {
    let loaded = load(root, SkillCheck::Skip, &CommandsConfig::default());
    assert!(loaded.errors.is_empty(), "{:#?}", loaded.errors);
    loaded
}

fn error(root: &Path, needle: &str) {
    let loaded = load(root, SkillCheck::Skip, &CommandsConfig::default());
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message.contains(needle)),
        "expected {needle:?}: {:#?}",
        loaded.errors
    );
}

fn texts(profile: &Profile) -> Vec<&str> {
    profile
        .append_system_prompt_files
        .iter()
        .map(|source| match source {
            PromptSource::Text { text, .. } => text.as_str(),
            PromptSource::File(_) => panic!("definitions carry text"),
        })
        .collect()
}

#[test]
fn missing_trees_and_documentation_are_not_definitions() {
    let root = tempfile::tempdir().unwrap();
    assert!(clean(root.path()).rows.is_empty());
    for namespace in ["agents", "subagents", "teams", "traits"] {
        for name in ["AGENTS.md", "CLAUDE.md", "README.md", "nested/probe.md"] {
            write(
                root.path(),
                &format!("{namespace}/{name}"),
                "not frontmatter",
            );
        }
    }
    assert!(clean(root.path()).rows.is_empty());
    assert_eq!(source_paths(root.path()).len(), 4);
}

#[test]
fn the_published_consensus_copy_is_not_a_team_definition() {
    let root = team_fixture();
    team_definition(root.path(), TEAM_STAGES, TEAM_ROLES, "Team.");
    let copy = crate::harness::team_prompt::consensus_copy_path(root.path());
    crate::harness::team_prompt::publish_consensus_copy(root.path()).unwrap();
    let loaded = clean(root.path());
    assert_eq!(
        loaded.teams.0.keys().collect::<Vec<_>>(),
        ["probe"],
        "{:#?}",
        loaded.rows
    );
    assert!(!source_paths(root.path()).contains(&copy));
}

#[test]
fn bases_are_emitted_in_both_namespaces_with_sources() {
    let root = fixture();
    let loaded = clean(root.path());
    assert_eq!(loaded.rows.len(), 6);
    for kind in ["claude", "codex", "pi"] {
        let profile = &loaded.agent_profiles.0[kind];
        assert_eq!(profile, &loaded.subagent_profiles.0[kind]);
        assert_eq!(profile.agent, kind);
        assert_eq!(
            profile.system_prompt_file,
            Some(PromptSource::Text {
                origin: root.path().join(format!("agents/{kind}.md")),
                text: "Base.".to_owned()
            })
        );
        assert_eq!(
            loaded.sources.agent_profiles[kind],
            root.path().join(format!("agents/{kind}.md"))
        );
        assert!(profile.args.is_none());
    }
    assert_eq!(source_paths(root.path()).len(), 7);
}

#[test]
fn runtime_resolution_aliases_and_defaults() {
    let root = fixture();
    for (name, fields, kind, model, effort) in [
        (
            "alias",
            "model: astra\ntools: [Bash]",
            "codex",
            "gpt-6-astra",
            "xhigh",
        ),
        (
            "fable",
            "model: fable\ntools: [Bash]",
            "claude",
            "fable",
            "high",
        ),
        (
            "custom",
            "agent: claude\nmodel: custom\ntools: [Bash]",
            "claude",
            "custom",
            "xhigh",
        ),
        (
            "prefix",
            "model: gpt-custom\ntools: [Bash]",
            "codex",
            "gpt-custom",
            "xhigh",
        ),
        (
            "pi-seat",
            "agent: pi\nmodel: custom",
            "pi",
            "custom",
            "xhigh",
        ),
    ] {
        definition(root.path(), &format!("agents/{name}.md"), fields, "");
        let loaded = clean(root.path());
        let profile = &loaded.agent_profiles.0[name];
        assert_eq!(profile.agent, kind);
        assert_eq!(profile.model.as_deref(), Some(model));
        assert_eq!(profile.effort.as_deref(), Some(effort));
        assert_eq!(
            profile.auto_compact.as_deref(),
            if kind == "pi" { None } else { Some("258k") }
        );
        assert!(profile.append_system_prompt_files.is_empty());
        if kind == "claude" {
            assert_eq!(profile.mode, Some(crate::agents::PermissionMode::Auto));
        }
    }
}

#[test]
fn chains_inherit_launch_fields_but_render_each_own_craft() {
    let root = fixture();
    write(root.path(), "traits/asking.md", "Ask well.");
    write(root.path(), "traits/writing.md", "Write well.");
    definition(
        root.path(),
        "agents/parent.md",
        "model: astra\ntools: [Bash, Skill]\nskills: [one]\nsubagents: [general]\nbudget: 2.5\nmodel-reminder: false\ntraits: [asking]",
        "Parent\n\n${traits}",
    );
    definition(
        root.path(),
        "agents/child.md",
        "agent: parent\neffort: low\nauto-compact: 200000\ntraits: [writing]\nskills: null\nsubagents: null",
        "Child\n\n${traits}\n\n${shell}",
    );
    let loaded = clean(root.path());
    let child = &loaded.agent_profiles.0["child"];
    assert_eq!(child.agent, "codex");
    assert_eq!(child.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(child.effort.as_deref(), Some("low"));
    assert_eq!(child.budget.as_deref(), Some("2.5"));
    assert_eq!(child.auto_compact.as_deref(), Some("200000"));
    assert_eq!(child.model_reminder, Some(false));
    assert_eq!(child.skills, Some(vec![]));
    assert_eq!(child.subagents, Some(vec![]));
    assert_eq!(
        texts(child),
        ["Parent\n\nAsk well.", "Child\n\nWrite well.\n\n${shell}"]
    );
    assert_eq!(child.args, loaded.agent_profiles.0["parent"].args);
}

#[test]
fn failed_parents_exclude_dependents_and_keep_independent_profiles() {
    let root = fixture();
    definition(
        root.path(),
        "agents/parent.md",
        "model: astra\ntools: [Bash]\ntraits: [missing]",
        "${traits}",
    );
    definition(root.path(), "agents/child.md", "agent: parent", "Child.");
    definition(root.path(), "agents/independent.md", "agent: pi", "");
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert_eq!(loaded.errors.len(), 2);
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message == "follows `parent`, which failed to load")
    );
    assert!(!loaded.agent_profiles.0.contains_key("child"));
    assert!(!loaded.agent_profiles.0.contains_key("parent"));
    assert!(loaded.agent_profiles.0.contains_key("independent"));
    for name in ["child", "parent"] {
        assert_eq!(
            loaded.failed[name],
            BTreeSet::from([root.path().join(format!("agents/{name}.md"))])
        );
    }
    assert!(!loaded.failed.contains_key("independent"));
}

#[test]
fn invalid_chain_targets_and_cycles_are_diagnosed() {
    let root = fixture();
    definition(root.path(), "subagents/foreign.md", "agent: pi", "");
    for (fields, expected) in [
        ("model: custom", "model 'custom' names no runtime"),
        ("", "it sets no model"),
        ("agent: missing", "follows unknown profile 'missing'"),
        ("agent: foreign", "rimz resolves `agent:` within agents"),
        (
            "agent: probe",
            "follows itself through `agent:` (probe -> probe)",
        ),
        (
            "agent: claude\nmodel: astra\ntools: []",
            "runs on 'claude' but model 'astra' runs on 'codex'",
        ),
        ("model: astra", "lists no `tools`, which codex needs"),
    ] {
        definition(root.path(), "agents/probe.md", fields, "");
        error(root.path(), expected);
    }
    definition(root.path(), "agents/probe.md", "agent: other", "");
    definition(root.path(), "agents/other.md", "agent: probe", "");
    error(root.path(), "other -> probe -> other");
}

#[test]
fn frontmatter_rejects_bad_shapes_unknown_and_retired_keys() {
    let root = fixture();
    for (text, expected) in [
        ("body", "missing YAML frontmatter"),
        ("---\nmodel: astra", "never closes"),
        ("---\n[one, two]\n---", "not a mapping"),
        ("---\n[\n---", "malformed"),
        (
            "---\ndescription: Probe\nmodle: astra\n---",
            "unknown field",
        ),
        ("---\nsoul: old\n---", "still selects `soul:`"),
        ("---\nmeka: old\n---", "still sets `meka:`"),
        ("---\nsignals: []\n---", "reads on a team role"),
        ("---\nflip-compact: off\n---", "a solo seat flips no stage"),
        ("---\nmodel: astra\n---", "no non-empty `description:`"),
        (
            "---\ndescription: |\n  first\n  second\n---",
            "multi-line `description:`",
        ),
        (
            "---\ndescription: Probe\nmodel-reminder: wrong\n---",
            "malformed frontmatter",
        ),
        (
            "---\ndescription: Probe\nmode: wrong\n---",
            "malformed frontmatter",
        ),
        (
            "---\ndescription: Probe\ntools: Bash\n---",
            "malformed frontmatter",
        ),
    ] {
        write(root.path(), "agents/probe.md", text);
        error(root.path(), expected);
    }
}

#[test]
fn names_and_kind_bases_are_validated() {
    let root = fixture();
    for name in ["claude", "a.b", "a/b", ""] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("name: '{name}'\nagent: pi"),
            "",
        );
        assert!(
            !load(root.path(), SkillCheck::Skip, &CommandsConfig::default())
                .errors
                .is_empty()
        );
    }
    definition(
        root.path(),
        "agents/probe.md",
        "name: duplicate\nagent: pi",
        "",
    );
    definition(
        root.path(),
        "subagents/probe.md",
        "name: duplicate\nagent: pi",
        "",
    );
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message.contains("declared by both"))
    );
    assert!(!loaded.agent_profiles.0.contains_key("duplicate"));
    assert!(!loaded.subagent_profiles.0.contains_key("duplicate"));
    assert_eq!(
        loaded.failed["duplicate"],
        BTreeSet::from([
            root.path().join("agents/probe.md"),
            root.path().join("subagents/probe.md"),
        ])
    );
    write(
        root.path(),
        "agents/claude.md",
        "---\ndescription: Base\nmodel: fable\n---\nBase.",
    );
    error(root.path(), "unknown field");
    write(
        root.path(),
        "agents/claude.md",
        "---\ndescription: Base\n---",
    );
    error(root.path(), "no prompt body");
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert_eq!(
        loaded.failed["claude"],
        BTreeSet::from([root.path().join("agents/claude.md")])
    );
}

#[test]
fn prompt_taking_kinds_require_their_base_body_or_not() {
    let root = tempfile::tempdir().unwrap();
    assert!(clean(root.path()).errors.is_empty());
    // amp takes no system prompt, so only a craft would need its base.
    definition(root.path(), "agents/runner.md", "agent: amp", "");
    assert!(clean(root.path()).agent_profiles.0.contains_key("runner"));
    std::fs::remove_file(root.path().join("agents/runner.md")).unwrap();
    definition(
        root.path(),
        "agents/fable.md",
        "model: fable\ntools: [Bash]",
        "",
    );
    let missing = format!(
        "runs on claude, whose kind base is missing — create {} with a nonempty body, \
         the system prompt every claude definition starts from",
        root.path().join("agents/claude.md").display()
    );
    error(root.path(), &missing);
    definition(
        root.path(),
        "agents/fable.md",
        "model: fable\ntools: [Bash]",
        "Craft.",
    );
    error(root.path(), &missing);
}

#[test]
fn agents_and_seats_allow_loaded_subagents_and_commands() {
    let root = fixture();
    definition(
        root.path(),
        "subagents/explorer.md",
        "model: opus\ntools: [Read]",
        "",
    );
    definition(
        root.path(),
        "agents/worker.md",
        "model: opus\ntools: [Bash]\nsubagents: [explorer, vim]",
        "Craft.",
    );
    team_definition(root.path(), TEAM_STAGES, TEAM_ROLES, "Pipeline.");
    let commands: CommandsConfig = toml::from_str("vim = \"nvim\"").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &commands);
    assert!(loaded.errors.is_empty(), "{:#?}", loaded.errors);
    assert!(loaded.agent_profiles.0.contains_key("worker"));
    assert!(loaded.teams.0.contains_key("probe"));
    error(root.path(), "allows unknown subagent profile(s) [\"vim\"]");
}

#[test]
fn an_agent_allowing_a_failed_subagent_fails_on_its_own_file() {
    let root = fixture();
    definition(root.path(), "subagents/helper.md", "agent: claude", "");
    definition(
        root.path(),
        "agents/planner.md",
        "agent: claude\ntools: [Bash]\nsubagents: [helper]",
        "",
    );
    let loaded = load(root.path(), SkillCheck::Skip, &CommandsConfig::default());
    assert!(!loaded.agent_profiles.0.contains_key("planner"));
    assert!(loaded.errors.iter().any(|error| {
        error.path.ends_with("agents/planner.md")
            && error.message == "allows subagent 'helper', which failed to load"
    }));
}

#[test]
fn traits_deduplicate_and_preserve_unrelated_placeholders() {
    let root = fixture();
    write(root.path(), "traits/one.md", " One.\n");
    write(root.path(), "traits/two.md", "Two.");
    let source = root.path().join("agents/probe.md");
    let render = |body, names: &[&str]| {
        traits::render(
            root.path(),
            &source,
            body,
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        render("Head\n\n${traits}\n\nTail ${HOME}", &["one", "two", "one"]).unwrap(),
        "Head\n\nOne.\n\nTwo.\n\nTail ${HOME}"
    );
    assert_eq!(
        render("Head\n\n${traits}\n\nTail", &[]).unwrap(),
        "Head\n\n\n\nTail"
    );
    assert_eq!(
        render("A\n${traits}\nB\n\n${traits}\n\nC", &["two"]).unwrap(),
        "A\nTwo.\nB\n\nC"
    );
    assert!(
        render("No token", &["one"])
            .unwrap_err()
            .message
            .contains("no `${traits}` token")
    );
    assert!(render("${traits}", &["missing"]).is_err());
    assert!(render("${traits}", &["../outside"]).is_err());
}

#[test]
fn auto_compact_bounds_and_spelling() {
    let root = fixture();
    for value in ["100k", "500k", "200000", "1m", "1M"] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: []\nauto-compact: {value}"),
            "",
        );
        assert_eq!(
            clean(root.path()).agent_profiles.0["probe"]
                .auto_compact
                .as_deref(),
            Some(value)
        );
    }
    for value in [
        "200",
        "2m",
        "80%",
        "99999",
        "1000001",
        "-200000",
        "0.2m",
        "18446744073709551615m",
    ] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: []\nauto-compact: '{value}'"),
            "",
        );
        error(root.path(), "token count from 100k through 1M");
    }
}

#[test]
fn subagent_allowlists_replace_native_delegation() {
    let root = fixture();
    definition(root.path(), "subagents/child.md", "agent: pi", "");
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Bash]\nsubagents: [' general ', child, claude, child]",
        "",
    );
    assert_eq!(
        clean(root.path()).agent_profiles.0["probe"]
            .subagents
            .as_deref(),
        Some(
            [
                "general".to_owned(),
                "child".to_owned(),
                "claude".to_owned()
            ]
            .as_slice()
        )
    );
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Agent]\nsubagents: []",
        "",
    );
    error(root.path(), "lists the Agent tool");
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: []\nsubagents: [missing]",
        "",
    );
    error(root.path(), "unknown subagent profile(s)");
    definition(
        root.path(),
        "subagents/child.md",
        "agent: pi\nsubagents: []",
        "",
    );
    error(root.path(), "a child cannot launch again");
}

#[test]
fn skills_are_structural_without_a_library_and_deduplicated() {
    let root = fixture();
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Skill]\nskills: [' one ', one, two]",
        "",
    );
    let loaded = clean(root.path());
    let skills = loaded.agent_profiles.0["probe"].skills.as_ref().unwrap();
    assert_eq!(
        skills.iter().map(SkillName::as_str).collect::<Vec<_>>(),
        ["one", "two"]
    );
    for name in ["../etc/passwd", "one:auto", "white space", ".", "a/b"] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: [Skill]\nskills: ['{name}']"),
            "",
        );
        error(root.path(), "one bare directory name");
    }
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Bash]\nskills: []",
        "",
    );
    error(root.path(), "lists no Skill tool");
    definition(
        root.path(),
        "agents/probe.md",
        "agent: pi\nskills: null",
        "",
    );
    assert_eq!(
        clean(root.path()).agent_profiles.0["probe"].skills,
        Some(vec![])
    );
}

#[test]
fn skill_library_checks_existence_and_runtime_specific_markers() {
    let root = fixture();
    for (kind, fields) in [
        ("claude", "tools: [Skill]"),
        ("codex", "tools: [Skill]"),
        ("pi", ""),
    ] {
        definition(
            root.path(),
            &format!("agents/{kind}-seat.md"),
            &format!("agent: {kind}\n{fields}\nskills: [one]"),
            "",
        );
    }
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 3);
    assert!(
        loaded
            .errors
            .iter()
            .all(|error| error.message.contains("missing at"))
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: colons: are accepted\ndisable-model-invocation: true\n---\nSkill.",
    );
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 2);
    assert!(loaded.agent_profiles.0.contains_key("codex-seat"));
    for kind in ["claude", "pi"] {
        let error = loaded
            .errors
            .iter()
            .find(|error| error.path.ends_with(format!("agents/{kind}-seat.md")))
            .unwrap();
        assert_eq!(
            error.message,
            format!(
                "lists skill 'one', which {} marks user-only for {kind} (`disable-model-invocation: true`); listing cannot lift that marker: drop it from `skills:` or remove the marker",
                root.path().join("skills/one/SKILL.md").display()
            )
        );
    }
    write(
        root.path(),
        "skills/one/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 3);
    let error = loaded
        .errors
        .iter()
        .find(|error| error.path.ends_with("agents/codex-seat.md"))
        .unwrap();
    assert_eq!(
        error.message,
        format!(
            "lists skill 'one', which {} marks user-only for codex (`policy.allow_implicit_invocation: false`); listing cannot lift that marker: drop it from `skills:` or remove the marker",
            root.path().join("skills/one/agents/openai.yaml").display()
        )
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "Skill without metadata.",
    );
    write(root.path(), "skills/one/agents/openai.yaml", "policy: [");
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 3, "{:?}", loaded.errors);
    for (seat, file) in [
        ("claude", "skills/one/SKILL.md"),
        ("pi", "skills/one/SKILL.md"),
        ("codex", "skills/one/agents/openai.yaml"),
    ] {
        let error = loaded
            .errors
            .iter()
            .find(|error| error.path.ends_with(format!("agents/{seat}-seat.md")))
            .unwrap();
        let prefix = format!(
            "lists skill 'one', invalid at {}: ",
            root.path().join(file).display()
        );
        assert!(error.message.starts_with(&prefix), "{}", error.message);
        assert!(loaded.failed[&format!("{seat}-seat")].contains(&error.path));
    }
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: colons: accepted\n---\nSkill.",
    );
    write(
        root.path(),
        "skills/one/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: true\n",
    );
    assert!(load_checked(root.path()).errors.is_empty());

    std::fs::remove_dir_all(root.path().join("skills/one")).unwrap();
    write(
        root.path(),
        ".claude/skills/one/SKILL.md",
        "---\ndescription: provider copy\n---\nSkill.",
    );
    let loaded = load_checked(root.path());
    assert!(loaded.agent_profiles.0.contains_key("claude-seat"));
    assert_eq!(loaded.errors.len(), 2, "{:?}", loaded.errors);
    let codex = loaded
        .errors
        .iter()
        .find(|error| error.path.ends_with("agents/codex-seat.md"))
        .unwrap();
    for searched in [
        root.path().join(".agents/skills/one/SKILL.md"),
        root.path().join("skills/one/SKILL.md"),
    ] {
        assert!(
            codex.message.contains(&searched.display().to_string()),
            "{}",
            codex.message
        );
    }

    write(
        root.path(),
        ".claude/skills/one/SKILL.md",
        "---\ndescription: provider copy\ndisable-model-invocation: true\n---\nSkill.",
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: library copy\n---\nSkill.",
    );
    let loaded = load_checked(root.path());
    assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
    assert!(loaded.errors[0].path.ends_with("agents/claude-seat.md"));
    assert!(loaded.errors[0].message.contains("user-only"));

    write(
        root.path(),
        ".claude/skills/one/SKILL.md",
        "---\ndescription: provider copy\n---\nSkill.",
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: library copy\ndisable-model-invocation: true\n---\nSkill.",
    );
    let loaded = load_checked(root.path());
    assert!(loaded.agent_profiles.0.contains_key("claude-seat"));
    assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
    assert!(loaded.errors[0].path.ends_with("agents/pi-seat.md"));

    std::fs::remove_file(root.path().join(".claude/skills/one/SKILL.md")).unwrap();
    std::fs::create_dir(root.path().join(".claude/skills/one/SKILL.md")).unwrap();
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: library copy\n---\nSkill.",
    );
    let loaded = load_checked(root.path());
    assert!(!loaded.agent_profiles.0.contains_key("claude-seat"));
    assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
    assert!(loaded.errors[0].path.ends_with("agents/claude-seat.md"));
    assert!(
        loaded.errors[0].message.contains("unreadable at")
            && loaded.errors[0].message.contains(
                &root
                    .path()
                    .join(".claude/skills/one/SKILL.md")
                    .display()
                    .to_string()
            ),
        "{}",
        loaded.errors[0].message
    );
}

#[test]
fn yaml_comments_quotes_and_null_tools_are_supported() {
    let root = fixture();
    definition(
        root.path(),
        "agents/probe.md",
        "model: 'astra' # comment\ntools: null\nbudget: '3.50'\nmodel-reminder: true",
        "",
    );
    let loaded = clean(root.path());
    assert_eq!(
        loaded.agent_profiles.0["probe"].budget.as_deref(),
        Some("3.50")
    );
    let argv = shlex::split(loaded.agent_profiles.0["probe"].args.as_deref().unwrap()).unwrap();
    assert!(argv.contains(&"features.shell_tool=false".to_owned()));
}
