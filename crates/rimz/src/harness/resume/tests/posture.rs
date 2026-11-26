//! Posture replay: the profile-declared settings a resumed session comes back
//! with, and how a broken profile degrades instead of stranding the session.

use super::*;

#[test]
fn mini_relaunch_replays_each_name_source_and_current_definition() {
    let profiles = project_profiles(
        "[profiles.worker]\ndescription = 'Mini worker'\nagent = 'opencode-mini'\n\
         [profiles.plain]\ndescription = 'Plain opencode'\nagent = 'opencode'\n",
    );
    let actual = [
        (Some("worker"), None, true),
        (Some("opencode-mini"), None, true),
        (Some("plain"), Some("opencode-mini"), true),
        (Some("worker"), Some("opencode"), false),
    ]
    .map(|(name, base, mini)| {
        let record = crate::agents::LaunchRecord {
            agent: base.map(str::to_owned),
            ..Default::default()
        };
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile: name,
                kind: &AgentKind::new_unchecked("opencode"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        (
            posture.degraded.is_none(),
            posture.launch.args.first().map(String::as_str) == Some("mini"),
            mini,
        )
    });
    assert_eq!(
        actual,
        [
            (true, true, true),
            (true, true, true),
            (true, true, true),
            (true, false, false)
        ]
    );
    let mut profiles =
        project_profiles("[profiles.worker]\ndescription = 'Plain worker'\nagent = 'opencode'\n");
    let posture = posture_for("opencode", Some("worker"), None, &profiles);
    assert!(posture.degraded.is_none());
    assert!(!posture.launch.args.iter().any(|arg| arg == "mini"));
    profiles.0.remove("worker");
    let posture = posture_for("opencode", Some("worker"), None, &profiles);
    assert!(posture.degraded.is_some());
    assert!(!posture.launch.args.iter().any(|arg| arg == "mini"));
}

#[test]
fn null_mode_resumes_default_with_or_without_records_profiles_and_degradation() {
    for (kind, expected) in [
        ("claude", PermissionMode::Auto),
        ("codex", PermissionMode::Auto),
        ("opencode", PermissionMode::Auto),
        ("pi", PermissionMode::Yolo),
    ] {
        for recorded in [false, true] {
            for (name, configured_kind, degraded) in [
                (None, kind, false),
                (Some("worker"), kind, false),
                (Some("gone"), kind, true),
                (Some("worker"), "qwen", true),
            ] {
                let record = crate::agents::LaunchRecord::default();
                let posture = resolve_posture(
                    PostureRequest {
                        record: recorded.then_some(&record),
                        profile: name,
                        kind: &AgentKind::new_unchecked(kind),
                        stamped_mode: None,
                        stamped_tier: None,
                    },
                    &profiles("worker", profile(configured_kind)),
                    None,
                );
                assert_eq!(
                    posture.degraded.is_some(),
                    degraded,
                    "{kind} {name:?} {recorded}"
                );
                assert_eq!(
                    posture.launch.mode,
                    Some(expected),
                    "{kind} {name:?} {recorded}"
                );
                assert_eq!(
                    posture.launch.args,
                    crate::agents::spec_by_kind(kind)
                        .unwrap()
                        .launch
                        .permission_args(expected)
                );
            }
        }
    }
}

#[test]
fn replayed_modes_map_and_raw_profile_flags_make_the_replay_unknown() {
    for (kind, expected) in [
        ("opencode", PermissionMode::Auto),
        ("pi", PermissionMode::Yolo),
    ] {
        let posture = posture_for(kind, None, Some(PermissionMode::Ask), &no_profiles());
        assert_eq!(posture.launch.mode, Some(expected));
        assert!(posture.launch.args.is_empty());
    }
    for stamped in [None, Some(PermissionMode::Auto)] {
        let mut worker = profile("codex");
        worker.args = Some("-c approvals_reviewer=user".into());
        let posture = posture_for(
            "codex",
            Some("worker"),
            stamped,
            &profiles("worker", worker),
        );
        assert_eq!(posture.launch.mode, None, "{stamped:?}");
        let mut expected = vec!["-c".to_owned(), "approvals_reviewer=user".to_owned()];
        if let Some(mode) = stamped {
            expected.extend(
                crate::agents::spec_by_kind("codex")
                    .unwrap()
                    .launch
                    .permission_args(mode),
            );
        }
        assert_eq!(posture.launch.args, expected);
    }
}

#[test]
fn recorded_agent_override_resolves_defaults_for_its_kind() {
    let mut worker = profile("claude");
    worker.kind_defaults = true;
    let profiles = profiles("worker", worker);
    let mut actual = Vec::new();
    for (kind, base) in [("claude", None), ("qwen", Some("qwen"))] {
        let record = crate::agents::LaunchRecord {
            agent: base.map(str::to_owned),
            ..Default::default()
        };
        let posture = resolve_posture(
            PostureRequest {
                record: base.map(|_| &record),
                profile: Some("worker"),
                kind: &AgentKind::new_unchecked(kind),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        actual.push((posture.degraded, posture.launch.mode, posture.launch.effort));
    }
    assert_eq!(
        actual,
        [
            (None, Some(PermissionMode::Auto), Some("xhigh".into())),
            (None, None, None),
        ]
    );
}

#[test]
fn resumed_tools_policy_reaches_the_exec_request() {
    let mut actual = Vec::new();
    for tools in [vec!["Bash"], vec!["Skill", "Read"]] {
        let mut worker = profile("claude");
        worker.tools = Some(
            crate::agents::ToolSet::parse(
                &tools.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            )
            .unwrap(),
        );
        let profiles = profiles("worker", worker);
        let posture = posture_for("claude", Some("worker"), None, &profiles);
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        let request = posture.launch.exec_request(
            AgentKind::new_unchecked("claude"),
            crate::harness::launch::ExecAction::Resume {
                session_id: "session".into(),
                extra_args: Vec::new(),
            },
        );
        let cell = posture
            .launch
            .into_cell(AgentKind::new_unchecked("claude"), Some("worker".into()));
        actual.push((cell.no_skill_tool, request.no_skill_tool));
    }
    assert_eq!(actual, [(true, true), (false, false)]);
    assert!(!bare_posture(None, &AgentKind::new_unchecked("claude")).no_skill_tool);
}

#[test]
fn recorded_posture_replays_verbatim_model_without_observed_switch() {
    let mut profiles = profiles("planner", routed_profile(Some("medium")));
    profiles
        .0
        .get_mut("planner")
        .unwrap()
        .definition_renders
        .as_mut()
        .unwrap()
        .renders
        .insert("claude".into(), profile("claude"));
    let record = crate::agents::LaunchRecord {
        model: Some("opus[1m]".into()),
        effort: Some("high".into()),
        fast: None,
        agent: None,
        auto_compact: None,
        ..Default::default()
    };
    let stamp = tier_stamp("opus");
    for profile in [Some("planner"), None] {
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile,
                kind: &AgentKind::new_unchecked("claude"),
                stamped_mode: None,
                stamped_tier: Some(&stamp),
            },
            &profiles,
            None,
        );
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        assert_eq!(posture.launch.model, record.model);
        assert_eq!(posture.launch.effort, record.effort);
        assert!(posture.launch.tier.is_none());
    }
    let matching = crate::agents::LaunchRecord {
        model: Some(stamp.model.clone()),
        ..record
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&matching),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert_eq!(posture.launch.tier, Some(stamp));
}

#[test]
fn relaunch_renders_code_mode_from_the_current_profile() {
    let resolve = |text: &str, kind: &str, record: Option<&crate::agents::LaunchRecord>| {
        let profile: Profile = toml::from_str(text).unwrap();
        resolve_posture(
            PostureRequest {
                record,
                profile: Some("coder"),
                kind: &AgentKind::new_unchecked(kind),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles("coder", profile),
            None,
        )
    };
    let switch = ["--tools", "+codemode"];
    let record = crate::agents::LaunchRecord::default();
    for record in [None, Some(&record)] {
        let on = resolve("agent = \"pi\"\ncode-mode = true", "pi", record);
        assert!(on.degraded.is_none(), "{:?}", on.degraded);
        assert_eq!(on.launch.code_mode, Some(true));
        let fresh = crate::harness::spec::profile_cell(
            "coder",
            &profiles(
                "coder",
                toml::from_str("agent = \"pi\"\ncode-mode = true").unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(on.launch.args, fresh.args);
        assert!(
            on.launch.args.windows(2).any(|part| part == switch),
            "{:?}",
            on.launch.args
        );

        let off = resolve("agent = \"pi\"\ncode-mode = false", "pi", record);
        // The exclusion joins the final argv at compile, from this replayed state.
        assert_eq!(off.launch.code_mode, Some(false));
        let auto = resolve("agent = \"pi\"\ncode-mode = \"auto\"", "pi", record);
        assert_eq!(auto.launch.code_mode, None);
        assert!(!auto.launch.args.iter().any(|arg| arg.contains("codemode")));

        let claude = resolve("agent = \"claude\"\ncode-mode = true", "claude", record);
        assert_eq!(
            claude.launch.code_mode,
            Some(true),
            "the env rides the posture"
        );
        let claude = resolve("agent = \"claude\"\ncode-mode = \"off\"", "claude", record);
        assert_eq!(
            claude.launch.code_mode,
            Some(false),
            "the env rides the posture"
        );
    }

    // A profile that moved to another kind degrades and loses it with the rest.
    let moved = resolve("agent = \"pi\"\ncode-mode = true", "claude", Some(&record));
    assert!(moved.degraded.is_some());
    assert_eq!(moved.launch.code_mode, None);
}

#[test]
fn recorded_fast_replays_over_the_current_profile() {
    let switch = ["-c", "service_tier=fast", "-c", "features.fast_mode=true"];
    let resolve = |profile: Profile, record: Option<&crate::agents::LaunchRecord>| {
        let posture = resolve_posture(
            PostureRequest {
                record,
                profile: Some("coder"),
                kind: &AgentKind::new_unchecked("codex"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles("coder", profile),
            None,
        );
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        posture.launch
    };
    let record = |fast| crate::agents::LaunchRecord {
        fast,
        ..Default::default()
    };
    let fast_profile: Profile = toml::from_str("agent = \"codex\"\nfast = true").unwrap();
    let off_profile: Profile = toml::from_str("agent = \"codex\"\nfast = false").unwrap();
    let off = ["-c", "service_tier=default"];
    let count = |args: &[String], switch: &[&str]| {
        args.windows(switch.len())
            .filter(|part| part.iter().zip(switch).all(|(arg, want)| arg == want))
            .count()
    };

    // The record wins over the profile, like model, and leaves at most one switch: a
    // `--fast` launch stays on, a `--no-fast` launch stays forced off, and a record
    // without `fast` (auto, or written before off was forced) renders nothing.
    for (profile, recorded, want) in [
        (profile("codex"), Some(true), Some(true)),
        (off_profile.clone(), Some(true), Some(true)),
        (fast_profile.clone(), Some(false), Some(false)),
        (profile("codex"), Some(false), Some(false)),
        (fast_profile.clone(), None, None),
        (off_profile.clone(), None, None),
    ] {
        let posture = resolve(profile.clone(), Some(&record(recorded)));
        let case = format!("{:?} over {:?}: {:?}", recorded, profile.fast, posture.args);
        assert_eq!(posture.fast, want, "{case}");
        assert_eq!(posture.record.as_deref().unwrap().fast, want, "{case}");
        assert_eq!(
            count(&posture.args, &switch),
            usize::from(want == Some(true)),
            "{case}"
        );
        assert_eq!(
            count(&posture.args, &off),
            usize::from(want == Some(false)),
            "{case}"
        );
    }

    // A legacy row without a record follows the profile.
    let posture = resolve(fast_profile, None);
    assert_eq!(posture.fast, Some(true));
    assert_eq!(count(&posture.args, &switch), 1);
    let posture = resolve(off_profile, None);
    assert_eq!(posture.fast, Some(false));
    assert_eq!(count(&posture.args, &off), 1);
}

#[test]
fn pi_recorded_fast_replays_without_argv_or_warnings() {
    let record = crate::agents::LaunchRecord {
        fast: Some(true),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("worker"),
            kind: &AgentKind::new_unchecked("pi"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles("worker", profile("pi")),
        None,
    );
    let replayed = crate::agents::LaunchRecord::replay_or_new(
        posture.launch.record.as_deref(),
        posture.launch.model.as_deref(),
        posture.launch.effort.as_deref(),
        posture.launch.fast,
    );
    assert_eq!(
        (posture.launch.fast, replayed.fast),
        (Some(true), Some(true))
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert!(
        posture.launch.notices.is_empty(),
        "{:?}",
        posture.launch.notices
    );
    assert!(posture.launch.args.is_empty(), "{:?}", posture.launch.args);
}

#[test]
fn resume_posture_reresolves_the_native_auto_compact_count() {
    let record: crate::agents::LaunchRecord = serde_json::from_value(serde_json::json!({
        "model": null, "effort": null, "agent": null, "auto_compact": 372_000
    }))
    .unwrap();
    for kind in ["claude", "antigravity"] {
        let actual = [Some("200k"), None].map(|configured| {
            let profiles = profiles(
                "worker",
                Profile {
                    auto_compact: configured.map(str::to_owned),
                    ..profile(kind)
                },
            );
            let posture = resolve_posture(
                PostureRequest {
                    record: Some(&record),
                    profile: Some("worker"),
                    kind: &AgentKind::new_unchecked(kind),
                    stamped_mode: None,
                    stamped_tier: None,
                },
                &profiles,
                None,
            );
            assert!(posture.degraded.is_none(), "{kind}: {:?}", posture.degraded);
            let cell = posture
                .launch
                .into_cell(AgentKind::new_unchecked(kind), Some("worker".into()));
            (
                cell.auto_compact,
                serde_json::to_value(cell.launch.record.unwrap()).unwrap()["auto_compact"].as_u64(),
            )
        });
        assert_eq!(
            actual,
            [(Some("200000".into()), Some(200_000)), (None, None)],
            "{kind}"
        );
    }
}

#[test]
fn resume_posture_records_auto_compact_without_refinalizing_argv() {
    let record = crate::agents::LaunchRecord {
        auto_compact: Some(200_000),
        ..Default::default()
    };
    for configured in [Some("372k"), None] {
        let profiles = profiles(
            "worker",
            Profile {
                auto_compact: configured.map(str::to_owned),
                args: Some("--model sonnet --model opus".into()),
                ..profile("claude")
            },
        );
        for record in [None, Some(&record)] {
            let posture = resolve_posture(
                PostureRequest {
                    record,
                    profile: Some("worker"),
                    kind: &AgentKind::new_unchecked("claude"),
                    stamped_mode: None,
                    stamped_tier: None,
                },
                &profiles,
                None,
            );
            assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
            let mut expected_args = Vec::new();
            if configured.is_some() {
                expected_args.extend(["--autocompact", "372000"]);
            }
            expected_args.extend(["--model", "sonnet", "--model", "opus"]);
            expected_args.extend(["--permission-mode", "auto"]);
            assert_eq!(posture.launch.args, expected_args);
            assert_eq!(posture.launch.model, None);
            assert_eq!(
                posture.launch.record.as_deref(),
                (record.is_some() || configured.is_some())
                    .then_some(crate::agents::LaunchRecord {
                        auto_compact: configured.map(|_| 372_000),
                        ..Default::default()
                    })
                    .as_ref(),
            );
        }
    }
}

#[test]
fn resume_posture_normalizes_unmapped_flags_before_recording() {
    // Resume requires a registered adapter; Copilot lacks both preset switches.
    let profiles = profiles(
        "worker",
        toml::from_str("agent = 'copilot'\nfast = true\nauto-compact = '272000'").unwrap(),
    );
    let record = crate::agents::LaunchRecord {
        fast: Some(true),
        auto_compact: Some(372_000),
        ..Default::default()
    };
    for record in [None, Some(&record)] {
        let posture = resolve_posture(
            PostureRequest {
                record,
                profile: Some("worker"),
                kind: &AgentKind::new_unchecked("copilot"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        assert_eq!(
            posture.launch.fast, None,
            "{record:?}: {:?}",
            posture.launch
        );
        assert!(posture.launch.args.is_empty(), "{:?}", posture.launch.args);
        let replayed = crate::agents::LaunchRecord::replay_or_new(
            posture.launch.record.as_deref(),
            posture.launch.model.as_deref(),
            posture.launch.effort.as_deref(),
            posture.launch.fast,
        );
        assert_eq!(replayed.fast, None, "{replayed:?}");
        assert_eq!(replayed.auto_compact, None);
    }
}

#[test]
fn off_record_keeps_a_fast_switch_the_profile_writes_in_raw_args() {
    let resolve = |profile: Profile, fast| {
        let record = crate::agents::LaunchRecord {
            fast,
            ..Default::default()
        };
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile: Some("coder"),
                kind: &AgentKind::new_unchecked("amp"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles("coder", profile),
            None,
        );
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        posture.launch
    };
    let count = |args: &[String]| args.iter().filter(|arg| *arg == "--fast").count();

    // A raw `--fast` the profile carries is the user's argv, not RimZ's switch:
    // a record without `fast` leaves it alone, and an on record does not double it.
    let raw: Profile = toml::from_str("agent = \"amp\"\nargs = \"--fast\"").unwrap();
    let posture = resolve(raw.clone(), None);
    assert_eq!(count(&posture.args), 1, "{:?}", posture.args);
    let posture = resolve(raw, Some(true));
    assert_eq!(posture.fast, Some(true));
    assert_eq!(count(&posture.args), 1, "{:?}", posture.args);

    // The profile's `fast = true` field is RimZ's switch, so an off record (a
    // `--no-fast` launch) and an auto one (no `fast` in the record) still drop it.
    let field: Profile = toml::from_str("agent = \"amp\"\nfast = true").unwrap();
    for recorded in [Some(false), None] {
        let posture = resolve(field.clone(), recorded);
        assert_eq!(posture.fast, recorded);
        assert_eq!(count(&posture.args), 0, "{:?}", posture.args);
    }
}

#[test]
fn recorded_model_override_keeps_the_stamped_family_render() {
    let (_root, profiles) = tiered_claude("planner", None);
    let record = crate::agents::LaunchRecord {
        model: Some("gpt-6-sol".into()),
        effort: Some("high".into()),
        fast: None,
        agent: None,
        auto_compact: None,
        ..Default::default()
    };
    let stamp = tier_stamp("astra");
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert!(
        codex_render(&posture.launch.args),
        "{:?}",
        posture.launch.args
    );
    assert_eq!(posture.launch.model, record.model);
    assert!(posture.launch.tier.is_none());
}

#[test]
fn recorded_provider_survives_a_cleared_tier() {
    let (_root, profiles) = tiered_claude("planner", None);
    let record = crate::agents::LaunchRecord {
        model: Some("gpt-6-sol".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert!(
        codex_render(&posture.launch.args),
        "{:?}",
        posture.launch.args
    );
    assert_eq!(posture.launch.model, record.model);
}

/// A record without a model, resolved over `profile` as `writer`.
fn modelless_posture(profile: Profile, effort: Option<&str>) -> ResumeLaunchPosture {
    modelless_posture_in(&profiles("writer", profile), effort)
}

/// A record without a model, resolved as `writer` over the whole table.
fn modelless_posture_in(profiles: &ProfilesConfig, effort: Option<&str>) -> ResumeLaunchPosture {
    let record = crate::agents::LaunchRecord {
        effort: effort.map(str::to_owned),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("writer"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    posture.launch
}

#[test]
fn record_without_model_relaunches_without_the_render_model() {
    let launch = modelless_posture(definition_profile(None, Some("haiku"), None), Some("high"));
    assert_eq!(
        preset_values(&launch.args, crate::agents::PresetField::Model),
        [] as [String; 0]
    );
    assert!(launch.model.is_none());
}

#[test]
fn record_without_model_ignores_a_model_the_profile_gained() {
    for render in [None, Some("haiku")] {
        let launch = modelless_posture(definition_profile(Some("sonnet"), render, None), None);
        assert_eq!(
            preset_values(&launch.args, crate::agents::PresetField::Model),
            [] as [String; 0],
            "render {render:?}"
        );
        assert!(launch.model.is_none());
    }
}

#[test]
fn record_without_model_or_effort_takes_the_kind_default_effort() {
    let launch = modelless_posture(definition_profile(None, Some("fable"), None), None);
    assert_eq!(
        preset_values(&launch.args, crate::agents::PresetField::Effort),
        ["xhigh"]
    );
    assert_eq!(launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(launch.record.unwrap().effort.as_deref(), Some("xhigh"));
}

#[test]
fn record_without_model_keeps_a_raw_profile_model_argument() {
    let launch = modelless_posture(
        definition_profile(None, Some("haiku"), Some("--model sonnet")),
        None,
    );
    assert_eq!(
        preset_values(&launch.args, crate::agents::PresetField::Model),
        ["sonnet"]
    );
}

#[test]
fn record_without_model_ignores_a_model_a_base_layer_gained() {
    for (args, expected) in [
        (None, vec![]),
        (Some("--model opus"), vec!["opus".to_owned()]),
    ] {
        let args_line = args.map_or(String::new(), |a| format!("args = '{a}'\n"));
        let table = project_profiles(&format!(
            "[profiles.base]\ndescription = 'Base'\nagent = 'claude'\nmodel = 'sonnet'\n{args_line}\
             [profiles.writer]\ndescription = 'Writer'\nagent = 'base'\n"
        ));
        let launch = modelless_posture_in(&table, None);
        assert_eq!(
            preset_values(&launch.args, crate::agents::PresetField::Model),
            expected,
            "args {args:?}"
        );
        assert!(launch.model.is_none());
    }
    let mut table = profiles("writer", definition_profile(None, Some("haiku"), None));
    let mut kind_base = profile("claude");
    kind_base.kind_base = true;
    kind_base.model = Some("sonnet".into());
    table.0.insert("claude".into(), kind_base);
    let launch = modelless_posture_in(&table, None);
    assert_eq!(
        preset_values(&launch.args, crate::agents::PresetField::Model),
        Vec::<String>::new()
    );
    assert!(launch.model.is_none());
}

#[test]
fn stamp_without_record_ignores_a_model_the_role_gained() {
    let stamp = tier_stamp("sonnet");
    let table = profiles("writer", definition_profile(None, Some("haiku"), None));
    let role: crate::config::RoleBinding =
        toml::from_str("role = 'coder'\nprofile = 'writer'\nmodel = 'opus'").unwrap();
    let launch = resolve_posture_with_role(
        PostureRequest {
            record: None,
            profile: Some("writer"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &table,
        None,
        Some(&role),
        None,
        &crate::config::Selection::default(),
    )
    .unwrap();
    assert_eq!(launch.model.as_deref(), Some("sonnet"));
    assert_eq!(
        preset_values(&launch.args, crate::agents::PresetField::Model),
        ["sonnet"]
    );
    assert_eq!(
        launch.tier.as_deref().map(|stamp| stamp.model.as_str()),
        Some("sonnet")
    );
}

#[test]
fn record_without_model_ignores_a_model_the_role_gained() {
    let record = crate::agents::LaunchRecord::default();
    let table = profiles("writer", definition_profile(None, None, None));
    for (role, expected) in [
        ("model = 'opus'", vec![]),
        ("model = 'opus'\nargs = '--model sonnet'", vec!["sonnet"]),
    ] {
        let role: crate::config::RoleBinding =
            toml::from_str(&format!("role = 'coder'\nprofile = 'writer'\n{role}")).unwrap();
        let launch = resolve_posture_with_role(
            PostureRequest {
                record: Some(&record),
                profile: Some("writer"),
                kind: &AgentKind::new_unchecked("claude"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &table,
            None,
            Some(&role),
            None,
            &crate::config::Selection::default(),
        )
        .unwrap();
        assert_eq!(
            preset_values(&launch.args, crate::agents::PresetField::Model),
            expected,
            "role {role:?}"
        );
        assert!(launch.model.is_none());
    }
}

#[test]
fn recorded_mode_beats_the_profile_default() {
    let mut planner = profile("claude");
    planner.mode = Some(PermissionMode::Yolo);
    let profiles = profiles("planner", planner);
    let record = crate::agents::LaunchRecord::default();
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: Some(PermissionMode::Ask),
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert_eq!(posture.launch.mode, Some(PermissionMode::Ask));
    assert!(
        !posture
            .launch
            .args
            .contains(&"--dangerously-skip-permissions".into())
    );
}

#[test]
fn absent_record_effort_adopts_kind_default_and_replays_it() {
    let profiles =
        project_profiles("[profiles.worker]\ndescription = 'Worker'\nagent = 'claude'\n");
    let matcher = find_definition("claude")
        .unwrap()
        .spec()
        .launch
        .preset_arg_matcher(crate::agents::PresetField::Effort)
        .unwrap();
    for base in [None, Some("worker")] {
        let mut record = crate::agents::LaunchRecord {
            model: Some("opus".into()),
            agent: base.map(str::to_owned),
            ..Default::default()
        };
        let expected = crate::agents::LaunchRecord {
            effort: Some("xhigh".into()),
            ..record.clone()
        };
        for _ in 0..2 {
            let posture = resolve_posture(
                PostureRequest {
                    record: Some(&record),
                    profile: Some("worker"),
                    kind: &AgentKind::new_unchecked("claude"),
                    stamped_mode: None,
                    stamped_tier: None,
                },
                &profiles,
                None,
            );
            assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
            assert_eq!(posture.launch.effort.as_deref(), Some("xhigh"));
            assert_eq!(posture.launch.model.as_deref(), Some("opus"));
            assert_eq!(posture.launch.record.as_deref(), Some(&expected));
            assert_eq!(
                matcher
                    .occurrences(&posture.launch.args)
                    .into_iter()
                    .map(|arg| arg.value)
                    .collect::<Vec<_>>(),
                ["xhigh"]
            );
            record = *posture.launch.record.unwrap();
        }
    }
}

#[test]
fn absent_record_effort_canonicalizes_declared_and_raw_profile_args() {
    let mut planner = profile("claude");
    planner.effort = Some("medium".into());
    planner.args = Some("--effort low".into());
    let profiles = profiles("planner", planner);
    let record = crate::agents::LaunchRecord {
        model: Some("opus".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    let matcher = find_definition("claude")
        .unwrap()
        .spec()
        .launch
        .preset_arg_matcher(crate::agents::PresetField::Effort)
        .unwrap();
    assert_eq!(
        matcher
            .occurrences(&posture.launch.args)
            .into_iter()
            .map(|arg| arg.value)
            .collect::<Vec<_>>(),
        ["medium"]
    );
    assert_eq!(posture.launch.effort.as_deref(), Some("medium"));
    assert_eq!(
        posture.launch.record.unwrap().effort.as_deref(),
        Some("medium")
    );
}

#[test]
fn absent_record_effort_preserves_raw_profile_args() {
    let mut planner = profile("claude");
    planner.args = Some("--effort high".into());
    let profiles = profiles("planner", planner);
    let record = crate::agents::LaunchRecord {
        model: Some("opus".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert!(posture.launch.effort.is_none());
    assert!(posture.launch.record.as_ref().unwrap().effort.is_none());
    assert!(
        posture
            .launch
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
}

#[test]
fn recorded_base_replays_and_missing_base_degrades() {
    let mut profiles = project_profiles(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
         [profiles.reviewer]\ndescription = 'Reviewer'\nagent = 'claude'\n\
         model = 'sonnet'\neffort = 'high'\nargs = '--verbose'\n",
    );
    let record = crate::agents::LaunchRecord {
        model: Some("opus[1m]".into()),
        effort: Some("medium".into()),
        fast: None,
        agent: Some("reviewer".into()),
        auto_compact: None,
        notes: Some("20261107/002336Z-loop-review".into()),
        loop_section: Some("Keep the loop brief across relaunches.".into()),
    };
    let resolve = |profiles: &ProfilesConfig| {
        resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile: Some("planner"),
                kind: &AgentKind::new_unchecked("claude"),
                stamped_mode: None,
                stamped_tier: None,
            },
            profiles,
            None,
        )
    };
    let posture = resolve(&profiles);
    assert!(posture.degraded.is_none());
    assert!(posture.launch.args.iter().any(|arg| arg == "--verbose"));
    assert_eq!(posture.launch.model, record.model);
    assert_eq!(posture.launch.effort, record.effort);
    profiles.0.remove("reviewer");
    let bare = resolve(&profiles);
    let expected = crate::agents::LaunchRecord {
        notes: record.notes.clone(),
        loop_section: record.loop_section.clone(),
        ..Default::default()
    };
    assert_eq!(bare.launch.record.as_deref(), Some(&expected));
    let request = bare.launch.exec_request(
        AgentKind::new_unchecked("claude"),
        crate::harness::launch::ExecAction::Resume {
            session_id: "loop-session".into(),
            extra_args: Vec::new(),
        },
    );
    assert!(matches!(request.reminder_section,
        Some(crate::harness::launch::ReminderSection::Loop(body))
        if Some(body.as_str()) == record.loop_section.as_deref()));
    assert!(bare.launch.model.is_none());
    assert!(bare.launch.effort.is_none());
    assert_eq!(bare.launch.fast, None);
    assert!(bare.degraded.unwrap().to_string().contains("reviewer"));
}

#[test]
fn tier_stamped_model_clients_restart_without_degrading() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex", "pi", "opencode"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base.\n---\nBase.",
        )
        .unwrap();
    }
    for kind in ["pi", "opencode"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}-worker.md")),
            format!(
                "---\ndescription: Worker.\nagent: {kind}\ntier: junior\ntools: [Bash]\n---\nCraft."
            ),
        )
        .unwrap();
    }
    let loaded = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &crate::config::CommandsConfig::default(),
        &root.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        None,
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let stamp = tier_stamp("sol");
    let actual: Vec<_> = ["pi", "opencode"]
        .map(|kind| {
            let posture = resolve_posture(
                PostureRequest {
                    record: None,
                    profile: Some(&format!("{kind}-worker")),
                    kind: &AgentKind::new_unchecked(kind),
                    stamped_mode: None,
                    stamped_tier: Some(&stamp),
                },
                &loaded.agent_profiles,
                None,
            );
            (posture.degraded, posture.launch.model)
        })
        .into();
    assert_eq!(
        actual,
        [(None, Some("sol".into())), (None, Some("sol".into()))]
    );
}

#[test]
fn resume_preserves_a_stamped_model_after_a_tier_rebind() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("definitions");
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::write(
        home.join("agents/claude.md"),
        "---\ndescription: Base.\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/planner.md"),
        "---\ndescription: Planner.\nagent: claude\ntier: senior\neffort: medium\ntools: [Bash]\n---\nPlan.",
    )
    .unwrap();
    let path = root.path().join("config.toml");
    let load = |tiers: &crate::config::tiers::TierConfig| {
        crate::config::definitions::load(
            &home,
            crate::config::definitions::SkillCheck::Skip,
            &crate::config::CommandsConfig::default(),
            &path,
            tiers,
            Some(crate::config::AutoCompactDefault::BuiltIn),
        )
    };
    let initial = load(&crate::config::tiers::TierConfig::default());
    assert!(initial.errors.is_empty(), "{:?}", initial.errors);
    assert_eq!(
        initial.agent_profiles.0["planner"].model.as_deref(),
        Some("opus")
    );
    std::fs::write(&path, "[tiers]\nsenior = ['fable']\nprincipal = []").unwrap();
    let config: crate::config::MachineConfig =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let rebound = load(&config.tiers);
    let model = "opus";
    let stamp = tier_stamp(model);
    let posture = resolve_posture(
        PostureRequest {
            record: None,
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &rebound.agent_profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert_eq!(posture.launch.model.as_deref(), Some(model));
    assert_eq!(posture.launch.effort.as_deref(), Some("medium"));
}

#[test]
fn fallen_back_resume_uses_stamped_family_render_and_model_defaults() {
    for effort in [None, Some("medium")] {
        let (_root, profiles) = tiered_claude("planner", effort);
        let stamp = tier_stamp("astra");
        let plan = plan_profiled(
            AgentState {
                profile: Some("planner".to_owned()),
                model: Some("hook-observed-model".to_owned()),
                tier: Some(stamp.clone()),
                ..agent("codex", "a1", "/code/qe", 1)
            },
            &profiles,
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.kind.as_str(), "codex");
        assert_eq!(
            request.identity.params.model.as_deref(),
            Some(stamp.model.as_str())
        );
        assert_eq!(request.identity.params.tier, Some(stamp));
        let expected = effort.or(crate::agents::definition_defaults("codex", Some("astra")).effort);
        assert_eq!(request.identity.params.effort.as_deref(), expected);
        assert!(
            matches!(request.action, crate::harness::launch::ExecAction::Resume { extra_args, .. } if codex_render(&extra_args))
        );
    }
}

#[test]
fn stamped_alias_resume_replays_the_recorded_session_model() {
    let (_root, profiles) = tiered_claude("planner", None);
    let stamp = tier_stamp("astra");
    let posture = resolve_posture(
        PostureRequest {
            record: None,
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none());
    assert_eq!(posture.launch.model.as_deref(), Some("astra"));
    let plan = plan_profiled(
        AgentState {
            profile: Some("planner".into()),
            model: Some("gpt-6.1-astra".into()),
            tier: Some(stamp.clone()),
            ..agent("codex", "a1", "/code/qe", 1)
        },
        &profiles,
    );
    let mut request = decode_exec_request(&single_pane_argv(&plan));
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    let machine = toml::from_str("[models.codex]\nastra = 'gpt-6.2-astra'").unwrap();
    let (warnings, movement, _) = crate::harness::launch_plan::resolve_model(
        &mut request,
        &machine,
        &runtime,
        &crate::agents::ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some(&AgentState {
            model: Some("gpt-6.1-astra".into()),
            ..agent("codex", "a1", "/repo", 1)
        }),
        None,
        &Default::default(),
    )
    .unwrap();
    assert!(warnings.is_empty());
    assert!(movement.is_none());
    assert_eq!(
        request.identity.params.model.as_deref(),
        Some("gpt-6.1-astra")
    );
    assert!(request.identity.params.tier.is_none());
    assert!(
        request
            .action
            .extra_args()
            .windows(2)
            .any(|args| args == ["--model", "gpt-6.1-astra"])
    );
    request.identity.params.model = Some("sol".into());
    request.identity.params.tier = Some(stamp);
    let machine = toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    crate::harness::launch_plan::resolve_model(
        &mut request,
        &machine,
        &runtime,
        &crate::agents::ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some(&AgentState {
            model: Some("gpt-6.1-astra".into()),
            ..agent("codex", "a1", "/repo", 1)
        }),
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(request.identity.params.model.as_deref(), Some("gpt-6-sol"));
}

#[test]
fn resume_replays_the_profile_declared_posture() {
    // A session that launched as `@planner` comes back as a planner: the
    // profile's model, effort, and system prompt ride the resume request,
    // not just the `@planner` handle.
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            auto_compact: Some("200k".to_owned()),
            ..profile("claude")
        },
    );
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles);

    assert!(plan.warnings.is_empty());
    let request = decode_exec_request(&single_pane_argv(&plan));
    let mut expected = crate::harness::spec::profile_cell("planner", &profiles)
        .expect("planner profile resolves")
        .args;
    expected.extend(["--permission-mode".to_owned(), "auto".to_owned()]);
    assert!(
        expected.iter().any(|arg| arg == "opus"),
        "profile argv should carry the model: {expected:?}"
    );
    assert_eq!(
        request.action,
        crate::harness::launch::ExecAction::Resume {
            session_id: "a1".to_owned(),
            extra_args: expected,
        }
    );
    assert!(
        matches!(&request.action, crate::harness::launch::ExecAction::Resume { extra_args, .. }
        if extra_args.windows(2).any(|args| args == ["--autocompact", "200000"]))
    );
    assert_eq!(request.identity.params.model.as_deref(), Some("opus"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
    assert_eq!(
        request
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(prompt.path())
    );
}

#[test]
fn relaunch_rereads_profile_isolation_without_replaying_the_effective_stamp() {
    use crate::config::Isolation::{Host, Scoped};
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        effective_isolation: Some(Host),
        ..agent("claude", "a1", "/code/qe", 1)
    };
    for isolation in [Host, Scoped] {
        let profiles = profiles(
            "planner",
            Profile {
                isolation: Some(isolation),
                ..profile("claude")
            },
        );
        let plan = plan_profiled(agent.clone(), &profiles);
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.isolation_default, Some(isolation));
        assert_eq!(request.identity.params.isolation, None);
    }
}

#[test]
fn resume_leaves_one_off_launch_values_out_of_the_posture() {
    // `model` on the rollup is observed, not declared — the user may have
    // switched it mid-session with `/model`. Only the profile speaks here.
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        model: Some("some-one-off-model".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles("planner", profile("claude")));

    let argv = single_pane_argv(&plan);
    assert!(
        !argv.iter().any(|arg| arg == "some-one-off-model"),
        "one-off model leaked into the resume argv: {argv:?}"
    );
    assert_eq!(decode_exec_request(&argv).identity.params.model, None);
}

#[test]
fn resume_replays_the_stamped_mode_when_the_profile_declares_none() {
    // The launch event records the permission posture the user granted, so a
    // profile-less agent still comes back with it.
    let agent = AgentState {
        mode: Some(PermissionMode::Yolo),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.action.extra_args(), yolo_argv("claude"));
    assert_eq!(request.identity.params.mode, Some(PermissionMode::Yolo));

    // A legacy row (no record) under a profile that declares no mode replays the stamp too.
    let posture = posture_for(
        "claude",
        Some("planner"),
        Some(PermissionMode::Yolo),
        &profiles("planner", profile("claude")),
    );
    assert_eq!(posture.degraded, None);
    assert_eq!(posture.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(posture.launch.args, yolo_argv("claude"));
}

#[test]
fn resume_degrades_to_bare_when_the_profile_is_gone() {
    // Rebirth runs unattended, so a profile dropped from config warns and
    // recovers rather than refusing to bring the session back.
    let agent = AgentState {
        profile: Some("retired".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    assert_eq!(plan.tabs.len(), 1, "the session still comes back");
    assert_eq!(plan.warnings.len(), 1);
    assert!(
        plan.warnings[0].contains("retired"),
        "warning should name the profile: {}",
        plan.warnings[0]
    );
    assert_eq!(
        decode_exec_request(&single_pane_argv(&plan))
            .action
            .extra_args(),
        &["--permission-mode", "auto"]
    );
}

#[test]
fn profile_mode_wins_over_the_stamped_mode() {
    // The profile is the standing decision; the stamp only fills a gap.
    let profiles = profiles(
        "planner",
        Profile {
            mode: Some(PermissionMode::Auto),
            ..profile("claude")
        },
    );

    let posture = posture_for(
        "claude",
        Some("planner"),
        Some(PermissionMode::Yolo),
        &profiles,
    );

    assert_eq!(posture.launch.mode, Some(PermissionMode::Auto));
    assert!(
        !yolo_argv("claude")
            .iter()
            .any(|arg| posture.launch.args.contains(arg)),
        "stamped yolo argv leaked past the profile's mode: {:?}",
        posture.launch.args
    );
}

#[test]
fn a_profile_prompt_file_that_vanished_degrades_instead_of_refusing() {
    // Rebirth is unattended: a deleted prompt file must not strand the session.
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(dir.path().join("missing.md").into()),
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert_eq!(
        posture.launch.args,
        crate::agents::spec_by_kind("codex")
            .unwrap()
            .launch
            .permission_args(PermissionMode::Auto)
    );
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::PromptFileMissing { .. })
    ));
}

#[test]
fn a_profile_prompt_fragment_that_vanished_degrades_instead_of_refusing() {
    let base = tempfile::NamedTempFile::new().expect("base prompt");
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(base.path().to_path_buf().into()),
            append_system_prompt_files: vec![dir.path().join("missing.md").into()],
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::PromptFileMissing { .. })
    ));
}

#[test]
fn opencode_resume_replays_the_profile_system_prompt_without_degrading() {
    let prompt = tempfile::NamedTempFile::new().unwrap();
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            ..profile("opencode")
        },
    );
    let agent = AgentState {
        profile: Some("planner".into()),
        ..agent("opencode", "ses_123", "/code/qe", 1)
    };
    let plan = plan_profiled(agent, &profiles);
    assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(
        request
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(prompt.path())
    );
    assert_eq!(
        request.action,
        crate::harness::launch::ExecAction::Resume {
            session_id: "ses_123".into(),
            extra_args: Vec::new(),
        }
    );
}

#[test]
fn unsupported_prompt_replacement_is_reported_as_a_resume_skip() {
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            ..profile("droid")
        },
    );
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        ..agent("droid", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles);

    assert!(plan.tabs.is_empty());
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(
        plan.skipped,
        [ResumeSkip {
            label: "droid:qe".to_owned(),
            reason: ResumeSkipReason::PromptUnsupported,
        }]
    );
}

#[test]
fn posture_reports_a_provider_switch_rather_than_refusing() {
    // Restart and fork escalate this; unattended resume degrades on it. Either
    // way the resolver reports rather than fails.
    let profiles = profiles("planner", profile("codex"));

    let posture = posture_for(
        "claude",
        Some("planner"),
        Some(PermissionMode::Yolo),
        &profiles,
    );

    // The bare fallback carries the stamped mode and its argv, nothing else.
    assert_eq!(
        posture,
        ResumePosture {
            launch: ResumeLaunchPosture {
                args: yolo_argv("claude"),
                mode: Some(PermissionMode::Yolo),
                ..Default::default()
            },
            degraded: Some(PostureDegrade::KindChanged {
                profile: "planner".into(),
                was: AgentKind::new_unchecked("claude"),
                now: AgentKind::new_unchecked("codex"),
            }),
        }
    );
}

#[test]
fn unsupported_record_effort_replays_without_degrading() {
    let mut worker = profile("kimi");
    worker.args = Some("--raw profile".into());
    let profiles = profiles("worker", worker);
    let record = crate::agents::LaunchRecord {
        effort: Some("high".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("worker"),
            kind: &AgentKind::new_unchecked("kimi"),
            stamped_mode: Some(PermissionMode::Yolo),
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert_eq!(posture.degraded, None);
    assert_eq!(posture.launch.effort, None);
    assert_eq!(posture.launch.record.as_deref().unwrap().effort, None);
    let mut expected = vec!["--raw".to_owned(), "profile".to_owned()];
    expected.extend(yolo_argv("kimi"));
    assert_eq!(posture.launch.args, expected);
}

#[test]
fn unsupported_profile_effort_replays_without_degrading() {
    let mut worker = profile("kimi");
    worker.effort = Some("high".into());
    worker.args = Some("--raw profile".into());
    let profiles = profiles("worker", worker);
    let record = crate::agents::LaunchRecord::default();
    for record in [None, Some(&record)] {
        let posture = resolve_posture(
            PostureRequest {
                record,
                profile: Some("worker"),
                kind: &AgentKind::new_unchecked("kimi"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        assert_eq!(posture.degraded, None);
        assert_eq!(posture.launch.effort, None);
        assert!(
            posture
                .launch
                .record
                .as_deref()
                .is_none_or(|record| record.effort.is_none())
        );
        assert_eq!(posture.launch.args, ["--raw", "profile"]);
    }
}

/// A profile renders its effort beside an Orchestrator id for a `--model` override to keep, so
/// a relaunch that keeps the id drops it from argv as well as from the record.
#[test]
fn orchestrator_profile_effort_relaunches_without_effort() {
    let id = "gemini-pro-orchestrator-neon";
    let mut worker = profile("antigravity");
    worker.model = Some(id.into());
    worker.effort = Some("high".into());
    let profiles = profiles("worker", worker);
    let record = crate::agents::LaunchRecord {
        model: Some(id.into()),
        ..Default::default()
    };
    for record in [None, Some(&record)] {
        let posture = resolve_posture(
            PostureRequest {
                record,
                profile: Some("worker"),
                kind: &AgentKind::new_unchecked("antigravity"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        assert_eq!(posture.degraded, None);
        assert_eq!(posture.launch.effort, None);
        assert!(
            posture
                .launch
                .record
                .as_deref()
                .is_none_or(|record| record.effort.is_none())
        );
        assert_eq!(posture.launch.args, ["--model", id], "{record:?}");
    }
}

#[test]
fn invalid_record_effort_degrades_to_the_bare_posture() {
    let profiles = profiles("worker", profile("hermes"));
    let record = crate::agents::LaunchRecord {
        effort: Some("bogus".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("worker"),
            kind: &AgentKind::new_unchecked("hermes"),
            stamped_mode: Some(PermissionMode::Yolo),
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert_eq!(
        posture,
        ResumePosture {
            launch: ResumeLaunchPosture {
                args: yolo_argv("hermes"),
                mode: Some(PermissionMode::Yolo),
                ..Default::default()
            },
            degraded: Some(PostureDegrade::Unresolved {
                profile: "worker".into(),
                reason: crate::agents::PresetErr::UnsupportedValue {
                    agent: "hermes",
                    field: "effort",
                    value: "bogus".into(),
                    values: &[
                        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"
                    ],
                }
                .to_string(),
            }),
        }
    );
}

#[test]
fn stamped_resume_replays_the_stamp_on_a_profile_that_lost_its_tier() {
    // The profile still runs on the session's kind, so the stamp's model builds there.
    let profiles = profiles("planner", profile("codex"));
    let plan = plan_profiled(
        AgentState {
            profile: Some("planner".to_owned()),
            tier: Some(tier_stamp("astra")),
            ..agent("codex", "a1", "/code/qe", 1)
        },
        &profiles,
    );
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.model.as_deref(), Some("astra"));
    let stamp = tier_stamp("astra");
    let posture = resolve_posture(
        PostureRequest {
            record: None,
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &profiles,
        None,
    );
    assert_eq!(posture.degraded, None);
    assert_eq!(posture.launch.model.as_deref(), Some("astra"));
    assert_eq!(
        posture
            .launch
            .tier
            .as_deref()
            .map(|stamp| stamp.model.as_str()),
        Some("astra")
    );
}

#[test]
fn a_pre_upgrade_alias_stamp_replays_the_recorded_model_over_its_kinds_base() {
    let mut base = profile("minimax");
    base.kind_base = true;
    base.model = Some("minimax-new".into());
    let profiles = profiles("minimax", base);
    let record = crate::agents::LaunchRecord {
        model: Some("minimax-old".into()),
        ..Default::default()
    };
    for name in ["minimax", "mcode"] {
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile: Some(name),
                kind: &AgentKind::new_unchecked("minimax"),
                stamped_mode: None,
                stamped_tier: None,
            },
            &profiles,
            None,
        );
        assert!(posture.degraded.is_none(), "{name}: {:?}", posture.degraded);
        assert_eq!(posture.launch.model, record.model, "{name}");
    }
}

#[test]
fn replay_rebuilds_tier_fallback_unversioned_model_and_team_seats_under_one_chain() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("teams")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\n---\nCodex house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/antigravity.md"),
        "---\ndescription: Antigravity base\n---\nAntigravity house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/astra.md"),
        "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: low\ntools: [Bash, Read]\n---\nAstra craft.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/tiered.md"),
        "---\ndescription: Tiered worker\ntier: senior\neffort: high\ntools: [Read]\n---\nTiered craft.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/finder.md"),
        "---\ndescription: Finder\nagent: codex\nmodel: sol\neffort: medium\ntools: [Read]\n---\nFind code.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("teams/squad.md"),
        "---\nleader: lead\nstages: [Build]\nroles:\n  - role: lead\n    agent: finder\n    model: sol\n    effort: high\n    owns: [Build]\n---\nTeam body.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();

    // 1. Tier fallback under X (`astra` on codex) that landed on claude (`opus`) replays on claude
    //    without `KindChanged` degrade and keeps X's craft and claude's base prompt.
    let fallback_record = crate::agents::LaunchRecord {
        model: Some("opus".into()),
        effort: Some("high".into()),
        agent: Some("astra".into()),
        ..Default::default()
    };
    let fallback_stamp = crate::agents::TierStamp {
        tier: crate::config::tiers::ModelTier::Senior,
        model: "opus".into(),
        used_tier: None,
        unbuildable: Vec::new(),
        skipped: Vec::new(),
    };
    let fallback_posture = resolve_posture(
        PostureRequest {
            record: Some(&fallback_record),
            profile: Some("tiered"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: Some(&fallback_stamp),
        },
        &machine.agents.profiles,
        None,
    );
    assert!(
        fallback_posture.degraded.is_none(),
        "tier fallback under X must not degrade: {:?}",
        fallback_posture.degraded
    );
    assert_eq!(fallback_posture.launch.model.as_deref(), Some("opus"));
    assert_eq!(fallback_posture.launch.effort.as_deref(), Some("high"));
    assert!(
        fallback_posture
            .launch
            .system_prompt_file
            .as_ref()
            .is_some_and(|src| src.origin().ends_with("agents/claude.md")),
        "{:?}",
        fallback_posture.launch.system_prompt_file
    );

    // 2. `--model neon` on an unversioned kind (`antigravity`, absent from the tier table) replays
    //    on antigravity without `KindChanged` degrade and carries antigravity's base prompt.
    let unversioned_record = crate::agents::LaunchRecord {
        model: Some("neon".into()),
        effort: Some("medium".into()),
        agent: None,
        ..Default::default()
    };
    let unversioned_posture = resolve_posture(
        PostureRequest {
            record: Some(&unversioned_record),
            profile: Some("finder"),
            kind: &AgentKind::new_unchecked("antigravity"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &machine.agents.profiles,
        None,
    );
    assert!(
        unversioned_posture.degraded.is_none(),
        "unversioned --model kind must not degrade: {:?}",
        unversioned_posture.degraded
    );
    assert_eq!(unversioned_posture.launch.model.as_deref(), Some("neon"));
    assert_eq!(unversioned_posture.launch.effort.as_deref(), Some("medium"));
    assert!(
        unversioned_posture
            .launch
            .system_prompt_file
            .as_ref()
            .is_some_and(|src| src.origin().ends_with("agents/antigravity.md")),
        "{:?}",
        unversioned_posture.launch.system_prompt_file
    );

    // 3. Pre-change record with `agent: None` (bare kind consumed by old routing) still replays on
    //    its recorded kind without degrading.
    let legacy_record = crate::agents::LaunchRecord {
        model: Some("opus".into()),
        effort: Some("high".into()),
        agent: None,
        ..Default::default()
    };
    let legacy_posture = resolve_posture(
        PostureRequest {
            record: Some(&legacy_record),
            profile: Some("tiered"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &machine.agents.profiles,
        None,
    );
    assert!(
        legacy_posture.degraded.is_none(),
        "{:?}",
        legacy_posture.degraded
    );
    assert_eq!(legacy_posture.launch.model.as_deref(), Some("opus"));

    // 4. Team seat with role writing model `sol` and effort `high`, launched under `--agent astra`
    //    (which writes model `astra`, effort `low`): replay keeps `astra` model (stripping role
    //    model) and role's `high` effort — matching fresh launch.
    let team_record = crate::agents::LaunchRecord {
        model: Some("astra".into()),
        effort: None,
        agent: Some("astra".into()),
        ..Default::default()
    };
    let team_posture = resolve_member_posture(
        PostureRequest {
            record: Some(&team_record),
            profile: Some("squad.lead"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &machine.agents.profiles,
        None,
        &machine.agents.teams,
        Some("squad"),
        Some("lead"),
    );
    assert!(
        team_posture.degraded.is_none(),
        "{:?}",
        team_posture.degraded
    );
    assert_eq!(team_posture.launch.model.as_deref(), Some("astra"));
    assert_eq!(team_posture.launch.effort.as_deref(), Some("high"));

    // Same team seat replay for a TOML RoleBinding writing `model = "opus"` and `effort = "high"`,
    // launched under `--agent codex` (bare kind, no model written) with recorded model `sol`:
    // role's `opus` must be stripped so replay neither overwrites `sol` nor emits DroppedModel.
    let mut toml_profiles: ProfilesConfig =
        toml::from_str("[worker]\nagent = 'claude'\n[codex]\nagent = 'codex'\n").unwrap();
    toml_profiles.0.get_mut("codex").unwrap().kind_base = true;
    let toml_teams: TeamsConfig = toml::from_str(
        "[squad]\nroles = [{ role = 'lead', profile = 'worker', model = 'opus', effort = 'high' }]",
    )
    .unwrap();
    let toml_record = crate::agents::LaunchRecord {
        model: Some("sol".into()),
        effort: None,
        agent: Some("codex".into()),
        ..Default::default()
    };
    let toml_team_posture = resolve_member_posture(
        PostureRequest {
            record: Some(&toml_record),
            profile: Some("worker"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &toml_profiles,
        None,
        &toml_teams,
        Some("squad"),
        Some("lead"),
    );
    assert!(
        toml_team_posture.degraded.is_none(),
        "{:?}",
        toml_team_posture.degraded
    );
    assert_eq!(toml_team_posture.launch.model.as_deref(), Some("sol"));
    assert_eq!(toml_team_posture.launch.effort.as_deref(), Some("high"));
    assert!(
        toml_team_posture.launch.notices.is_empty(),
        "replaying a recorded model must not emit DroppedModel for stripped role model: {:?}",
        toml_team_posture.launch.notices
    );
}

#[test]
fn replay_rebuilds_bare_kind_and_toml_profiles_under_override_without_returning_dropped_models() {
    let root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\ntools: [Read]\n---\nCodex house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/tiered_x.md"),
        "---\ndescription: Tiered override\ntier: senior\neffort: low\nbudget: 7\n\
         tools: [Read]\n---\nTiered override craft.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    // Default senior tier puts opus (claude) ahead of astra (codex), so `tiered_x` resolves to
    // claude at load time.
    assert_eq!(machine.agents.profiles.0["tiered_x"].agent, "claude");

    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.toml_l]\ndescription = 'TOML L'\nagent = 'claude'\neffort = 'high'\nmodel = 'opus'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();
    let profiles = crate::config::effective::load_with_roots(&machine, project.path(), root.path())
        .unwrap()
        .profiles;

    let fallback_stamp = crate::agents::TierStamp {
        tier: crate::config::tiers::ModelTier::Senior,
        model: "astra".into(),
        used_tier: None,
        unbuildable: Vec::new(),
        skipped: Vec::new(),
    };
    let fallback_record = crate::agents::LaunchRecord {
        model: Some("astra".into()),
        effort: None,
        agent: Some("tiered_x".into()),
        ..Default::default()
    };

    // Bare kind (`profile: None`) and kind base (`profile: Some("claude")`) replayed under a
    // tier-carrying X whose current tier resolves to claude, while the recorded session ran on
    // codex with astra.
    for launched in [None, Some("claude")] {
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&fallback_record),
                profile: launched,
                kind: &AgentKind::new_unchecked("codex"),
                stamped_mode: Some(PermissionMode::Auto),
                stamped_tier: Some(&fallback_stamp),
            },
            &profiles,
            None,
        );
        assert!(
            posture.degraded.is_none(),
            "launched={launched:?} must not degrade: {:?}",
            posture.degraded
        );
        assert_eq!(posture.launch.model.as_deref(), Some("astra"));
        assert_eq!(posture.launch.effort.as_deref(), Some("low"));
        assert_eq!(posture.launch.budget.as_deref(), Some("$7.00"));
        assert!(
            posture
                .launch
                .system_prompt_file
                .as_ref()
                .is_some_and(|src| src.origin().ends_with("agents/codex.md")),
            "{:?}",
            posture.launch.system_prompt_file
        );
        assert!(
            posture
                .launch
                .append_system_prompt_files
                .iter()
                .any(|src| src.origin().ends_with("agents/tiered_x.md")),
            "{:?}",
            posture.launch.append_system_prompt_files
        );
    }

    // TOML L replayed under the same tier-carrying X whose recorded session ran on codex.
    let toml_fallback = resolve_posture(
        PostureRequest {
            record: Some(&fallback_record),
            profile: Some("toml_l"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: Some(&fallback_stamp),
        },
        &profiles,
        None,
    );
    assert!(
        toml_fallback.degraded.is_none(),
        "TOML L replay under tiered X must not degrade: {:?}",
        toml_fallback.degraded
    );
    assert_eq!(toml_fallback.launch.model.as_deref(), Some("astra"));
    assert_eq!(toml_fallback.launch.effort.as_deref(), Some("high"));
    assert!(
        toml_fallback
            .launch
            .system_prompt_file
            .as_ref()
            .is_some_and(|src| src.origin().ends_with("agents/codex.md")),
        "{:?}",
        toml_fallback.launch.system_prompt_file
    );

    // TOML L whose claude model (`opus`) was dropped at fresh launch under `--agent codex` must
    // not get `opus` back on replay when the record carries no model.
    let dropped_record = crate::agents::LaunchRecord {
        model: None,
        effort: None,
        agent: Some("codex".into()),
        ..Default::default()
    };
    let dropped_posture = resolve_posture(
        PostureRequest {
            record: Some(&dropped_record),
            profile: Some("toml_l"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert!(
        dropped_posture.degraded.is_none(),
        "dropped-model replay must not degrade: {:?}",
        dropped_posture.degraded
    );
    assert_eq!(dropped_posture.launch.model, None);
    assert!(
        !dropped_posture
            .launch
            .args
            .iter()
            .any(|arg| arg.contains("opus")),
        "dropped model must not return in argv: {:?}",
        dropped_posture.launch.args
    );
}

#[test]
fn kind_base_under_non_markdown_override_replays_after_tier_fallback() {
    let root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\ntools: [Read]\n---\nCodex house.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.toml_codex]\ndescription = 'TOML codex'\nagent = 'codex'\nmodel = 'sol'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();
    let profiles = crate::config::effective::load_with_roots(&machine, project.path(), root.path())
        .unwrap()
        .profiles;
    let stamp = crate::agents::TierStamp {
        tier: crate::config::tiers::ModelTier::Senior,
        model: "opus".into(),
        used_tier: None,
        unbuildable: Vec::new(),
        skipped: Vec::new(),
    };
    for override_name in ["codex", "toml_codex"] {
        let record = crate::agents::LaunchRecord {
            model: Some("opus".into()),
            effort: None,
            agent: Some(override_name.into()),
            ..Default::default()
        };
        let posture = resolve_posture(
            PostureRequest {
                record: Some(&record),
                profile: Some("claude"),
                kind: &AgentKind::new_unchecked("claude"),
                stamped_mode: Some(PermissionMode::Auto),
                stamped_tier: Some(&stamp),
            },
            &profiles,
            None,
        );
        assert!(
            posture.degraded.is_none(),
            "{override_name}: fallback replay must not degrade: {:?}",
            posture.degraded
        );
        assert_eq!(
            posture.launch.model.as_deref(),
            Some("opus"),
            "{override_name}"
        );
        assert_eq!(
            posture.launch.tier.as_deref(),
            Some(&stamp),
            "{override_name}"
        );
    }
}

#[test]
fn kind_base_routed_by_model_to_non_tier_kind_replays_without_degrade() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/antigravity.md"),
        "---\ndescription: Antigravity base\n---\nAntigravity house.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    let record = crate::agents::LaunchRecord {
        model: Some("neon".into()),
        effort: None,
        agent: None,
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("claude"),
            kind: &AgentKind::new_unchecked("antigravity"),
            stamped_mode: Some(PermissionMode::Auto),
            stamped_tier: None,
        },
        &machine.agents.profiles,
        None,
    );
    assert!(
        posture.degraded.is_none(),
        "replaying claude --model neon must not degrade: {:?}",
        posture.degraded
    );
    assert_eq!(posture.launch.model.as_deref(), Some("neon"));
    assert!(
        posture
            .launch
            .system_prompt_file
            .as_ref()
            .is_some_and(|src| src.origin().ends_with("agents/antigravity.md")),
        "{:?}",
        posture.launch.system_prompt_file
    );
}

#[test]
fn project_kind_named_rows_never_stand_in_for_machine_kind_bases() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (kind, extra) in [("claude", "tools: [Read]\n"), ("codex", "effort: high\n")] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            format!("---\ndescription: Base\n{extra}---\n{kind} house.\n"),
        )
        .unwrap();
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.codex]\ndescription = 'Shadow codex'\nagent = \"codex\"\neffort = \"low\"\n\
         [profiles.antigravity]\ndescription = 'Shadow agy'\nagent = \"antigravity\"\neffort = \"low\"\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();
    let machine = || {
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap()
    };
    let route = |scope: ProfileScope, model: &str| {
        let machine = machine();
        let launch =
            crate::config::effective::load_with_roots(&machine, project.path(), root.path())
                .unwrap();
        let selection = crate::config::Selection::new(&machine.tiers).model(Some(model));
        crate::harness::plan::resolve_launch(
            &launch,
            scope,
            &machine.agents.commands,
            Some("claude"),
            &selection,
        )
        .map(|resolved| {
            (
                resolved.layout.agent_cells().next().unwrap().clone(),
                launch,
            )
        })
        .map_err(|error| error.to_string())
    };
    // Machine `agents/codex.md` stays the codex kind base (`effort: high`), unaffected by the
    // failed `[profiles.codex]` row; routing onto antigravity takes its built-in base, never the
    // failed `[profiles.antigravity]` row.
    let (fresh, launch) = route(ProfileScope::Agents, "sol").expect("routes onto machine codex.md");
    assert_eq!(fresh.kind.as_str(), "codex");
    assert_eq!(fresh.launch.model.as_deref(), Some("sol"));
    assert_eq!(fresh.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        crate::harness::spec::explain_profile_chain(
            "claude",
            None,
            "codex",
            true,
            &launch.profiles,
            None
        ),
        ["claude", "codex"]
    );

    let record = crate::agents::LaunchRecord {
        model: Some("sol".into()),
        effort: Some("high".into()),
        ..Default::default()
    };
    let posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("claude"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &launch.profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert_eq!(posture.launch.effort.as_deref(), Some("high"));
    assert_eq!(posture.launch.system_prompt_file, fresh.system_prompt_file);

    let (built_in_agy, _) = route(ProfileScope::Agents, "neon").expect("routes onto built-in");
    assert_eq!(
        (
            built_in_agy.kind.as_str(),
            built_in_agy
                .system_prompt_file
                .as_ref()
                .and_then(|base| base.built_in_kind()),
            built_in_agy.launch.effort.as_deref()
        ),
        ("antigravity", Some("antigravity"), None)
    );

    // Direct launches of the failed kind-named project rows refuse with the rename fix.
    for kind in ["codex", "antigravity"] {
        let err = launch
            .block_failed_profile(kind, ProfileScope::Agents, &launch.profiles)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("[profiles.{kind}]"))
                && err.contains("takes the name of an agent runtime"),
            "{err}"
        );
    }

    // A definition that failed to load keeps its refusal, whatever the project writes.
    for (model, kind) in [("sol", "codex"), ("neon", "antigravity")] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\nunknown-key: 1\n---\n",
        )
        .unwrap();
        let refused = route(ProfileScope::Agents, model).map(|_| ()).unwrap_err();
        assert!(
            refused.contains("cannot run this definition")
                && refused.contains(&format!("{kind}.md")),
            "{refused}"
        );
    }
}
