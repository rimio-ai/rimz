use super::*;

#[test]
fn parse_and_render_cover_design_examples_and_traps() {
    let cases = [
        // (raw, home_series, expected_series, expected_name, expected_full)
        ("Gemini 4 Neon", Some("Gemini"), None, "Neon 4", "Neon 4"),
        (
            "Gemini 4 Neon",
            None,
            Some("Gemini"),
            "Neon 4",
            "Gemini · Neon 4",
        ),
        (
            "gemini-4-carbon",
            Some("Gemini"),
            None,
            "Carbon 4",
            "Carbon 4",
        ),
        ("gpt-6.2-sol", Some("GPT"), None, "Sol 6.2", "Sol 6.2"),
        ("gpt-6.2-sol", None, Some("GPT"), "Sol 6.2", "GPT · Sol 6.2"),
        (
            "claude-opus-4-8",
            Some("Claude"),
            None,
            "Opus 4.8",
            "Opus 4.8",
        ),
        (
            "claude-opus-4-8",
            Some("Gemini"),
            Some("Claude"),
            "Opus 4.8",
            "Claude · Opus 4.8",
        ),
        (
            "claude-opus-4-8",
            None,
            Some("Claude"),
            "Opus 4.8",
            "Claude · Opus 4.8",
        ),
        ("gpt-5.5", Some("GPT"), None, "GPT 5.5", "GPT 5.5"),
        ("gpt-5.5", None, None, "GPT 5.5", "GPT 5.5"),
        ("grok-4.5", Some("Grok"), None, "Grok 4.5", "Grok 4.5"),
        ("grok-4.5", None, None, "Grok 4.5", "Grok 4.5"),
        (
            "gpt-5.1-codex-max",
            Some("GPT"),
            None,
            "Codex Max 5.1",
            "Codex Max 5.1",
        ),
        (
            "gpt-5.1-codex-max",
            None,
            Some("GPT"),
            "Codex Max 5.1",
            "GPT · Codex Max 5.1",
        ),
        (
            "gemini-3.8-flash-lite",
            Some("Gemini"),
            None,
            "Flash Lite 3.8",
            "Flash Lite 3.8",
        ),
        (
            "gemini-3.8-flash-lite",
            None,
            Some("Gemini"),
            "Flash Lite 3.8",
            "Gemini · Flash Lite 3.8",
        ),
        (
            "deepseek-v4-pro",
            None,
            Some("DeepSeek"),
            "Pro V4",
            "DeepSeek · Pro V4",
        ),
        ("kimi-k2.5", Some("Kimi"), None, "Kimi K2.5", "Kimi K2.5"),
        ("kimi-k2.5", None, None, "Kimi K2.5", "Kimi K2.5"),
        (
            "MiniMax-M3.1-Flash-Preview",
            Some("MiniMax"),
            None,
            "Flash Preview M3.1",
            "Flash Preview M3.1",
        ),
        (
            "MiniMax-M3.1-Flash-Preview",
            None,
            Some("MiniMax"),
            "Flash Preview M3.1",
            "MiniMax · Flash Preview M3.1",
        ),
        ("o4-mini", Some("GPT"), None, "O4 Mini", "O4 Mini"),
        ("o4-mini", None, None, "O4 Mini", "O4 Mini"),
        (
            "o3-mini-high",
            Some("GPT"),
            None,
            "O3 Mini High",
            "O3 Mini High",
        ),
        ("o3-mini-high", None, None, "O3 Mini High", "O3 Mini High"),
        (
            "claude-3-5-haiku-20241022",
            Some("Claude"),
            None,
            "Haiku 3.5",
            "Haiku 3.5",
        ),
        (
            "claude-3-5-haiku-20241022",
            None,
            Some("Claude"),
            "Haiku 3.5",
            "Claude · Haiku 3.5",
        ),
        (
            "kimi-for-coding",
            Some("Kimi"),
            None,
            "For Coding",
            "For Coding",
        ),
        (
            "kimi-for-coding",
            None,
            Some("Kimi"),
            "For Coding",
            "Kimi · For Coding",
        ),
        (
            "gemini-pro-orchestrator-neon",
            Some("Gemini"),
            None,
            "Pro Orchestrator Neon",
            "Pro Orchestrator Neon",
        ),
        ("auto", Some("GPT"), None, "Auto", "Auto"),
        ("opus", Some("Claude"), None, "Opus", "Opus"),
        ("high", None, None, "High", "High"),
        (
            "Orchestrator (Neon)",
            Some("Gemini"),
            None,
            "Orchestrator (Neon)",
            "Orchestrator (Neon)",
        ),
        ("gpt-5-2025-08-07", Some("GPT"), None, "GPT 5", "GPT 5"),
        ("gpt-5-2025-08-07", None, None, "GPT 5", "GPT 5"),
        ("gpt-4o-mini", Some("GPT"), None, "4o Mini", "4o Mini"),
        ("gpt-4o-mini", None, Some("GPT"), "4o Mini", "GPT · 4o Mini"),
        ("gpt-oss-120b", Some("GPT"), None, "Oss 120b", "Oss 120b"),
        (
            "qwen3-coder-30b-a3b-instruct",
            Some("Qwen"),
            None,
            "Coder 30b A3b Instruct 3",
            "Coder 30b A3b Instruct 3",
        ),
        (
            "qwen3-coder-30b-a3b-instruct",
            None,
            Some("Qwen"),
            "Coder 30b A3b Instruct 3",
            "Qwen · Coder 30b A3b Instruct 3",
        ),
        ("hy4", None, None, "Hy 4", "Hy 4"),
        ("qwen3", Some("Qwen"), None, "Qwen 3", "Qwen 3"),
        (
            "mystery-model",
            None,
            Some("Mystery"),
            "Model",
            "Mystery · Model",
        ),
        (
            "Legacy Model",
            None,
            Some("Legacy"),
            "Model",
            "Legacy · Model",
        ),
        (
            "codex-auto-review",
            Some("GPT"),
            Some("Codex"),
            "Auto Review",
            "Codex · Auto Review",
        ),
        (
            "Fable 5.1@high",
            Some("Claude"),
            None,
            "Fable 5.1",
            "Fable 5.1",
        ),
        (
            "claude-opus-4-8[1m]@xhigh",
            Some("Claude"),
            None,
            "Opus 4.8",
            "Opus 4.8",
        ),
        (
            "custom:DeepSeek-V4-Pro-0",
            None,
            Some("DeepSeek"),
            "Pro V4",
            "DeepSeek · Pro V4",
        ),
        (
            "custom:openai/gpt-oss-120b-0",
            None,
            Some("GPT"),
            "Oss 120b",
            "GPT · Oss 120b",
        ),
        (
            "custom:Qwen/Qwen3-Coder-0",
            None,
            Some("Qwen"),
            "Coder 3",
            "Qwen · Coder 3",
        ),
        (
            "custom:Qwen/Qwen3-Coder-0",
            Some("Qwen"),
            None,
            "Coder 3",
            "Coder 3",
        ),
        ("", None, None, "", ""),
    ];

    let actual: Vec<_> = cases
        .iter()
        .map(|(raw, home, _, _, _)| {
            let rendered = ParsedModel::parse(raw).render(*home);
            (
                rendered.series.clone(),
                rendered.name.clone(),
                rendered.full(),
            )
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, _, series, name, full)| {
            (
                series.map(str::to_owned),
                (*name).to_owned(),
                (*full).to_owned(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
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
    assert_eq!(astra.render(Some("GPT")).full(), "Astra 6");
    assert_eq!(astra.render(None).full(), "GPT · Astra 6");
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
            "Claude · Opus 4.8",
            ("opus", Some("4.8")),
        ),
        ("cursor/model", None, "Model", "Model", ("model", None)),
        (
            "minimax/MiniMax-M3.1-Flash-Preview#high",
            Some("MiniMax"),
            "Flash Preview M3.1",
            "MiniMax · Flash Preview M3.1",
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
