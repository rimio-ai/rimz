use super::*;
use crate::agents::{AgentCost, AgentCurrentUsage, AgentState, single_line_description};

fn row_time() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

#[test]
fn compose_channel_uses_explicit_then_worktree_basename() {
    assert_eq!(
        compose_channel(Some("design"), Some("auth")).as_deref(),
        Some("design")
    );
    assert_eq!(compose_channel(None, Some("auth")).as_deref(), Some("auth"));
    assert_eq!(compose_channel(None, None), None);
}

#[test]
fn serde_keeps_cards_flat_with_row_kind_key() {
    let agent = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Running,
            prompt: Some("fix auth flow".to_owned()),
            tool_calls: BTreeMap::from([("Read".to_owned(), 4)]),
            usage: AgentUsageSummary {
                context_pct: Some(42),
                context_window: Some(200_000),
                total_tokens: Some(84_000),
                cache_read_input_tokens: Some(60_000),
                cache_write_input_tokens: Some(4_000),
                fresh_input_tokens: Some(20_000),
                output_tokens: Some(1_000),
            },
            ..AgentCard::default()
        })),
    };

    let value = serde_json::to_value(&agent).unwrap();

    assert_eq!(value["row_kind"], "agent");
    assert!(value.get("card").is_none());
    assert!(value.get("unread").is_none());
    assert!(value.get("usage").is_none());
    for key in [
        "context_pct",
        "context_window",
        "total_tokens",
        "cache_read_input_tokens",
        "cache_write_input_tokens",
        "fresh_input_tokens",
        "output_tokens",
    ] {
        assert!(value.get(key).is_some(), "missing flat key {key}");
    }
    assert_eq!(value["prompt"], "fix auth flow");
    assert_eq!(value["tool_calls"]["Read"], 4);
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), agent);

    let process = SidebarRow {
        id: "process:%1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: row_time(),
        card: RowCard::Process(ProcessCard {
            state: ProcessState::Stuck,
            command_detail: Some("cargo build --release".to_owned()),
            foreign_user: None,
            rss_kb: Some(512 * 1_024),
            cpu_pct: Some(42),
            io_bps: Some(1_024),
        }),
    };

    let value = serde_json::to_value(&process).unwrap();

    assert_eq!(value["row_kind"], "process");
    assert!(value.get("card").is_none());
    assert!(value.get("status").is_none());
    assert_eq!(value["state"], "stuck");
    assert_eq!(value["command_detail"], "cargo build --release");
    assert_eq!(
        serde_json::from_value::<SidebarRow>(value).unwrap(),
        process
    );

    let unread = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: true,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Success,
            ..AgentCard::default()
        })),
    };

    let value = serde_json::to_value(&unread).unwrap();

    assert_eq!(value["unread"], true);
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), unread);
}

#[test]
fn agent_card_without_active_time_field_deserializes_as_unknown() {
    let mut value = serde_json::to_value(AgentCard {
        estimated_active_secs: Some(1_500),
        ..AgentCard::default()
    })
    .unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("estimated_active_secs");

    assert_eq!(
        serde_json::from_value::<AgentCard>(value)
            .unwrap()
            .estimated_active_secs,
        None
    );
}

#[test]
fn display_name_prefers_agent_handle_and_falls_back_to_row_name() {
    let mut agent = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            handle: Some("planner".to_owned()),
            team: None,
            launch_group: None,
            launch_ordinal: None,

            ..AgentCard::default()
        })),
    };
    assert_eq!(agent.display_name(), "planner");

    agent.as_agent_mut().unwrap().handle = None;
    assert_eq!(agent.display_name(), "claude");

    let process = SidebarRow {
        id: "process:%1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: row_time(),
        card: RowCard::Process(ProcessCard::default()),
    };
    assert_eq!(process.display_name(), "cargo");
}

#[test]
fn agent_card_activity_description_matches_agent_state_precedence() {
    let context = AgentContext {
        session_preview: Some(" \n\t".to_owned()),
        session_name: Some("<task-notification>control</task-notification>".to_owned()),
        ..AgentContext::new("codex", row_time())
    };
    let mut card = AgentCard {
        context: Some(context.clone()),
        description: Some("\u{0007}".to_owned()),
        task: Some(" ship\nwide ".to_owned()),
        first_prompt: Some("first prompt".to_owned()),
        prompt: Some("latest prompt".to_owned()),
        ..AgentCard::default()
    };
    let mut state = AgentState::stub("codex", "sess", AgentStatus::Running);
    state.context = Some(context);
    state.description = card.description.clone();
    state.task = card.task.clone();
    state.first_prompt = card.first_prompt.clone();
    state.prompt = card.prompt.clone();

    assert_eq!(card.activity_description(), state.activity_description());
    assert_eq!(
        card.activity_description()
            .and_then(single_line_description)
            .as_deref(),
        Some("ship wide")
    );

    card.task = Some("<system-reminder>control</system-reminder>".to_owned());
    state.task = card.task.clone();
    assert_eq!(card.activity_description(), Some("first prompt"));
    assert_eq!(card.activity_description(), state.activity_description());
    card.first_prompt = Some("<task-notification>control</task-notification>".to_owned());
    state.first_prompt = card.first_prompt.clone();
    assert_eq!(card.activity_description(), Some("latest prompt"));
    assert_eq!(card.activity_description(), state.activity_description());
}

#[test]
fn agent_card_session_history_requires_positive_evidence() {
    assert!(!AgentCard::default().has_session_history());
    assert!(
        !AgentCard {
            usage: AgentUsageSummary {
                total_tokens: Some(0),
                ..AgentUsageSummary::default()
            },
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            context: Some(context_with_tokens(AgentTokenUsage {
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(1),
                    ..AgentCurrentUsage::default()
                }),
                ..AgentTokenUsage::default()
            })),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        !AgentCard {
            context: Some(context_with_tokens(AgentTokenUsage {
                current_usage: Some(AgentCurrentUsage::default()),
                ..AgentTokenUsage::default()
            })),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            usage: AgentUsageSummary {
                total_tokens: Some(1),
                ..AgentUsageSummary::default()
            },
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            compaction_count: 1,
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            tool_calls: BTreeMap::from([("Read".to_owned(), 1)]),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            context: Some(context_with_cost(0.01)),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        !AgentCard {
            context: Some(context_with_cost(0.0)),
            ..AgentCard::default()
        }
        .has_session_history()
    );
}

fn context_with_cost(total_cost_usd: f64) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: Some(AgentCost {
            total_cost_usd: Some(total_cost_usd),
            ..AgentCost::default()
        }),
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        settle: None,
        observed_at: row_time(),
    }
}

fn context_with_tokens(tokens: AgentTokenUsage) -> AgentContext {
    AgentContext {
        tokens: Some(tokens),
        ..context_with_cost(0.0)
    }
}

#[test]
fn context_gauge_percent_only_trusts_a_sidecar_percentage_paired_with_a_window() {
    // A sidecar percentage drawn against an unknown window cannot share a
    // denominator with the displayed window, so the gauge prefers the
    // fold-derived scalar — which the fold tied to the resolved window — over
    // it, avoiding a bar that disagrees with its window label.
    let untethered = AgentCard {
        usage: AgentUsageSummary {
            context_pct: Some(16),
            context_window: Some(1_000_000),
            ..AgentUsageSummary::default()
        },
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: None,
            used_percentage: Some(82),
            remaining_percentage: Some(18),
            current_context_tokens: None,
            current_usage: None,
            session_usage: None,
        })),
        ..AgentCard::default()
    };
    assert_eq!(untethered.context_gauge_percent(), Some(16));

    // With its own window present, the sidecar percentage is authoritative and
    // shares the denominator the identity line shows.
    let tethered = AgentCard {
        usage: AgentUsageSummary {
            context_pct: Some(16),
            ..AgentUsageSummary::default()
        },
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(40),
            remaining_percentage: Some(60),
            current_context_tokens: None,
            current_usage: None,
            session_usage: None,
        })),
        ..AgentCard::default()
    };
    assert_eq!(tethered.context_gauge_percent(), Some(40));

    let token_only = AgentCard {
        usage: AgentUsageSummary {
            context_pct: Some(0),
            ..AgentUsageSummary::default()
        },
        context: Some(context_with_tokens(AgentTokenUsage {
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(20),
                output_tokens: Some(2),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(80),
            }),
            ..AgentTokenUsage::default()
        })),
        ..AgentCard::default()
    };
    assert_eq!(token_only.context_gauge_percent(), None);
}

#[test]
fn context_gauge_percent_derives_from_sidecar_usage_when_percentage_is_absent() {
    let derived = AgentCard {
        usage: AgentUsageSummary {
            context_pct: Some(0),
            ..AgentUsageSummary::default()
        },
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: Some(258_400),
            used_percentage: None,
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(6_700),
                output_tokens: Some(825),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(56_900),
            }),
            session_usage: None,
        })),
        ..AgentCard::default()
    };

    assert_eq!(
        derived.context_gauge_percent(),
        Some(24),
        "rich Codex context derives the filled bar from current usage over its own window"
    );
}

#[test]
fn current_context_scalar_controls_total_and_lifecycle_split_correlation() {
    let mut card = AgentCard {
        usage: AgentUsageSummary {
            cache_read_input_tokens: Some(80),
            cache_write_input_tokens: Some(5),
            fresh_input_tokens: Some(15),
            output_tokens: Some(9),
            ..AgentUsageSummary::default()
        },
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: Some(1_000),
            current_context_tokens: Some(100),
            ..AgentTokenUsage::default()
        })),
        ..AgentCard::default()
    };

    assert_eq!(card.context_used_tokens(), Some(100));
    assert_eq!(card.context_gauge_percent(), Some(10));
    assert_eq!(card.call_split().map(|split| split.filled()), Some(100));

    card.context
        .as_mut()
        .and_then(|context| context.tokens.as_mut())
        .unwrap()
        .current_context_tokens = Some(101);
    assert_eq!(card.context_used_tokens(), Some(101));
    assert_eq!(card.call_split(), None);
}
