use super::*;
use rimz::agents::attribution::{MessageCounts, Presence, SubagentStat, TeamRef};
use rimz::ids::AgentKind;

fn member(handle: &str, role: Option<&str>, provider: &str, model: &str) -> AttributionMember {
    AttributionMember {
        handle: handle.to_owned(),
        role: role.map(ToOwned::to_owned),
        name: None,
        kind: AgentKind::new_unchecked(provider.to_ascii_lowercase()),
        provider: provider.to_owned(),
        model: Some(model.to_owned()),
        effort: Some("high".to_owned()),
        presence: Presence::Exited,
        me: false,
        launch_ordinal: Some(0),
        sessions: 2,
        registered_at: Some(jiff::Timestamp::UNIX_EPOCH),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
        active_secs: Some(3_900),
        asks: 2,
        asks_answered: 1,
        tool_calls: 7,
        compactions: 1,
        messages: MessageCounts {
            from_user: 2,
            from_teammates: 5,
            to_teammates: 4,
        },
        tokens: TokenSplit {
            input: 1_200,
            output: 800,
            cache_write: 2_000,
            cache_read: 3_000,
        },
        cost_usd: Some(1.60),
        subagents: vec![
            SubagentStat {
                task: Some("explorer".to_owned()),
                count: 4,
                cost_usd: Some(0.90),
            },
            SubagentStat {
                task: None,
                count: 1,
                cost_usd: None,
            },
        ],
        models: vec![ModelStat {
            model: Some("claude-opus-4-8".to_owned()),
            tokens: TokenSplit {
                input: 1_200,
                output: 800,
                cache_write: 2_000,
                cache_read: 3_000,
            },
            cost_usd: Some(1.60),
        }],
    }
}

fn report() -> Attribution {
    let team_member = member("@planner", Some("plan|ner"), "Claude", "fable`2");
    let mut stray = member("@codex", None, "Codex", "gpt-5.5");
    stray.tokens = TokenSplit {
        input: 300,
        output: 100,
        ..TokenSplit::default()
    };
    stray.models = vec![ModelStat {
        model: Some("gpt-5.5".to_owned()),
        tokens: stray.tokens,
        cost_usd: stray.cost_usd,
    }];
    let group_totals = |members: &[AttributionMember]| EffortTotals {
        agents: u32::try_from(members.len()).expect("small fixture"),
        active_secs: Some(3_900 * members.len() as u64),
        wall_clock_secs: 4_000,
        cost_usd: Some(1.60 * members.len() as f64),
        asks: 2 * members.len() as u64,
        asks_answered: members.len() as u64,
        tool_calls: 7 * members.len() as u64,
        compactions: members.len() as u32,
        messages: MessageCounts {
            from_user: 2 * members.len() as u64,
            from_teammates: 5 * members.len() as u64,
            to_teammates: 4 * members.len() as u64,
        },
        tokens: members
            .iter()
            .fold(TokenSplit::default(), |mut tokens, member| {
                tokens.add_assign(member.tokens);
                tokens
            }),
    };
    let team_members = vec![team_member];
    let other_members = vec![stray];
    Attribution {
        schema: 7,
        generated_at: jiff::Timestamp::UNIX_EPOCH,
        rimz_version: "test".to_owned(),
        scope: AttributionScope::default(),
        totals: group_totals(&[team_members[0].clone(), other_members[0].clone()]),
        models: team_members[0]
            .models
            .iter()
            .chain(&other_members[0].models)
            .cloned()
            .collect(),
        groups: vec![
            AttributionGroup {
                team: Some(TeamRef {
                    name: "forge".to_owned(),
                    roles: vec!["planner".to_owned()],
                }),
                totals: group_totals(&team_members),
                members: team_members,
            },
            AttributionGroup {
                team: None,
                totals: group_totals(&other_members),
                members: other_members,
            },
        ],
    }
}

#[test]
fn panel_groups_team_and_stray_members() {
    let mut output = anstream::StripStream::new(Vec::new());
    render_panel(&mut output, &report()).expect("render panel");
    insta::assert_snapshot!(String::from_utf8(output.into_inner()).expect("utf8"), @"
    forge team · 1 agent · 1h05m active · $1.60 · 7 messages (2 from you)

      @planner (plan|ner) · Claude · fable`2@high
          effort:    1h05m active · $1.60
          subagents: 4 × explorer, 1 × other · $0.90
          activity:  2 asks · 7 tool calls · 1 compaction
          messages:  2 from you · 5 from teammates · 4 to teammates
          tokens:    1.2k input, 800 output, 2k cache write, 3k cache read

    Other agents · 1 agent · 1h05m active · $1.60 · 7 messages (2 from you)

      @codex · Codex · gpt-5.5@high
          effort:    1h05m active · $1.60
          subagents: 4 × explorer, 1 × other · $0.90
          activity:  2 asks · 7 tool calls · 1 compaction
          messages:  2 from you · 5 from teammates · 4 to teammates
          tokens:    300 input, 100 output

    Models
      claude-opus-4-8: $1.60 · 1.2k input, 800 output, 2k cache write, 3k cache read
      gpt-5.5:         $1.60 · 300 input, 100 output

    Total · 2 agents · 2h10m active · $3.20 · 14 messages (4 from you)
    ");
}

#[test]
fn panel_omits_redundant_caption_for_single_teamless_group() {
    let mut report = report();
    let teamless = report.groups.pop().expect("teamless fixture group");
    report.totals = teamless.totals.clone();
    report.models = teamless.members[0].models.clone();
    report.groups = vec![teamless];

    let mut output = anstream::StripStream::new(Vec::new());
    render_panel(&mut output, &report).expect("render panel");
    insta::assert_snapshot!(String::from_utf8(output.into_inner()).expect("utf8"), @"
      @codex · Codex · gpt-5.5@high
          effort:    1h05m active · $1.60
          subagents: 4 × explorer, 1 × other · $0.90
          activity:  2 asks · 7 tool calls · 1 compaction
          messages:  2 from you · 5 from teammates · 4 to teammates
          tokens:    300 input, 100 output

    Models
      gpt-5.5: $1.60 · 300 input, 100 output

    Total · 1 agent · 1h05m active · $1.60 · 7 messages (2 from you)
    ");
}

#[test]
fn panel_omits_total_that_repeats_the_only_caption() {
    let mut report = report();
    report.groups.truncate(1);
    report.totals = report.groups[0].totals.clone();
    report.models = report.groups[0].members[0].models.clone();

    let mut output = anstream::StripStream::new(Vec::new());
    render_panel(&mut output, &report).expect("render panel");
    let output = String::from_utf8(output.into_inner()).expect("utf8");

    assert!(output.starts_with("forge team ·"));
    assert!(output.contains("\nModels\n  claude-opus-4-8:"));
    assert!(!output.contains("Total ·"));
}

#[test]
fn markdown_escapes_values_and_renders_grouped_bullets() {
    let mut output = Vec::new();
    render_markdown(&mut output, &report()).expect("render markdown");
    insta::assert_snapshot!(String::from_utf8(output).expect("utf8"), @r#"
    <details>
    <summary>Implemented by <a href="https://github.com/rimio-ai/rimz">RimZ</a> agents · 2 agents · 2h10m active · $3.20 · 14 messages (4 from you)</summary>

    <br/>

    **Agents**

    **forge team**

    - **plan|ner** — Claude fable&#96;2@high
      - effort: 1h05m active · $1.60
      - subagents: 4 × explorer, 1 × other · $0.90
      - activity: 2 asks · 7 tool calls · 1 compaction
      - messages: 2 from you · 5 from teammates · 4 to teammates
      - tokens: 1.2k input, 800 output, 2k cache write, 3k cache read

    **Other agents**

    - **@codex** — Codex `gpt-5.5@high`
      - effort: 1h05m active · $1.60
      - subagents: 4 × explorer, 1 × other · $0.90
      - activity: 2 asks · 7 tool calls · 1 compaction
      - messages: 2 from you · 5 from teammates · 4 to teammates
      - tokens: 300 input, 100 output

    **Models**

    - `claude-opus-4-8` — $1.60 · 1.2k input, 800 output, 2k cache write, 3k cache read
    - `gpt-5.5` — $1.60 · 300 input, 100 output

    </details>
    "#);
}

#[test]
fn markdown_single_team_keeps_the_group_name_in_the_summary_only() {
    let mut report = report();
    report.groups.pop();
    report.totals = report.groups[0].totals.clone();
    report.models = report.groups[0].members[0].models.clone();
    let mut output = Vec::new();

    render_markdown(&mut output, &report).expect("render markdown");
    let output = String::from_utf8(output).expect("utf8");

    assert!(output.contains("<code>forge</code> team"));
    assert!(!output.contains("**forge team**"));
}

#[test]
fn markdown_escapes_emphasis_and_link_punctuation() {
    let mut report = report();
    report.groups[0].members[0].role = Some(r"plan*ner_[x]\tail".to_owned());
    let mut output = Vec::new();

    render_markdown(&mut output, &report).expect("render markdown");
    let output = String::from_utf8(output).expect("utf8");

    assert!(output.contains(r"- **plan\*ner\_\[x\]\\tail**"));
}

#[test]
fn markdown_model_code_span_keeps_punctuation_verbatim() {
    let mut spanned = member("@coder", Some("coder"), "Qwen", "qwen2_5-coder");
    assert_eq!(
        markdown_code(&model_label(&spanned)),
        "`qwen2_5-coder@high`"
    );

    spanned.model = Some("llama3*8b".to_owned());
    assert_eq!(markdown_code(&model_label(&spanned)), "`llama3*8b@high`");

    spanned.model = Some("a[1]".to_owned());
    assert_eq!(markdown_code(&model_label(&spanned)), "`a[1]@high`");

    spanned.model = Some("fable`2".to_owned());
    assert_eq!(markdown_code(&model_label(&spanned)), "fable&#96;2@high");
}

#[test]
fn identity_omits_a_role_already_carried_by_the_handle() {
    let matching = member("@planner#auth", Some("planner"), "Claude", "fable-2");
    let displaced = member("@quiet-fox", Some("planner"), "Claude", "fable-2");

    assert_eq!(identity_label(&matching), "@planner#auth");
    assert_eq!(identity_label(&displaced), "@quiet-fox (planner)");
}

#[test]
fn activity_labels_name_only_recorded_components() {
    let mut sample = member("@coder", Some("coder"), "Codex", "gpt-5.5");
    sample.asks = 0;
    sample.tool_calls = 0;
    sample.compactions = 0;
    assert_eq!(activity_label(&sample), None);

    sample.asks = 1;
    assert_eq!(activity_label(&sample).as_deref(), Some("1 ask"));

    sample.asks = 0;
    sample.tool_calls = 1;
    assert_eq!(activity_label(&sample).as_deref(), Some("1 tool call"));

    sample.tool_calls = 0;
    sample.compactions = 1;
    assert_eq!(activity_label(&sample).as_deref(), Some("1 compaction"));

    sample.tool_calls = 2;
    sample.compactions = 3;
    sample.asks = 4;
    assert_eq!(
        activity_label(&sample).as_deref(),
        Some("4 asks · 2 tool calls · 3 compactions")
    );
}

#[test]
fn renderers_omit_activity_when_none_is_recorded() {
    let mut report = report();
    for group in &mut report.groups {
        for member in &mut group.members {
            member.asks = 0;
            member.tool_calls = 0;
            member.compactions = 0;
        }
    }

    let mut panel = anstream::StripStream::new(Vec::new());
    render_panel(&mut panel, &report).expect("render panel");
    assert!(
        !String::from_utf8(panel.into_inner())
            .expect("utf8")
            .contains("activity:")
    );

    let mut markdown = Vec::new();
    render_markdown(&mut markdown, &report).expect("render markdown");
    assert!(
        !String::from_utf8(markdown)
            .expect("utf8")
            .contains("  - activity:")
    );
}

#[test]
fn token_labels_name_only_recorded_components() {
    let mut sample = member("@coder", Some("coder"), "Codex", "gpt-5.5");
    sample.tokens.cache_write = 0;
    assert_eq!(
        token_split_label(&sample.tokens).as_deref(),
        Some("1.2k input, 800 output, 3k cache read")
    );

    sample.tokens = TokenSplit::default();
    assert_eq!(token_split_label(&sample.tokens), None);
}

#[test]
fn renderers_show_unpriced_unknown_model_tokens() {
    let mut report = report();
    report.groups.truncate(1);
    let member = &mut report.groups[0].members[0];
    member.tokens = TokenSplit {
        input: 200,
        ..TokenSplit::default()
    };
    member.cost_usd = None;
    member.subagents.clear();
    member.models = vec![ModelStat {
        model: None,
        tokens: member.tokens,
        cost_usd: None,
    }];
    report.models = member.models.clone();
    report.groups[0].totals.tokens = report.models[0].tokens;
    report.groups[0].totals.cost_usd = None;
    report.totals = report.groups[0].totals.clone();

    let mut panel = anstream::StripStream::new(Vec::new());
    render_panel(&mut panel, &report).expect("render panel");
    let panel = String::from_utf8(panel.into_inner()).expect("utf8");
    assert!(panel.ends_with("\nModels\n  unknown: 200 input\n"));

    let mut markdown = Vec::new();
    render_markdown(&mut markdown, &report).expect("render markdown");
    let markdown = String::from_utf8(markdown).expect("utf8");
    assert!(markdown.ends_with("\n**Models**\n\n- unknown — 200 input\n\n</details>\n"));
}

#[test]
fn absent_details_are_omitted_without_placeholders() {
    let mut report = report();
    report.groups.truncate(1);
    report.groups[0].members[0].active_secs = None;
    report.groups[0].members[0].cost_usd = None;
    report.groups[0].members[0].asks = 0;
    report.groups[0].members[0].tool_calls = 0;
    report.groups[0].members[0].compactions = 0;
    report.groups[0].members[0].messages = MessageCounts::default();
    report.groups[0].members[0].tokens = TokenSplit::default();
    report.groups[0].members[0].subagents.clear();
    report.groups[0].members[0].models.clear();
    report.models.clear();
    report.groups[0].totals.active_secs = None;
    report.groups[0].totals.cost_usd = None;
    report.groups[0].totals.messages = MessageCounts::default();
    report.groups[0].totals.tokens = TokenSplit::default();
    report.totals = report.groups[0].totals.clone();

    let mut panel = anstream::StripStream::new(Vec::new());
    render_panel(&mut panel, &report).expect("render panel");
    let panel = String::from_utf8(panel.into_inner()).expect("utf8");
    assert!(!panel.contains("unknown"));
    assert!(!panel.contains("none recorded"));
    assert!(!panel.contains("effort:"));
    assert!(!panel.contains("activity:"));
    assert!(!panel.contains("messages:"));
    assert!(!panel.contains("tokens:"));
    assert!(!panel.contains("subagents:"));
    assert!(!panel.contains("Models"));

    let mut markdown = Vec::new();
    render_markdown(&mut markdown, &report).expect("render markdown");
    let markdown = String::from_utf8(markdown).expect("utf8");
    assert!(!markdown.contains("unknown"));
    assert!(!markdown.contains("none recorded"));
    assert!(!markdown.contains("**Models**"));
}

#[test]
fn token_counts_change_units_at_decimal_boundaries() {
    assert_eq!(token_count(999), "999");
    assert_eq!(token_count(1_000), "1k");
    assert_eq!(token_count(1_100), "1.1k");
    assert_eq!(token_count(999_949), "999.9k");
    assert_eq!(token_count(999_950), "1m");
    assert_eq!(token_count(1_000_000), "1m");
    assert_eq!(token_count(999_949_999), "999.9m");
    assert_eq!(token_count(999_950_000), "1b");
}

#[test]
fn attribution_headers_show_the_lane_boundary() {
    let mut report = report();
    let timestamp = "2026-09-06T13:26:00.155953104Z"
        .parse::<jiff::Timestamp>()
        .unwrap();
    let local = timestamp.to_zoned(crate::cli::machine_config().time_zone());
    let boundary = format!(
        "since {} {:02}:{:02}",
        local.date(),
        local.hour(),
        local.minute()
    );
    for since in [None, Some(timestamp)] {
        report.scope.since = since;
        let mut panel = anstream::StripStream::new(Vec::new());
        render_panel(&mut panel, &report).expect("render panel");
        let panel = String::from_utf8(panel.into_inner()).expect("utf8");
        let mut markdown = Vec::new();
        render_markdown(&mut markdown, &report).expect("render markdown");
        let markdown = String::from_utf8(markdown).expect("utf8");
        assert_eq!(panel.starts_with(&format!("{boundary}\n")), since.is_some());
        assert_eq!(
            markdown.contains(&format!(" · {boundary}</summary>")),
            since.is_some()
        );
    }
    report.groups.clear();
    let mut panel = anstream::StripStream::new(Vec::new());
    render_panel(&mut panel, &report).expect("render empty panel");
    assert_eq!(
        String::from_utf8(panel.into_inner()).expect("utf8"),
        format!("{boundary}\nNo agent attribution records in this scope.\n")
    );
    let mut markdown = Vec::new();
    render_markdown(&mut markdown, &report).expect("render empty markdown");
    assert!(markdown.is_empty());
}

#[test]
fn empty_scope_is_muted_for_people_and_silent_for_markdown() {
    let mut report = report();
    report.groups.clear();
    report.models.clear();
    report.totals = EffortTotals::default();
    let mut panel = anstream::StripStream::new(Vec::new());
    render_panel(&mut panel, &report).expect("render panel");
    assert_eq!(
        String::from_utf8(panel.into_inner()).expect("utf8"),
        "No agent attribution records in this scope.\n"
    );
    let mut markdown = Vec::new();
    render_markdown(&mut markdown, &report).expect("render markdown");
    assert!(markdown.is_empty());
}
