use super::*;

fn compile_seat(
    project: &Path,
    request: &ExecRequest,
    machine: &crate::config::MachineConfig,
    effective: Option<&LaunchAgents>,
) -> LaunchPlan {
    let id = crate::WorkspaceId::from_project_root(project);
    let runtime = RuntimePaths::under(id.clone(), project).unwrap();
    let state = StatePaths::under(id, project).unwrap();
    compile(LaunchPlanInputs {
        request,
        cwd: project,
        project_root: project,
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        machine,
        effective,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Host,
        bwrap: None,
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
        hibernate: false,
        recorded_system_prompt_digest: None,
    })
    .unwrap()
}

fn settings(plan: &LaunchPlan) -> Option<serde_json::Value> {
    let process = plan.process();
    let values: Vec<_> = process
        .provider_argv
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .collect();
    assert!(values.len() <= 1, "{:?}", process.provider_argv);
    values.first().map(|pair| {
        if let Some((path, value, _)) = &process.settings_artifact {
            assert_eq!(Path::new(&pair[1]), path);
            value.clone()
        } else {
            serde_json::from_str(&pair[1]).unwrap()
        }
    })
}

fn native_idle_compaction(plan: &LaunchPlan) -> Option<bool> {
    settings(plan).and_then(|value| value["idleCompaction"].as_bool())
}

#[test]
fn native_idle_compaction_follows_the_resolved_seat_floor() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    machine.agents.profiles = toml::from_str(
        "[warm]\nagent = 'claude'\nidle-compact = '80k'\n\
         [inherited]\nagent = 'warm'\n\
         [cold]\nagent = 'inherited'\nidle-compact = 'off'",
    )
    .unwrap();
    machine.agents.teams = toml::from_str(
        "[forge]\nroles = [\
         {role = 'warm', profile = 'cold', idle-compact = '40k'},\
         {role = 'cold', profile = 'warm', idle-compact = 'off'},\
         {role = 'inherited', profile = 'inherited'}]",
    )
    .unwrap();
    let cases = [
        ("default", "100k", None, None, (false, false), false, true),
        (
            "harness off",
            "off",
            None,
            None,
            (false, false),
            false,
            false,
        ),
        (
            "profile",
            "off",
            Some("warm"),
            None,
            (false, false),
            false,
            true,
        ),
        (
            "profile chain",
            "off",
            Some("inherited"),
            None,
            (false, false),
            false,
            true,
        ),
        (
            "profile off",
            "100k",
            Some("cold"),
            None,
            (false, false),
            false,
            false,
        ),
        (
            "role",
            "off",
            Some("cold"),
            Some("warm"),
            (false, false),
            false,
            true,
        ),
        (
            "role off",
            "100k",
            Some("warm"),
            Some("cold"),
            (false, false),
            false,
            false,
        ),
        (
            "role profile",
            "off",
            Some("inherited"),
            Some("inherited"),
            (false, false),
            false,
            true,
        ),
        (
            "child",
            "100k",
            Some("warm"),
            Some("warm"),
            (true, true),
            false,
            false,
        ),
        (
            "peer",
            "100k",
            Some("warm"),
            Some("warm"),
            (false, true),
            false,
            true,
        ),
        (
            "parent only",
            "100k",
            None,
            None,
            (true, false),
            false,
            true,
        ),
        ("headless", "100k", None, None, (false, false), true, false),
    ];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (label, floor, profile, role, (parent, depth), headless, covered) in cases {
        machine.harness.idle_compact = floor.parse().unwrap();
        let effective =
            crate::config::effective::load_with_roots(&machine, project.path(), config.path())
                .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params.profile = profile.map(str::to_owned);
        request.identity.params.team = role.map(|_| "forge".to_owned());
        request.identity.params.role = role.map(str::to_owned);
        request.identity.params.parent_agent_id = parent.then(|| "parent".into());
        request.identity.params.launch_depth = depth.then_some(1);
        request.subagent = parent && depth;
        request.headless = headless.then(|| crate::agents::HeadlessRequest {
            schema_open: false,
            schema: crate::harness::headless::judge::LOOP_ANSWER_SCHEMA.into(),
            schema_file: "/check/schema.json".into(),
            verdict_file: "/check/verdict.json".into(),
        });
        for effective in [None, Some(&effective)] {
            let plan = compile_seat(project.path(), &request, &machine, effective);
            actual.push((label, native_idle_compaction(&plan)));
            expected.push((label, covered.then_some(false)));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn native_idle_compaction_requires_every_configured_lifetime_to_exceed_the_margin() {
    let project = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let cases = [
        ("", true),
        ("cache_keepalive = false", true),
        ("[prompt_cache_ttl]\nclaude = 'off'", false),
        ("[prompt_cache_ttl]\nclaude = '30s'", false),
        ("[prompt_cache_ttl]\nclaude = '1m'", false),
        ("[prompt_cache_ttl]\nclaude = '61s'", true),
        (
            "[prompt_cache_ttl]\nclaude = {subscription = '1h', api-key = '5m'}",
            true,
        ),
        (
            "[prompt_cache_ttl]\nclaude = {subscription = 'off', api-key = '5m'}",
            false,
        ),
        (
            "[prompt_cache_ttl]\nclaude = {subscription = '1h', api-key = 'off'}",
            false,
        ),
        (
            "[prompt_cache_ttl]\nclaude = {subscription = '1m', api-key = '5m'}",
            false,
        ),
        (
            "[prompt_cache_ttl]\nclaude = {subscription = '1h', api-key = '30s'}",
            false,
        ),
        (
            "prompt_cache_margin = '70m'\n[prompt_cache_ttl]\nclaude = {subscription = '2h', api-key = '2h'}",
            false,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(raw, _)| {
            machine.harness = toml::from_str(raw).unwrap();
            native_idle_compaction(&compile_seat(project.path(), &request, &machine, None))
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, covered)| covered.then_some(false))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn native_idle_compaction_uses_effective_tables_and_leaves_other_kinds_unchanged() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), config.path()).unwrap();
    effective.profiles = toml::from_str("[cold]\nagent = 'claude'\nidle-compact = 'off'").unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.profile = Some("cold".into());
    let actual = [None, Some(&effective)].map(|effective| {
        native_idle_compaction(&compile_seat(project.path(), &request, &machine, effective))
    });
    assert_eq!(actual, [Some(false), None]);
    let mut disabled = crate::config::MachineConfig::default();
    disabled.harness.idle_compact = crate::config::IdleCompact::Off;
    for kind in ["codex", "pi"] {
        request.kind = AgentKind::new_unchecked(kind);
        let covered = compile_seat(project.path(), &request, &machine, None);
        let uncovered = compile_seat(project.path(), &request, &disabled, None);
        assert_eq!(
            covered.process().provider_argv,
            uncovered.process().provider_argv
        );
    }
}

#[test]
fn native_idle_compaction_preserves_inline_and_file_settings_on_every_launch_action() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for action in ["launch", "resume", "fork"] {
        for explicit in [None, Some(true), Some(false)] {
            let mut original = serde_json::json!({"theme": "dark"});
            if let Some(value) = explicit {
                original["idleCompaction"] = value.into();
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
                let request = request("claude", action_with_args(action, args));
                let plan = compile_seat(project.path(), &request, &machine, None);
                let merged = settings(&plan).unwrap();
                assert_eq!(merged["theme"], "dark");
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    original.to_string()
                );
                actual.push((action, explicit, file, merged["idleCompaction"].as_bool()));
                expected.push((action, explicit, file, Some(explicit.unwrap_or(false))));
            }
        }
    }
    assert_eq!(actual, expected);
}
