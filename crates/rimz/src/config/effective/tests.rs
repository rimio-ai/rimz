use super::*;
use crate::agents::PermissionMode;
use crate::config::agents::AgentsConfig;
use crate::config::{CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig};
use std::collections::BTreeMap;
use tempfile::tempdir;

/// `rimz agents <spec>` under these flags, as the launch resolves it.
#[expect(clippy::too_many_arguments, reason = "one argument per launch flag")]
fn launch_under(
    launch: &LaunchAgents,
    tiers: &crate::config::tiers::TierConfig,
    scope: ProfileScope,
    spec: &str,
    tier: Option<crate::config::tiers::ModelTier>,
    model: Option<&str>,
    agent: Option<&str>,
    unavailable: impl Fn(&str, &str) -> Option<crate::agents::TierSkipReason> + Send + Sync + 'static,
) -> std::result::Result<
    crate::harness::plan::ResolvedLaunch,
    crate::harness::plan::ResolveLaunchError,
> {
    let selection = crate::config::Selection::new(tiers)
        .agent(agent)
        .model(model)
        .tier(tier)
        .availability(unavailable);
    crate::harness::plan::resolve_launch(
        launch,
        scope,
        &CommandsConfig::default(),
        Some(spec),
        &selection,
    )
}

/// The first agent cell of a resolved launch.
fn first_cell(resolved: crate::harness::plan::ResolvedLaunch) -> crate::harness::spec::AgentCell {
    resolved.layout.agent_cells().next().unwrap().clone()
}

#[test]
fn keep_warm_declared_horizon_resolves_seats_without_live_admission() {
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for (name, content) in [
        ("pi", "---\ndescription: Pi base\n---\nPi house.\n"),
        (
            "base",
            "---\ndescription: Base\nagent: pi\nkeep-warm: 1h\n---\n",
        ),
        ("worker", "---\ndescription: Worker\nagent: base\n---\n"),
        (
            "off",
            "---\ndescription: Off\nagent: worker\nkeep-warm: off\n---\n",
        ),
    ] {
        std::fs::write(home.path().join(format!("agents/{name}.md")), content).unwrap();
    }
    let machine =
        MachineConfig::parse_text(&home.path().join("config.toml"), "", home.path()).unwrap();
    let profiles = machine.agents.profiles;
    let teams: TeamsConfig = toml::from_str(
        "[forge]\nkeep-warm = '2h'\nroles = [\
         {role = 'lead', profile = 'worker', keep-warm = '3h'},\
         {role = 'quiet', profile = 'worker', keep-warm = 'off'}]\n\
         [staged]\nkeep-warm = {max = 'off', while = 'team.stage!=Done'}",
    )
    .unwrap();
    let harness = toml::from_str("keep_warm = '4h'\ncache_keepalive_max = '5h'").unwrap();
    let actual: Vec<_> = [
        (Some("forge"), None, None, false),
        (Some("forge"), Some("lead"), None, false),
        (Some("forge"), Some("quiet"), None, false),
        (Some("missing"), None, Some("worker"), false),
        (None, None, Some("worker"), false),
        (None, None, Some("off"), false),
        (None, None, None, false),
        (Some("staged"), None, None, false),
        (None, None, Some("worker"), true),
    ]
    .map(|(team, role, profile, launched_child)| {
        KeepWarmSeat {
            team,
            role,
            profile,
            agent: None,
            kind: None,
            scope: ProfileScope::Agents,
            launched_child,
        }
        .declared_horizon(&teams, &profiles, &ProfilesConfig::default(), &harness)
        .map(|(max, clause)| (max.as_secs(), clause))
    })
    .into();
    assert_eq!(
        actual,
        [
            Some((7200, None)),
            Some((10800, None)),
            None,
            None,
            Some((3600, None)),
            None,
            Some((14400, None)),
            Some((18000, Some("team.stage!=Done".into()))),
            None,
        ]
    );
    let harness = toml::from_str("cache_keepalive_max = 'off'").unwrap();
    assert_eq!(
        KeepWarmSeat {
            team: Some("staged"),
            role: None,
            profile: None,
            agent: None,
            kind: None,
            scope: ProfileScope::Agents,
            launched_child: false,
        }
        .declared_horizon(&teams, &profiles, &ProfilesConfig::default(), &harness),
        None
    );
}

#[test]
fn flags_and_role_models_choose_every_agent_cell_of_a_team() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for (name, fields) in [("claude", ""), ("codex", ""), ("lead", "agent: claude\n")] {
        std::fs::write(
            home.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Probe\n{fields}tools: [Read]\n---\nCraft.\n"),
        )
        .unwrap();
    }
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.lead]\ndescription = 'Lead'\nagent = 'claude'\n\
         [agents.teams.crew]\nleader = 'lead'\nlayout = 'lead,claude'\n\
         roles = [{ role = 'lead', profile = 'lead', model = 'astra' }]\n",
    );
    let cells = |tier, model| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "crew",
            tier,
            model,
            None,
            |_, _| None,
        )
        .unwrap()
        .layout
        .agent_cells()
        .map(|cell| {
            (
                cell.kind.to_string(),
                cell.launch.model.clone(),
                cell.launch.tier.as_ref().map(|stamp| stamp.tier),
            )
        })
        .collect::<Vec<_>>()
    };
    let senior = super::super::tiers::ModelTier::Senior;
    assert_eq!(
        cells(None, None)[0],
        ("codex".to_owned(), Some("astra".to_owned()), None),
        "a role's model moves the seat to its provider"
    );
    assert_eq!(
        cells(None, Some("opus")),
        [
            ("claude".to_owned(), Some("opus".to_owned()), None),
            ("claude".to_owned(), Some("opus".to_owned()), None),
        ],
        "--model outranks the role's model and reaches the layout-only cell"
    );
    let tiered = cells(Some(senior), None);
    assert_eq!(
        tiered.iter().map(|(_, _, tier)| *tier).collect::<Vec<_>>(),
        [Some(senior), Some(senior)],
        "--tier outranks the role's model and reaches the layout-only cell: {tiered:?}"
    );
    // Under an `--agent` of the profile's own kind the role's model is judged from the kind it
    // moves the seat to, as a Markdown seat's is: claude keeps none of codex's, so it drops.
    let lead = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "crew.lead",
            None,
            None,
            Some("claude"),
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(
        (lead.kind.as_str(), lead.launch.model.as_deref()),
        ("claude", None)
    );
    assert!(
        lead.notices
            .contains(&crate::harness::spec::CellNotice::DroppedModel {
                model: "astra".to_owned()
            }),
        "{:?}",
        lead.notices
    );

    let commands = CommandsConfig(BTreeMap::from([(
        "logs".to_owned(),
        "tail -f log".to_owned(),
    )]));
    let error = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &commands,
        Some("logs"),
        &crate::config::Selection::new(&machine.tiers).tier(Some(senior)),
    )
    .map(|_| ())
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--tier needs an agent cell, and `logs` launches none"
    );
}

/// A project role whose model moves its seat onto a kind it cannot run on fails its own team:
/// every other launch of the project loads, and the team's launches refuse with the build error.
#[test]
fn a_role_that_cannot_run_on_the_kind_its_model_picks_fails_only_its_team() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for (kind, fields) in [("claude", "tools: [Read]\n"), ("codex", "")] {
        std::fs::write(
            home.path().join(format!("agents/{kind}.md")),
            format!("---\ndescription: Base\n{fields}---\n{kind} house.\n"),
        )
        .unwrap();
    }
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.writer]\ndescription = 'Writer'\nagent = 'claude'\n\
         [profiles.healthy]\ndescription = 'Healthy'\nagent = 'claude'\n\
         [agents.teams.squad]\nleader = 'coder'\nlayout = 'coder'\n\
         roles = [{ role = 'coder', profile = 'writer', model = 'astra' }]\n",
    );
    let resolve = |spec: &str| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            None,
            None,
            None,
            |_, _| None,
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    assert_eq!(resolve("healthy"), Ok(()));
    for spec in ["squad", "squad.coder"] {
        let error = resolve(spec).unwrap_err();
        assert!(
            error.contains("cannot run on codex") && error.contains("requires a tools list"),
            "{spec}: {error}"
        );
    }
}

#[test]
fn a_project_team_without_a_leader_fails_alone_and_warns() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.writer]\ndescription = 'Writer'\nagent = 'claude'\n\
         [agents.teams.crew]\n\
         roles = [{ role = 'planner', profile = 'writer' }, { role = 'coder', profile = 'writer' }]\n\
         [agents.teams.pair]\nlayout = 'claude,writer'\n\
         [agents.teams.twin]\nlayout = 'claude,claude-fast'\n\
         [agents.teams.led]\nleader = 'planner'\nroles = [{ role = 'planner', profile = 'writer' }]\n",
    );
    let resolve = |spec: &str| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            None,
            None,
            None,
            |_, _| None,
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    assert_eq!([resolve("led"), resolve("writer")], [Ok(()), Ok(())]);
    let crew = "team `crew` must declare a `leader`; add `leader = \"<role>\"` naming one of its \
                roles: planner, coder";
    let pair = "team `pair` must declare a `leader`; add `leader = \"<profile>\"` naming the \
                profile or kind that only one of its layout cells carries: claude, writer";
    let twin = "team `twin` must declare a `leader`; no cell of its layout claude,claude-fast \
                has a profile or kind of its own, so give one cell a profile no other cell carries \
                and add `leader = \"<profile>\"` naming it";
    for (spec, message) in [
        ("crew", crew),
        ("crew.coder", crew),
        ("pair", pair),
        ("twin", twin),
    ] {
        let error = resolve(spec).unwrap_err();
        assert!(error.contains(message), "{spec}: {error}");
    }
    assert_eq!(
        ["crew", "pair", "twin", "led"].map(|team| launch.teams.0.contains_key(team)),
        [false, false, false, true]
    );
    assert!(launch.block_failed_team("crew.planner").is_err());
    let config = project.path().join(".rimz/config.toml");
    assert_eq!(
        launch.project_definition_errors(),
        [crew, pair, twin].map(|message| format!("{}: {message}", config.display()))
    );

    // Each value the message offers, once written, loads and launches.
    for leader in ["claude", "writer"] {
        let project = tempdir().unwrap();
        let (machine, launch) = load_trusted_project(
            home.path(),
            project.path(),
            "",
            &format!(
                "[profiles.writer]\ndescription = 'Writer'\nagent = 'claude'\n\
                 [agents.teams.pair]\nleader = '{leader}'\nlayout = 'claude,writer'\n"
            ),
        );
        let launched = launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "pair",
            None,
            None,
            None,
            |_, _| None,
        );
        assert!(launched.is_ok(), "{leader}: {:?}", launched.err());
        assert_eq!(launch.project_definition_errors(), [] as [String; 0]);
    }
}

/// A launch whose final kind base comes from the tables rather than from a definition built for
/// that kind (a bare name or a kind base, under no `--agent` definition) reads that base as a
/// definition does: a failed one is excluded from a tier walk and refuses a launch with no walk,
/// and a failed project row named after a runtime is not the base a walk lands on. A failed
/// reference refuses ahead of a tier error, wherever it sits in the layout.
#[test]
fn a_failed_kind_base_is_read_on_every_bare_path_and_refuses_ahead_of_a_tier_error() {
    let write = |home: &std::path::Path, files: &[(&str, &str)]| {
        std::fs::create_dir_all(home.join("agents")).unwrap();
        for (name, text) in files {
            std::fs::write(home.join(format!("agents/{name}.md")), text).unwrap();
        }
    };
    let tiers =
        "[tiers]\nintern = []\njunior = ['sol']\nsenior = ['astra', 'opus']\nprincipal = []\n";
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    write(
        home.path(),
        &[
            (
                "claude",
                "---\ndescription: Base\ntools: [Read]\n---\nClaude.\n",
            ),
            ("codex", "---\ndescription: Broken\n---\n"),
            (
                "ok",
                "---\ndescription: Ok\nagent: claude\ntools: [Read]\n---\nOk.\n",
            ),
            (
                "bad",
                "---\ndescription: Bad\nagent: nonexistent-parent\n---\nBad.\n",
            ),
        ],
    );
    let machine =
        MachineConfig::parse_text(&home.path().join("config.toml"), tiers, home.path()).unwrap();
    let launch = load_with_roots(&machine, project.path(), home.path()).unwrap();
    let tier = |name: &str| name.parse::<super::super::tiers::ModelTier>().ok();
    let run = |spec: &str, tier_name: &str, agent: Option<&str>| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            tier(tier_name),
            None,
            agent,
            |_, _| None,
        )
        .map(|resolved| {
            let cell = first_cell(resolved);
            (cell.kind.to_string(), cell.launch.model)
        })
        .map_err(|err| {
            err.to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    };
    let codex = home.path().join("agents/codex.md").display().to_string();
    assert_eq!(
        run("claude", "junior", Some("claude")),
        Ok(("claude".to_owned(), Some("opus".to_owned()))),
        "a kind-base --agent excludes the failed base, so the walk climbs past it"
    );
    assert_eq!(
        run("cursor", "senior", None),
        Ok(("claude".to_owned(), Some("opus".to_owned()))),
        "a bare name's walk excludes the failed base"
    );
    assert_eq!(
        run("codex", "principal", None),
        Err(format!(
            "{codex}: kind base has no prompt body — write the system prompt every codex \
             definition starts from below the frontmatter"
        )),
        "the failed spec refuses ahead of the tier error"
    );
    let bad = format!(
        "{}: follows unknown profile 'nonexistent-parent'",
        home.path().join("agents/bad.md").display()
    );
    for spec in ["ok,bad", "bad,ok"] {
        assert_eq!(run(spec, "principal", None), Err(bad.clone()), "{spec}");
    }

    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    write(
        home.path(),
        &[(
            "codex",
            "---\ndescription: Base\ntools: [Read]\n---\nCodex.\n",
        )],
    );
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "[tiers]\nprincipal = ['astra']\nsenior = ['opus']\n",
        "[profiles.codex]\ndescription = 'Shadow'\nagent = 'codex'\n",
    );
    assert!(launch.definition_failure_for("codex").is_some());
    let cell = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "claude",
            tier("principal"),
            None,
            None,
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(
        (cell.kind.as_str(), cell.launch.model.as_deref()),
        ("codex", Some("astra")),
        "a failed project row named after the kind is not the base the walk lands on"
    );

    // With no written base for the kind either, the walk lands on its built-in base, and the
    // failed row refuses only the launches whose words name it.
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    write(home.path(), &[]);
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "[tiers]\nprincipal = ['astra']\nsenior = ['opus']\n",
        "[profiles.codex]\ndescription = 'Shadow'\nagent = 'codex'\n",
    );
    let failed = launch.definition_failure_for("codex").unwrap();
    let run = |spec: &str, tier_name: Option<&str>| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            tier_name.and_then(tier),
            None,
            None,
            |_, _| None,
        )
        .map(|resolved| {
            let cell = first_cell(resolved);
            (cell.kind.to_string(), cell.launch.model)
        })
        .map_err(|err| err.to_string())
    };
    assert_eq!(
        run("claude", Some("principal")),
        Ok(("codex".to_owned(), Some("astra".to_owned()))),
        "the built-in base is what the walk lands on"
    );
    for spec in ["codex", "codex,claude", "claude,codex"] {
        let refusal = run(spec, None).unwrap_err();
        assert!(refusal.contains(&failed), "{spec}: {refusal}");
    }
}

/// A `team.role` launch applies `--agent` to the one role it starts: a sibling role it does not
/// start resolves with no flags, while a launch of the whole team applies them to every role.
#[test]
fn a_team_role_launch_applies_its_agent_to_that_role_alone() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(project.path().join("prompt.md"), "Review.\n").unwrap();
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.coder]\ndescription = 'Coder'\nagent = 'claude'\n\
         [profiles.reviewer]\ndescription = 'Reviewer'\nagent = 'claude'\n\
         system-prompt-file = 'prompt.md'\n\
         [agents.teams.crew]\nleader = 'coder'\nroles = [\
         { role = 'coder', profile = 'coder' }, { role = 'reviewer', profile = 'reviewer' }]\n",
    );
    let resolve = |spec: &str| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            None,
            None,
            Some("cursor"),
            |_, _| None,
        )
        .map(|resolved| first_cell(resolved).kind.to_string())
        .map_err(|err| err.to_string())
    };
    assert_eq!(resolve("crew.coder"), Ok("cursor".to_owned()));
    let whole = resolve("crew").unwrap_err();
    assert!(whole.contains("reviewer"), "{whole}");
}

#[test]
fn a_kind_base_sent_by_model_to_a_failed_machine_base_names_its_load_failure() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for (kind, content) in [
        ("claude", "---\ndescription: Base\n---\nclaude house.\n"),
        ("codex", "---\ndescription: Base\n---\n"),
    ] {
        std::fs::write(home.path().join(format!("agents/{kind}.md")), content).unwrap();
    }
    let machine =
        MachineConfig::parse_text(&home.path().join("config.toml"), "", home.path()).unwrap();
    let launch = load_with_roots(&machine, project.path(), home.path()).unwrap();
    let error = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "claude",
        None,
        Some("sol"),
        None,
        |_, _| None,
    )
    .map(|_| ())
    .unwrap_err()
    .to_string();
    let codex = home.path().join("agents/codex.md");
    assert!(
        error.contains(&format!(
            "{}: kind base has no prompt body",
            codex.display()
        )),
        "{error}"
    );
    assert!(!error.contains("kind base is missing"), "{error}");
}

#[test]
fn smart_compact_resolves_role_profile_scope_and_explicit_off() {
    use crate::store::message::AutoCompact;
    let agents_home = tempdir().unwrap();
    let subagents_home = tempdir().unwrap();
    std::fs::create_dir_all(agents_home.path().join("agents")).unwrap();
    std::fs::create_dir_all(subagents_home.path().join("agents")).unwrap();
    std::fs::create_dir_all(subagents_home.path().join("subagents")).unwrap();
    for (root, rel, content) in [
        (
            agents_home.path(),
            "agents/claude.md",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
        ),
        (
            agents_home.path(),
            "agents/base.md",
            "---\ndescription: Base\nagent: claude\nsmart-compact: 200k\n---\n",
        ),
        (
            agents_home.path(),
            "agents/worker.md",
            "---\ndescription: Worker\nagent: base\n---\n",
        ),
        (
            agents_home.path(),
            "agents/off.md",
            "---\ndescription: Off\nagent: base\nsmart-compact: off\n---\n",
        ),
        (
            agents_home.path(),
            "agents/solo.md",
            "---\ndescription: Solo\nagent: claude\nsmart-compact: 70%\n---\n",
        ),
        (
            subagents_home.path(),
            "agents/claude.md",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
        ),
        (
            subagents_home.path(),
            "subagents/base.md",
            "---\ndescription: Sub base\nagent: claude\nsmart-compact: 50k\n---\n",
        ),
        (
            subagents_home.path(),
            "subagents/worker.md",
            "---\ndescription: Sub worker\nagent: base\n---\n",
        ),
    ] {
        std::fs::write(root.join(rel), content).unwrap();
    }
    let machine = MachineConfig::parse_text(
        &agents_home.path().join("config.toml"),
        "",
        agents_home.path(),
    )
    .unwrap();
    let sub_machine = MachineConfig::parse_text(
        &subagents_home.path().join("config.toml"),
        "",
        subagents_home.path(),
    )
    .unwrap();
    let mut profiles = machine.agents.profiles;
    profiles
        .0
        .insert("cycle".into(), toml::from_str("agent = 'cycle'").unwrap());
    let children = sub_machine.subagents.profiles;
    let teams: TeamsConfig = toml::from_str(
        "[forge]\nroles = [\
         {role = 'lead', profile = 'worker', smart-compact = '80%'},\
         {role = 'quiet', profile = 'worker', smart-compact = 'off'}]",
    )
    .unwrap();
    let harness = crate::config::HarnessConfig::default();
    let actual: Vec<_> = [
        (None, "worker", ProfileScope::Agents),
        (Some("lead"), "worker", ProfileScope::Agents),
        (Some("quiet"), "worker", ProfileScope::Agents),
        (None, "off", ProfileScope::Agents),
        (None, "worker", ProfileScope::Subagents),
        (None, "solo", ProfileScope::Subagents),
        (None, "missing", ProfileScope::Agents),
        (None, "cycle", ProfileScope::Agents),
    ]
    .map(|(role, profile, scope)| {
        let seat = SmartCompactSeat {
            team: Some("forge"),
            role,
            profile: Some(profile),
            agent: None,
            kind: None,
            scope,
        };
        (
            seat.resolve(&teams, &profiles, &children, &harness),
            seat.explicit(&teams, &profiles, &children).is_some(),
        )
    })
    .into();
    assert_eq!(
        actual,
        [
            (Some(AutoCompact::Tokens(200_000)), true),
            (Some(AutoCompact::Percent(80)), true),
            (None, true),
            (None, true),
            (Some(AutoCompact::Tokens(50_000)), true),
            (Some(AutoCompact::Percent(70)), true),
            (harness.smart_compact, false),
            (harness.smart_compact, false),
        ]
    );
}

#[test]
fn failed_profile_gate_follows_the_supplied_table_without_replacing_resolution_errors() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    let (_machine, loaded) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.worker]\ndescription = 'Claude worker'\nagent = 'claude'\n\
         [profiles.other]\ndescription = 'Codex worker'\nagent = 'codex'\n",
    );
    let mut profiles = loaded.profiles.clone();
    // Drop the claude kind base to mirror a failed kind base that never loaded into `profiles`.
    profiles.0.remove("claude");
    let mut launch = super::load_with_roots(
        &MachineConfig::default(),
        project.path(),
        &project.path().join("empty-home"),
    )
    .unwrap();
    let detail = "/definitions/agents/claude.md: invalid frontmatter";
    launch
        .failed_definitions
        .insert("claude".into(), detail.into());
    let names = [" claude ", "worker", "other", "missing"];
    let actual: Vec<_> = names
        .map(|name| {
            launch
                .block_failed_profile(name, ProfileScope::Agents, &profiles)
                .err()
                .map(|error| error.to_string())
        })
        .into();
    assert_eq!(
        actual,
        [Some(detail.to_owned()), Some(detail.to_owned()), None, None,]
    );
}

#[test]
fn agent_override_errors_reach_the_launch_failure_ladder() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(root.path().join("agents/broken.md"), "---\n[broken\n---\n").unwrap();
    let mut machine =
        MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path()).unwrap();
    let broken = machine.definition_failure_for("broken").unwrap();
    machine.agents.profiles.0.insert(
        "main-only".into(),
        toml::from_str("agent = 'claude'").unwrap(),
    );
    machine.subagents.profiles.0.insert(
        "sub-only".into(),
        toml::from_str("agent = 'claude'").unwrap(),
    );
    let errors = [
        (ProfileScope::Agents, "bogus"),
        (ProfileScope::Agents, "broken"),
        (ProfileScope::Agents, "sub-only"),
    ]
    .map(|(scope, name)| {
        let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
        launch_under(
            &launch,
            &machine.tiers,
            scope,
            "claude",
            None,
            None,
            Some(name),
            |_, _| None,
        )
        .map_err(|error| error.to_string())
        .err()
        .unwrap_or_default()
    });
    assert_eq!(
        [
            errors[0].starts_with(
                "unknown agent profile or kind `bogus` for --agent; valid profiles and kinds: ",
            ),
            errors[1] == broken,
            errors[2].contains("is defined under [subagents.profiles]")
                && errors[2].contains("`rimz agents` resolves [agents.profiles]"),
        ],
        [true; 3],
        "{errors:#?}"
    );
    // On subagent scope, `--agent main-only` resolves from `[agents.profiles]`.
    let sub_launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    launch_under(
        &sub_launch,
        &machine.tiers,
        ProfileScope::Subagents,
        "claude",
        None,
        None,
        Some("main-only"),
        |_, _| None,
    )
    .expect("subagents scope resolves --agent from [agents.profiles]");
}

#[test]
fn definition_routing_overlay_keeps_load_choice_and_provider_fields() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("agents/worker.md"),
        "---\ndescription: Worker\ntier: senior\ntools: [Bash]\n---\nCraft.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/skilled.md"),
        "---\ndescription: Skilled\ntier: senior\ntools: [Bash, Skill]\nskills: [one]\n---\nCraft.",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("skills/one")).unwrap();
    std::fs::write(
        root.path().join("skills/one/SKILL.md"),
        "---\ndescription: One\ndisable-model-invocation: true\n---\nSkill.",
    )
    .unwrap();
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let user_only = format!(
        "lists skill 'one', which {} marks user-only for claude (`disable-model-invocation: true`); listing cannot lift that marker: drop it from `skills:` or remove the marker",
        root.path().join("skills/one/SKILL.md").display()
    );
    for (name, runtime) in [
        ("exact", "agent: claude\nmodel: claude-future"),
        ("other", "tier: principal"),
        ("codex-worker", "agent: codex\ntier: junior"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Worker\n{runtime}\ntools: [Bash]\n---\nCraft."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Check {
            env: &env,
            machine_isolation: crate::config::Isolation::Scoped,
            cwd: None,
        },
        &CommandsConfig::default(),
        &root.path().join("config.toml"),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    let project = tempdir().unwrap();
    let launch = load_with_roots(&machine, project.path(), root.path()).unwrap();
    let cell = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "skilled",
            None,
            None,
            None,
            |_, _| None,
        )
        .unwrap(),
    );
    let stamp = cell.launch.tier.as_ref().unwrap();
    assert_eq!(
        stamp.unbuildable,
        vec![crate::agents::TierUnbuildable {
            model: "opus".into(),
            error: user_only.clone(),
        }]
    );
    let error = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "skilled",
        None,
        Some("opus"),
        None,
        |_, _| None,
    )
    .unwrap_err();
    for fragment in ["skilled", "opus", "claude", user_only.as_str()] {
        assert!(error.to_string().contains(fragment), "{error}");
    }
    for family in ["codex", "codex-worker"] {
        let error = launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "exact",
            None,
            Some("opus"),
            Some(family),
            |_, _| None,
        )
        .unwrap_err();
        for fragment in ["--agent", family, "opus", "claude", "codex"] {
            assert!(error.to_string().contains(fragment), "{error}");
        }
    }
    let selected = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "worker",
            None,
            None,
            Some("codex"),
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(selected.kind.as_str(), "codex");
    assert_eq!(selected.launch.model.as_deref(), Some("astra"));
    assert!(
        selected
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/codex.md")
    );
    assert_eq!(machine.agents.profiles.0["worker"].agent, "claude");
    let wire = serde_json::to_value(&launch.profiles.0["worker"]).unwrap();
    assert!(wire.get("model_tier").is_none());
    assert!(wire.get("renders").is_none());
    assert!(wire.get("tier_stamp").is_none());
    assert_eq!(
        launch.profiles.0["other"].model,
        machine.agents.profiles.0["other"].model
    );
    let cell = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "exact:author",
            Some(super::super::tiers::ModelTier::Senior),
            None,
            Some("codex"),
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(cell.launch.model.as_deref(), Some("astra"));
    let mut project_row = launch.clone();
    project_row.profiles.0.insert(
        "project".into(),
        toml::from_str("agent = 'claude'\nmodel = 'opus'").unwrap(),
    );
    let cell = first_cell(
        launch_under(
            &project_row,
            &machine.tiers,
            ProfileScope::Agents,
            "project",
            Some(super::super::tiers::ModelTier::Senior),
            None,
            None,
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(
        (
            cell.kind.as_str(),
            cell.launch.tier.as_ref().map(|stamp| stamp.tier)
        ),
        ("claude", Some(super::super::tiers::ModelTier::Senior)),
        "--tier on a row with no definition walks as on a bare name"
    );
    for reason in [
        crate::agents::TierSkipReason::LoggedOut,
        crate::agents::TierSkipReason::Exhausted { until: None },
        crate::agents::TierSkipReason::DailyCap {
            spend_usd: 10.into(),
            cap_usd: 5.into(),
            window: None,
        },
    ] {
        let launch = load_with_roots(&machine, project.path(), root.path()).unwrap();
        let skip = reason.clone();
        let unavailable = move |kind: &str, _: &str| (kind == "claude").then(|| skip.clone());
        let cell = first_cell(
            launch_under(
                &launch,
                &machine.tiers,
                ProfileScope::Agents,
                "worker",
                None,
                None,
                None,
                unavailable,
            )
            .unwrap(),
        );
        let wire = serde_json::to_value(&cell.launch).unwrap();
        assert_eq!(cell.kind.as_str(), "codex");
        assert_eq!(wire["tier"]["model"], "astra");
        assert_eq!(wire["tier"]["skipped"][0]["model"], "opus");
        assert_eq!(
            wire["tier"]["skipped"][0]["reason"],
            serde_json::to_value(reason).unwrap()["reason"]
        );
    }
}

#[test]
fn mini_tier_and_model_overrides_keep_the_selected_interface() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/opencode.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/worker.md"),
        "---\ndescription: Mini\nagent: opencode-mini\ntier: junior\n---\nCraft.",
    )
    .unwrap();
    let mut machine = MachineConfig::default();
    let loaded = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        None,
    );
    assert!(
        loaded.errors.is_empty(),
        "mini must load: {:?}",
        loaded.errors
    );
    machine.agents.profiles = loaded.agent_profiles;
    let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    let actual = [
        (Some(crate::config::tiers::ModelTier::Senior), None),
        (None, Some("deepseek-v4.1-flash")),
    ]
    .map(|(tier, model)| {
        let mut resolved = launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "worker",
            tier,
            model,
            None,
            |_, _| None,
        )
        .unwrap();
        crate::harness::plan::finalize_launch_layout(
            &mut resolved.layout,
            crate::harness::plan::LaunchFinalizeOptions {
                smart_compact: None,
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &crate::agents::LaunchPreset {
                    model: model.map(str::to_owned),
                    ..Default::default()
                },
                passthrough: &[],
                budget: None,
                max_turns: None,
                machine_config: None,
            },
        )
        .unwrap();
        let cell = resolved.layout.agent_cells().next().unwrap();
        (
            cell.kind.to_string(),
            cell.args.first().cloned(),
            cell.launch.model.clone(),
        )
    });
    assert_eq!(
        actual,
        [
            ("opencode".into(), Some("mini".into()), Some("opus".into())),
            (
                "opencode".into(),
                Some("mini".into()),
                Some("deepseek-v4.1-flash".into())
            ),
        ]
    );
}

#[test]
fn concrete_models_keep_multi_provider_definitions_on_their_engine_until_agent_switches() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", ""),
        ("pi", ""),
        ("worker", "agent: pi\ntools: [Bash]"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Worker\n{fields}\n---\nWork."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    machine.agents.teams =
        toml::from_str("[crew]\nleader = 'code'\nroles = [{role = 'code', profile = 'worker'}]")
            .unwrap();
    let mut launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    launch.subagent_profiles = launch.profiles.clone();
    assert!(
        launch.profiles.0["worker"]
            .definition
            .as_ref()
            .unwrap()
            .build(None, "claude", None)
            .is_ok()
    );
    let outcomes = [
        (ProfileScope::Agents, "worker"),
        (ProfileScope::Subagents, "worker"),
        (ProfileScope::Agents, "crew"),
    ]
    .map(|(scope, target)| {
        let mut layout = launch_under(
            &launch,
            &machine.tiers,
            scope,
            target,
            None,
            Some("sonnet"),
            None,
            |_, _| None,
        )
        .unwrap()
        .layout;
        crate::harness::plan::finalize_launch_layout(
            &mut layout,
            crate::harness::plan::LaunchFinalizeOptions {
                smart_compact: None,
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &crate::agents::LaunchPreset {
                    model: Some("sonnet".to_owned()),
                    ..Default::default()
                },
                passthrough: &[],
                budget: None,
                max_turns: None,
                machine_config: None,
            },
        )
        .unwrap();
        let cell = layout.agent_cells().next().unwrap();
        (
            cell.kind.to_string(),
            cell.launch.model.clone(),
            cell.launch.tier.is_none(),
            launch.profiles_for(scope).0["worker"].tier_stamp.is_none(),
            cell.args
                .windows(2)
                .any(|pair| pair == ["--model", "sonnet"]),
        )
    });
    assert_eq!(
        outcomes,
        std::array::from_fn(|_| ("pi".to_owned(), Some("sonnet".to_owned()), true, true, true))
    );
}

#[test]
fn opencode_model_overrides_finalize_without_provider_model_args() {
    let root = tempdir().unwrap();
    let mut machine = MachineConfig::default();
    machine.agents.profiles =
        toml::from_str("[worker]\nagent = 'opencode'\nmodel = 'opencode-go/deepseek-v4.1-flash'")
            .unwrap();
    machine.agents.teams =
        toml::from_str("[crew]\nleader = 'code'\nroles = [{role = 'code', profile = 'worker'}]")
            .unwrap();
    let mut launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    launch.subagent_profiles = launch.profiles.clone();
    for (scope, target) in [
        (ProfileScope::Agents, "worker"),
        (ProfileScope::Subagents, "worker"),
        (ProfileScope::Agents, "crew"),
    ] {
        let mut layout = launch_under(
            &launch,
            &machine.tiers,
            scope,
            target,
            None,
            Some("gpt-5"),
            Some("opencode"),
            |_, _| None,
        )
        .unwrap_or_else(|error| panic!("OpenCode must accept another provider's model: {error}"))
        .layout;
        crate::harness::plan::finalize_launch_layout(
            &mut layout,
            crate::harness::plan::LaunchFinalizeOptions {
                smart_compact: None,
                agent_base: Some("opencode"),
                permission_mode: None,
                isolation: None,
                preset: &crate::agents::LaunchPreset {
                    model: Some("gpt-5".into()),
                    ..Default::default()
                },
                passthrough: &[],
                budget: None,
                max_turns: None,
                machine_config: None,
            },
        )
        .unwrap();
        let cell = layout.agent_cells().next().unwrap();
        assert_eq!(cell.kind.as_str(), "opencode");
        assert_eq!(cell.launch.model.as_deref(), Some("gpt-5"));
        assert_eq!(
            cell.launch
                .record
                .as_ref()
                .and_then(|record| record.model.as_deref()),
            Some("gpt-5")
        );
        assert!(cell.args.is_empty(), "{:?}", cell.args);
    }
}

#[test]
fn fast_suffix_routes_the_base_definition() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("agents/reviewer.md"),
        "---\ndescription: Reviewer\ntier: junior\ntools: [Bash]\n---\nCraft.",
    )
    .unwrap();
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &root.path().join("config.toml"),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    let project = tempdir().unwrap();
    let launch = load_with_roots(&machine, project.path(), root.path()).unwrap();

    // `reviewer-fast --tier senior --family codex` routes the `reviewer`
    // definition, as `reviewer --tier senior --family codex` would.
    let reviewer = first_cell(
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "reviewer-fast",
            Some(super::super::tiers::ModelTier::Senior),
            None,
            Some("codex"),
            |_, _| None,
        )
        .unwrap(),
    );
    assert_eq!(reviewer.kind.as_str(), "codex");
    assert_eq!(reviewer.launch.model.as_deref(), Some("astra"));
    assert_eq!(
        reviewer.launch.tier.as_ref().map(|stamp| stamp.tier),
        Some(super::super::tiers::ModelTier::Senior)
    );
}

#[test]
fn team_cli_models_route_each_markdown_role() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", ""),
        ("writer", "agent: claude\ntier: junior\ntools: [Bash]"),
        ("coder", "agent: codex\ntier: junior\ntools: [Bash]"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Role\n{fields}\n---\nWork."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    machine.agents.teams = toml::from_str("[forge]\nleader = 'writer'\nroles = [{role = 'writer', profile = 'writer'}, {role = 'coder', profile = 'coder'}]").unwrap();
    for (tier, model, spent) in [
        (None, Some("opus"), true),
        (Some(super::super::tiers::ModelTier::Senior), None, false),
    ] {
        let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
        let mut layout = launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            "forge",
            tier,
            model,
            None,
            move |kind, _| {
                (spent && kind == "claude").then_some(crate::agents::TierSkipReason::LoggedOut)
            },
        )
        .unwrap()
        .layout;
        crate::harness::plan::finalize_launch_layout(
            &mut layout,
            crate::harness::plan::LaunchFinalizeOptions {
                smart_compact: None,
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &crate::agents::LaunchPreset {
                    model: model.map(str::to_owned),
                    ..Default::default()
                },
                passthrough: &[],
                budget: None,
                max_turns: None,
                machine_config: None,
            },
        )
        .unwrap();
        let cells: Vec<_> = layout.agent_cells().collect();
        for (index, cell) in cells.iter().enumerate() {
            let codex = model.is_none() && index == 1;
            assert_eq!(cell.kind.as_str(), if codex { "codex" } else { "claude" });
            assert_eq!(
                cell.launch.model.as_deref(),
                Some(if codex { "astra" } else { "opus" })
            );
            assert_eq!(cell.launch.tier.as_ref().map(|stamp| stamp.tier), tier);
        }
    }
    machine
        .agents
        .profiles
        .0
        .insert("coder".into(), toml::from_str("agent = 'codex'").unwrap());
    let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    let tiered = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "forge",
        Some(super::super::tiers::ModelTier::Senior),
        None,
        None,
        |_, _| None,
    )
    .unwrap();
    let coder = tiered.layout.agent_cells().nth(1).unwrap();
    assert_eq!(
        (
            coder.kind.as_str(),
            coder.launch.tier.as_ref().map(|stamp| stamp.tier)
        ),
        ("codex", Some(super::super::tiers::ModelTier::Senior)),
        "--tier reaches a role whose row carries no definition, as a bare name"
    );
    let mut layout = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "forge",
        None,
        Some("opus"),
        None,
        |kind, _| (kind == "claude").then_some(crate::agents::TierSkipReason::LoggedOut),
    )
    .unwrap()
    .layout;
    crate::harness::plan::finalize_launch_layout(
        &mut layout,
        crate::harness::plan::LaunchFinalizeOptions {
            smart_compact: None,
            agent_base: None,
            permission_mode: None,
            isolation: None,
            preset: &crate::agents::LaunchPreset {
                model: Some("opus".into()),
                ..Default::default()
            },
            passthrough: &[],
            budget: None,
            max_turns: None,
            machine_config: None,
        },
    )
    .unwrap();
    let cells: Vec<_> = layout.agent_cells().collect();
    assert_eq!(cells[0].launch.model.as_deref(), Some("opus"));
    assert_eq!(cells[1].launch.model.as_deref(), Some("opus"));
    assert!(cells[0].launch.tier.is_none());
    assert!(cells[1].launch.tier.is_none());
}

#[test]
fn trusted_repo_cannot_set_allowed_tools() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    seed_definition_home(config.path());
    let (_machine, effective) = load_trusted_project(
        config.path(),
        project.path(),
        "",
        "[profiles.worker]\ndescription = 'Worker'\nagent = 'claude'\nallowed_tools = ['Bash(*)']\nallowed-tools = ['Bash(*)']",
    );
    assert!(!effective.profiles.0.contains_key("worker"));
    let detail = effective
        .definition_failure_for("worker")
        .expect("unknown allowed-tools keys must fail the row");
    assert!(
        detail.contains("[profiles.worker]")
            && (detail.contains("allowed_tools") || detail.contains("allowed-tools")),
        "{detail}"
    );
}

#[test]
fn model_aliases_are_machine_only() {
    for trusted in [false, true] {
        let project = tempdir().unwrap();
        let config = tempdir().unwrap();
        write_project_config(&project, "[models.codex]\nsol = 'gpt-6.1-sol'");
        if trusted {
            crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
        }
        let result = load(&AgentsConfig::default(), project.path(), config.path());
        assert!(result.is_err(), "project model aliases must be refused");
        let error = result.err().unwrap().to_string();
        assert!(
            error.contains("project config cannot set [models]"),
            "{error}"
        );
        assert!(error.contains("move it to ~/.rimz/config.toml"), "{error}");
    }
}

#[test]
fn model_tiers_are_machine_only_and_project_models_are_concrete() {
    let profile: Profile =
        toml::from_str("agent = 'claude'\nmodel_tier = {tier = 'principal', fell_back = true}")
            .unwrap();
    assert!(profile.model_tier.is_none());
    for text in [
        "[profiles.worker]\nagent = 'claude'\ntier = 'senior'",
        "[subagents.profiles.worker]\nagent = 'codex'\ntier = 'intern'",
        "[agents.teams.work]\nroles = [{role = 'worker', profile = 'claude', tier = 'junior'}]",
        "[profiles.worker]\nagent = 'claude'\nmodel = 'senior'",
        "[subagents.profiles.worker]\nagent = 'codex'\nmodel = 'intern'",
        "[agents.teams.work]\nroles = [{role = 'worker', profile = 'claude', model = 'junior'}]",
    ] {
        let value: toml::Value = toml::from_str(text).unwrap();
        let result = repo_config_from_value(&value);
        assert!(result.is_err(), "{text}");
        let error = result.err().unwrap().to_string();
        assert!(error.contains("Markdown definitions"), "{error}");
    }
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(&project, "[tiers]");
    assert!(matches!(
        load(&AgentsConfig::default(), project.path(), config.path()),
        Err(EffectiveConfigErr::ProjectTiers)
    ));
}

#[test]
fn env_reminder_overlay_requires_trust_and_wins_in_both_directions() {
    for machine_value in [false, true] {
        let project = tempdir().unwrap();
        let config = tempdir().unwrap();
        let machine = AgentsConfig {
            env_reminder: machine_value,
            ..Default::default()
        };
        write_project_config(
            &project,
            &format!("[agents]\nenv-reminder = {}\n", !machine_value),
        );
        assert_eq!(
            load(&machine, project.path(), config.path())
                .unwrap()
                .env_reminder,
            machine_value
        );
        crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
        assert_eq!(
            load(&machine, project.path(), config.path())
                .unwrap()
                .env_reminder,
            !machine_value
        );
    }
}

#[test]
fn retired_git_reminder_rejects_project_trust() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(&project, "[agents]\ngit-reminder = false\n");
    match crate::trust::grant_with_roots(project.path(), config.path()) {
        Err(crate::trust::TrustErr::RemovedProjectKey { detail, .. }) => {
            assert!(detail.contains("env-reminder"));
        }
        other => panic!("expected RemovedProjectKey, got {other:?}"),
    }
}

#[test]
fn project_lsp_policy_is_machine_only() {
    for field in [
        "reserve-percent = 20",
        "reserve-min = '2G'",
        "idle-timeout = '1m'",
        "recycle-factor = 1",
        "recycle-min = '0'",
    ] {
        let value = toml::from_str(&format!("[lsp]\n{field}")).unwrap();
        let error = repo_config_from_value(&value)
            .err()
            .expect("reject project policy");
        assert!(error.to_string().contains("project config cannot set lsp."));
        assert!(error.to_string().contains("move it to ~/.rimz/config.toml"));
    }
}

#[test]
fn project_prompt_fields_reject_inline_text() {
    for declaration in [
        "[profiles.planner]\nagent = 'claude'\nsystem-prompt-file = { origin = 'x', text = 'y' }",
        "[profiles.planner]\nagent = 'claude'\nappend-system-prompt-files = [{ origin = 'x', text = 'y' }]",
        "[agents.teams.review]\nappend-system-prompt-files = [{ origin = 'x', text = 'y' }]",
        "[[agents.teams.review.roles]]\nrole = 'planner'\nprofile = 'claude'\nsystem-prompt-file = { origin = 'x', text = 'y' }",
    ] {
        assert!(
            toml::from_str::<crate::trust::ProjectConfig>(declaration).is_err(),
            "{declaration}"
        );
    }
    for field in [
        "system-prompt-file = { origin = 'x', text = 'y' }",
        "append-system-prompt-files = [{ origin = 'x', text = 'y' }]",
    ] {
        assert!(toml::from_str::<Profile>(&format!("agent = 'claude'\n{field}")).is_err());
    }
}

fn load(machine: &AgentsConfig, project_root: &Path, config_root: &Path) -> Result<LaunchAgents> {
    let config = MachineConfig {
        agents: machine.clone(),
        ..Default::default()
    };
    super::load_with_roots(&config, project_root, config_root)
}

fn profile(agent: &str, args: Option<&str>) -> Profile {
    Profile {
        empty_dir: None,
        allowed_tools: None,
        variant: None,
        definition: None,
        kind_defaults: false,
        kind_base: false,
        built_in_base: false,
        base_native_wins: Vec::new(),
        tools: None,
        auto_compact_default: None,
        model_tier: None,
        tier_stamp: None,
        agent: agent.to_owned(),
        isolation: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
        idle_compact: None,
        smart_compact: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        code_mode: None,
        budget: None,
        timeout: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        skills: None,
        seat_consensus_skill: false,
        mcps: None,
        args: args.map(ToOwned::to_owned),
    }
}

fn profiles(entries: impl IntoIterator<Item = (&'static str, Profile)>) -> ProfilesConfig {
    ProfilesConfig::from(
        entries
            .into_iter()
            .map(|(name, profile)| (name.to_owned(), profile))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn write_project_config(dir: &tempfile::TempDir, text: &str) {
    let config_dir = dir.path().join(".rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir .rimz");
    std::fs::write(config_dir.join("config.toml"), text).expect("write config");
}

#[test]
fn diagnosis_reaches_through_a_project_trust_parse_error() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\nagent = \"codex\"\n",
    );

    let Err(error) = load(&AgentsConfig::default(), project.path(), config.path()) else {
        panic!("duplicate project key must fail");
    };
    let diagnosis = error.diagnosis().expect("nested trust diagnosis");

    assert_eq!(diagnosis.line(), Some(3));
    assert_eq!(
        diagnosis.problem(),
        "`agent` is defined more than once in the same table"
    );
}

fn role(role: &str, profile: &str) -> RoleBinding {
    RoleBinding {
        signals: Vec::new(),
        owns: Vec::new(),
        flip_compact: None,
        idle_compact: None,
        smart_compact: None,
        keep_warm: None,
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        fast: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

fn machine_agents(profiles: ProfilesConfig, teams: TeamsConfig) -> AgentsConfig {
    AgentsConfig {
        profiles,
        teams,
        ..AgentsConfig::default()
    }
}

fn effective_profiles(
    machine: &ProfilesConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProfilesConfig> {
    load(
        &machine_agents(machine.clone(), TeamsConfig::default()),
        project_root,
        config_root,
    )
    .map(|launch| launch.profiles)
}

fn effective_subagent_profiles(
    machine: &ProfilesConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProfilesConfig> {
    let mut config = MachineConfig::default();
    config.subagents.profiles = machine.clone();
    super::load_with_roots(&config, project_root, config_root)
        .map(|launch| launch.subagent_profiles)
}

fn effective_teams(
    machine: &TeamsConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<TeamsConfig> {
    load(
        &machine_agents(ProfilesConfig::default(), machine.clone()),
        project_root,
        config_root,
    )
    .map(|launch| launch.teams)
}

fn block_untrusted_profile_reference(
    spec: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<()> {
    let agents = machine_agents(profiles.clone(), teams.clone());
    let launch = load(&agents, project_root, config_root)?;
    launch.block_untrusted_reference(ProfileScope::Agents, spec, commands)
}

fn load_project_tasks(
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProjectTasks> {
    project_tasks(project_root, config_root).map(|tasks| tasks.expect("project tasks"))
}

#[test]
fn trusted_repo_profiles_inherit_native_compaction_defaults() {
    use crate::config::{AutoCompactDefault, AutoCompactWindow};

    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    seed_definition_home(config.path());
    std::fs::write(
        config.path().join("agents/pi.md"),
        "---\ndescription: Pi base\ntools: [Read]\n---\nPi house.\n",
    )
    .unwrap();
    std::fs::write(
        config.path().join("agents/antigravity.md"),
        "---\ndescription: Antigravity base\ntools: [Read]\n---\nAntigravity house.\n",
    )
    .unwrap();
    std::fs::write(
        config.path().join("agents/md-worker.md"),
        "---\ndescription: MD Worker\nagent: codex\n---\nMD worker.\n",
    )
    .unwrap();
    std::fs::write(
        config.path().join("agents/md-agy.md"),
        "---\ndescription: MD Agy\nagent: antigravity\n---\nMD agy.\n",
    )
    .unwrap();
    std::fs::write(
        config.path().join("agents/md-agy-explicit.md"),
        "---\ndescription: MD Agy Explicit\nagent: antigravity\nauto-compact: 400k\n---\nMD explicit.\n",
    )
    .unwrap();
    std::fs::write(
        config.path().join("agents/md-bad-ac.md"),
        "---\ndescription: MD Bad\nagent: codex\nauto-compact: bogus\n---\nMD bad.\n",
    )
    .unwrap();
    write_project_config(
        &project,
        "[profiles.worker]\ndescription = 'Worker'\nagent = 'codex'\n\
         [profiles.agy-row]\ndescription = 'Agy'\nagent = 'antigravity'\n\
         [profiles.agy-explicit]\ndescription = 'Agy explicit'\nagent = 'antigravity'\nauto-compact = '400k'\n\
         [profiles.bad-ac]\ndescription = 'Bad'\nagent = 'codex'\nauto-compact = 'bogus'\n\
         [profiles.bad-off]\ndescription = 'Bad off'\nagent = 'antigravity'\nauto-compact = 'off'\n\
         [profiles.parent]\ndescription = 'Parent'\nagent = 'codex'\nauto-compact = '200k'\n\
         [profiles.child]\ndescription = 'Child'\nagent = 'parent'\n\
         [profiles.pi-row]\ndescription = 'Pi'\nagent = 'pi'\n\
         [profiles.amp-row]\ndescription = 'Amp'\nagent = 'amp'\n\
         [profiles.claude-row]\ndescription = 'Claude'\nagent = 'claude'\nauto-compact = '200k'\n\
         [profiles.planner]\ndescription = 'Planner'\nagent = 'claude-row'\n\
         [subagents.profiles.sub-worker]\ndescription = 'Sub worker'\nagent = 'codex'\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
    for (setting, expected_default, expected_codex, expected_agy) in [
        (
            None,
            Some(AutoCompactDefault::BuiltIn),
            Some(272_000),
            Some(272_000),
        ),
        (
            Some(AutoCompactWindow::Tokens(300_000)),
            Some(AutoCompactDefault::Tokens(300_000)),
            Some(300_000),
            Some(300_000),
        ),
        (Some(AutoCompactWindow::Off), None, None, None),
    ] {
        let machine_toml = match setting {
            None => String::new(),
            Some(AutoCompactWindow::Tokens(n)) => format!("[harness]\nauto_compact = '{n}'\n"),
            Some(AutoCompactWindow::Off) => "[harness]\nauto_compact = 'off'\n".to_owned(),
        };
        let cfg_path = config.path().join("config.toml");
        std::fs::write(&cfg_path, &machine_toml).unwrap();
        let machine = MachineConfig::parse_text(&cfg_path, &machine_toml, config.path()).unwrap();
        let effective = load_with_roots(&machine, project.path(), config.path()).unwrap();
        let md_worker = agents_spec::resolve_profile("md-worker", &effective.profiles).unwrap();
        let md_agy = agents_spec::resolve_profile("md-agy", &effective.profiles).unwrap();
        let md_agy_explicit =
            agents_spec::resolve_profile("md-agy-explicit", &effective.profiles).unwrap();
        for (name, profiles) in [
            ("worker", &effective.profiles),
            ("sub-worker", &effective.subagent_profiles),
        ] {
            let worker = agents_spec::resolve_profile(name, profiles).unwrap();
            assert_eq!(worker.auto_compact_default, expected_default);
            assert_eq!(worker.auto_compact_default, md_worker.auto_compact_default);
            assert_eq!(
                worker
                    .auto_compact_default
                    .and_then(|d| d.resolve_for_kind(&worker.kind)),
                expected_codex
            );
        }
        let agy = agents_spec::resolve_profile("agy-row", &effective.profiles).unwrap();
        assert_eq!(agy.auto_compact_default, expected_default);
        assert_eq!(agy.auto_compact_default, md_agy.auto_compact_default);
        assert_eq!(
            agy.auto_compact_default
                .and_then(|d| d.resolve_for_kind(&agy.kind)),
            expected_agy
        );
        let agy_explicit =
            agents_spec::resolve_profile("agy-explicit", &effective.profiles).unwrap();
        assert_eq!(agy_explicit.auto_compact.as_deref(), Some("400000"));
        assert_eq!(agy_explicit.auto_compact, md_agy_explicit.auto_compact);
        let pi = agents_spec::resolve_profile("pi-row", &effective.profiles).unwrap();
        assert_eq!(
            pi.auto_compact_default
                .and_then(|d| d.resolve_for_kind(&pi.kind)),
            expected_agy
        );
        let amp = agents_spec::resolve_profile("amp-row", &effective.profiles).unwrap();
        assert_eq!(
            amp.auto_compact_default
                .and_then(|d| d.resolve_for_kind(&amp.kind)),
            None,
            "a kind with no enforcer takes no default"
        );
        let md_bad_reason = machine
            .definition_failure_for("md-bad-ac")
            .expect("md-bad-ac must fail");
        let md_suffix = md_bad_reason
            .split_once(": ")
            .map(|(_, rest)| rest)
            .unwrap();
        let proj_bad_reason = effective
            .definition_failure_for("bad-ac")
            .expect("bad-ac must fail per row");
        assert!(
            proj_bad_reason.contains("[profiles.bad-ac]") && proj_bad_reason.contains(md_suffix),
            "project auto-compact validation must match Markdown: proj={proj_bad_reason} md={md_bad_reason}"
        );
        let proj_off_reason = effective
            .definition_failure_for("bad-off")
            .expect("bad-off must fail per row");
        assert!(
            proj_off_reason.contains("[profiles.bad-off]")
                && proj_off_reason.contains("auto-compact: off"),
            "{proj_off_reason}"
        );
        for name in ["parent", "child", "claude-row", "planner"] {
            assert_eq!(
                agents_spec::resolve_profile(name, &effective.profiles)
                    .unwrap()
                    .auto_compact
                    .as_deref(),
                Some("200000")
            );
        }
        for name in ["pi-row", "codex"] {
            assert_eq!(
                agents_spec::resolve_profile(name, &effective.profiles)
                    .unwrap()
                    .auto_compact,
                None
            );
        }
    }
}

#[test]
fn trusted_repo_profile_overlays_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    write_project_config(
        &project,
        "[profiles.planner]\ndescription = \"Planner\"\nagent = \"claude\"\nargs = \"--repo\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let cfg_path = config.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let mut machine = MachineConfig::parse_text(&cfg_path, "", config.path()).unwrap();
    machine
        .agents
        .profiles
        .0
        .insert("planner".into(), profile("claude", Some("--machine")));

    let effective = load_with_roots(&machine, project.path(), config.path())
        .expect("effective")
        .profiles;

    assert_eq!(
        effective.0.get("planner").and_then(|p| p.args.as_deref()),
        Some("--repo")
    );
}

#[test]
fn trusted_load_reads_a_non_table_packs_subagents_or_lsp_as_absent() {
    let config = tempdir().expect("config");
    let cfg_path = config.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let machine = MachineConfig::parse_text(&cfg_path, "", config.path()).unwrap();
    // `packs::expectation_warnings` warns on a `packs` that is not a table itself, and a launch
    // reads any non-table `subagents` or `lsp` as absent.
    let failures: Vec<_> = [
        "packs = \"crew/core\"\n",
        "subagents = [{ helper = {} }]\n",
        "lsp = [{ servers = {} }]\n",
    ]
    .into_iter()
    .filter_map(|text| {
        let project = tempdir().expect("project");
        write_project_config(&project, text);
        crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
        let effective = load_with_roots(&machine, project.path(), config.path());
        effective.err().map(|error| (text, error.to_string()))
    })
    .collect();

    assert_eq!(failures, Vec::<(&str, String)>::new());
}

#[test]
fn trusted_project_tasks_load_with_project_root() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nargs = [\"rimz\", \"agents\", \"codex\", \"triage\"]\ntimeout = \"1h\"\naccount = \"work\"\ncron = \"0 8 * * *\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");
    let wait = loaded.tasks.0.get("wait").expect("wait task");

    assert_eq!(loaded.state, TrustState::Trusted);
    assert_eq!(loaded.config_path, project.path().join(".rimz/config.toml"));
    assert_eq!(wait.root, project.path());
    assert_eq!(wait.account, Some("work".parse().expect("login name")));
    assert_eq!(wait.args, ["rimz", "agents", "codex", "triage"]);
    assert_eq!(wait.timeout.as_deref(), Some("1h"));
}

#[test]
fn dependabot_repair_is_a_command_row_and_loads() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, include_str!("../../../../../.rimz/config.toml"));
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");
    let task = &loaded.tasks.0["dependabot-repair"];
    assert_eq!(task.refused, None);
    assert_eq!(
        task.args,
        [
            "uv",
            "run",
            "--no-project",
            "--script",
            "loops/dependabot/dependabot.py",
            "run",
            "--quiet-period",
            "1"
        ]
    );
    assert_eq!(task.timeout.as_deref(), Some("15m"));
    assert_eq!(task.every.as_deref(), Some("1h"));
}

#[test]
fn untrusted_project_tasks_stay_visible_with_state() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\ncron = \"0 8 * * *\"\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");

    assert_eq!(loaded.state, TrustState::Untrusted);
    assert!(loaded.tasks.0.contains_key("wait"));
}

#[test]
fn project_tasks_reject_machine_local_fields() {
    let cases = [
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nwhen = [\"team.stage=Done\"]\n",
            "when",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\nfor = \"30m\"\n",
            "for",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\ndir = \"/tmp/linked\"\ncron = \"0 8 * * *\"\n",
            "dir",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nroot = \"/tmp/other\"\ncron = \"0 8 * * *\"\n",
            "root",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nwait = { kind = \"codex\", session = \"sess\", handle = \"@codex\" }\ncron = \"0 8 * * *\"\n",
            "wait",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\ndeadline = \"2026-07-01T12:00:00Z\"\ncron = \"0 8 * * *\"\n",
            "deadline",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nsignal = \"ci.failed\"\nwait-meta = { armed_by = { kind = \"human\" }, armed_at = \"2026-07-01T12:00:00Z\" }\n",
            "wait-meta",
        ),
        (
            "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\ncron = \"0 8 * * *\"\nbooked-by = [{ kind = \"claude\", agent_id = \"launch_lead\" }]\n",
            "booked-by",
        ),
    ];
    for (text, field) in cases {
        let project = tempdir().expect("project");
        let config = tempdir().expect("config");
        write_project_config(&project, text);

        let err = project_tasks(project.path(), config.path()).expect_err("invalid field");

        assert!(matches!(
            err,
            EffectiveConfigErr::Tasks {
                source: ProjectTasksErr::UnsupportedField { field: found, .. },
                ..
            } if found == field
        ));
    }
}

#[test]
fn project_tasks_validate_schedule_shape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nevery = \"soon\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("invalid schedule");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::Schedule(crate::harness::schedule::ScheduleErr::BadEvery { name, value }),
            ..
        } if name == "wait" && value == "soon"
    ));
}

#[test]
fn project_tasks_must_repeat() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nsignal = \"deploy.done\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("one-shot project task");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::MustRepeat { ref task },
            ..
        } if task == "wait"
    ));
}

#[test]
fn project_tasks_accept_signals_and_reject_machine_only_trigger_fields() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nsignal = \"ci.failed\"\nrepeat = true\nmatch = { branch = \"feature\" }\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("signal project task");
    let task = &loaded.tasks.0["wait"];
    assert_eq!(task.signal.as_deref(), Some("ci.failed"));
    assert_eq!(
        task.matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );

    for (field, value) in [("watch", "\"cargo test\""), ("each-worktree", "true")] {
        write_project_config(
            &project,
            &format!(
                "[tasks.wait]\nargs = [\"true\"]\ntimeout = \"1m\"\nsignal = \"ci.failed\"\nrepeat = true\n{field} = {value}\n"
            ),
        );
        let err = project_tasks(project.path(), config.path()).expect_err(field);
        assert!(matches!(
            err,
            EffectiveConfigErr::Tasks {
                source: ProjectTasksErr::UnsupportedField { field: actual, .. },
                ..
            } if actual == field
        ));
    }
}

#[test]
fn untrusted_repo_profiles_are_inert_until_referenced() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    let machine = profiles([("local", profile("claude", Some("--local")))]);

    write_project_config(&project, "display_name = \"Query Engine\"\n");
    let effective = effective_profiles(&machine, project.path(), config.path()).expect("effective");
    assert_eq!(effective, machine);

    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");
    let effective = effective_profiles(&machine, project.path(), config.path()).expect("effective");
    assert_eq!(effective, machine);
    block_untrusted_profile_reference(
        Some("local"),
        &effective,
        &CommandsConfig::default(),
        &TeamsConfig::default(),
        project.path(),
        config.path(),
    )
    .expect("machine profile stays launchable");

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("planner"),
            &effective,
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_repo_profile_reference_is_detected_inside_requested_shape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\n\n[profiles.claude]\nagent = \"claude\"\n",
    );
    let profiles = ProfilesConfig::default();
    let commands = CommandsConfig::default();
    let teams = TeamsConfig::default();

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("planner,codex"),
            &profiles,
            &commands,
            &teams,
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
    for spec in ["planner:lead", "codex/planner:lead"] {
        assert!(matches!(
            block_untrusted_profile_reference(
                Some(spec),
                &profiles,
                &commands,
                &teams,
                project.path(),
                config.path(),
            ),
            Err(EffectiveConfigErr::Blocked {
                state: "untrusted",
                ..
            })
        ));
    }
    block_untrusted_profile_reference(
        Some("claude"),
        &profiles,
        &commands,
        &teams,
        project.path(),
        config.path(),
    )
    .expect("repo profile named like a built-in kind stays inert for the built-in launch");

    let commands = CommandsConfig(BTreeMap::from([(
        "planner:lead".to_owned(),
        "true".to_owned(),
    )]));
    block_untrusted_profile_reference(
        Some("planner:lead"),
        &profiles,
        &commands,
        &teams,
        project.path(),
        config.path(),
    )
    .expect("exact machine command with a colon stays launchable");
}

#[test]
fn repo_profile_cannot_inherit_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.child]\nagent = \"machine-base\"\n");
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("machine-base", profile("claude", Some("--machine")))]);

    let err = effective_profiles(&machine, project.path(), config.path()).expect_err("closed");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::RepoProfileEscapesTrust { profile, base },
            ..
        } if profile == "child" && base == "machine-base"
    ));
}

#[test]
fn repo_empty_dir_profiles_are_admitted_in_both_scopes() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    seed_definition_home(config.path());
    let (_machine, effective) = load_trusted_project(
        config.path(),
        project.path(),
        "",
        "[profiles.searcher]\ndescription = 'Searcher'\nagent = 'claude'\nempty-dir = true\n\
         [subagents.profiles.sub-searcher]\ndescription = 'Sub searcher'\nagent = 'claude'\nempty-dir = true\n",
    );
    for (profiles, name) in [
        (&effective.profiles, "searcher"),
        (&effective.subagent_profiles, "sub-searcher"),
    ] {
        assert!(profiles.0.contains_key(name));
        assert_eq!(
            serde_json::to_value(&profiles.0[name]).unwrap()["empty-dir"],
            true
        );
    }
    let binding: RoleBinding = toml::from_str("role = 'lead'\nprofile = 'searcher'").unwrap();
    let error = crate::harness::spec::resolve_role(&binding, &effective.profiles)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("searcher") && error.contains("empty-dir") && error.contains("share"),
        "{error}"
    );
}

#[test]
fn repo_profiles_cannot_set_isolation_in_either_scope() {
    for namespace in ["profiles", "subagents.profiles"] {
        let project = tempdir().expect("project");
        let config = tempdir().expect("config");
        write_project_config(
            &project,
            &format!("[{namespace}.child]\nagent = \"claude\"\nisolation = \"host\"\n"),
        );
        crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
        let err = effective_profiles(&ProfilesConfig::default(), project.path(), config.path())
            .expect_err("machine-only isolation");
        assert!(err.to_string().contains("--isolation"), "{err}");
        assert!(
            matches!(err, EffectiveConfigErr::Agents { source: LayoutErr::RepoProfileSetsIsolation { profile }, .. } if profile == "child")
        );
    }
}

#[test]
fn repo_profile_typo_reports_unknown_base_not_machine_escape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    write_project_config(
        &project,
        "[profiles.child]\ndescription = \"Child\"\nagent = \"typoo\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let cfg_path = config.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let mut machine = MachineConfig::parse_text(&cfg_path, "", config.path()).unwrap();
    machine
        .agents
        .profiles
        .0
        .insert("machine-base".into(), profile("claude", Some("--machine")));

    let effective =
        load_with_roots(&machine, project.path(), config.path()).expect("per-row failure loads");
    assert!(!effective.profiles.0.contains_key("child"));
    let detail = effective
        .definition_failure_for("child")
        .expect("typo fails the row");
    assert!(
        detail.contains("[profiles.child]") && detail.contains("typoo"),
        "{detail}"
    );
}

#[test]
fn trusted_repo_subagent_profile_overlays_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    write_project_config(
        &project,
        "[subagents.profiles.reviewer]\ndescription = \"Reviewer\"\nagent = \"codex\"\nargs = \"--repo\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let cfg_path = config.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let mut machine = MachineConfig::parse_text(&cfg_path, "", config.path()).unwrap();
    machine
        .subagents
        .profiles
        .0
        .insert("reviewer".into(), profile("claude", Some("--machine")));

    let effective = load_with_roots(&machine, project.path(), config.path())
        .expect("effective")
        .subagent_profiles;

    let reviewer = effective.0.get("reviewer").expect("reviewer profile");
    assert_eq!(reviewer.agent, "codex");
    assert_eq!(reviewer.args.as_deref(), Some("--repo"));
}

#[test]
fn trusted_repo_allowlist_can_reference_repo_subagent_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    let (_machine, effective) = load_trusted_project(
        config.path(),
        project.path(),
        "",
        "[profiles.planner]\ndescription = \"Planner\"\nagent = \"claude\"\nsubagents = [\"repo-child\"]\n\
         [subagents.profiles.repo-child]\ndescription = \"Repo child\"\nagent = \"codex\"\n",
    );

    assert_eq!(
        effective
            .profiles
            .0
            .get("planner")
            .and_then(|profile| profile.subagents.as_deref()),
        Some(["repo-child".to_owned()].as_slice())
    );
    assert!(effective.subagent_profiles.0.contains_key("repo-child"));
    let mut sources = crate::config::AgentSpecSources::default();
    effective.overlay_profile_sources(&mut sources);
    assert_eq!(
        sources.profile(ProfileScope::Agents, "planner"),
        Some(project.path().join(".rimz/config.toml").as_path())
    );
    assert_eq!(
        sources.profile(ProfileScope::Subagents, "repo-child"),
        Some(project.path().join(".rimz/config.toml").as_path())
    );
    for (scope, name) in [
        (ProfileScope::Agents, "planner"),
        (ProfileScope::Subagents, "repo-child"),
    ] {
        assert_eq!(
            serde_json::to_value(sources.profile_origin(scope, name)).unwrap(),
            serde_json::json!({"origin": "project"})
        );
    }
}

#[test]
fn untrusted_repo_subagent_profiles_are_inert_and_block_on_reference() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[subagents.profiles.reviewer]\nagent = \"codex\"\n",
    );
    let machine = profiles([("local", profile("claude", Some("--local")))]);
    let mut machine_config = MachineConfig::default();
    machine_config.subagents.profiles = machine.clone();
    let launch =
        super::load_with_roots(&machine_config, project.path(), config.path()).expect("effective");

    assert_eq!(launch.subagent_profiles, machine);
    assert!(matches!(
        launch.block_untrusted_reference(
            ProfileScope::Subagents,
            Some("reviewer"),
            &CommandsConfig::default(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn repo_subagent_profile_cannot_inherit_machine_subagent_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[subagents.profiles.child]\nagent = \"machine-base\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("machine-base", profile("claude", Some("--machine")))]);

    let err = effective_subagent_profiles(&machine, project.path(), config.path())
        .expect_err("subagent namespace stays trust-closed");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::RepoProfileEscapesTrust { profile, base },
            ..
        } if profile == "child" && base == "machine-base"
    ));
}

#[test]
fn repo_profiles_resolve_repo_and_builtin_bases() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    let (_machine, effective) = load_trusted_project(
        config.path(),
        project.path(),
        "",
        "[profiles.base]\ndescription = \"Base\"\nagent = \"codex\"\nmode = \"ask\"\n\n\
         [profiles.child]\ndescription = \"Child\"\nagent = \"base\"\nargs = \"--child\"\n",
    );

    let child =
        crate::harness::spec::resolve_profile("child", &effective.profiles).expect("resolve child");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(child.args.as_deref(), Some("--child"));
}

#[test]
fn repo_prompt_file_paths_resolve_against_rimz_dir() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    let (_machine, effective) = load_trusted_project(
        config.path(),
        project.path(),
        "",
        "[profiles.planner]\ndescription = \"Planner\"\nagent = \"claude\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
    );

    assert_eq!(
        effective
            .profiles
            .0
            .get("planner")
            .and_then(|profile| profile.system_prompt_file.as_ref()),
        Some(&crate::config::PromptSource::File(
            project.path().join(".rimz/prompts/planner.md")
        ))
    );
}

#[test]
fn trusted_repo_team_overlays_machine_team_and_resolves_prompt_paths() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    seed_definition_home(config.path());
    write_project_config(
        &project,
        "[profiles.planner]\ndescription = \"Planner\"\nagent = \"claude\"\n\n\
         [agents.teams.review]\nleader = \"planner\"\n\n\
         [[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let cfg_path = config.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let mut machine = MachineConfig::parse_text(&cfg_path, "", config.path()).unwrap();
    machine.agents.teams.0.insert(
        "review".to_owned(),
        Team {
            roles: vec![role("local", "local-profile")],
            keep_warm: None,
            leader: None,
            layout: None,
            scratch_files: None,
            consensus_file: None,
            consensus: Vec::new(),
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    );

    let effective = load_with_roots(&machine, project.path(), config.path())
        .expect("effective")
        .teams;

    let role = &effective.0.get("review").expect("repo team").roles[0];
    assert_eq!(role.role, "planner");
    assert_eq!(role.profile, "planner");
    assert_eq!(
        role.system_prompt_file.as_ref(),
        Some(&crate::config::PromptSource::File(
            project.path().join(".rimz/prompts/planner.md")
        ))
    );
}

#[test]
fn repo_team_prompt_bases_are_checked_on_the_merged_launch_table() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    let base_path = config.path().join("agents/claude.md");
    let mut actual = Vec::new();
    for has_base in [true, false] {
        if has_base {
            std::fs::create_dir_all(config.path().join("agents")).unwrap();
            std::fs::write(
                &base_path,
                "---\ndescription: Claude base\ntools: [Read]\n---\nKind base.\n",
            )
            .unwrap();
        } else {
            let _ = std::fs::remove_file(&base_path);
        }
        let machine =
            MachineConfig::load_from(&config.path().join("config.toml"), config.path()).unwrap();
        for layer in [
            "consensus-file = 'consensus.md'",
            "append-system-prompt-files = ['pipeline.md']",
        ] {
            write_project_config(
                &project,
                &format!(
                    "[profiles.worker]\ndescription = 'Worker'\nagent = 'claude'\n\
                     [agents.teams.crew]\n{layer}\nleader = 'lead'\nlayout = 'lead'\n\
                     roles = [{{role = 'lead', profile = 'worker', owns = ['Plan']}}]\n"
                ),
            );
            crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
            let loaded = load_with_roots(&machine, project.path(), config.path());
            actual.push(loaded.as_ref().map(|_| ()).map_err(ToString::to_string));
            let Ok(launch) = loaded else {
                continue;
            };
            let origin = if has_base {
                base_path.clone()
            } else {
                std::path::PathBuf::from("built-in kind base claude")
            };
            let layout = crate::harness::spec::resolve_team(
                "crew",
                &launch.teams,
                &launch.profiles,
                &CommandsConfig::default(),
            )
            .unwrap();
            let cell = layout.agent_cells().next().unwrap();
            assert_eq!(
                cell.system_prompt_file.as_ref().map(|s| s.origin()),
                Some(origin.as_path())
            );
            let prompt = cell.team_prompt.as_ref().unwrap();
            if layer.starts_with("consensus-file") {
                assert_eq!(
                    prompt.consensus,
                    crate::harness::team_prompt::Consensus::File(
                        project.path().join(".rimz/consensus.md")
                    )
                );
            } else {
                assert_eq!(
                    prompt.files,
                    [crate::config::PromptSource::File(
                        project.path().join(".rimz/pipeline.md")
                    )]
                );
            }
        }
    }
    assert_eq!(actual, vec![Ok(()); 4]);
}

#[test]
fn repo_teams_compose_consensus_pieces_from_the_machine_trait_claims() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    seed_definition_home(config.path());
    std::fs::create_dir_all(config.path().join("traits")).unwrap();
    std::fs::write(config.path().join("traits/house.md"), "House.").unwrap();
    let machine =
        MachineConfig::load_from(&config.path().join("config.toml"), config.path()).unwrap();
    let crew = |extra: &str| {
        format!(
            "[profiles.worker]\ndescription = 'Worker'\nagent = 'claude'\n\
             [agents.teams.crew]\nleader = 'lead'\nconsensus-file = 'consensus.md'\n\
             append-system-prompt-files = ['pipeline.md']\n{extra}\n\
             roles = [{{role = 'lead', profile = 'worker', owns = ['Plan', 'Reflect']}}]\n"
        )
    };
    write_project_config(&project, &crew("consensus = ['reflect', 'house']"));
    crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
    let launch = load_with_roots(&machine, project.path(), config.path()).unwrap();
    let team = &launch.teams.0["crew"];
    assert_eq!(
        team.consensus_file.as_deref(),
        Some(project.path().join(".rimz/consensus.md").as_path())
    );
    assert_eq!(
        team.append_system_prompt_files,
        [
            crate::config::PromptSource::Text {
                origin: crate::harness::team_prompt::reflect_copy_path(config.path()),
                text: crate::harness::team_prompt::SHIPPED_REFLECT
                    .trim()
                    .to_owned(),
            },
            crate::config::PromptSource::Text {
                origin: config.path().join("traits/house.md"),
                text: "House.".to_owned(),
            },
            crate::config::PromptSource::File(project.path().join(".rimz/pipeline.md")),
        ]
    );
    write_project_config(
        &project,
        &crew("consensus = ['reflect']").replace(", 'Reflect'", ""),
    );
    crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
    let Err(refused) = load_with_roots(&machine, project.path(), config.path()) else {
        panic!("a shipped reflect piece without a Reflect stage loaded");
    };
    assert_eq!(
        refused.to_string(),
        format!(
            "invalid project agents config at {}: team 'crew' lists the shipped `reflect` \
             consensus piece but declares no `Reflect` stage; add a `Reflect` stage, or drop \
             `reflect` from `consensus`",
            project.path().join(".rimz/config.toml").display()
        )
    );
}

#[test]
fn repo_team_roles_require_repo_profiles_even_for_builtin_kinds() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"claude\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let err = effective_teams(&TeamsConfig::default(), project.path(), config.path())
        .expect_err("repo team stays closed over repo profiles");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::UnknownRoleProfile {
                team,
                role,
                profile,
            },
            ..
        } if team == "review" && role == "planner" && profile == "claude"
    ));
}

#[test]
fn untrusted_repo_team_reference_is_blocked() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\n",
    );

    assert_eq!(
        effective_teams(&TeamsConfig::default(), project.path(), config.path())
            .expect("untrusted effective teams"),
        TeamsConfig::default()
    );
    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review"),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review.planner"),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_repo_profile_inside_machine_team_layout_is_blocked() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");
    let machine_profiles = profiles([("local", profile("codex", None))]);
    let machine_teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        Team {
            roles: vec![role("coder", "local")],
            keep_warm: None,
            leader: None,
            layout: Some("coder,planner".to_owned()),
            scratch_files: None,
            consensus_file: None,
            consensus: Vec::new(),
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    )]));

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review"),
            &machine_profiles,
            &CommandsConfig::default(),
            &machine_teams,
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_layout_trust_uses_shared_structural_cells_and_inline_precedence() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");

    for spec in [
        "planner:lead+claude,codex/term",
        "claude+planner:lead,codex/term",
        "claude+term,planner:lead/codex",
        "claude+term,codex/planner:lead",
    ] {
        assert!(
            matches!(
                block_untrusted_profile_reference(
                    Some(spec),
                    &ProfilesConfig::default(),
                    &CommandsConfig::default(),
                    &TeamsConfig::default(),
                    project.path(),
                    config.path(),
                ),
                Err(EffectiveConfigErr::Blocked { .. })
            ),
            "{spec}"
        );
    }

    let exact_machine_command = CommandsConfig(BTreeMap::from([(
        "planner:lead".to_owned(),
        "true".to_owned(),
    )]));
    block_untrusted_profile_reference(
        Some("planner:lead"),
        &ProfilesConfig::default(),
        &exact_machine_command,
        &TeamsConfig::default(),
        project.path(),
        config.path(),
    )
    .expect("exact machine cell containing a colon stays inert");
}

fn write_home_machine_config(home: &tempfile::TempDir, text: &str) -> MachineConfig {
    write_project_config(home, text);
    let rimz_home = home.path().join(".rimz");
    MachineConfig::load_from(&rimz_home.join("config.toml"), &rimz_home).expect("machine config")
}

#[test]
fn home_machine_lsp_servers_are_machine_entries_not_untrusted_declarations() {
    let home = tempdir().unwrap();
    let machine = write_home_machine_config(
        &home,
        "[lsp.servers.rust]\ncommand = [\"rust-analyzer\"]\nextensions = [\"rs\"]\nroot-markers = [\"Cargo.toml\"]\n",
    );
    let loaded = load_with_roots(&machine, home.path(), &home.path().join(".rimz")).unwrap();
    assert!(loaded.untrusted_lsp_servers.is_empty());
    assert!(loaded.lsp_servers.contains_key("rust"));
}

#[test]
fn home_machine_lsp_policy_keys_are_not_project_policy() {
    let home = tempdir().unwrap();
    let machine = write_home_machine_config(&home, "[lsp]\nreserve-percent = 10\n");
    load_with_roots(&machine, home.path(), &home.path().join(".rimz")).unwrap();
}

#[test]
fn home_project_config_stays_a_project_layer_when_rimz_home_is_elsewhere() {
    let home = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(
        &home,
        "[lsp.servers.rust]\ncommand = [\"rust-analyzer\"]\nextensions = [\"rs\"]\nroot-markers = [\"Cargo.toml\"]\n",
    );
    let loaded = load(&AgentsConfig::default(), home.path(), config.path()).unwrap();
    assert_eq!(loaded.untrusted_lsp_servers, vec!["rust".to_owned()]);
}

#[test]
fn a_project_row_with_a_key_outside_the_row_refuses_alone() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.ok]\nargs = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n\
         [tasks.old]\nagent = \"codex\"\nprompt = \"wait\"\nevery = \"1h\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");

    assert_eq!(loaded.tasks.0["ok"].refused, None);
    let refused = loaded.tasks.0["old"]
        .refused
        .as_ref()
        .expect("old row refuses");
    assert!(
        refused.detail.contains("unknown field `agent`"),
        "{refused:?}"
    );
}

#[test]
fn tier_routes_read_an_alias_launch_word_as_its_kind() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [("agy", "tools: [Read]"), ("mcode", "tools: [Read]")] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Base\n{fields}\n---\nWork."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    let route = |word: &str| {
        let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            word,
            Some(super::super::tiers::ModelTier::Intern),
            None,
            None,
            |_, _| None,
        )
        .map(|resolved| {
            let cell = first_cell(resolved);
            (cell.kind.to_string(), cell.launch.model, cell.launch.tier)
        })
        .map_err(|error| error.to_string())
    };
    for (alias, kind) in [("agy", "antigravity"), ("mcode", "minimax")] {
        let routed = route(alias);
        assert_eq!(
            routed
                .as_ref()
                .ok()
                .and_then(|(_, _, stamp)| stamp.as_ref())
                .map(|stamp| stamp.tier),
            Some(super::super::tiers::ModelTier::Intern),
            "{alias}: {routed:?}"
        );
        assert_eq!(routed, route(kind), "{alias}");
    }
}

#[test]
fn kind_base_model_route_onto_a_kind_with_only_the_built_in_base_takes_it() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
    )
    .unwrap();
    let machine =
        MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path()).unwrap();
    let launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    let cell = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "claude",
        None,
        Some("neon"),
        None,
        |_, _| None,
    )
    .map(first_cell)
    .expect("claude kind base routes onto the built-in antigravity base");
    assert_eq!(
        (
            cell.kind.as_str(),
            cell.system_prompt_file
                .as_ref()
                .and_then(|base| base.built_in_kind()),
            cell.launch.model.as_deref()
        ),
        ("antigravity", Some("antigravity"), Some("neon"))
    );

    // A bare kind with no base row of its own stays accepted as before.
    launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "codex",
        None,
        Some("neon"),
        None,
        |_, _| None,
    )
    .map(|_| ())
    .expect("bare kind without base row stays accepted");
}

fn seed_definition_home(home: &std::path::Path) {
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::create_dir_all(home.join("subagents")).unwrap();
    std::fs::create_dir_all(home.join("skills/review")).unwrap();
    std::fs::create_dir_all(home.join("skills/manual-only")).unwrap();
    std::fs::write(
        home.join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read, Skill]\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/codex.md"),
        "---\ndescription: Codex base\ntools: [Read, Skill]\n---\nCodex house.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/antigravity.md"),
        "---\ndescription: Antigravity base\ntools: [Read, Skill]\n---\nAgy house.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review skill\n---\nReview body.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("skills/manual-only/SKILL.md"),
        "---\nname: manual-only\ndescription: Manual skill\ndisable-model-invocation: true\n---\nManual.\n",
    )
    .unwrap();
}

fn load_trusted_project(
    home: &std::path::Path,
    project: &std::path::Path,
    machine_toml: &str,
    project_toml: &str,
) -> (MachineConfig, LaunchAgents) {
    let cfg_path = home.join("config.toml");
    std::fs::write(&cfg_path, machine_toml).unwrap();
    let machine = MachineConfig::parse_text(&cfg_path, machine_toml, home).unwrap();
    std::fs::create_dir_all(project.join(".rimz")).unwrap();
    std::fs::write(project.join(".rimz/config.toml"), project_toml).unwrap();
    crate::trust::grant_with_roots(project, home).unwrap();
    let launch = load_with_roots(&machine, project, home).expect("project config must load");
    (machine, launch)
}

fn assert_row_refused(
    machine: &MachineConfig,
    launch: &LaunchAgents,
    scope: ProfileScope,
    project: &std::path::Path,
    table: &str,
    name: &str,
    expected_fragment: &str,
) {
    let err = crate::harness::plan::resolve_launch(
        launch,
        scope,
        &machine.agents.commands,
        Some(name),
        &crate::config::Selection::default(),
    )
    .expect_err(&format!("{table}.{name} must refuse"));
    let text = err.to_string();
    let cfg = project.join(".rimz/config.toml").display().to_string();
    assert!(
        text.contains(&cfg),
        "{table}.{name}: missing config path {cfg} in: {text}"
    );
    assert!(
        text.contains(&format!("[{table}.{name}]")),
        "{table}.{name}: missing section [{table}.{name}] in: {text}"
    );
    assert!(
        text.contains(expected_fragment),
        "{table}.{name}: missing `{expected_fragment}` in: {text}"
    );
}

#[test]
fn project_rows_1_through_15_refuse_per_row_with_contract_details() {
    // Rows 1, 4, 6, 10, 11, 12, 13, 14 (unknown skill & promptless prompt), 15, plus per-row
    // checks.
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::os::unix::fs::symlink("gone", home.path().join("skills/not-installed")).unwrap();
    let mut deep = String::new();
    deep.push_str("[profiles.d0]\ndescription = 'd0'\nagent = 'claude'\n");
    for i in 1..=16 {
        deep.push_str(&format!(
            "[profiles.d{i}]\ndescription = 'd{i}'\nagent = 'd{}'\n",
            i - 1
        ));
    }
    let config = format!(
        r#"
[profiles.ok]
description = "Passing sibling."
agent = "claude"

[profiles.r1_missing]
agent = "claude"

[profiles.r1_multiline]
description = "line one\nline two"
agent = "claude"

[profiles.r4_foreign]
description = "Foreign model."
agent = "claude"
model = "sol"

[profiles.r6_rel]
description = "Relative effort."
agent = "claude"
effort = "+1"

[profiles.r10_manual]
description = "User-only skill."
agent = "claude"
skills = ["manual-only"]

[subagents.profiles.r11_idle]
description = "Idle compact on subagent."
agent = "claude"
idle-compact = "100k"

[profiles.r12_yolo]
description = "Cell word yolo."
agent = "claude"
subagents = ["codex-yolo"]

[profiles.r12_fast]
description = "Cell word fast."
agent = "claude"
subagents = ["codex-fast"]

[profiles.r12_general]
description = "Row 12 undefined general"
agent = "claude"
subagents = ["general"]

[profiles.r12_mini]
description = "Cell word mini."
agent = "claude"
subagents = ["opencode-mini"]

[profiles.r12_term]
description = "Cell word term."
agent = "claude"
subagents = ["term"]

[profiles.r13_unknown]
description = "Unknown tools key."
agent = "claude"
tools = ["Read"]

[profiles.r14_skill]
description = "Broken skill link."
agent = "claude"
skills = ["not-installed"]

[profiles.r14_promptless]
description = "Prompt file on cursor."
agent = "cursor"
system-prompt-file = "prompt.md"

[profiles."r15.dot"]
description = "Dotted name."
agent = "claude"

[profiles.claude]
description = "Provider kind name."
agent = "claude"

[profiles.agy]
description = "Provider alias name."
agent = "claude"

[profiles.opencode-mini]
description = "Provider variant name."
agent = "claude"

[profiles.r15_both]
description = "In both tables."
agent = "claude"

[subagents.profiles.r15_both]
description = "In both tables."
agent = "claude"

[profiles.cyc_a]
description = "Cycle A."
agent = "cyc_b"

[profiles.cyc_b]
description = "Cycle B."
agent = "cyc_a"

[profiles.unk_base]
description = "Unknown base."
agent = "no-such-parent"

[profiles.droid_model]
description = "Droid model."
agent = "droid"
model = "custom-model"

[profiles.bad_budget]
description = "Bad budget."
agent = "claude"
budget = "not-a-budget"

[profiles.bad_compact]
description = "Bad compact."
agent = "claude"
auto-compact = "10k"

[profiles.bad_args]
description = "Bad args."
agent = "claude"
args = "--flag 'unclosed"

[profiles.bad_warm]
description = "Keep warm while."
agent = "claude"
keep-warm = {{ max = "1h", while = "team.stage!=Done" }}

[profiles.plus_sub]
description = "Plus subagent."
agent = "claude"
subagents = ["+codex"]

{deep}
"#
    );
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", &config);
    // Passing sibling still launches!
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("ok"),
        &crate::config::Selection::default(),
    )
    .expect("passing sibling launches");

    for (scope, table, name, fragment) in [
        (
            ProfileScope::Agents,
            "profiles",
            "r1_missing",
            "description",
        ),
        (
            ProfileScope::Agents,
            "profiles",
            "r1_multiline",
            "multi-line",
        ),
        (
            ProfileScope::Agents,
            "profiles",
            "r4_foreign",
            "runs on 'codex'",
        ),
        (ProfileScope::Agents, "profiles", "r6_rel", "relative"),
        (ProfileScope::Agents, "profiles", "r10_manual", "user-only"),
        (
            ProfileScope::Subagents,
            "subagents.profiles",
            "r11_idle",
            "idle-compact",
        ),
        (ProfileScope::Agents, "profiles", "r12_yolo", "codex-yolo"),
        (ProfileScope::Agents, "profiles", "r12_fast", "codex-fast"),
        (
            ProfileScope::Agents,
            "profiles",
            "r12_mini",
            "opencode-mini",
        ),
        (ProfileScope::Agents, "profiles", "r12_term", "term"),
        (ProfileScope::Agents, "profiles", "r13_unknown", "tools"),
        (
            ProfileScope::Agents,
            "profiles",
            "r14_skill",
            "not-installed",
        ),
        (
            ProfileScope::Agents,
            "profiles",
            "r14_promptless",
            "takes no system prompt",
        ),
        (ProfileScope::Agents, "profiles", "r15.dot", "unsafe"),
        (ProfileScope::Agents, "profiles", "claude", "agent runtime"),
        (ProfileScope::Agents, "profiles", "agy", "agent runtime"),
        (
            ProfileScope::Agents,
            "profiles",
            "opencode-mini",
            "agent runtime",
        ),
        (ProfileScope::Agents, "profiles", "r15_both", "both"),
        (
            ProfileScope::Subagents,
            "subagents.profiles",
            "r15_both",
            "both",
        ),
        (ProfileScope::Agents, "profiles", "cyc_a", "follows itself"),
        (
            ProfileScope::Agents,
            "profiles",
            "unk_base",
            "unknown profile",
        ),
        (ProfileScope::Agents, "profiles", "d16", "16"),
        (
            ProfileScope::Agents,
            "profiles",
            "droid_model",
            "does not support profile field `model`",
        ),
        (ProfileScope::Agents, "profiles", "bad_budget", "budget"),
        (
            ProfileScope::Agents,
            "profiles",
            "bad_compact",
            "auto-compact",
        ),
        (
            ProfileScope::Agents,
            "profiles",
            "bad_args",
            "shell quoting",
        ),
        (ProfileScope::Agents, "profiles", "bad_warm", "keep-warm"),
        (
            ProfileScope::Agents,
            "profiles",
            "plus_sub",
            "Markdown definitions only",
        ),
    ] {
        assert_row_refused(
            &machine,
            &launch,
            scope,
            project.path(),
            table,
            name,
            fragment,
        );
    }
    // Depth 16 itself (`d15`, chain length 16) loads.
    assert!(launch.profiles.0.contains_key("d15"));
    assert_row_refused(
        &machine,
        &launch,
        ProfileScope::Agents,
        project.path(),
        "profiles",
        "r12_general",
        &format!(
            "allows unknown subagent profile(s) `general`; add it as a \
             `[subagents.profiles.general]` row in {} or as {}, or remove it from the \
             `subagents` list that names it",
            project.path().join(".rimz/config.toml").display(),
            home.path().join("subagents/general.md").display()
        ),
    );

    // Row 2: machine kind base exists without tools/capabilities (claude, codex, antigravity).
    let home2 = tempdir().unwrap();
    let project2 = tempdir().unwrap();
    std::fs::create_dir_all(home2.path().join("agents")).unwrap();
    for kind in ["claude", "codex", "antigravity"] {
        std::fs::write(
            home2.path().join(format!("agents/{kind}.md")),
            format!("---\ndescription: {kind} base\n---\n{kind} house.\n"),
        )
        .unwrap();
    }
    let (m2, l2) = load_trusted_project(
        home2.path(),
        project2.path(),
        "",
        "[profiles.c]\ndescription = 'c'\nagent = 'claude'\n\
         [profiles.x]\ndescription = 'x'\nagent = 'codex'\n\
         [profiles.a]\ndescription = 'a'\nagent = 'antigravity'\n",
    );
    for (name, kind) in [("c", "claude"), ("x", "codex"), ("a", "antigravity")] {
        let base_path = home2.path().join(format!("agents/{kind}.md"));
        assert_row_refused(
            &m2,
            &l2,
            ProfileScope::Agents,
            project2.path(),
            "profiles",
            name,
            &format!("machine kind base {}", base_path.display()),
        );
    }

    // Row 3: with no machine kind base, the row takes its tools from the built-in one.
    let home3 = tempdir().unwrap();
    let project3 = tempdir().unwrap();
    let (m3, l3) = load_trusted_project(
        home3.path(),
        project3.path(),
        "",
        "[profiles.a]\ndescription = 'a'\nagent = 'claude'\n",
    );
    crate::harness::plan::resolve_launch(
        &l3,
        ProfileScope::Agents,
        &m3.agents.commands,
        Some("a"),
        &crate::config::Selection::new(&m3.tiers),
    )
    .expect("a row on the built-in claude base launches");

    // Rows 8, 9, and 14 (RimZ capability in skills beside capabilities base).
    let home4 = tempdir().unwrap();
    let project4 = tempdir().unwrap();
    std::fs::create_dir_all(home4.path().join("agents")).unwrap();
    std::fs::create_dir_all(home4.path().join("skills/review")).unwrap();
    std::fs::write(
        home4.path().join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review\n---\nBody.\n",
    )
    .unwrap();
    std::fs::write(
        home4.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read, Agent]\n---\nHouse.\n",
    )
    .unwrap();
    std::fs::write(
        home4.path().join("agents/codex.md"),
        "---\ndescription: Codex base\ncapabilities: [read, write, bash]\n---\nHouse.\n",
    )
    .unwrap();
    let (m4, l4) = load_trusted_project(
        home4.path(),
        project4.path(),
        "",
        "[profiles.r8]\ndescription = 'No Skill tool'\nagent = 'claude'\nskills = ['review']\n\
         [profiles.r9]\ndescription = 'Agent tool + subagents'\nagent = 'claude'\nsubagents = ['codex']\n\
         [profiles.r14_cap]\ndescription = 'Cap in skills'\nagent = 'codex'\nskills = ['rimz-watch']\n",
    );
    assert_row_refused(
        &m4,
        &l4,
        ProfileScope::Agents,
        project4.path(),
        "profiles",
        "r8",
        "no Skill tool",
    );
    assert_row_refused(
        &m4,
        &l4,
        ProfileScope::Agents,
        project4.path(),
        "profiles",
        "r9",
        "lists `Agent` in `tools:`",
    );
    assert_row_refused(
        &m4,
        &l4,
        ProfileScope::Agents,
        project4.path(),
        "profiles",
        "r14_cap",
        "capabilities",
    );
}

#[test]
fn project_rows_5_7_16_17_18_and_tier_resolve_as_definitions() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read, Skill]\nmode: plan\nskills: [review]\n\
         mcps: [rimz-lsp]\nbudget: 12\nidle-compact: 150k\nsmart-compact: 200k\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/md_l.md"),
        "---\ndescription: Markdown L\nagent: claude\n---\nMD L craft.\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("skills/other")).unwrap();
    std::fs::write(
        home.path().join("skills/other/SKILL.md"),
        "---\nname: other\ndescription: Other\n---\nOther.\n",
    )
    .unwrap();
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "[tiers]\nsenior = ['opus', 'astra']\n",
        r#"
[profiles.parent]
description = "Claude parent."
agent = "claude"
subagents = ["codex"]
idle-compact = "120k"
keep-warm = "1h"

[profiles.r5_child]
description = "Follows codex model."
agent = "parent"
model = "sol"

[profiles.r7_effort]
description = "Default effort."
agent = "claude"

[profiles.r16_x]
description = "Project X."
agent = "claude"
mode = "ask"
skills = ["other"]
mcps = ["rimz-watch"]
subagents = []
budget = "5"
idle-compact = "180k"
smart-compact = "150k"

[profiles.r18_chain]
description = "Chained auto-compact default."
agent = "parent"

[subagents.profiles.r17_sub]
description = "Subagent inheriting direct row."
agent = "parent"

[agents.teams.proj_team]
leader = "lead"
roles = [{ role = "lead", profile = "r7_effort", model = "sonnet" }]
"#,
    );

    // Row 5: child on claude parent with codex model runs on codex.
    let r5 = agents_spec::resolve_profile("r5_child", &launch.profiles).unwrap();
    assert_eq!(r5.kind.as_str(), "codex");
    assert_eq!(r5.launch.model.as_deref(), Some("sol"));

    // Row 7: unset effort gets built-in default (`xhigh`).
    let r7 = agents_spec::resolve_profile("r7_effort", &launch.profiles).unwrap();
    assert_eq!(r7.launch.effort.as_deref(), Some("xhigh"));

    // Row 16: project X over Markdown L beats provider base on mode, skills, mcps, subagents,
    // budget, and compaction settings.
    let r16 =
        agents_spec::resolve_profile_under("md_l", Some("r16_x"), &launch.profiles, None).unwrap();
    assert_eq!(r16.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(
        r16.skills
            .as_ref()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["other"]
    );
    assert_eq!(
        r16.mcps
            .as_ref()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["rimz-watch"]
    );
    assert_eq!(r16.launch.budget.as_deref(), Some("$5.00"));
    assert_eq!(
        SmartCompactSeat {
            team: None,
            role: None,
            profile: Some("md_l"),
            agent: Some("r16_x"),
            kind: Some("claude"),
            scope: ProfileScope::Agents,
        }
        .explicit(&launch.teams, &launch.profiles, &launch.subagent_profiles),
        Some(crate::config::FlipCompact::Threshold(
            crate::store::message::AutoCompact::Tokens(150_000)
        ))
    );
    assert_eq!(
        IdleCompactSeat {
            team: None,
            role: None,
            profile: Some("md_l"),
            agent: Some("r16_x"),
            kind: Some("claude"),
            scope: ProfileScope::Agents,
            launched_child: false,
        }
        .resolve(
            &launch.teams,
            &launch.profiles,
            &launch.subagent_profiles,
            &machine.harness
        ),
        Some(180_000)
    );
    assert_eq!(
        crate::harness::subagent_policy::catalog(
            Some("md_l"),
            Some("r16_x"),
            Some("claude"),
            ProfileScope::Agents,
            &launch.profiles,
            &launch.subagent_profiles,
            &machine.agents.commands,
        ),
        crate::harness::subagent_policy::SubagentCatalog::NoneAllowed
    );

    // Row 17: subagent inherits project [profiles] row, dropping direct-only fields.
    let r17_row = &launch.subagent_profiles.0["r17_sub"];
    assert_eq!(r17_row.agent, "claude");
    assert_eq!(r17_row.subagents, None);
    assert_eq!(r17_row.idle_compact, None);
    assert_eq!(r17_row.keep_warm, None);

    // Row 18: auto-compact default applied on chained row.
    let r18 = agents_spec::resolve_profile("r18_chain", &launch.profiles).unwrap();
    assert_eq!(
        r18.auto_compact_default,
        Some(crate::config::AutoCompactDefault::BuiltIn)
    );
    assert_eq!(
        r18.auto_compact_default
            .and_then(|d| d.resolve_for_kind(&r18.kind)),
        Some(272_000)
    );

    // `--tier` on a project team with TOML role `model`.

    let team_resolved = launch_under(
        &launch,
        &machine.tiers,
        ProfileScope::Agents,
        "proj_team",
        Some(super::super::tiers::ModelTier::Senior),
        None,
        Some("codex"),
        |_, _| None,
    )
    .expect("--tier on project team succeeds");
    let seat = team_resolved.layout.agent_cells().next().unwrap();
    assert_eq!(seat.kind.as_str(), "codex");
    assert_eq!(seat.launch.model.as_deref(), Some("astra"));
    assert_eq!(
        seat.launch.tier.as_ref().map(|t| t.tier),
        Some(super::super::tiers::ModelTier::Senior)
    );
}

#[test]
fn project_row_and_markdown_with_same_lines_produce_identical_launch() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/md_same.md"),
        "---\ndescription: Same lines.\nagent: claude\nmodel: opus\neffort: high\nmode: ask\n\
         budget: 9\nfast: on\nskills: [review]\nmcps: [rimz-lsp]\n---\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/md_other.md"),
        "---\ndescription: Other.\nagent: codex\nmodel: sol\neffort: low\nmode: plan\n---\n",
    )
    .unwrap();
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        r#"
[profiles.toml_same]
description = "Same lines."
agent = "claude"
model = "opus"
effort = "high"
mode = "ask"
budget = "9"
fast = "on"
skills = ["review"]
mcps = ["rimz-lsp"]

[agents.teams.t_toml]
leader = "lead"
roles = [{ role = "lead", profile = "toml_same" }]
"#,
    );
    let mut teams = launch.teams.clone();
    let mut md_team = teams.0["t_toml"].clone();
    md_team.roles[0].profile = "md_same".to_owned();
    teams.0.insert("t_md".to_owned(), md_team);

    let compare = |spec_a: &str, ov_a: Option<&str>, spec_b: &str, ov_b: Option<&str>| {
        let mut launch_cmp = launch.clone();
        launch_cmp.teams = teams.clone();
        let a = crate::harness::plan::resolve_launch(
            &launch_cmp,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec_a),
            &crate::config::Selection::default().agent(ov_a),
        )
        .unwrap();
        let b = crate::harness::plan::resolve_launch(
            &launch_cmp,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec_b),
            &crate::config::Selection::default().agent(ov_b),
        )
        .unwrap();
        let ca = a.layout.agent_cells().next().unwrap();
        let cb = b.layout.agent_cells().next().unwrap();
        assert_eq!(ca.kind, cb.kind);
        assert_eq!(ca.args, cb.args);
        assert_eq!(ca.system_prompt_file, cb.system_prompt_file);
        assert_eq!(ca.append_system_prompt_files, cb.append_system_prompt_files);
        assert_eq!(ca.launch.model, cb.launch.model);
        assert_eq!(ca.launch.effort, cb.launch.effort);
        assert_eq!(ca.launch.mode, cb.launch.mode);
        assert_eq!(ca.launch.budget, cb.launch.budget);
        assert_eq!(ca.skills, cb.skills);
        assert_eq!(ca.mcps, cb.mcps);
        assert_eq!(ca.tools, cb.tools);
    };

    // As L, as X over MD L, under MD X, and as a team seat.
    compare("toml_same", None, "md_same", None);
    compare("md_other", Some("toml_same"), "md_other", Some("md_same"));
    compare("toml_same", Some("md_other"), "md_same", Some("md_other"));
    compare("t_toml", None, "t_md", None);
}

#[test]
fn project_row_failure_is_isolated_from_passing_rows_and_unrelated_teams() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        r#"
[profiles.good]
description = "Good profile."
agent = "claude"

[profiles.bad]
agent = "claude"

[agents.teams.good_team]
leader = "lead"
roles = [{ role = "lead", profile = "good" }]

[agents.teams.bad_team]
leader = "lead"
roles = [{ role = "lead", profile = "bad" }]
"#,
    );
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("good"),
        &crate::config::Selection::default(),
    )
    .expect("good profile launches");
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("good_team"),
        &crate::config::Selection::default(),
    )
    .expect("unrelated team launches");

    for (spec, agent_ov) in [
        (Some("bad"), None),
        (Some("good"), Some("bad")),
        (Some("bad_team"), None),
        (Some("bad_team.lead"), None),
    ] {
        let err = crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            spec,
            &crate::config::Selection::default().agent(agent_ov),
        )
        .expect_err(&format!("spec={spec:?} ov={agent_ov:?} must refuse"));
        let text = err.to_string();
        assert!(text.contains("[profiles.bad]"), "{text}");
        assert!(text.contains("description"), "{text}");
    }
}

#[test]
fn failed_project_row_shadows_machine_namesake_and_kind_row_keeps_machine_defs_working() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/shared.md"),
        "---\ndescription: Machine shared\nagent: claude\n---\nShared craft.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/md_child.md"),
        "---\ndescription: Machine child\nagent: claude\n---\nChild craft.\n",
    )
    .unwrap();
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        "[profiles.shared]\nagent = 'claude'\n\
         [profiles.claude]\ndescription = 'Shadow kind'\nagent = 'claude'\n",
    );
    assert_row_refused(
        &machine,
        &launch,
        ProfileScope::Agents,
        project.path(),
        "profiles",
        "shared",
        "description",
    );
    for (spec, ov) in [
        (Some("claude"), None),
        (Some("claude-yolo"), None),
        (Some("md_child"), Some("claude")),
    ] {
        let err = crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            spec,
            &crate::config::Selection::default().agent(ov),
        )
        .expect_err(&format!("spec={spec:?} ov={ov:?} must refuse"));
        assert!(err.to_string().contains("[profiles.claude]"), "{err}");
    }
    // Machine Markdown definitions on `claude` still work!
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("md_child"),
        &crate::config::Selection::default(),
    )
    .expect("machine definition on claude stays unaffected");
}

#[test]
fn project_toml_only_keys_preserve_prompt_order_and_drop_args_on_kind_switch() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    let (machine, launch) = load_trusted_project(
        home.path(),
        project.path(),
        "",
        r#"
[profiles.parent]
description = "Parent."
agent = "claude"
system-prompt-file = "p-base.md"
append-system-prompt-files = ["p-app.md"]
args = "--parent-claude"

[profiles.child]
description = "Child."
agent = "parent"
append-system-prompt-files = ["c-app.md"]
"#,
    );
    let same = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("child"),
        &crate::config::Selection::default(),
    )
    .unwrap();
    let cell = same.layout.agent_cells().next().unwrap();
    assert_eq!(
        cell.system_prompt_file,
        Some(crate::config::PromptSource::File(
            project.path().join(".rimz/p-base.md")
        ))
    );
    assert_eq!(
        cell.append_system_prompt_files,
        vec![
            crate::config::PromptSource::File(project.path().join(".rimz/p-app.md")),
            crate::config::PromptSource::File(project.path().join(".rimz/c-app.md")),
        ]
    );
    assert!(
        cell.args.iter().any(|a| a == "--parent-claude"),
        "{:?}",
        cell.args
    );

    let switched = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("child"),
        &crate::config::Selection::default().agent(Some("codex")),
    )
    .unwrap();
    let switched_cell = switched.layout.agent_cells().next().unwrap();
    assert_eq!(switched_cell.kind.as_str(), "codex");
    assert!(
        !switched_cell.args.iter().any(|a| a == "--parent-claude"),
        "{:?}",
        switched_cell.args
    );
}

#[test]
fn project_config_loads_without_machine_load_contexts() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(
        &project,
        "[profiles.promptless]\ndescription = 'Amp row.'\nagent = 'amp'\n\
         [profiles.needs_base]\ndescription = 'Claude row.'\nagent = 'claude'\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
    let machine = MachineConfig::default();
    assert!(machine.definition_contexts.is_none());
    let launch = load_with_roots(&machine, project.path(), config.path()).unwrap();
    assert!(launch.profiles.0.contains_key("promptless"));
    assert_eq!(launch.profiles.0["needs_base"].tools, None);
}

#[test]
fn machine_definition_under_edited_project_override_never_serves_stale_memo() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/md_l.md"),
        "---\ndescription: MD L\nagent: claude\n---\nMD L craft.\n",
    )
    .unwrap();
    let cfg_path = home.path().join("config.toml");
    let machine = MachineConfig::parse_text(&cfg_path, "", home.path()).unwrap();

    // First project config: X sets effort = low.
    write_project_config(
        &project,
        "[profiles.px]\ndescription = 'X v1'\nagent = 'claude'\neffort = 'low'\n",
    );
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let launch1 = load_with_roots(&machine, project.path(), home.path()).unwrap();
    let r1 =
        agents_spec::resolve_profile_under("md_l", Some("px"), &launch1.profiles, None).unwrap();
    assert_eq!(r1.launch.effort.as_deref(), Some("low"));

    // Second project config in the same process sharing `machine`: X sets effort = high.
    write_project_config(
        &project,
        "[profiles.px]\ndescription = 'X v2'\nagent = 'claude'\neffort = 'high'\n",
    );
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let launch2 = load_with_roots(&machine, project.path(), home.path()).unwrap();
    let r2 =
        agents_spec::resolve_profile_under("md_l", Some("px"), &launch2.profiles, None).unwrap();
    assert_eq!(r2.launch.effort.as_deref(), Some("high"));
}

#[test]
fn team_role_args_drop_on_provider_switch_for_model_and_tier_on_fresh_and_replay() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/antigravity.md"),
        "---\ndescription: Antigravity base.\ntools: [Read]\n---\nAntigravity house.\n",
    )
    .unwrap();
    let machine_toml = "\
        [tiers]\n\
        senior = [\"gpt-5.5\"]\n\
        junior = [\"sonnet\"]\n";
    let project_toml = "\
        [profiles.ok]\n\
        description = \"Project Claude row.\"\n\
        agent = \"claude\"\n\
        \n\
        [agents.teams.crew]\n\
        leader = \"lead\"\n\
        roles = [{ role = \"lead\", profile = \"ok\", args = \"--role-claude-flag\" }]\n";
    let (machine, base_launch) =
        load_trusted_project(home.path(), project.path(), machine_toml, project_toml);
    let cases = [
        // A provider switch drops role args.
        (None, Some("gpt-5.5"), "codex", false),
        (
            Some(crate::config::tiers::ModelTier::Senior),
            None,
            "codex",
            false,
        ),
        // Non-tier provider switch exercises the definition rebuild on replay as well.
        (None, Some("neon"), "antigravity", false),
        // The same provider keeps role args.
        (None, Some("opus"), "claude", true),
        (
            Some(crate::config::tiers::ModelTier::Junior),
            None,
            "claude",
            true,
        ),
    ];

    for (tier, model, expected_kind, expect_role_arg) in cases {
        let spec = "crew.lead";
        let mut resolved = launch_under(
            &base_launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            tier,
            model,
            None,
            |_, _| None,
        )
        .unwrap_or_else(|err| panic!("{spec} tier={tier:?} model={model:?}: {err}"));
        let preset = crate::agents::LaunchPreset {
            model: model.map(str::to_owned),
            ..Default::default()
        };
        crate::harness::plan::finalize_launch_layout(
            &mut resolved.layout,
            crate::harness::plan::LaunchFinalizeOptions {
                smart_compact: None,
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &preset,
                passthrough: &[],
                budget: None,
                max_turns: None,
                machine_config: None,
            },
        )
        .unwrap_or_else(|err| panic!("finalize {spec} tier={tier:?} model={model:?}: {err}"));
        let fresh = resolved.layout.agent_cells().next().unwrap();
        assert_eq!(
            fresh.kind.as_str(),
            expected_kind,
            "fresh kind for {spec} tier={tier:?} model={model:?}"
        );
        assert_eq!(
            fresh.args.iter().any(|arg| arg == "--role-claude-flag"),
            expect_role_arg,
            "fresh args for {spec} tier={tier:?} model={model:?}: {:?}",
            fresh.args
        );

        let record = crate::agents::LaunchRecord {
            model: fresh.launch.model.clone(),
            effort: fresh.launch.effort.clone(),
            fast: fresh.launch.fast,
            ..Default::default()
        };
        let replay = crate::harness::resume::resolve_member_posture(
            crate::harness::resume::PostureRequest {
                record: Some(&record),
                profile: Some("ok"),
                kind: &fresh.kind,
                stamped_mode: fresh.launch.mode,
                stamped_tier: fresh.launch.tier.as_deref(),
            },
            &base_launch.profiles,
            None,
            &base_launch.teams,
            Some("crew"),
            Some("lead"),
        );
        assert!(
            replay.degraded.is_none(),
            "replay degraded for {spec} tier={tier:?} model={model:?}: {:?}",
            replay.degraded
        );
        assert_eq!(
            replay
                .launch
                .args
                .iter()
                .any(|arg| arg == "--role-claude-flag"),
            expect_role_arg,
            "replay args for {spec} tier={tier:?} model={model:?}: {:?}",
            replay.launch.args
        );
        let mut fresh_sorted = fresh.args.clone();
        fresh_sorted.sort();
        let mut replay_sorted = replay.launch.args.clone();
        replay_sorted.sort();
        assert_eq!(
            replay_sorted, fresh_sorted,
            "replay argv must match fresh argv for {spec} tier={tier:?} model={model:?}"
        );
        assert_eq!(replay.launch.model, fresh.launch.model);
        assert_eq!(replay.launch.effort, fresh.launch.effort);
        assert_eq!(replay.launch.tier, fresh.launch.tier);
    }
}

#[test]
fn mutual_agent_and_subagent_failure_reports_root_cause_for_both() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    let project_toml = "\
        [profiles.a]\n\
        description = \"Broken agent\"\n\
        agent = \"claude\"\n\
        budget = \"garbage\"\n\
        subagents = [\"s\"]\n\
        \n\
        [subagents.profiles.s]\n\
        description = \"Child following a\"\n\
        agent = \"a\"\n\
        \n\
        [profiles.chain_base]\n\
        description = \"Broken chain root\"\n\
        agent = \"claude\"\n\
        budget = \"garbage\"\n\
        \n\
        [profiles.chain_mid]\n\
        description = \"Mid depending on broken root\"\n\
        agent = \"chain_base\"\n\
        subagents = [\"chain_sub\"]\n\
        \n\
        [subagents.profiles.chain_sub]\n\
        description = \"Sub following mid\"\n\
        agent = \"chain_mid\"\n\
        \n\
        [profiles.good_parent]\n\
        description = \"Healthy parent\"\n\
        agent = \"claude\"\n\
        subagents = [\"bad_child\"]\n\
        \n\
        [subagents.profiles.bad_child]\n\
        description = \"Broken child\"\n\
        agent = \"good_parent\"\n\
        budget = \"garbage\"\n";
    let (_machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    let detail_a = launch.definition_failure_for("a").expect("a must fail");
    assert!(
        detail_a.contains("garbage"),
        "a must name its own budget error, got: {detail_a}"
    );
    assert!(
        !detail_a.contains("allows subagent"),
        "a must report its own budget error rather than cycling through s, got: {detail_a}"
    );
    let detail_s = launch.definition_failure_for("s").expect("s must fail");
    assert!(
        detail_s.contains("follows `a`")
            && detail_s.contains("caused by:")
            && detail_s.contains("garbage"),
        "s must trace back to a's budget error, got: {detail_s}"
    );

    let detail_mid = launch
        .definition_failure_for("chain_mid")
        .expect("chain_mid must fail");
    assert!(
        detail_mid.contains("garbage"),
        "chain_mid must end at chain_base's budget error, got: {detail_mid}"
    );
    let detail_sub = launch
        .definition_failure_for("chain_sub")
        .expect("chain_sub must fail");
    assert!(
        detail_sub.contains("garbage"),
        "chain_sub must end at chain_base's budget error, got: {detail_sub}"
    );

    let detail_bad_child = launch
        .definition_failure_for("bad_child")
        .expect("bad_child must fail");
    assert!(
        detail_bad_child.contains("garbage"),
        "bad_child must name its budget error, got: {detail_bad_child}"
    );
    let detail_good_parent = launch
        .definition_failure_for("good_parent")
        .expect("good_parent must fail");
    assert!(
        detail_good_parent.contains("allows subagent 'bad_child'")
            && detail_good_parent.contains("caused by:")
            && detail_good_parent.contains("garbage"),
        "good_parent must trace to bad_child's budget error, got: {detail_good_parent}"
    );
}

#[test]
fn failed_project_row_and_team_never_fall_through_to_other_readings() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/antigravity.md"),
        "---\ndescription: Antigravity base.\n---\nAntigravity house.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/foo.md"),
        "---\ndescription: Machine foo.\nagent: claude\n---\nMachine foo.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/shared.md"),
        "---\ndescription: Machine shared.\nagent: claude\n---\nMachine shared.\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("teams")).unwrap();
    std::fs::write(
        home.path().join("teams/mteam.md"),
        "---\nleader: lead\nstages: [Build]\nlayout: \"lead,shared\"\nroles:\n  - role: lead\n    agent: foo\n    owns: [Build]\n---\nBuild.\n",
    )
    .unwrap();
    // A machine team that fails its own check, named like an executable on every PATH.
    std::fs::write(
        home.path().join("teams/sh.md"),
        "---\nleader: lead\nstages: [Build]\nlayout: \"lead,ghost\"\nroles:\n  - role: lead\n    agent: foo\n    owns: [Build]\n---\nBuild.\n",
    )
    .unwrap();

    let project_toml = "\
        [profiles.ok]\n\
        description = \"Healthy\"\n\
        agent = \"claude\"\n\
        \n\
        [profiles.claude-yolo]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.claude-fast]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.git]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.peer]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.agy]\n\
        agent = \"antigravity\"\n\
        \n\
        [profiles.work]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.shared]\n\
        agent = \"claude\"\n\
        \n\
        [agents.teams.foo]\n\
        leader = \"lead\"\n\
        layout = \"lead\"\n\
        [[agents.teams.foo.roles]]\n\
        role = \"lead\"\n\
        profile = \"work\"\n";
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    for (spec, expected) in [
        ("claude-yolo", "description"),
        ("claude-fast", "description"),
        ("git", "description"),
        ("git,ok", "description"),
        ("git:lead", "description"),
        ("peer", "description"),
        ("agy", "takes the name of an agent runtime"),
        ("agy-fast", "takes the name of an agent runtime"),
        ("agy-yolo", "takes the name of an agent runtime"),
        ("foo", "description"),
        ("foo.lead", "description"),
        ("mteam", "description"),
        ("sh", "layout token `ghost`"),
        ("sh.lead", "layout token `ghost`"),
    ] {
        let err = crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec),
            &crate::config::Selection::default(),
        )
        .expect_err(&format!("{spec} must refuse instead of falling through"));
        let msg = err.to_string();
        assert!(
            msg.contains(expected),
            "{spec} must report `{expected}`, got: {msg}"
        );
    }

    // An `[agents.commands]` entry keeps winning over a same-named failed project row.
    let mut commands = machine.agents.commands.clone();
    commands.0.insert("git".to_owned(), "git status".to_owned());
    let resolved = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &commands,
        Some("git"),
        &crate::config::Selection::default(),
    )
    .expect("[agents.commands] git must still win over failed project [profiles.git]");
    assert!(
        resolved.layout.agent_cells().next().is_none(),
        "git command cell should not resolve as an agent"
    );
    // ... and over a same-named failed machine team.
    commands.0.insert("sh".to_owned(), "sh -c true".to_owned());
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &commands,
        Some("sh"),
        &crate::config::Selection::default(),
    )
    .expect("[agents.commands] sh must still win over the failed machine team sh");
}

#[test]
fn failed_project_row_in_team_layout_variant_fails_only_that_team() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    let project_toml = "\
        [profiles.ok]\n\
        description = \"Healthy\"\n\
        agent = \"claude\"\n\
        \n\
        [profiles.work]\n\
        agent = \"claude\"\n\
        \n\
        [agents.teams.tc]\n\
        leader = \"lead\"\n\
        layout = \"lead,work-fast\"\n\
        [[agents.teams.tc.roles]]\n\
        role = \"lead\"\n\
        profile = \"ok\"\n";
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    // Healthy rows still launch.
    crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("ok"),
        &crate::config::Selection::default(),
    )
    .expect("healthy row ok must still launch");

    // Only the team citing the failed row fails.
    for spec in ["tc", "tc.lead"] {
        let err = crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec),
            &crate::config::Selection::default(),
        )
        .expect_err(&format!("{spec} must fail with work's detail"));
        let msg = err.to_string();
        assert!(
            msg.contains("description"),
            "{spec} must surface work's failure detail, got: {msg}"
        );
    }
}

#[test]
fn tier_refuses_failed_project_row_team_and_markdown_ahead_of_eligibility() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/bad_md.md"),
        "---\nagent: claude\n---\nMissing description.\n",
    )
    .unwrap();
    let project_toml = "\
        [profiles.ok]\n\
        description = \"Healthy\"\n\
        agent = \"claude\"\n\
        \n\
        [profiles.bad]\n\
        agent = \"claude\"\n\
        \n\
        [agents.teams.broken_team]\n\
        leader = \"lead\"\n\
        layout = \"lead\"\n\
        [[agents.teams.broken_team.roles]]\n\
        role = \"lead\"\n\
        profile = \"bad\"\n";
    let (machine, base_launch) =
        load_trusted_project(home.path(), project.path(), "", project_toml);

    for target in [
        "bad",
        "bad-fast",
        "broken_team",
        "broken_team.lead",
        "bad_md",
    ] {
        let err = launch_under(
            &base_launch,
            &machine.tiers,
            ProfileScope::Agents,
            target,
            Some(super::super::tiers::ModelTier::Senior),
            None,
            None,
            |_, _| None,
        )
        .map(|_| ())
        .expect_err(&format!("--tier on {target} must refuse"));
        let msg = err.to_string();
        assert!(
            msg.contains("description") && !msg.contains("Markdown definitions"),
            "--tier on {target} must report failure detail ahead of eligibility, got: {msg}"
        );
    }
}

#[test]
fn failed_subagent_kind_named_and_override_never_bypass_guard_on_replay_or_seat_settings() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/worker.md"),
        "---\ndescription: Machine worker.\nagent: claude\nkeep-warm: 1h\n---\nMachine worker.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/ov.md"),
        "---\ndescription: Machine override.\nagent: claude\nkeep-warm: 2h\n---\nMachine ov.\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("subagents")).unwrap();
    std::fs::write(
        home.path().join("subagents/ok_sub.md"),
        "---\ndescription: Healthy subagent.\nagent: claude\n---\nOk sub.\n",
    )
    .unwrap();

    let project_toml = "\
        [profiles.codex]\n\
        description = \"Illegal kind-named agent\"\n\
        agent = \"codex\"\n\
        \n\
        [subagents.profiles.worker]\n\
        agent = \"claude\"\n\
        \n\
        [subagents.profiles.claude]\n\
        description = \"Illegal kind-named subagent\"\n\
        agent = \"claude\"\n\
        \n\
        [subagents.profiles.ov]\n\
        agent = \"claude\"\n";
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    // 1. Failing [subagents.profiles.worker] with machine agents/worker.md must NOT resolve to
    //    machine worker on replay or seat settings.
    let mut sub_worker =
        crate::testkit::agent_state("claude", "sub-worker", jiff::Timestamp::UNIX_EPOCH);
    sub_worker.profile = Some("worker".into());
    sub_worker.profile_scope = Some(ProfileScope::Subagents);
    sub_worker.worktree_path = Some(project.path().display().to_string());
    sub_worker.record = Some(Box::default());

    let mut kind_named =
        crate::testkit::agent_state("codex", "kind-named", jiff::Timestamp::UNIX_EPOCH);
    kind_named.profile = Some("codex".into());
    kind_named.worktree_path = Some(project.path().display().to_string());
    kind_named.record = Some(Box::default());

    let mut failed_ov =
        crate::testkit::agent_state("claude", "failed-ov", jiff::Timestamp::UNIX_EPOCH);
    failed_ov.profile = Some("ok_sub".into());
    failed_ov.profile_scope = Some(ProfileScope::Subagents);
    failed_ov.worktree_path = Some(project.path().display().to_string());
    failed_ov.record = Some(Box::new(crate::agents::LaunchRecord {
        agent: Some("ov".into()),
        ..Default::default()
    }));

    let workspace = crate::WorkspaceResolver::resolve_under(
        project.path(),
        None,
        &home.path().join("rimz-state"),
    )
    .unwrap();
    let state = crate::StatePaths::for_project_root(&workspace.project_root).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let logins = crate::agents::room_accounts(&state.workspace_record, None, &machine).unwrap();
    let catalog = crate::agents::LoginCatalog::default();
    let options = crate::harness::resume::SingleResumeOptions::default();
    for (label, agent) in [
        ("sub_worker", &sub_worker),
        ("kind_named", &kind_named),
        ("failed_ov", &failed_ov),
    ] {
        let (plan, _) = crate::harness::resume::plan_recorded_resume(
            agent,
            crate::harness::resume::ResumeContext {
                in_place: false,
                project_root: Some(project.path()),
                workspace_id: &state.workspace_id,
                rimz_bin: std::path::Path::new("rimz"),
                runtime: &runtime,
                profiles: &launch.profiles,
                subagent_profiles: &launch.subagent_profiles,
                selection: &crate::config::Selection::default(),
                max: 10,
                logins: &logins,
                catalog: &catalog,
            },
            &options,
            |_| true,
            |_| true,
        );
        assert_eq!(
            plan.tabs.len(),
            1,
            "{label}: expected 1 planned tab, got {}",
            plan.tabs.len()
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("resuming bare")),
            "{label}: failed project row must degrade with reason instead of silently falling back, got warnings: {:?}",
            plan.warnings
        );
    }

    // Child resume gate (`block_failed_launch` via `profiles_for_launch`) refuses all three:
    // failing `[subagents.profiles.worker]`, kind-named `[subagents.profiles.claude]`, and
    // failing override `[subagents.profiles.ov]` shadowing machine `agents/ov.md`.
    for (label, kind, profile, record_agent, expected) in [
        (
            "child_worker",
            "claude",
            Some("worker"),
            None,
            "description",
        ),
        (
            "child_claude",
            "claude",
            Some("claude"),
            None,
            "takes the name of an agent runtime",
        ),
        (
            "child_failed_ov",
            "claude",
            Some("ok_sub"),
            Some("ov"),
            "description",
        ),
    ] {
        let record = crate::agents::LaunchRecord {
            agent: record_agent.map(str::to_owned),
            ..Default::default()
        };
        let err = launch
            .block_failed_launch(
                &crate::ids::AgentKind::new_unchecked(kind),
                profile,
                Some(&record),
                ProfileScope::Subagents,
            )
            .expect_err(&format!("{label} must refuse on child resume"));
        assert!(
            err.to_string().contains(expected),
            "{label} must report `{expected}`, got: {err}"
        );
    }

    // Seat settings must NOT read machine agents/worker.md for failing [subagents.profiles.worker]
    // or machine agents/ov.md for failing [subagents.profiles.ov].
    let kw_worker = resolve_seat_setting(
        Some("worker"),
        None,
        Some("claude"),
        ProfileScope::Subagents,
        &launch.profiles,
        &launch.subagent_profiles,
        false,
        |p| p.keep_warm.clone(),
    );
    assert!(
        kw_worker.is_none(),
        "failed [subagents.profiles.worker] must not read machine worker keep-warm: {kw_worker:?}"
    );
    let kw_ov = resolve_seat_setting(
        Some("ok_sub"),
        Some("ov"),
        Some("claude"),
        ProfileScope::Subagents,
        &launch.profiles,
        &launch.subagent_profiles,
        false,
        |p| p.keep_warm.clone(),
    );
    assert!(
        kw_ov.is_none(),
        "failed [subagents.profiles.ov] override must not read machine ov keep-warm: {kw_ov:?}"
    );
    // Kind-named failing [profiles.codex] must not read retained kind base codex.
    let mut launch_with_base_kw = launch.clone();
    launch_with_base_kw
        .profiles
        .0
        .get_mut("codex")
        .unwrap()
        .keep_warm = launch.profiles.0["worker"].keep_warm.clone();
    let kw_codex = resolve_seat_setting(
        Some("codex"),
        None,
        Some("codex"),
        ProfileScope::Agents,
        &launch_with_base_kw.profiles,
        &launch_with_base_kw.subagent_profiles,
        false,
        |p| p.keep_warm.clone(),
    );
    assert!(
        kw_codex.is_none(),
        "failed [profiles.codex] must not read retained kind base keep-warm: {kw_codex:?}"
    );
}

#[test]
fn machine_definition_under_project_override_validates_project_subagents_and_isolates_memo() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::create_dir_all(home.path().join("subagents")).unwrap();
    std::fs::write(
        home.path().join("subagents/mkid.md"),
        "---\ndescription: Machine subagent.\nagent: claude\n---\nMachine kid.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/l.md"),
        "---\ndescription: Machine leaf.\nagent: claude\n---\nMachine leaf.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/m.md"),
        "---\ndescription: Machine override.\nagent: claude\nsubagents: [mkid]\n---\nMachine ov.\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/l_bad.md"),
        "---\ndescription: Machine leaf naming project subagent directly.\nagent: claude\nsubagents: [kid]\n---\nBad leaf.\n",
    )
    .unwrap();

    let project_toml = "\
        [subagents.profiles.kid]\n\
        description = \"Project subagent\"\n\
        agent = \"claude\"\n\
        \n\
        [profiles.x]\n\
        description = \"Project override\"\n\
        agent = \"claude\"\n\
        subagents = [\"kid\"]\n";
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    // Ordinary launch: machine `l` under project `--agent x` must succeed and inherit `kid`.
    let resolved = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("l"),
        &crate::config::Selection::default().agent(Some("x")),
    )
    .expect("machine l under project --agent x listing project subagent kid must succeed");
    let cell = resolved.layout.agent_cells().next().unwrap();
    assert_eq!(cell.kind.as_str(), "claude");

    // Replay of `l` under recorded `--agent x` must also succeed without degrading.
    let record = crate::agents::LaunchRecord {
        agent: Some("x".to_owned()),
        ..Default::default()
    };
    let replay = crate::harness::resume::resolve_posture(
        crate::harness::resume::PostureRequest {
            record: Some(&record),
            profile: Some("l"),
            kind: &crate::ids::AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &launch.profiles,
        None,
    );
    assert!(
        replay.degraded.is_none(),
        "replay of l under project --agent x must not degrade: {:?}",
        replay.degraded
    );

    // Machine `l_bad` (listing `kid` itself, not inherited from project X) still fails at load.
    assert!(
        launch.definition_failure_for("l_bad").is_some(),
        "machine definition writing project-only subagent itself must still fail"
    );

    // Building `l` under machine `--agent m` after project `x` must remain isolated.
    let resolved_m = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Agents,
        &machine.agents.commands,
        Some("l"),
        &crate::config::Selection::default().agent(Some("m")),
    )
    .expect("machine l under machine --agent m must succeed");
    assert_eq!(
        resolved_m
            .layout
            .agent_cells()
            .next()
            .unwrap()
            .kind
            .as_str(),
        "claude"
    );
}

#[test]
fn project_prompt_pieces_keep_trunk_order_and_repeats_across_direct_override_and_replay() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::write(
        home.path().join("agents/md_base.md"),
        "---\ndescription: MD base.\nagent: claude\n---\nMD_BASE\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/md_kid.md"),
        "---\ndescription: MD kid.\nagent: md_base\n---\nMD_KID\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/md_top.md"),
        "---\ndescription: MD top.\nagent: md_kid\n---\nMD_TOP\n",
    )
    .unwrap();

    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(project.path().join(".rimz/a.md"), "AAA\n").unwrap();
    std::fs::write(project.path().join(".rimz/b.md"), "BBB\n").unwrap();
    std::fs::write(project.path().join(".rimz/c.md"), "CCC\n").unwrap();

    let project_toml = "\
        [profiles.base]\n\
        description = \"Base\"\n\
        agent = \"claude\"\n\
        append-system-prompt-files = [\"a.md\"]\n\
        \n\
        [profiles.kid]\n\
        description = \"Kid\"\n\
        agent = \"base\"\n\
        append-system-prompt-files = [\"b.md\", \"a.md\"]\n\
        \n\
        [profiles.rep]\n\
        description = \"Rep\"\n\
        agent = \"claude\"\n\
        append-system-prompt-files = [\"b.md\", \"a.md\", \"b.md\"]\n\
        \n\
        [profiles.one]\n\
        description = \"One\"\n\
        agent = \"base\"\n\
        append-system-prompt-files = [\"c.md\"]\n\
        \n\
        [profiles.top]\n\
        description = \"Top\"\n\
        agent = \"kid\"\n\
        append-system-prompt-files = [\"c.md\"]\n";
    let (machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    let read_pieces = |sources: &[crate::config::PromptSource]| -> Vec<String> {
        sources
            .iter()
            .map(|src| match src {
                crate::config::PromptSource::Text { text, .. } => text.trim().to_owned(),
                crate::config::PromptSource::File(path) => {
                    std::fs::read_to_string(path).unwrap().trim().to_owned()
                }
            })
            .collect()
    };

    let pieces_for = |spec: &str, ov: Option<&str>| {
        let resolved = crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec),
            &crate::config::Selection::default().agent(ov),
        )
        .unwrap_or_else(|e| panic!("resolve {spec} ov={ov:?}: {e}"));
        let cell = resolved.layout.agent_cells().next().unwrap();
        read_pieces(&cell.append_system_prompt_files)
    };

    assert_eq!(pieces_for("kid", None), vec!["AAA", "BBB", "AAA"]);
    assert_eq!(pieces_for("rep", None), vec!["BBB", "AAA", "BBB"]);
    assert_eq!(
        pieces_for("one", Some("kid")),
        vec!["BBB", "AAA", "AAA", "CCC"]
    );
    assert_eq!(
        pieces_for("base", Some("top")),
        vec!["BBB", "AAA", "CCC", "AAA"]
    );
    assert_eq!(
        pieces_for("md_base", Some("md_top")),
        vec!["MD_BASE", "MD_KID", "MD_TOP"]
    );

    // Replay of `one --agent kid` and `base --agent top` preserves exact order and repeats.
    for (profile, ov, expected) in [
        ("one", "kid", vec!["BBB", "AAA", "AAA", "CCC"]),
        ("base", "top", vec!["BBB", "AAA", "CCC", "AAA"]),
    ] {
        let record = crate::agents::LaunchRecord {
            agent: Some(ov.to_owned()),
            ..Default::default()
        };
        let replay = crate::harness::resume::resolve_posture(
            crate::harness::resume::PostureRequest {
                record: Some(&record),
                profile: Some(profile),
                kind: &crate::ids::AgentKind::new_unchecked("claude"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &launch.profiles,
            None,
        );
        assert!(replay.degraded.is_none(), "{:?}", replay.degraded);
        assert_eq!(
            read_pieces(&replay.launch.append_system_prompt_files),
            expected,
            "replay {profile} --agent {ov}"
        );
    }
}

#[test]
fn advisory_project_subagent_shadow_and_kind_named_cause_chain() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::create_dir_all(home.path().join("subagents")).unwrap();
    // Broken machine subagent (frontmatter problem) shadowed by a broken project subagent.
    std::fs::write(home.path().join("subagents/dup.md"), "invalid frontmatter").unwrap();

    let project_toml = "\
        [subagents.profiles.dup]\n\
        agent = \"claude\"\n\
        \n\
        [profiles.caller_dup]\n\
        description = \"Calls dup\"\n\
        agent = \"claude\"\n\
        subagents = [\"dup\"]\n\
        \n\
        [subagents.profiles.codex]\n\
        description = \"Kind-named subagent\"\n\
        agent = \"codex\"\n\
        \n\
        [profiles.caller_k]\n\
        description = \"Calls codex subagent\"\n\
        agent = \"claude\"\n\
        subagents = [\"codex\"]\n";
    let (_machine, launch) = load_trusted_project(home.path(), project.path(), "", project_toml);

    let detail_dup = launch
        .definition_failure_for("caller_dup")
        .expect("caller_dup must fail");
    assert!(
        detail_dup.contains("description") && !detail_dup.contains("frontmatter"),
        "caller_dup must trace to project dup's error, not machine dup's problem: {detail_dup}"
    );

    let detail_k = launch
        .definition_failure_for("caller_k")
        .expect("caller_k must fail");
    assert!(
        detail_k.contains("caused by:") && detail_k.contains("[subagents.profiles.codex]"),
        "caller_k must keep caused-by chain to kind-named failed subagent row: {detail_k}"
    );
}

/// A machine definition whose `subagents:` list allows a subagent that fails to load in a trusted
/// project fails in that project, whatever the list's form, and every gate that reads the failure
/// record refuses it; the same rows outside the project, and rows that do not allow the name,
/// launch as before.
#[test]
fn machine_definition_allowing_a_failed_project_subagent_fails_in_that_project() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    let untrusted = tempdir().unwrap();
    let outside = tempdir().unwrap();
    seed_definition_home(home.path());
    std::fs::create_dir_all(home.path().join("teams")).unwrap();
    for (file, fields) in [
        ("subagents/shd.md", "agent: claude"),
        ("subagents/good.md", "agent: claude"),
        ("subagents/ok.md", "agent: claude"),
        ("agents/m1.md", "agent: claude\nsubagents: [shd]"),
        ("agents/m2.md", "agent: claude\nsubagents: [+shd]"),
        ("agents/both.md", "agent: claude\nsubagents: [good, shd]"),
        ("agents/plain.md", "agent: claude"),
        ("agents/m_ok.md", "agent: claude\nsubagents: [ok, good]"),
        ("agents/codex.md", "tools: [Read, Skill]\nsubagents: [shd]"),
        ("agents/on_codex.md", "agent: codex"),
        ("agents/codex_own.md", "agent: codex\nsubagents: [good]"),
        ("agents/pi.md", "subagents: [+shd]"),
        ("agents/opencode.md", "subagents: [shd]"),
    ] {
        std::fs::write(
            home.path().join(file),
            format!("---\ndescription: {file}\n{fields}\n---\nBody.\n"),
        )
        .unwrap();
    }
    std::fs::write(
        home.path().join("teams/lay.md"),
        "---\nleader: lead\nstages: [Build]\nlayout: \"lead,m1\"\nroles:\n  - role: lead\n    \
         agent: plain\n    owns: [Build]\n---\nBuild.\n",
    )
    .unwrap();
    for (team, seat) in [("crew", "\n    subagents: [shd]"), ("calm", "")] {
        std::fs::write(
            home.path().join(format!("teams/{team}.md")),
            format!(
                "---\nleader: lead\nstages: [Build]\nroles:\n  - role: lead\n    agent: plain\n    \
                 owns: [Build]{seat}\n---\nBuild.\n"
            ),
        )
        .unwrap();
    }
    let project_toml = "\
        [profiles.x]\n\
        description = \"X\"\n\
        agent = \"claude\"\n\
        subagents = [\"kid\"]\n\
        [profiles.y]\n\
        description = \"Y\"\n\
        agent = \"claude\"\n\
        [subagents.profiles.kid]\n\
        description = \"K\"\n\
        agent = \"claude\"\n\
        [subagents.profiles.ok]\n\
        description = \"Project ok\"\n\
        agent = \"claude\"\n\
        [subagents.profiles.shd]\n\
        agent = \"claude\"\n";
    // Every tier runs only codex, whose base lists `shd`, so a walk has no other model.
    let machine_toml = "[tiers]\nintern = [\"gpt-5.4\"]\njunior = [\"gpt-5.1-codex\"]\n\
                        senior = [\"gpt-5.5\"]\nprincipal = [\"gpt-5\"]\n";
    let (machine, launch) =
        load_trusted_project(home.path(), project.path(), machine_toml, project_toml);
    let commands = &machine.agents.commands;
    let resolve = |launch: &LaunchAgents, spec: &str, ov: Option<&str>| {
        let selection = crate::config::Selection::new(&machine.tiers).agent(ov);
        crate::harness::plan::resolve_launch(
            launch,
            ProfileScope::Agents,
            commands,
            Some(spec),
            &selection,
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    let routed = |spec: &str, tier: Option<super::super::tiers::ModelTier>, model: Option<&str>| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            tier,
            model,
            None,
            |_, _| None,
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    let cause = format!(
        "\ncaused by: {}: [subagents.profiles.shd]: has no non-empty `description:`",
        project.path().join(".rimz/config.toml").display()
    );
    let refusal = |file: &str| {
        format!(
            "{}: allows subagent 'shd', which failed to load{cause}",
            home.path().join(file).display()
        )
    };
    let refused = |launch: &LaunchAgents, spec: &str, ov: Option<&str>, file: &str| {
        resolve(launch, spec, ov)
            .err()
            .filter(|text| text.contains(&refusal(file)))
            .map(|_| ())
            .ok_or_else(|| format!("{spec} --agent {ov:?}: {:?}", resolve(launch, spec, ov)))
    };
    let cases = [
        ("m1", None, "agents/m1.md"),
        ("m2", None, "agents/m2.md"),
        ("m1", Some("x"), "agents/m1.md"),
        ("m2", Some("x"), "agents/m2.md"),
        ("y", Some("m1"), "agents/m1.md"),
        ("both", None, "agents/both.md"),
        ("codex", None, "agents/codex.md"),
        ("on_codex", None, "agents/on_codex.md"),
        ("crew", None, "teams/crew.md"),
        ("pi", None, "agents/pi.md"),
        ("opencode-mini", None, "agents/opencode.md"),
        ("plain", Some("opencode-mini"), "agents/opencode.md"),
    ];
    assert_eq!(
        cases
            .iter()
            .map(|(spec, ov, file)| refused(&launch, spec, *ov, file))
            .collect::<Vec<_>>(),
        vec![Ok(()); cases.len()],
    );
    let launches = [
        ("plain", None),
        ("plain", Some("x")),
        ("m_ok", None),
        ("codex_own", None),
        ("calm", None),
        ("x", None),
    ];
    assert_eq!(
        launches
            .iter()
            .map(|(spec, ov)| resolve(&launch, spec, *ov))
            .collect::<Vec<_>>(),
        vec![Ok(()); launches.len()],
    );
    let intern = Some(super::super::tiers::ModelTier::Intern);
    let codex_model = Some("gpt-5.1-codex");
    let routes = [
        ("plain", None, codex_model, "agents/plain.md"),
        ("plain", intern, None, "agents/plain.md"),
        ("claude", None, codex_model, "agents/codex.md"),
        ("claude", intern, None, "agents/codex.md"),
    ];
    assert_eq!(
        routes
            .iter()
            .map(|(spec, tier, model, file)| {
                routed(spec, *tier, *model)
                    .err()
                    .filter(|text| text.contains(&refusal(file)))
                    .map(drop)
                    .ok_or_else(|| {
                        format!(
                            "{spec} {tier:?} {model:?}: {:?}",
                            routed(spec, *tier, *model)
                        )
                    })
            })
            .collect::<Vec<_>>(),
        vec![Ok(()); routes.len()],
    );
    assert_eq!(routed("codex_own", None, codex_model), Ok(()));
    let shd = crate::harness::plan::resolve_launch(
        &launch,
        ProfileScope::Subagents,
        commands,
        Some("shd"),
        &crate::config::Selection::new(&machine.tiers),
    )
    .map(drop)
    .map_err(|err| err.to_string());
    assert!(
        shd.as_ref()
            .is_err_and(|text| text.contains("[subagents.profiles.shd]")),
        "{shd:?}"
    );
    assert_eq!(
        launch.project_definition_errors().len(),
        1,
        "{:?}",
        launch.project_definition_errors()
    );
    // A machine team with a failed seat leaves the team table, as a project team does, so every
    // gate that takes a team name refuses it through its failure record.
    // So does one whose layout cell rests on a failed row, the built-in `peer` (`claude,codex`)
    // over the failed `codex` base among them.
    assert_eq!(
        ["crew", "calm", "lay", "peer"].map(|team| (
            launch.teams.0.contains_key(team),
            launch.definition_failure_for(team),
        )),
        [
            (false, Some(refusal("teams/crew.md"))),
            (true, None),
            (false, Some(refusal("agents/m1.md"))),
            (false, Some(refusal("agents/codex.md"))),
        ],
    );

    // Fork, restart, child resume, and the exec wrapper all refuse through `block_failed_launch`.
    let claude = crate::ids::AgentKind::new_unchecked("claude");
    for (profile, agent) in [
        (Some("m1"), None),
        (Some("y"), Some("m1")),
        (Some("m2"), None),
        (Some("opencode-mini"), None),
        (Some("plain"), Some("opencode-mini")),
    ] {
        let record = crate::agents::LaunchRecord {
            agent: agent.map(str::to_owned),
            ..Default::default()
        };
        let gate = launch
            .block_failed_launch(&claude, profile, Some(&record), ProfileScope::Agents)
            .map_err(|err| err.to_string());
        assert!(
            gate.as_ref().is_err_and(|text| text.contains(&cause)),
            "{profile:?} --agent {agent:?}: {gate:?}"
        );
    }

    std::fs::create_dir_all(untrusted.path().join(".rimz")).unwrap();
    std::fs::write(untrusted.path().join(".rimz/config.toml"), project_toml).unwrap();
    for root in [untrusted.path(), outside.path()] {
        let elsewhere = load_with_roots(&machine, root, home.path()).unwrap();
        assert!(elsewhere.teams.0.contains_key("crew"), "{}", root.display());
        assert_eq!(
            ["m1", "m2", "both", "codex", "on_codex", "crew"]
                .map(|spec| resolve(&elsewhere, spec, None)),
            [const { Ok(()) }; 6],
            "{}",
            root.display()
        );
    }
}

/// The machine-definition fixture of the tests below: `files` written into `home` beside the
/// seeded kind bases, loaded with `project_toml` trusted and `tiers` the intern row, the only one.
fn load_definitions_in_project(
    home: &std::path::Path,
    project: &std::path::Path,
    files: &[(&str, &str)],
    tiers: &str,
    project_toml: &str,
) -> (MachineConfig, LaunchAgents) {
    seed_definition_home(home);
    for (file, fields) in files {
        std::fs::write(
            home.join(file),
            format!("---\ndescription: {file}\n{fields}\n---\nBody.\n"),
        )
        .unwrap();
    }
    let machine_toml =
        format!("[tiers]\nintern = [{tiers}]\njunior = []\nsenior = []\nprincipal = []\n");
    load_trusted_project(home, project, &machine_toml, project_toml)
}

/// A launch under `--tier` when `tier` is set, reduced to its refusal's first line.
fn launch_first_line(
    machine: &MachineConfig,
    launch: &LaunchAgents,
    scope: ProfileScope,
    spec: &str,
    tier: Option<super::super::tiers::ModelTier>,
) -> std::result::Result<(), String> {
    let selection = crate::config::Selection::new(&machine.tiers).tier(tier);
    crate::harness::plan::resolve_launch(
        launch,
        scope,
        &machine.agents.commands,
        Some(spec),
        &selection,
    )
    .map(drop)
    .map_err(|err| {
        err.to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    })
}

/// A launch reports only the failures of the tables its scope resolves from, tiered or not: a
/// healthy project subagent launches under `--subagent-profile` beside a same-named machine agent
/// that fails in the project, and an agents launch names its failed agents file over a failed
/// project subagent of the same name. A name only the other table holds refuses with that table's
/// failure, its project row over the machine file it shadows. A spec the launch resolves (a
/// `-fast` name, a fast cell, a permission-mode cell) launches beside a failed subagent row that
/// another reading of the spec names. A healthy row in the other table shadows its broken
/// machine file there, so a launch of that name, or an `--agent` of it, gets the wrong-doorway
/// refusal rather than that file's error.
#[test]
fn a_launch_reads_failures_only_from_the_tables_its_scope_resolves_from() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    let (machine, launch) = load_definitions_in_project(
        home.path(),
        project.path(),
        &[
            ("subagents/shd.md", "agent: claude"),
            ("agents/bothns.md", "agent: claude\nsubagents: [shd]"),
            ("agents/twin.md", "agent: nosuch"),
            ("agents/only_agent.md", "agent: claude\nsubagents: [shd]"),
            ("subagents/dup.md", "agent: nosuch"),
            ("agents/fa-fast.md", "agent: claude"),
            ("agents/fb.md", "agent: claude"),
            ("subagents/sx.md", "agent: nosuch"),
            ("agents/ax.md", "agent: nosuch"),
            ("subagents/sy.md", "agent: nosuch"),
        ],
        "\"opus\"",
        "\
        [subagents.profiles.shd]\n\
        agent = \"claude\"\n\
        [subagents.profiles.twin]\n\
        agent = \"claude\"\n\
        [subagents.profiles.bothns]\n\
        description = \"Both\"\n\
        agent = \"claude\"\n\
        [subagents.profiles.only_sub]\n\
        agent = \"claude\"\n\
        [subagents.profiles.dup]\n\
        agent = \"claude\"\n\
        [subagents.profiles.fa]\n\
        agent = \"claude\"\n\
        [subagents.profiles.fb-fast]\n\
        agent = \"claude\"\n\
        [subagents.profiles.claude-yolo]\n\
        agent = \"claude\"\n\
        [profiles.sx]\n\
        description = \"Agent sx\"\n\
        agent = \"claude\"\n\
        [profiles.ax]\n\
        description = \"Agent ax\"\n\
        agent = \"claude\"\n\
        [subagents.profiles.sy]\n\
        description = \"Subagent sy\"\n\
        agent = \"claude\"\n",
    );
    let intern = Some(super::super::tiers::ModelTier::Intern);
    let agents_file = |name: &str| {
        home.path()
            .join(format!("agents/{name}.md"))
            .display()
            .to_string()
    };
    let twin = format!("{}: follows unknown profile 'nosuch'", agents_file("twin"));
    let bothns = format!(
        "{}: allows subagent 'shd', which failed to load",
        agents_file("bothns")
    );
    let only_agent = format!(
        "{}: allows subagent 'shd', which failed to load",
        agents_file("only_agent")
    );
    let config = project.path().join(".rimz/config.toml");
    let row = |name: &str| {
        Err(format!(
            "{}: [subagents.profiles.{name}]: has no non-empty `description:`",
            config.display()
        ))
    };
    let into_subagents = "profile `ax` is defined under [agents.profiles], but `rimz subagents` \
         resolves [subagents.profiles]; move or copy the profile to the expected section";
    let into_agents = "profile `sy` is defined under [subagents.profiles], but `rimz agents` \
         resolves [agents.profiles]; launch it with `rimz agents sy --subagent-profile`, or move \
         or copy the profile to the expected section";
    let cases = [
        (ProfileScope::Subagents, "bothns", Ok(())),
        (ProfileScope::Agents, "twin", Err(twin)),
        (ProfileScope::Agents, "bothns", Err(bothns)),
        (ProfileScope::Agents, "only_sub", row("only_sub")),
        (ProfileScope::Agents, "fb,only_sub", row("only_sub")),
        (ProfileScope::Subagents, "only_agent", Err(only_agent)),
        // The other table's failure is its project row, which shadows the machine file.
        (ProfileScope::Agents, "dup", row("dup")),
        // Each spec resolves, beside a failed subagent row named by another reading of it.
        (ProfileScope::Agents, "fa-fast", Ok(())),
        (ProfileScope::Agents, "fb-fast", Ok(())),
        (ProfileScope::Agents, "claude-yolo", Ok(())),
        (ProfileScope::Agents, "sx", Ok(())),
        // A healthy row in the other table shadows its broken machine file, so the launch's own
        // wrong-doorway refusal answers.
        (
            ProfileScope::Subagents,
            "ax",
            Err(into_subagents.to_owned()),
        ),
        (ProfileScope::Agents, "sy", Err(into_agents.to_owned())),
    ];
    let (mut outcomes, mut expected) = (Vec::new(), Vec::new());
    for tier in [None, intern] {
        for (scope, spec, outcome) in &cases {
            let first_line = launch_first_line(&machine, &launch, *scope, spec, tier);
            outcomes.push((*spec, tier.is_some(), first_line));
            expected.push((*spec, tier.is_some(), outcome.clone()));
        }
    }
    // An agents launch's `--agent` reads the subagents table only for its failure.
    let with_agent = |tier| {
        let selection = crate::config::Selection::new(&machine.tiers)
            .agent(Some("sy"))
            .tier(tier);
        crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some("claude"),
            &selection,
        )
        .map(drop)
        .map_err(|err| {
            err.to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    };
    outcomes.push(("claude --agent sy", false, with_agent(None)));
    outcomes.push(("claude --agent sy", true, with_agent(intern)));
    expected.push(("claude --agent sy", false, Err(into_agents.to_owned())));
    expected.push(("claude --agent sy", true, Err(into_agents.to_owned())));
    let ax_hint = crate::harness::plan::check_subagent_profile_spec(
        &launch,
        &machine.agents.commands,
        Some("ax"),
    )
    .map_err(|err| {
        err.to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    });
    outcomes.push(("check ax", false, ax_hint));
    expected.push((
        "check ax",
        false,
        Err(
            "--subagent-profile takes one profile name defined under [subagents.profiles], and \
             `ax` is defined only under [agents.profiles]; drop --subagent-profile to launch it"
                .to_owned(),
        ),
    ));
    assert_eq!(outcomes, expected);
    // `--subagent-profile` names the failed subagent file over the healthy agents row.
    assert_eq!(
        crate::harness::plan::check_subagent_profile_spec(
            &launch,
            &machine.agents.commands,
            Some("sx"),
        )
        .map_err(|err| err
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()),
        Err(format!(
            "{}: follows unknown profile 'nosuch'",
            home.path().join("subagents/sx.md").display()
        )),
    );
}

/// A `subagents:` entry spelled as a kind alias is checked as the kind it names: with a failed
/// project subagent `antigravity`, a machine agent allowing `agy` fails as one allowing
/// `antigravity` does, plain or `+`, quoting the entry as written.
#[test]
fn a_subagents_alias_entry_fails_with_the_failed_row_of_its_kind() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    let (machine, launch) = load_definitions_in_project(
        home.path(),
        project.path(),
        &[
            ("agents/agy_plain.md", "agent: claude\nsubagents: [agy]"),
            ("agents/agy_plus.md", "agent: claude\nsubagents: [+agy]"),
            (
                "agents/kind_plain.md",
                "agent: claude\nsubagents: [antigravity]",
            ),
            (
                "agents/kind_plus.md",
                "agent: claude\nsubagents: [+antigravity]",
            ),
        ],
        "\"opus\"",
        "[subagents.profiles.antigravity]\ndescription = \"Agy\"\nagent = \"claude\"\n",
    );
    let cases = [
        ("agy_plain", "agy"),
        ("agy_plus", "agy"),
        ("kind_plain", "antigravity"),
        ("kind_plus", "antigravity"),
    ];
    let resolve = |spec: &str| {
        crate::harness::plan::resolve_launch(
            &launch,
            ProfileScope::Agents,
            &machine.agents.commands,
            Some(spec),
            &crate::config::Selection::new(&machine.tiers),
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    assert_eq!(
        cases.map(|(spec, _)| resolve(spec)),
        cases.map(|(spec, entry)| Err(format!(
            "{}: allows subagent '{entry}', which failed to load\ncaused by: {}: \
             [subagents.profiles.antigravity]: takes the name of an agent runtime; rename it",
            home.path().join(format!("agents/{spec}.md")).display(),
            project.path().join(".rimz/config.toml").display(),
        ))),
    );
}

/// A failed tier walk lists the reasons of the kinds it tried and no other, under `--tier` and in a
/// `tier:` definition's load: on a codex-only tier table, the engine kinds that no tier row
/// reaches stay out of the error, and a walk that reaches one, through `--agent`, lists it.
#[test]
fn a_failed_tier_walk_lists_only_the_kinds_it_tried() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();
    let (machine, launch) = load_definitions_in_project(
        home.path(),
        project.path(),
        &[
            ("subagents/shd.md", "agent: claude"),
            ("agents/codex.md", "tools: [Read, Skill]\nsubagents: [shd]"),
            ("agents/plain.md", "agent: claude"),
            ("subagents/broken.md", "agent: nosuch"),
            // A broken base, since a missing one falls back to the built-in kind base.
            ("agents/opencode.md", "[broken"),
            (
                "agents/tiered.md",
                "agent: codex\ntier: intern\ntools: []\nsubagents: [broken]",
            ),
        ],
        "\"gpt-5.4\"",
        "[subagents.profiles.shd]\nagent = \"claude\"\n",
    );
    let intern = Some(super::super::tiers::ModelTier::Intern);
    let walk = |spec: &str, family: Option<&str>| {
        launch_under(
            &launch,
            &machine.tiers,
            ProfileScope::Agents,
            spec,
            intern,
            None,
            family,
            |_, _| None,
        )
        .map(drop)
        .map_err(|err| err.to_string())
    };
    let tried = walk("plain", None).unwrap_err();
    assert!(
        tried.starts_with(&format!(
            "no eligible model at tier intern or above; codex: {}: allows subagent 'shd'",
            home.path().join("agents/plain.md").display()
        )) && !tried.contains("; opencode: ")
            && !tried.contains("; pi: "),
        "{tried}"
    );
    let base = home.path().join("agents/opencode.md");
    let failed_base = format!("whose kind base {} failed to load", base.display());
    let engine = walk("plain", Some("opencode")).unwrap_err();
    assert!(
        engine.contains(&format!("; opencode: runs on opencode, {failed_base}")),
        "{engine}"
    );
    // The `claude` base row holds renders and no definition, so its walk reads the renders'
    // exclusions.
    assert!(launch.profiles.0["claude"].definition.is_none());
    let renders = walk("claude", Some("opencode")).unwrap_err();
    assert!(
        renders.contains(&format!("; opencode: {failed_base}")) && !renders.contains("codex: "),
        "{renders}"
    );
    // A `tier:` definition's load walk lists only the kinds it tried, too.
    let tiered =
        launch_first_line(&machine, &launch, ProfileScope::Agents, "tiered", None).unwrap_err();
    assert!(
        tiered.starts_with(&format!(
            "{}: no eligible model at tier intern or above; codex: ",
            home.path().join("agents/tiered.md").display()
        )) && !tiered.contains("; opencode: ")
            && !tiered.contains("; pi: "),
        "{tiered}"
    );
}
