use super::*;

#[test]
fn hold_kinds_keep_the_ticket_wire_names_and_classify_only_repairs() {
    for (wire, needs_fix) in [
        ("capacity", false),
        ("check_failed", true),
        ("queue", false),
        ("machine_cap", false),
        ("automation_cap", false),
        ("stagger", false),
        ("workflow_limit", false),
        ("step_limit", false),
        ("pressure", false),
        ("memory", false),
        ("disk", true),
        ("store", true),
    ] {
        let kind: HoldKind = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(serde_json::to_value(kind).unwrap(), wire);
        assert_eq!(kind.needs_fix(), needs_fix, "{wire}");
    }
}

#[test]
fn durable_waits_ignore_unknown_hold_kinds() {
    for wire in ["task_cap", "future_hold"] {
        let value = serde_json::json!({
            "node":"build", "outcome":"capacity", "hold":wire,
            "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z",
            "count":1, "recheck_at":null, "kinds":[], "reason":"held", "gave_up":false
        });
        let wait = serde_json::from_value::<crate::store::workflow::Wait>(value.clone());
        assert!(wait.is_ok(), "{wire}: {wait:?}");
        assert_eq!(wait.unwrap().hold, None);
        let mut journal = value.clone();
        journal["event"] = serde_json::json!("wait");
        let journal = serde_json::from_value::<crate::store::workflow::JournalEvent>(journal);
        assert!(journal.is_ok(), "{wire}: {journal:?}");
        assert!(matches!(
            journal.unwrap(),
            crate::store::workflow::JournalEvent::Wait {
                hold: None,
                count: 1,
                ..
            }
        ));
        let record = serde_json::from_value::<
            crate::harness::schedule::launch_ledger::CapacityRecord,
        >(value);
        assert!(record.is_ok(), "{wire}: {record:?}");
        assert_eq!(record.unwrap().hold, None);
    }
}
#[test]
fn link_tier_orders_good_below_degraded_below_bad_and_spells_snake_case() {
    assert!(LinkTier::Good < LinkTier::Degraded);
    assert!(LinkTier::Degraded < LinkTier::Bad);
    assert_eq!(LinkTier::Bad.max(LinkTier::Good), LinkTier::Bad);
    for (tier, json) in [
        (LinkTier::Good, "\"good\""),
        (LinkTier::Degraded, "\"degraded\""),
        (LinkTier::Bad, "\"bad\""),
    ] {
        assert_eq!(serde_json::to_string(&tier).unwrap(), json);
        assert_eq!(serde_json::from_str::<LinkTier>(json).unwrap(), tier);
    }
}

#[test]
fn mux_name_other_flips_backend() {
    assert_eq!(MuxName::Zellij.other(), MuxName::Tmux);
    assert_eq!(MuxName::Tmux.other(), MuxName::Zellij);
}

fn named_lane(home: LaneHome, path: &str, name: &str) -> Lane {
    Lane::own(home, path).named(name.parse().unwrap())
}

#[test]
fn lane_readings_follow_the_home_and_name() {
    let cases = [
        (Lane::own(LaneHome::Trunk, "/p/rimz"), true, "main", None),
        (Lane::own(LaneHome::Trunk, "/p/main"), true, "main", None),
        (
            Lane::own(LaneHome::Worktree, "/w/auth"),
            false,
            "auth",
            Some("auth"),
        ),
        (
            Lane::own(LaneHome::Worktree, "/w/rimz"),
            false,
            "rimz",
            Some("rimz"),
        ),
        (Lane::own(LaneHome::Foreign, "/tmp/x"), false, "x", None),
        (
            named_lane(LaneHome::Trunk, "/p/rimz", "design"),
            false,
            "design",
            Some("design"),
        ),
        (
            named_lane(LaneHome::Trunk, "/p/rimz", "rimz"),
            false,
            "rimz",
            Some("rimz"),
        ),
        (
            named_lane(LaneHome::Foreign, "/tmp/x", "design"),
            false,
            "design",
            Some("design"),
        ),
    ];
    for (lane, trunk, routing, pin) in cases {
        assert_eq!(lane.is_trunk(), trunk, "{lane:?}");
        assert_eq!(lane.routing_name().as_str(), routing, "{lane:?}");
        assert_eq!(
            lane.pin_name().as_ref().map(LaneName::as_str),
            pin,
            "{lane:?}"
        );
    }
    assert_eq!(LaneName::TRUNK, "main");
    assert_eq!(
        named_lane(LaneHome::Worktree, "/w/auth", "x").path(),
        "/w/auth"
    );
    let spaced = Lane::own(LaneHome::Foreign, "/tmp/my dir#2");
    assert_eq!(spaced.routing_name().as_str(), "my-dir-2");
    assert_eq!(
        Lane::own(LaneHome::Foreign, "/").routing_name().as_str(),
        "-"
    );
}

#[test]
fn adoption_reads_an_old_stamp_at_its_checkout() {
    let trunk = Lane::own(LaneHome::Trunk, "/p/rimz");
    let worktree = Lane::own(LaneHome::Worktree, "/w/auth");
    let cases = [
        (worktree.clone(), None, worktree.clone()),
        (worktree.clone(), Some(""), worktree.clone()),
        (worktree.clone(), Some("auth"), worktree.clone()),
        (trunk.clone(), Some("main"), trunk.clone()),
        (
            trunk.clone(),
            Some("rimz"),
            named_lane(LaneHome::Trunk, "/p/rimz", "rimz"),
        ),
        (
            trunk.clone(),
            Some("design"),
            named_lane(LaneHome::Trunk, "/p/rimz", "design"),
        ),
        (
            worktree.clone(),
            Some("main"),
            named_lane(LaneHome::Worktree, "/w/auth", "main"),
        ),
        (
            worktree.clone(),
            Some("docs"),
            named_lane(LaneHome::Worktree, "/w/auth", "docs"),
        ),
        (
            named_lane(LaneHome::Trunk, "/p/rimz", "design"),
            Some("other"),
            named_lane(LaneHome::Trunk, "/p/rimz", "design"),
        ),
    ];
    for (own, stamp, expect) in cases {
        assert_eq!(own.clone().adopt(stamp), expect, "{own:?} {stamp:?}");
    }
}

#[test]
fn lane_names_refuse_hash_and_whitespace_and_flatness_is_narrower() {
    for name in ["design", "rimz/forge", "my.proj/forge"] {
        assert!(name.parse::<LaneName>().is_ok(), "{name}");
    }
    for name in ["", "#ops", "a#b", "two words"] {
        assert!(name.parse::<LaneName>().is_err(), "{name}");
    }
    for name in ["design", "ops_2", "run-42"] {
        assert!(LaneName::is_flat(name), "{name}");
    }
    for name in ["", "a.b", "a/b", "#ops", "two words"] {
        assert!(!LaneName::is_flat(name), "{name}");
    }
}

#[test]
fn team_instances_round_trip_and_decode_an_old_lane_under_its_dashed_name() {
    let auth: TeamInstanceId = "forge#auth".parse().unwrap();
    assert_eq!(
        auth,
        TeamInstanceId::new("forge", Some(LaneName::dashed("auth"))).unwrap()
    );
    let external: TeamInstanceId = "forge#external".parse().unwrap();
    assert_eq!(external.lane(), None);
    let named_external = TeamInstanceId::new("forge", Some(LaneName::dashed("external"))).unwrap();
    assert_eq!(named_external, external);
    for instance in [&auth, &external, &named_external] {
        assert_eq!(
            &instance.to_string().parse::<TeamInstanceId>().unwrap(),
            instance
        );
        let encoded = serde_json::to_value(instance).unwrap();
        assert_eq!(encoded, serde_json::json!(instance.to_string()));
        assert_eq!(
            &serde_json::from_value::<TeamInstanceId>(encoded).unwrap(),
            instance
        );
    }
    assert!("forge#feat x".parse::<TeamInstanceId>().is_err());
    let old: TeamInstanceId = serde_json::from_value(serde_json::json!("forge#feat x")).unwrap();
    assert_eq!(old.to_string(), "forge#feat-x");
    for refused in ["forge", "#auth", "forge#", "for ge#auth"] {
        assert!(serde_json::from_value::<TeamInstanceId>(serde_json::json!(refused)).is_err());
    }
}

#[test]
fn record_lanes_decode_an_old_lane_under_its_dashed_name_and_empty_as_absent() {
    #[derive(serde::Deserialize)]
    struct Record {
        #[serde(default, deserialize_with = "lenient_lane_name")]
        channel: Option<LaneName>,
    }
    let read = |value| {
        serde_json::from_value::<Record>(value)
            .unwrap()
            .channel
            .map(String::from)
    };
    assert_eq!(
        read(serde_json::json!({"channel": "feat x"})).as_deref(),
        Some("feat-x")
    );
    assert_eq!(
        read(serde_json::json!({"channel": "a#b"})).as_deref(),
        Some("a-b")
    );
    assert_eq!(
        read(serde_json::json!({"channel": "auth"})).as_deref(),
        Some("auth")
    );
    assert_eq!(read(serde_json::json!({"channel": ""})), None);
    assert_eq!(read(serde_json::json!({"channel": null})), None);
    assert_eq!(read(serde_json::json!({})), None);
}

#[test]
fn workspace_id_is_stable_for_same_root() {
    let a = WorkspaceId::from_project_root(Path::new("/tmp/repo"));
    let b = WorkspaceId::from_project_root(Path::new("/tmp/repo"));
    assert_eq!(a, b);
    assert!(a.as_str().starts_with("ws_"));
    assert_eq!(a.as_str().len(), 3 + 24);
}

#[test]
fn workspace_id_parser_accepts_only_canonical_shape() {
    let id = WorkspaceId::parse("ws_0123456789abcdefABCDEF01").expect("valid");
    assert_eq!(id.as_str(), "ws_0123456789abcdefABCDEF01");
    assert!(WorkspaceId::parse("not-a-workspace-id").is_err());
    assert!(WorkspaceId::parse("0123456789abcdefABCDEF01").is_err());
    assert!(WorkspaceId::parse("ws_short").is_err());
    assert!(WorkspaceId::parse("ws_0123456789abcdefABCDEFG").is_err());
}

#[test]
fn workspace_dir_name_round_trips_and_rejects_malformed_names() {
    let id = WorkspaceId::parse("ws_abcdef0123456789abcdef01").unwrap();
    let minted = WorkspaceDirName::mint("my-repo", &id, 4);
    assert_eq!(minted.as_str(), "my-repo-abcd");
    let parsed = WorkspaceDirName::parse(minted.as_str()).expect("minted parses");
    assert_eq!(parsed, minted);
    assert_eq!((parsed.as_str(), parsed.hex()), ("my-repo-abcd", "abcd"));
    assert!(parsed.may_name(&id));
    assert_eq!(WorkspaceDirName::mint("x", &id, 7).hex(), "abcdef");
    assert_eq!(WorkspaceDirName::mint("x", &id, 99).hex(), id.hex());
    assert_eq!(
        WorkspaceDirName::fallback(&id).as_str(),
        "ws-abcdef0123456789abcdef01"
    );
    for bad in [
        "repo",
        "repo-abc",
        "repo-abcde",
        "repo-abcg",
        "-abcd",
        "--abcd",
        "a b-abcd",
        "ws_abcdef0123456789abcdef01",
    ] {
        assert_eq!(WorkspaceDirName::parse(bad), None, "{bad}");
    }
}

#[test]
fn workspace_basename_slug_sanitizes_and_caps() {
    let slug = |root: &str, max| WorkspaceDirName::basename_slug(Path::new(root), max);
    assert_eq!(slug("/src/my repo!", 32), "my-repo");
    assert_eq!(slug("/src/abcdefghijk", 8), "abcdefgh");
    assert_eq!(slug("/src/abcdefg-ij", 8), "abcdefg");
    assert_eq!(slug("/", 32), "root");
    assert_eq!(slug("/src/---", 32), "root");
}

#[test]
fn uuid_prefixed_ids_reject_non_canonical_input() {
    assert!(RunId::parse("run_0123456789abcdef0123456789abcdef").is_ok());
    assert!(RunId::parse("evt_0123456789abcdef0123456789abcdef").is_err());
    assert!(RunId::parse("run_short").is_err());
    assert!(RunId::parse("run_0123456789abcdef0123456789abcdeg").is_err());
    assert!(RunId::parse("run_0123456789abcdef0123456789ABCDEF").is_err());
    assert!(EventId::parse("evt_0123456789abcdef0123456789abcdef").is_ok());
    assert!(SidebarInstanceId::parse("sb_0123456789abcdef0123456789abcdef").is_ok());
}

#[test]
fn message_ids_are_short_time_sortable_tokens() {
    let first = MessageId::new();
    let second = MessageId::new();

    assert_ne!(first, second);
    assert!(first.as_str().starts_with("msg_"));
    assert_eq!(first.as_str().len(), "msg_".len() + 16);
    assert!(
        first.as_str()["msg_".len()..]
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'v'))
    );
    assert!(second.as_str() >= first.as_str());
    assert!(MessageId::parse(first.as_str()).is_ok());
    assert!(MessageId::parse("msg_0123456789abcdef").is_ok());
    assert!(MessageId::parse("msg_0123456789abcde").is_err());
    assert!(MessageId::parse("msg_0123456789abcdew").is_err());
    assert!(MessageId::parse("msg_0123456789ABCDEF").is_err());
}

#[test]
fn message_id_successor_carries_suffix_overflow() {
    let last_suffix = MessageId::parse("msg_0000000007vvvvvv").unwrap();
    let successor = last_suffix.successor();
    assert_eq!(successor.as_str(), "msg_0000000008000000");
    assert!(successor.as_str() > last_suffix.as_str());
    assert_eq!(MessageId::parse(successor.as_str()).unwrap(), successor);
}

#[test]
fn ask_ids_are_short_time_sortable_tokens() {
    let first = AskId::new();
    let second = AskId::new();

    assert_eq!(first.as_str().len(), 20);
    assert!(first.as_str().starts_with("ask_"));
    assert!(second.as_str() >= first.as_str());
    assert!(AskId::parse(first.as_str()).is_ok());
    assert!(AskId::parse("ask_0123456789abcdef").is_ok());
    assert!(AskId::parse("ask_0123456789abcde").is_err());
    assert!(AskId::parse("ask_0123456789abcdew").is_err());
    assert!(AskId::parse("ask_0123456789ABCDEF").is_err());
}

#[test]
fn uuid_prefixed_ids_reject_bad_json_values() {
    let parsed: Result<RunId, _> = serde_json::from_str("\"run_0123456789abcdef0123456789abcdef\"");
    assert!(parsed.is_ok());

    let parsed: Result<RunId, _> = serde_json::from_str("\"not-a-run-id\"");
    assert!(parsed.is_err());
}

#[test]
fn short_returns_the_12_hex_tail() {
    // `short()` slices from the end, so it works across prefixes of different
    // lengths ("run" vs "sb").
    for (short, full) in [
        {
            let id = RunId::new();
            (id.short().to_owned(), id.as_str().to_owned())
        },
        {
            let id = SidebarInstanceId::new();
            (id.short().to_owned(), id.as_str().to_owned())
        },
    ] {
        assert_eq!(short.len(), 12);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
        // The short id is the hex tail of the UUID portion.
        assert!(full.ends_with(&short));
    }
}

#[test]
fn short_disambiguates_same_millisecond_ids() {
    // The first 12 hex of a v7 UUID are the millisecond timestamp, so two ids
    // minted together share them; `short()` must take the random tail instead
    // or their socket paths collide and one `bind` steals the other's.
    let a = SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").unwrap();
    let b = SidebarInstanceId::parse("sb_019e8c565bbd7b22854f93a905e1034c").unwrap();
    assert_eq!(
        &a.as_str()[3..15],
        &b.as_str()[3..15],
        "same-millisecond ids share the leading v7 timestamp",
    );
    assert_ne!(
        a.short(),
        b.short(),
        "the random tail disambiguates same-millisecond ids",
    );
}

#[test]
fn agent_identity_newtypes_serialize_transparently() {
    // The rollup cache and snapshot JSON shapes must stay byte-identical
    // to the plain-string era — the newtypes are compile-time-only.
    let kind = AgentKind::new_unchecked("claude");
    assert_eq!(serde_json::to_string(&kind).unwrap(), r#""claude""#);
    let back: AgentKind = serde_json::from_str(r#""claude""#).unwrap();
    assert_eq!(back, kind);
    assert!(kind == "claude");

    let session = AgentSessionId::from("sess-1");
    assert_eq!(serde_json::to_string(&session).unwrap(), r#""sess-1""#);
    let back: AgentSessionId = serde_json::from_str(r#""sess-1""#).unwrap();
    assert_eq!(back, session);
    assert!(session == "sess-1");

    // Open set: an unknown kind decodes fine — replay of a removed
    // adapter's events must fold, not fail.
    let unknown: AgentKind = serde_json::from_str(r#""opencode""#).unwrap();
    assert_eq!(unknown.as_str(), "opencode");
}

#[test]
fn pane_id_round_trips_parts() {
    let id = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    assert_eq!(id.as_str(), "zellij:terminal_3");
    assert_eq!(id.mux(), MuxName::Zellij);
    assert_eq!(id.raw(), "terminal_3");

    let parsed_zellij: PaneId = "zellij:terminal_3".parse().expect("valid pane id");
    assert_eq!(parsed_zellij.mux(), MuxName::Zellij);
    assert_eq!(parsed_zellij.raw(), "terminal_3");

    let parsed: PaneId = "tmux:%5".parse().expect("valid pane id");
    assert_eq!(parsed.mux(), MuxName::Tmux);
    assert_eq!(parsed.raw(), "%5");
}

#[test]
fn pane_id_creation_ordinal_reads_trailing_digits() {
    assert_eq!(
        PaneId::from_parts(MuxName::Zellij, "terminal_176").creation_ordinal(),
        Some(176)
    );
    assert_eq!(
        PaneId::from_parts(MuxName::Tmux, "%3").creation_ordinal(),
        Some(3)
    );
    assert_eq!(
        PaneId::from_parts(MuxName::Tmux, "pane").creation_ordinal(),
        None
    );
}

#[test]
fn login_name_accepts_the_grammar_and_refuses_everything_else() {
    for name in [
        "default",
        "work",
        "a",
        "0",
        "team-1",
        "team_1",
        &"x".repeat(32),
    ] {
        assert!(name.parse::<LoginName>().is_ok(), "{name}");
    }
    for name in [
        "",
        "-work",
        "_work",
        "Work",
        "wörk",
        "work dir",
        "work/dir",
        &"x".repeat(33),
    ] {
        assert!(name.parse::<LoginName>().is_err(), "{name}");
    }
    assert!(LoginName::default_login().is_default());
    assert!(!"work".parse::<LoginName>().unwrap().is_default());
}

#[test]
fn login_key_round_trips_through_its_rendered_form() {
    let key: LoginKey = "claude@work".parse().unwrap();
    assert_eq!(key.kind, "claude");
    assert_eq!(key.name.as_str(), "work");
    assert_eq!(key.to_string(), "claude@work");
    assert_eq!(serde_json::to_string(&key).unwrap(), "\"claude@work\"",);
    assert_eq!(
        serde_json::from_str::<LoginKey>("\"codex@default\"").unwrap(),
        LoginKey::default_for(AgentKind::new_unchecked("codex"))
    );
    assert!("claude".parse::<LoginKey>().is_err());
    assert!("@work".parse::<LoginKey>().is_err());
    assert!("claude@Work".parse::<LoginKey>().is_err());
}

#[test]
fn pane_id_rejects_unknown_mux_prefix() {
    assert!(PaneId::parse("kitty:1").is_err());
    assert!(PaneId::parse("no-colon").is_err());
    assert!(PaneId::parse("tmux:").is_err());
    assert!(PaneId::parse("zellij:").is_err());
}

#[test]
fn pane_id_deserialize_rejects_unknown_mux_prefix() {
    let parsed: PaneId = serde_json::from_str(r#""tmux:%5""#).expect("valid pane id");
    assert_eq!(parsed.raw(), "%5");
    assert!(serde_json::from_str::<PaneId>(r#""not-a-pane""#).is_err());
}
