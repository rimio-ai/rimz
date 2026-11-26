use super::*;
use crate::agents::AgentStatus;
use crate::config::Profile;
use std::collections::BTreeMap;

fn caller(allowed: Option<Vec<&str>>) -> (AgentState, ProfilesConfig) {
    let mut caller = AgentState::stub("claude", "parent", AgentStatus::Running);
    caller.profile = Some("planner".to_owned());
    let profiles = ProfilesConfig::from(BTreeMap::from([(
        "planner".to_owned(),
        Profile {
            empty_dir: None,
            allowed_tools: None,
            variant: None,
            definition_renders: None,
            definition: None,
            kind_defaults: false,
            kind_base: false,
            built_in_base: false,
            tools: None,
            auto_compact_default: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "claude".to_owned(),
            description: None,
            model_reminder: None,
            keep_warm: None,
            idle_compact: None,
            smart_compact: None,
            subagents: allowed.map(|values| values.into_iter().map(str::to_owned).collect()),
            mode: None,
            model: None,
            effort: None,
            fast: None,
            code_mode: None,
            budget: None,
            timeout: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            seat_consensus_skill: false,
            mcps: None,
            args: None,
        },
    )]));
    (caller, profiles)
}

#[test]
fn allowlist_checks_spec_and_agent_override() {
    let (caller, profiles) = caller(Some(vec!["explorer", "designer"]));
    let empty = ProfilesConfig::default();
    assert!(check_allowed(&caller, &profiles, &empty, " explorer ").is_ok());

    let error = check_allowed(&caller, &profiles, &empty, "codex").unwrap_err();
    assert!(error.to_string().contains("profile `planner`"));
    assert!(error.to_string().contains("[explorer, designer]"));
}

#[test]
fn caller_recorded_agent_override_governs_allowlist_and_catalog() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("subagents")).unwrap();
    for (rel, content) in [
        (
            "agents/antigravity.md",
            "---\ndescription: Antigravity base\nsubagents: [other]\ntools: [Read]\n---\nAntigravity.",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\ntools: [Read]\n---\nCodex.",
        ),
        (
            "agents/astra.md",
            "---\ndescription: Astra\nagent: codex\nsubagents: [advisor]\n---\nAstra.",
        ),
        (
            "agents/mute.md",
            "---\ndescription: Mute\nagent: codex\nsubagents: []\n---\nMute.",
        ),
        (
            "agents/reviewer.md",
            "---\ndescription: Reviewer\nagent: antigravity\n---\nReviewer.",
        ),
        (
            "subagents/advisor.md",
            "---\ndescription: Advisor\nagent: antigravity\n---\nAdvisor.",
        ),
        (
            "subagents/other.md",
            "---\ndescription: Other\nagent: codex\n---\nOther.",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    let machine = crate::config::MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        None,
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    let profiles = definitions.agent_profiles;
    let subagents = definitions.subagent_profiles;

    let make_caller = |profile: Option<&str>, recorded_agent: Option<&str>| {
        let mut agent = AgentState::stub("codex", "parent", AgentStatus::Running);
        agent.profile = profile.map(str::to_owned);
        if let Some(base) = recorded_agent {
            agent.record = Some(Box::new(crate::agents::LaunchRecord {
                agent: Some(base.to_owned()),
                ..Default::default()
            }));
        }
        agent
    };
    let reviewer_astra = make_caller(Some("reviewer"), Some("astra"));
    let bare_astra = make_caller(None, Some("astra"));
    let bare_mute = make_caller(None, Some("mute"));
    let reviewer_missing = make_caller(Some("reviewer"), Some("deleted"));
    let actual = vec![
        (
            "reviewer under astra allows advisor",
            check_allowed(&reviewer_astra, &profiles, &subagents, "advisor").is_ok(),
        ),
        (
            "reviewer under astra refuses other",
            check_allowed(&reviewer_astra, &profiles, &subagents, "other").is_err(),
        ),
        (
            "bare codex under astra allows advisor",
            check_allowed(&bare_astra, &profiles, &subagents, "advisor").is_ok(),
        ),
        (
            "bare codex under astra refuses other",
            check_allowed(&bare_astra, &profiles, &subagents, "other").is_err(),
        ),
        (
            "bare codex under mute refuses advisor",
            check_allowed(&bare_mute, &profiles, &subagents, "advisor").is_err(),
        ),
        (
            "unresolved X on reviewer under codex leaves list unrestricted",
            check_allowed(&reviewer_missing, &profiles, &subagents, "other").is_ok(),
        ),
    ];
    assert_eq!(
        actual,
        vec![
            ("reviewer under astra allows advisor", true),
            ("reviewer under astra refuses other", true),
            ("bare codex under astra allows advisor", true),
            ("bare codex under astra refuses other", true),
            ("bare codex under mute refuses advisor", true),
            (
                "unresolved X on reviewer under codex leaves list unrestricted",
                true
            ),
        ]
    );
}

#[test]
fn missing_policy_allows_all_and_empty_policy_allows_none() {
    let (unrestricted, profiles) = caller(None);
    let empty = ProfilesConfig::default();
    assert!(check_allowed(&unrestricted, &profiles, &empty, "codex").is_ok());

    let mut bare = unrestricted;
    bare.profile = None;
    assert!(check_allowed(&bare, &profiles, &empty, "codex").is_ok());

    let (caller, profiles) = caller(Some(Vec::new()));
    assert!(check_allowed(&caller, &profiles, &empty, "explorer").is_err());
}

#[test]
fn mini_catalog_and_allowlist_keep_literal_runtime_names() {
    let (mut agent, profiles) = caller(Some(vec!["opencode-mini"]));
    let children: ProfilesConfig = toml::from_str("[opencode]\nagent = 'opencode'").unwrap();
    let listed = catalog(
        Some("planner"),
        None,
        None,
        ProfileScope::Agents,
        &profiles,
        &children,
        &CommandsConfig::default(),
    );
    assert!(matches!(listed, SubagentCatalog::Available(specs)
                if specs.len() == 1 && specs[0].name == "opencode-mini"
                    && specs[0].agent.as_deref() == Some("opencode")));
    agent.profile = Some("planner".into());
    assert!(check_allowed(&agent, &profiles, &children, "opencode-mini").is_ok());
    assert!(check_allowed(&agent, &profiles, &children, "opencode").is_err());
    let (agent, profiles) = caller(Some(vec!["opencode"]));
    assert!(check_allowed(&agent, &profiles, &children, "opencode-mini").is_err());
}

#[test]
fn alias_allowlists_name_their_kinds_base() {
    let children: ProfilesConfig =
        toml::from_str("[minimax]\nagent = 'minimax'\n[antigravity]\nagent = 'antigravity'")
            .unwrap();
    for (alias, kind) in [("mcode", "minimax"), ("agy", "antigravity")] {
        for (allowed, requested) in [(alias, kind), (kind, alias)] {
            let (agent, profiles) = caller(Some(vec![allowed]));
            let listed = catalog(
                Some("planner"),
                None,
                None,
                ProfileScope::Agents,
                &profiles,
                &children,
                &CommandsConfig::default(),
            );
            assert!(
                matches!(&listed, SubagentCatalog::Available(specs)
                        if specs.len() == 1 && specs[0].name == kind),
                "{allowed}: {listed:?}"
            );
            assert!(
                check_allowed(&agent, &profiles, &children, requested).is_ok(),
                "{allowed}"
            );
        }
    }

    let (agent, profiles) = caller(Some(vec!["agy"]));
    let commands = CommandsConfig(BTreeMap::from([("agy".to_owned(), "echo hi".to_owned())]));
    let listed = catalog(
        Some("planner"),
        None,
        None,
        ProfileScope::Agents,
        &profiles,
        &ProfilesConfig::default(),
        &commands,
    );
    assert!(
        matches!(&listed, SubagentCatalog::Available(specs)
                if specs.len() == 1
                    && specs[0].name == "agy"
                    && specs[0].source == SubagentProfileSource::Command),
        "command agy: {listed:?}"
    );
    assert!(check_allowed(&agent, &profiles, &ProfilesConfig::default(), "agy").is_ok());
}

#[test]
fn catalog_combines_profiles_and_commands() {
    let (_, profiles) = caller(None);
    let subagent_profiles = ProfilesConfig::from(BTreeMap::from([(
        "explorer".to_owned(),
        Profile {
            empty_dir: None,
            allowed_tools: None,
            variant: None,
            definition_renders: None,
            definition: None,
            kind_defaults: false,
            kind_base: false,
            built_in_base: false,
            tools: None,
            auto_compact_default: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "claude".to_owned(),
            description: Some("Finds files and traces code paths".to_owned()),
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            idle_compact: None,
            smart_compact: None,
            mode: None,
            model: Some("sonnet".to_owned()),
            effort: Some("low".to_owned()),
            fast: None,
            code_mode: None,
            budget: None,
            timeout: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            seat_consensus_skill: false,
            mcps: None,
            args: None,
        },
    )]));
    let commands = CommandsConfig(BTreeMap::from([(
        "lint".to_owned(),
        "cargo clippy".to_owned(),
    )]));

    let SubagentCatalog::Available(specs) = catalog(
        None,
        None,
        None,
        ProfileScope::Agents,
        &profiles,
        &subagent_profiles,
        &commands,
    ) else {
        panic!("unrestricted catalog is available");
    };
    assert_eq!(
        specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        ["explorer", "lint"]
    );
    assert_eq!(specs[0].source, SubagentProfileSource::Profile);
    assert_eq!(specs[1].source, SubagentProfileSource::Command);
}

#[test]
fn catalog_filters_by_literal_name_and_distinguishes_disabled() {
    let (_, profiles) = caller(Some(vec!["lint"]));
    let commands = CommandsConfig(BTreeMap::from([(
        "lint".to_owned(),
        "cargo clippy".to_owned(),
    )]));
    let SubagentCatalog::Available(specs) = catalog(
        Some("planner"),
        None,
        None,
        ProfileScope::Agents,
        &profiles,
        &ProfilesConfig::default(),
        &commands,
    ) else {
        panic!("non-empty allowlist is available");
    };
    assert_eq!(
        specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        ["lint"]
    );

    let (_, profiles) = caller(Some(Vec::new()));
    for commands in [CommandsConfig::default(), commands] {
        assert_eq!(
            catalog(
                Some("planner"),
                None,
                None,
                ProfileScope::Agents,
                &profiles,
                &ProfilesConfig::default(),
                &commands
            ),
            SubagentCatalog::NoneAllowed
        );
    }
}

#[test]
fn catalog_without_profiles_or_commands_is_disabled() {
    let (_, profiles) = caller(None);
    assert_eq!(
        catalog(
            Some("planner"),
            None,
            None,
            ProfileScope::Agents,
            &profiles,
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
        ),
        SubagentCatalog::Disabled
    );
}

#[test]
fn catalog_with_no_matching_allowed_specs_is_disabled() {
    let (_, profiles) = caller(Some(vec!["missing"]));
    let commands = CommandsConfig(BTreeMap::from([(
        "lint".to_owned(),
        "cargo clippy".to_owned(),
    )]));
    assert_eq!(
        catalog(
            Some("planner"),
            None,
            None,
            ProfileScope::Agents,
            &profiles,
            &ProfilesConfig::default(),
            &commands,
        ),
        SubagentCatalog::Disabled
    );
}

#[test]
fn reminder_renders_available_specs_and_disabled_policy() {
    let available = SubagentCatalog::Available(vec![
        SubagentProfile {
            name: "explorer".to_owned(),
            source: SubagentProfileSource::Profile,
            agent: Some("claude".to_owned()),
            model: Some("sonnet".to_owned()),
            effort: Some("low".to_owned()),
            description: Some("Finds files and traces code paths".to_owned()),
        },
        SubagentProfile {
            name: "lint".to_owned(),
            source: SubagentProfileSource::Command,
            agent: None,
            model: None,
            effort: None,
            description: None,
        },
    ]);
    let text = reminder(&available, "skill(rimz-subagents)");
    assert_eq!(
        text,
        format!(
            "{AVAILABLE_PREAMBLE}Launch them through skill(rimz-subagents). `subagent(x)` in \
                 any text means the profile `x` in this list:\n\n\
                 - `explorer`: Finds files and traces code paths\n\
                 - `lint`"
        )
    );
    assert_eq!(
        reminder(&available, "mcp(subagent)"),
        format!(
            "{AVAILABLE_PREAMBLE}Launch them through mcp(subagent). `subagent(x)` in any text \
                 means the profile `x` in this list:\n\n\
                 - `explorer`: Finds files and traces code paths\n\
                 - `lint`"
        )
    );
    assert_eq!(
        reminder(&SubagentCatalog::Disabled, "skill(rimz-subagents)"),
        "### Subagents\n\nNo subagent profile is configured for you, so none is listed here. Do the work yourself with your direct tools."
    );
    assert_eq!(
        reminder(&SubagentCatalog::NoneAllowed, "skill(rimz-subagents)"),
        "### Subagents\n\nSubagents are disabled for this agent: its profile allows none, so any subagent launch is refused. Do the work yourself with your direct tools."
    );
}

#[test]
fn unchanged_profile_reads_subagents_directly_without_parent_or_kind_base_walk() {
    let home = tempfile::tempdir().unwrap();
    let def = |rel: &str, yaml: &str| {
        let path = home.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("---\ndescription: d\n{yaml}\n---\nB.\n")).unwrap();
    };
    def("agents/claude.md", "tools: [Read]\nsubagents: [other]");
    def("subagents/advisor.md", "agent: claude");
    def("subagents/other.md", "agent: claude");
    def("agents/parent.md", "agent: claude\nsubagents: [advisor]");
    def("agents/child.md", "agent: parent");
    def("agents/preset.md", "agent: claude\ntools: [default]");
    let loaded = crate::config::definitions::load(
        home.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &home.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        Some(crate::config::AutoCompactDefault::BuiltIn),
    );
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    let mut profiles = loaded.agent_profiles;
    assert_eq!(
        profiles.0["child"].definition.as_ref().unwrap().chain(),
        &["child", "parent"]
    );
    // Clear `child`'s own allowlist so a post-launch walk up `parent` or `claude` would leak.
    profiles.0.get_mut("child").unwrap().subagents = None;
    let empty = ProfilesConfig::default();
    let check = |name| {
        allowed_specs(
            Some(name),
            None,
            Some("claude"),
            ProfileScope::Agents,
            &profiles,
            &empty,
        )
    };
    assert_eq!(check("child"), None);
    assert_eq!(check("parent"), Some(vec!["advisor".to_owned()]));
    // Claude's preset holds the native Agent tool, so no lower list reaches it.
    for agent in [None, Some("claude")] {
        assert_eq!(
            allowed_specs(
                Some("preset"),
                agent,
                Some("claude"),
                ProfileScope::Agents,
                &profiles,
                &empty,
            ),
            None,
            "{agent:?}"
        );
    }
}
