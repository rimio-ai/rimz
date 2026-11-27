use super::*;

#[test]
fn parse_and_render_cover_design_examples_and_traps() {
    // (raw, home_series, expected_series, expected_full). A row without a home
    // under a series that has one runs under both.
    let cases = [
        // The design's Expected names table.
        ("claude-opus-4-8", Some("Claude"), None, "Opus 4.8"),
        ("claude-opus-4-8", None, Some("Claude"), "Claude Opus 4.8"),
        ("claude-opus-4-8-fast", Some("Claude"), None, "Opus 4.8"),
        (
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            Some("Claude"),
            None,
            "Sonnet 4.5",
        ),
        (
            "gpt-5.1-codex-max",
            Some("GPT"),
            Some("GPT"),
            "GPT 5.1 Codex Max",
        ),
        ("gpt-5.1-codex-max", None, Some("GPT"), "GPT 5.1 Codex Max"),
        ("gpt-5.5", Some("GPT"), Some("GPT"), "GPT 5.5"),
        ("gpt-5.5", None, Some("GPT"), "GPT 5.5"),
        (
            "gemini-3.1-flash-lite-preview",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 3.1 Flash Lite",
        ),
        (
            "gemini-3.1-flash-lite-preview",
            None,
            Some("Gemini"),
            "Gemini 3.1 Flash Lite",
        ),
        (
            "gemini-3.1-pro-preview-customtools",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 3.1 Pro",
        ),
        (
            "gemini-3.1-pro-preview-customtools",
            None,
            Some("Gemini"),
            "Gemini 3.1 Pro",
        ),
        (
            "gemini-2.5-flash-preview-09-2025",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 2.5 Flash",
        ),
        (
            "gemini-2.5-flash-preview-09-2025",
            None,
            Some("Gemini"),
            "Gemini 2.5 Flash",
        ),
        (
            "qwen3-coder-30b-a3b-instruct",
            Some("Qwen"),
            Some("Qwen"),
            "Qwen 3 Coder 30b",
        ),
        (
            "qwen3-coder-30b-a3b-instruct",
            None,
            Some("Qwen"),
            "Qwen 3 Coder 30b",
        ),
        (
            "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
            Some("Qwen"),
            Some("Qwen"),
            "Qwen 2.5 Coder 32b",
        ),
        (
            "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
            None,
            Some("Qwen"),
            "Qwen 2.5 Coder 32b",
        ),
        (
            "llama-3.3-70b-instruct",
            None,
            Some("Llama"),
            "Llama 3.3 70b",
        ),
        ("gemma-3-27b-it", None, Some("Gemma"), "Gemma 3 27b"),
        (
            "mistral-small-3.2-24b-instruct",
            None,
            Some("Mistral"),
            "Mistral 3.2 Small 24b",
        ),
        (
            "grok-4-fast-non-reasoning",
            Some("Grok"),
            Some("Grok"),
            "Grok 4",
        ),
        ("grok-4-fast-non-reasoning", None, Some("Grok"), "Grok 4"),
        (
            "grok-4-1-fast-reasoning",
            Some("Grok"),
            Some("Grok"),
            "Grok 4.1",
        ),
        ("grok-4-1-fast-reasoning", None, Some("Grok"), "Grok 4.1"),
        (
            "grok-code-fast-1",
            Some("Grok"),
            Some("Grok"),
            "Grok 1 Code",
        ),
        ("grok-code-fast-1", None, Some("Grok"), "Grok 1 Code"),
        ("deepseek-v4-pro", None, Some("DeepSeek"), "DeepSeek V4 Pro"),
        (
            "deepseek-coder-v2",
            None,
            Some("DeepSeek"),
            "DeepSeek V2 Coder",
        ),
        ("kimi-k2.5", Some("Kimi"), Some("Kimi"), "Kimi K2.5"),
        ("kimi-k2.5", None, Some("Kimi"), "Kimi K2.5"),
        (
            "MiniMax-M2.5",
            Some("MiniMax"),
            Some("MiniMax"),
            "MiniMax M2.5",
        ),
        ("MiniMax-M2.5", None, Some("MiniMax"), "MiniMax M2.5"),
        ("deepseek-chat", None, Some("DeepSeek"), "DeepSeek Chat"),
        ("opus", Some("Claude"), None, "Opus"),
        ("opus", None, None, "Opus"),
        ("auto", Some("GPT"), None, "Auto"),
        ("auto", None, None, "Auto"),
        // Cases the table does not walk.
        ("claude-2.1", Some("Claude"), Some("Claude"), "Claude 2.1"),
        (
            "claude-3-5-sonnet-20241022",
            Some("Claude"),
            None,
            "Sonnet 3.5",
        ),
        (
            "claude-3-5-sonnet-20241022",
            None,
            Some("Claude"),
            "Claude Sonnet 3.5",
        ),
        ("deepseek-v3-0324", None, Some("DeepSeek"), "DeepSeek V3"),
        // A snapshot code stays in the parsed version and drops from the name.
        ("gpt-4-0613", Some("GPT"), Some("GPT"), "GPT 4"),
        ("grok-4.20-0309", None, Some("Grok"), "Grok 4.20"),
        ("mistral-large-2411", None, Some("Mistral"), "Mistral Large"),
        ("mistral-small-2503", None, Some("Mistral"), "Mistral Small"),
        ("codestral-2501", None, Some("Codestral"), "Codestral"),
        // A vendor segment stays the series where the rest leads with a
        // version-like word, and a region segment may hold a hyphen.
        ("deepseek.r1-v1:0", None, Some("DeepSeek"), "DeepSeek R1"),
        ("us.deepseek.v3.2", None, Some("DeepSeek"), "DeepSeek V3.2"),
        ("deepseek.v3-v1:0", None, Some("DeepSeek"), "DeepSeek V3"),
        (
            "us-gov.anthropic.claude-sonnet-4-5-20250929-v1:0",
            Some("Claude"),
            None,
            "Sonnet 4.5",
        ),
        // A revision tail goes even where it holds the id's only digits.
        ("us.amazon.nova-pro-v1:0", None, Some("Nova"), "Nova Pro"),
        (
            "qwen3-coder-30b-a2.5b-instruct",
            None,
            Some("Qwen"),
            "Qwen 3 Coder 30b",
        ),
        ("gpt-4o-mini", Some("GPT"), Some("GPT"), "GPT 4o Mini"),
        ("gpt-4o-mini", None, Some("GPT"), "GPT 4o Mini"),
        ("o4-mini", Some("GPT"), None, "O4 Mini"),
        ("o4-mini", None, None, "O4 Mini"),
        ("grok-fast", Some("Grok"), Some("Grok"), "Grok"),
        ("grok-fast", None, Some("Grok"), "Grok"),
        // Fixture names and the shapes today's callers feed.
        (
            "Gemini 4 Neon",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 4 Neon",
        ),
        ("Gemini 4 Neon", None, Some("Gemini"), "Gemini 4 Neon"),
        (
            "gemini-4-carbon",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 4 Carbon",
        ),
        ("gpt-6.2-sol", Some("GPT"), Some("GPT"), "GPT 6.2 Sol"),
        ("gpt-6.2-sol", None, Some("GPT"), "GPT 6.2 Sol"),
        (
            "claude-opus-4-8",
            Some("Gemini"),
            Some("Claude"),
            "Claude Opus 4.8",
        ),
        ("grok-4.5", Some("Grok"), Some("Grok"), "Grok 4.5"),
        (
            "gemini-3.8-flash-lite",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini 3.8 Flash Lite",
        ),
        (
            "MiniMax-M3.1-Flash-Preview",
            Some("MiniMax"),
            Some("MiniMax"),
            "MiniMax M3.1 Flash",
        ),
        ("o3-mini-high", Some("GPT"), None, "O3 Mini High"),
        (
            "claude-3-5-haiku-20241022",
            Some("Claude"),
            None,
            "Haiku 3.5",
        ),
        (
            "claude-3-5-haiku-20241022",
            None,
            Some("Claude"),
            "Claude Haiku 3.5",
        ),
        ("claude-mythos-preview", Some("Claude"), None, "Mythos"),
        (
            "kimi-for-coding",
            Some("Kimi"),
            Some("Kimi"),
            "Kimi For Coding",
        ),
        (
            "gemini-pro-orchestrator-neon",
            Some("Gemini"),
            Some("Gemini"),
            "Gemini Pro Orchestrator Neon",
        ),
        ("high", None, None, "High"),
        (
            "Orchestrator (Neon)",
            Some("Gemini"),
            None,
            "Orchestrator (Neon)",
        ),
        ("gpt-5-2025-08-07", Some("GPT"), Some("GPT"), "GPT 5"),
        ("gpt-oss-120b", Some("GPT"), Some("GPT"), "GPT Oss 120b"),
        ("hy4", None, Some("Hy"), "Hy 4"),
        ("qwen3", Some("Qwen"), Some("Qwen"), "Qwen 3"),
        ("mystery-model", None, Some("Mystery"), "Mystery Model"),
        ("Legacy Model", None, Some("Legacy"), "Legacy Model"),
        (
            "codex-auto-review",
            Some("GPT"),
            Some("Codex"),
            "Codex Auto Review",
        ),
        ("Fable 5.1@high", Some("Claude"), None, "Fable 5.1"),
        ("Fable 5.1@high", None, None, "Fable 5.1"),
        (
            "claude-opus-4-8[1m]@xhigh",
            Some("Claude"),
            None,
            "Opus 4.8",
        ),
        (
            "custom:DeepSeek-V4-Pro-0",
            None,
            Some("DeepSeek"),
            "DeepSeek V4 Pro",
        ),
        (
            "custom:openai/gpt-oss-120b-0",
            None,
            Some("GPT"),
            "GPT Oss 120b",
        ),
        (
            "custom:Qwen/Qwen3-Coder-0",
            None,
            Some("Qwen"),
            "Qwen 3 Coder",
        ),
        (
            "custom:Qwen/Qwen3-Coder-0",
            Some("Qwen"),
            Some("Qwen"),
            "Qwen 3 Coder",
        ),
        ("gemini-pro-latest", None, Some("Gemini"), "Gemini Pro"),
        ("", None, None, ""),
    ];

    let actual: Vec<_> = cases
        .iter()
        .map(|(raw, home, _, _)| {
            let rendered = ParsedModel::parse(raw).render(*home);
            (*raw, *home, rendered.series.clone(), rendered.full())
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(raw, home, series, full)| {
            (*raw, *home, series.map(str::to_owned), (*full).to_owned())
        })
        .collect();
    assert_eq!(actual, expected);

    // With a series shown and neither a line name nor a version, every word
    // after the series is a child; with no series the first word leads.
    let versionless = ParsedModel::parse("gemini-pro-latest").render(None);
    assert_eq!(
        (versionless.core.as_str(), versionless.children.as_slice()),
        ("", ["Pro".to_owned()].as_slice())
    );
    assert_eq!(versionless.name(0), "");
    assert_eq!(ParsedModel::parse("o4-mini").render(None).core, "O4");
}

#[test]
fn home_series_declarations_match_adapter_ruling() {
    let declared = [
        ("claude", Some("Claude")),
        ("codex", Some("GPT")),
        ("antigravity", Some("Gemini")),
        ("grok", Some("Grok")),
        ("kimi", Some("Kimi")),
        ("minimax", Some("MiniMax")),
        ("qwen", Some("Qwen")),
        ("pi", None),
        ("opencode", None),
        ("cursor", None),
        ("droid", None),
        ("copilot", None),
        ("amp", None),
        ("kiro", None),
        ("hermes", None),
        ("muse", None),
        ("qoder", None),
        ("omp", None),
    ];
    let actual: Vec<_> = declared
        .iter()
        .map(|(kind, _)| (*kind, home_series(kind)))
        .collect();
    assert_eq!(actual, declared.to_vec());

    // Brand-colour lookup finds the adapter declaring the shown series
    // case-insensitively and returns None for undeclared series.
    let owners = [
        ("Gemini", Some("antigravity")),
        ("gemini", Some("antigravity")),
        ("Claude", Some("claude")),
        ("GPT", Some("codex")),
        ("Grok", Some("grok")),
        ("Kimi", Some("kimi")),
        ("MiniMax", Some("minimax")),
        ("Qwen", Some("qwen")),
        ("DeepSeek", None),
        ("Mystery", None),
    ];
    let actual_owners: Vec<_> = owners
        .iter()
        .map(|(series, _)| (*series, home_series_owner(series)))
        .collect();
    assert_eq!(actual_owners, owners.to_vec());
}

#[test]
fn model_label_source_prefers_id_unless_versionless_id_meets_digit_label() {
    let cases = [
        // Id with version beats label even when label also carries a version
        // (id keeps the series that labels like "Opus 4.8" drop).
        (
            Some("Opus 4.8"),
            Some("claude-opus-4-8"),
            Some("claude-opus-4-8"),
            Some("claude-opus-4-8"),
        ),
        (
            Some("Opus"),
            Some("claude-opus-5"),
            Some("claude-opus-5"),
            Some("claude-opus-5"),
        ),
        // Cursor `auto` + versioned or digit-bearing label keeps the label.
        (
            Some("GPT-5.6 Sol"),
            Some("auto"),
            Some("auto"),
            Some("GPT-5.6 Sol"),
        ),
        (
            Some("GPT-4o Mini"),
            Some("auto"),
            Some("auto"),
            Some("GPT-4o Mini"),
        ),
        (Some("o4-mini"), Some("auto"), Some("auto"), Some("o4-mini")),
        (
            Some("Sonnet 3.5 (New)"),
            Some("auto"),
            Some("auto"),
            Some("Sonnet 3.5 (New)"),
        ),
        // Qwen decorated launch id with versioned label picks the label.
        (
            Some("deepseek-v4-pro"),
            None,
            Some("[DeepSeek] deepseek-v4-pro"),
            Some("deepseek-v4-pro"),
        ),
        (
            Some("Opus"),
            None,
            Some("claude-opus-5"),
            Some("claude-opus-5"),
        ),
        (
            None,
            Some("claude-opus-5"),
            Some("claude-opus-4-8"),
            Some("claude-opus-5"),
        ),
        (Some("Opus"), None, None, Some("Opus")),
        (None, None, None, None),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(display, id, launch, _)| model_label_source(*display, *id, *launch))
        .collect();
    let expected: Vec<_> = cases.iter().map(|(_, _, _, expected)| *expected).collect();
    assert_eq!(actual, expected);
}

#[test]
fn match_key_normalizes_slugs_labels_aliases_and_legacy_refusal_keys() {
    let cases = [
        (
            "claude-opus-4-8",
            Some(("opus", Some("4.8"))),
            "claude 4.8 opus",
        ),
        (
            "claude-opus-4-8[1m]",
            Some(("opus", Some("4.8"))),
            "claude 4.8 opus",
        ),
        ("opus[1m]", Some(("opus", None)), "opus"),
        ("Opus 4.8", Some(("opus", Some("4.8"))), "opus 4.8"),
        ("Fable 5.1@high", Some(("fable", Some("5.1"))), "fable 5.1"),
        ("fAbLe", Some(("fable", None)), "fable"),
        ("gpt-6.2-sol", Some(("sol", Some("6.2"))), "gpt 6.2 sol"),
        ("gpt 6.2 sol", Some(("sol", Some("6.2"))), "gpt 6.2 sol"),
        ("sol 6.2", Some(("sol", Some("6.2"))), "sol 6.2"),
        ("gpt-5.5", Some(("gpt", Some("5.5"))), "gpt 5.5"),
        ("o4-mini", Some(("o4 mini", None)), "o4 mini"),
        // Cloud ids match their bare id; fast and packaging words still
        // separate keys, and a snapshot code extends the version.
        (
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            Some(("sonnet", Some("4.5"))),
            "claude 4.5 sonnet",
        ),
        (
            "claude-sonnet-4-5",
            Some(("sonnet", Some("4.5"))),
            "claude 4.5 sonnet",
        ),
        (
            "global.anthropic.claude-opus-4-8-v1:0",
            Some(("opus", Some("4.8"))),
            "claude 4.8 opus",
        ),
        (
            "meta.llama3-3-70b-instruct-1:0",
            Some(("70b instruct", Some("3.3"))),
            "llama 3.3 70b instruct",
        ),
        (
            "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
            Some(("coder 32b instruct", Some("2.5"))),
            "qwen 2.5 coder 32b instruct",
        ),
        (
            "gemini-2.5-flash-preview-09-2025",
            Some(("flash preview", Some("2.5"))),
            "gemini 2.5 flash preview",
        ),
        (
            "deepseek-v3-0324",
            Some(("deepseek", Some("v3.0324"))),
            "deepseek v3.0324",
        ),
        ("gpt-4-0613", Some(("gpt", Some("4.0613"))), "gpt 4.0613"),
        (
            "grok-4.20-0309",
            Some(("grok", Some("4.20.0309"))),
            "grok 4.20.0309",
        ),
        // Keys the merge base wrote for cloud, MM-YYYY and snapshot ids read
        // as the key of their id.
        (
            "global.anthropic.claude 4.8 opus v1:0",
            Some(("opus", Some("4.8"))),
            "claude 4.8 opus",
        ),
        (
            "us.anthropic.claude 4.5.20250929 sonnet v1:0",
            Some(("sonnet", Some("4.5"))),
            "claude 4.5 sonnet",
        ),
        (
            "gemini 2.5 flash preview 09 2025",
            Some(("flash preview", Some("2.5"))),
            "gemini 2.5 flash preview",
        ),
        ("gpt 4.0613", Some(("gpt", Some("4.0613"))), "gpt 4.0613"),
        (
            "deepseek v3.0324",
            Some(("deepseek", Some("v3.0324"))),
            "deepseek v3.0324",
        ),
        (
            "claude-opus-4-8-fast",
            Some(("opus fast", Some("4.8"))),
            "claude 4.8 opus fast",
        ),
        (
            "grok-4-fast-non-reasoning",
            Some(("fast non reasoning", Some("4"))),
            "grok 4 fast non reasoning",
        ),
        ("", None, ""),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(raw, _, _)| {
            let parsed = ParsedModel::parse(raw);
            let refusal = parsed.refusal_key();
            let key = parsed.match_key();
            (key.map(|k| (k.name, k.version)), refusal)
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, pair, refusal)| {
            (
                pair.map(|(n, v)| (n.to_owned(), v.map(str::to_owned))),
                (*refusal).to_owned(),
            )
        })
        .collect();
    assert_eq!(actual, expected);

    let fable_scope = ParsedModel::parse("fable").match_key().unwrap();
    for (raw, matches) in [
        ("claude-fable-5-1", true),
        ("claude-fable-5-1[1m]", true),
        ("fable[1m]", true),
        ("Fable 5.1@high", true),
        ("fAbLe", true),
        ("Fablework", false),
        ("Opus 4.6", false),
    ] {
        assert_eq!(
            ParsedModel::parse(raw)
                .match_key()
                .is_some_and(|key| key.matches_scope(&fable_scope)),
            matches,
            "{raw}"
        );
    }

    let mythos_scope = ParsedModel::parse("mythos").match_key().unwrap();
    for (raw, matches) in [
        ("claude-mythos-preview", true),
        ("claude-mythos-5-preview", true),
        ("claude-mythos-5", true),
        ("claude-mythoswork-5", false),
    ] {
        assert_eq!(
            ParsedModel::parse(raw)
                .match_key()
                .is_some_and(|key| key.matches_scope(&mythos_scope)),
            matches,
            "{raw}"
        );
    }

    let sonnet_scope = ParsedModel::parse("sonnet").match_key().unwrap();
    for (raw, matches) in [
        ("us.anthropic.claude-sonnet-4-5-20250929-v1:0", true),
        ("claude-sonnet-4-5", true),
        ("claude-opus-4-8", false),
    ] {
        assert_eq!(
            ParsedModel::parse(raw)
                .match_key()
                .is_some_and(|key| key.matches_scope(&sonnet_scope)),
            matches,
            "{raw}"
        );
    }

    assert!(ParsedModel::parse("gpt-4-0613").family().is_empty());
    assert_eq!(
        ParsedModel::parse("deepseek.r1-v1:0").match_key(),
        ParsedModel::parse("deepseek-r1").match_key()
    );

    let codex_5_1_scope = ParsedModel::parse("codex 5.1").match_key().unwrap();
    assert!(
        ParsedModel::parse("gpt-5.1-codex-max")
            .match_key()
            .is_some_and(|key| key.matches_scope(&codex_5_1_scope))
    );
    assert!(
        !ParsedModel::parse("gpt-5.2-codex-max")
            .match_key()
            .is_some_and(|key| key.matches_scope(&codex_5_1_scope))
    );
}

#[test]
fn refusal_key_is_a_fixed_point_of_parse_across_model_shapes() {
    let ids = [
        "gpt-5.1-codex-max",
        "gemini-3-flash-preview",
        "gemini-3-pro-preview",
        "Gemini 3.8 Flash Lite",
        "grok-4.5-build-free",
        "gpt-oss-120b",
        "kimi-for-coding",
        "gemini-pro-orchestrator-neon",
        "claude-opus-4-8",
        "claude-opus-4-8[1m]",
        "gpt-6.2-sol",
        "gpt-5.5",
        "grok-4.5",
        "deepseek-v4-pro",
        "kimi-k2.5",
        "MiniMax-M3.1-Flash-Preview",
        "o4-mini",
        "claude-3-5-haiku-20241022",
        "qwen3-coder-30b-a3b-instruct",
        "Orchestrator (Neon)",
        "Fable 5.2",
        "fable",
        "auto",
        "gemini-2.0-flash-001",
        "gemini-2.5-pro-preview-05-06",
        "claude-3-5-sonnet-v2",
        "custom:openai/gpt-oss-120b-0",
        "custom:Qwen/Qwen3-Coder-0",
        "openrouter/anthropic/claude-opus-4-8",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "global.anthropic.claude-opus-4-8-v1:0",
        "meta.llama3-3-70b-instruct-1:0",
        "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
        "gemini-2.5-flash-preview-09-2025",
        "deepseek-v3-0324",
        "gpt-4-0613",
        "claude-opus-4-8-fast",
        "grok-4-fast-non-reasoning",
        "grok-4.20-0309",
        "mistral-large-2411",
        "deepseek.r1-v1:0",
        "us.deepseek.v3.2",
        "us-gov.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "us.amazon.nova-pro-v1:0",
        "global.anthropic.claude 4.8 opus v1:0",
        "us.anthropic.claude 4.5.20250929 sonnet v1:0",
        "gemini 2.5 flash preview 09 2025",
    ];
    for id in ids {
        let key = crate::agents::model_refusal::model_key(id);
        assert_eq!(
            ParsedModel::parse(&key).match_key(),
            ParsedModel::parse(id).match_key(),
            "refusal key `{key}` for `{id}` must parse back to the same match key"
        );
    }
}

#[test]
fn window_qualifiers_and_verbatim_brackets_are_preserved() {
    for raw in [
        "claude-opus-5-5[1m]",
        "Claude Opus 5.5 (1M context)",
        "Opus 5.5 (1M)",
    ] {
        let parsed = ParsedModel::parse(raw);
        assert_eq!(parsed.render(Some("Claude")).full(), "Opus 5.5", "{raw}");
        assert_eq!(parsed.window(), Some("1M"), "{raw}");
    }
    let astra = ParsedModel::parse("gpt-6-astra[200k]");
    assert_eq!(astra.render(Some("GPT")).full(), "GPT 6 Astra");
    assert_eq!(astra.render(None).full(), "GPT 6 Astra");
    assert_eq!(astra.window(), Some("200k"));

    for raw in [
        "Sonnet 3.5 (New)",
        "Opus (é)",
        "Opus [é]",
        "Opus (18446744073709551615M)",
        "Opus (1.5M)",
    ] {
        let parsed = ParsedModel::parse(raw);
        assert_eq!(parsed.render(None).full(), raw);
        assert_eq!(parsed.window(), None);
    }
    assert_eq!(window_short(1_000_000), "1M");
    assert_eq!(window_short(1_050_000), "1M");
    assert_eq!(window_short(272_000), "272k");
    assert_eq!(window_short(523), "523");
}

#[test]
fn parse_strips_routing_prefix_and_variant_tail_for_display_and_matching() {
    let cases = [
        (
            "moonshot/kimi-k2.5",
            Some("Kimi"),
            "Kimi K2.5",
            "Kimi K2.5",
            ("kimi", Some("k2.5")),
        ),
        (
            "openrouter/anthropic/claude-opus-4-8",
            Some("Claude"),
            "Opus 4.8",
            "Claude Opus 4.8",
            ("opus", Some("4.8")),
        ),
        ("cursor/model", None, "Model", "Model", ("model", None)),
        (
            "minimax/MiniMax-M3.1-Flash-Preview#high",
            Some("MiniMax"),
            "MiniMax M3.1 Flash",
            "MiniMax M3.1 Flash",
            ("flash preview", Some("m3.1")),
        ),
        (
            "MiniMax-M3.1#fast",
            Some("MiniMax"),
            "MiniMax M3.1",
            "MiniMax M3.1",
            ("minimax", Some("m3.1")),
        ),
    ];

    for (raw, home, with_home, without_home, (key_name, key_version)) in cases {
        let parsed = ParsedModel::parse(raw);
        assert_eq!(parsed.render(home).full(), with_home, "{raw} (home)");
        assert_eq!(parsed.render(None).full(), without_home, "{raw} (full)");
        let key = parsed.match_key().expect("match key");
        assert_eq!(
            (key.name.as_str(), key.version.as_deref()),
            (key_name, key_version),
            "{raw} (match_key)"
        );
    }

    for (raw, expected) in [("auto", "auto"), ("opus", "opus"), ("fable", "fable")] {
        let key = ParsedModel::parse(raw).match_key().expect("lone match key");
        assert_eq!(key.name, expected);
        assert_eq!(key.version, None);
    }
}
