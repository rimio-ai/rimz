use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn delegated_readings_require_a_metered_quota_surface() {
    for (kind, provider, expected) in [
        ("pi", "openai", false),
        ("pi", "openai-codex", true),
        ("pi", "anthropic", true),
        ("opencode", "openai", true),
        ("opencode", "anthropic", true),
        ("opencode", "other", false),
    ] {
        let kind = crate::ids::AgentKind::new_unchecked(kind);
        assert_eq!(
            expects_usage_reading(&kind, Some(true), Some(provider)),
            expected,
            "{kind}/{provider}"
        );
        assert!(!expects_usage_reading(&kind, Some(false), Some(provider)));
        assert!(!expects_usage_reading(&kind, None, Some(provider)));
    }
}

#[test]
fn undeclared_sub_provider_quota_surfaces_do_not_expect_readings() {
    let kind = crate::ids::AgentKind::new_unchecked("copilot");
    assert!(expects_usage_reading(&kind, Some(true), None));
    assert!(!expects_usage_reading(&kind, Some(true), Some("openai")));
}

#[test]
fn reading_expectations_keep_static_windows_and_direct_metered_accounts() {
    for kind in ["claude", "codex", "kimi"] {
        let kind = crate::ids::AgentKind::new_unchecked(kind);
        assert!(expects_usage_reading(&kind, None, None), "{kind}");
        assert!(!expects_usage_reading(&kind, Some(false), None), "{kind}");
    }
    for (kind, expected) in [
        ("copilot", true),
        ("qwen", true),
        ("antigravity", true),
        ("grok", false),
        ("cursor", false),
        ("amp", false),
    ] {
        let kind = crate::ids::AgentKind::new_unchecked(kind);
        assert_eq!(
            expects_usage_reading(&kind, Some(true), None),
            expected,
            "{kind}"
        );
        assert!(!expects_usage_reading(&kind, None, None), "{kind}");
    }
    let qwen = crate::ids::AgentKind::new_unchecked("qwen");
    for provider in [
        "Alibaba Coding Plan (International)",
        "Alibaba Coding Plan (China)",
    ] {
        assert!(expects_usage_reading(&qwen, Some(true), Some(provider)));
    }
}

fn config(adapter: Adapter, used: Option<&str>, domain: &'static [u8]) -> Config {
    let used = used.map(str::to_owned);
    Config {
        adapter,
        load_credentials: Box::new(|| Credentials {
            stamp: None,
            entries: Ok(BTreeMap::new()),
        }),
        used_provider: Box::new(move || {
            used.clone()
                .map_or(UsedProvider::Silent, UsedProvider::Named)
        }),
        api_key_types: match adapter {
            Adapter::Pi => &["api_key"],
            Adapter::OpenCode => &["key"],
        },
        account_key_domain: domain,
    }
}

fn future_ms() -> u64 {
    unix_now_ms() + 60_000
}

#[test]
fn go_sessions_link_console_credentials_but_exact_credentials_win() {
    let outcomes = [None, Some("oauth"), Some("key")].map(|exact_kind| {
        let mut body = serde_json::json!({
            "anthropic": {"type":"oauth", "access":"a", "expires":future_ms()},
            "opencode": {"type":"oauth", "access":"c", "expires":future_ms()}
        });
        if let Some(kind) = exact_kind {
            body["opencode-go"] = serde_json::json!({
                "type":kind, "access":"exact", "expires":future_ms()
            });
        }
        let mut config = config(Adapter::OpenCode, Some("opencode-go"), b"open");
        let display_body = body.clone();
        config.load_credentials = Box::new(move || Credentials {
            stamp: Some(42),
            entries: Ok(serde_json::from_value(display_body.clone()).unwrap()),
        });
        let AccountProbe::Found(account) = probe_account(&config) else {
            panic!("Go session should select a stored credential")
        };
        let usage = select_usage(body.to_string().as_bytes(), &config)
            .map(|selected| selected.provider)
            .map_err(|error| matches!(error, Error::ApiKeyOnly { .. }));
        (account.sub_provider, usage)
    });
    assert_eq!(
        outcomes,
        [
            (Some("opencode".to_owned()), Ok("opencode".to_owned())),
            (Some("opencode-go".to_owned()), Ok("opencode-go".to_owned())),
            (Some("opencode-go".to_owned()), Err(true)),
        ]
    );
}

#[test]
fn console_oauth_has_display_only_scope_and_leaves_the_plan_to_usage() {
    let surfaces =
        [Adapter::OpenCode, Adapter::Pi].map(|adapter| adapter.has_quota_surface("opencode"));
    assert_eq!(surfaces, [true, false]);
    let mut config = config(Adapter::OpenCode, Some("opencode"), b"open");
    config.load_credentials = Box::new(|| Credentials {
        stamp: Some(42),
        entries: Ok(serde_json::from_str(r#"{"opencode":{"type":"oauth"}}"#).unwrap()),
    });
    let AccountProbe::Found(account) = probe_account(&config) else {
        panic!("Console credential should select an account")
    };
    assert_eq!(
        account.scope,
        ProviderAccountScope::sub_provider("opencode", "oauth")
    );
    assert_eq!(account.plan, None);
    assert_eq!(account.metered, Some(true));
    let kind = crate::ids::AgentKind::new_unchecked("opencode");
    assert!(expects_usage_reading(&kind, Some(true), Some("opencode")));
    assert!(!expects_usage_reading(&kind, Some(false), Some("opencode")));
}

#[test]
fn console_org_changes_account_identity_without_exposing_it() {
    let keys = [
        None,
        Some(""),
        Some("private-org-one"),
        Some("private-org-two"),
    ]
    .map(|org| {
        let body = serde_json::json!({"opencode": {
            "type":"oauth", "access":"same-token", "refresh":"stable-refresh",
            "expires":future_ms(), "metadata":{"orgID":org}
        }});
        let selected = select_usage(
            body.to_string().as_bytes(),
            &config(Adapter::OpenCode, Some("opencode"), b"open"),
        )
        .unwrap();
        selected.account_key
    });
    assert_eq!(keys[0], keys[1]);
    assert_ne!(
        keys[2], keys[3],
        "a Console org switch changes the cache owner"
    );
    for key in keys {
        assert_eq!(key.len(), 64);
        assert!(!key.contains("private-org"));
        assert!(!key.contains("same-token"));
    }
}

#[test]
fn pi_opencode_display_selection_preserves_exact_labels_and_leaves_silence_undecided() {
    let credentials: BTreeMap<String, Credential> = serde_json::from_str(
        r#"{
            "anthropic":{"type":"oauth"},
            "openai":{"type":"key"},
            "opencode":{"type":"external"}
        }"#,
    )
    .unwrap();
    let selected = select_display(&credentials, Some("openai"), Adapter::OpenCode).unwrap();
    assert_eq!(selected.0, "openai");
    assert_eq!(
        provider_label(
            &config(Adapter::OpenCode, None, b"open"),
            selected.0,
            selected.1.kind.as_deref()
        ),
        "OpenAI API Key"
    );
    assert_eq!(
        provider_label(
            &config(Adapter::OpenCode, None, b"open"),
            "opencode",
            Some("external")
        ),
        "OpenCode External"
    );
    assert_eq!(
        provider_label(&config(Adapter::Pi, None, b"pi"), "custom", None),
        "custom"
    );
    assert_eq!(
        provider_label(&config(Adapter::Pi, None, b"pi"), "opencode", None),
        "opencode"
    );
    assert!(select_display(&credentials, None, Adapter::OpenCode).is_none());
}

#[test]
fn classification_covers_named_fallback_and_silent_for_both_adapters() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for adapter in [Adapter::Pi, Adapter::OpenCode] {
        for (resolution, count, provider, plan, metered, provisional) in [
            (
                UsedProvider::Named("anthropic".into()),
                2,
                Some("anthropic"),
                Some("Anthropic API Key"),
                Some(false),
                false,
            ),
            (
                UsedProvider::Named("openai".into()),
                2,
                Some("openai"),
                Some("OpenAI OAuth"),
                Some(true),
                false,
            ),
            (
                UsedProvider::Named("unlisted".into()),
                2,
                Some("unlisted"),
                None,
                Some(false),
                false,
            ),
            (
                UsedProvider::Fallback("openai".into()),
                2,
                Some("openai"),
                Some("OpenAI OAuth"),
                Some(true),
                true,
            ),
            (
                UsedProvider::Fallback("unlisted".into()),
                2,
                Some("unlisted"),
                None,
                Some(false),
                true,
            ),
            (
                UsedProvider::Fallback("anthropic".into()),
                1,
                Some("anthropic"),
                Some("Anthropic API Key"),
                Some(false),
                false,
            ),
            (
                UsedProvider::Silent,
                1,
                Some("anthropic"),
                Some("Anthropic API Key"),
                Some(false),
                false,
            ),
            (UsedProvider::Silent, 2, None, None, None, true),
        ] {
            let key_type = if adapter == Adapter::Pi {
                "api_key"
            } else {
                "key"
            };
            let mut body = serde_json::json!({"anthropic": {"type": key_type}});
            if count == 2 {
                body["openai"] = serde_json::json!({"type": "oauth"});
            }
            let mut config = config(adapter, None, b"test");
            config.used_provider = Box::new(move || resolution.clone());
            config.load_credentials = Box::new(move || Credentials {
                stamp: Some(42),
                entries: Ok(serde_json::from_value(body.clone()).unwrap()),
            });
            let AccountProbe::Found(account) = probe_account(&config) else {
                panic!("nonempty credentials should be found")
            };
            actual.push((
                account.sub_provider,
                account.plan,
                account.metered,
                account.provisional,
            ));
            expected.push((
                provider.map(str::to_owned),
                plan.map(str::to_owned),
                metered,
                provisional,
            ));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn credentials_classify_every_provider_without_nesting_or_exposing_secrets() {
    let mut config = config(Adapter::Pi, Some("openai-codex"), b"pi");
    config.load_credentials = Box::new(|| Credentials {
        stamp: Some(42),
        entries: Ok(serde_json::from_value(serde_json::json!({
            "anthropic": {"type": "api_key", "key": "private-key"},
            "openai-codex": {"type": "oauth", "access": "private-token", "accountId": "acct"}
        }))
        .unwrap()),
    });
    let AccountProbe::Found(account) = probe_account(&config) else {
        panic!("account")
    };
    assert_eq!(account.credentials.len(), 2);
    let api = &account.credentials["anthropic"];
    let oauth = &account.credentials["openai-codex"];
    assert_eq!(api.plan.as_deref(), Some("Anthropic API Key"));
    assert_eq!(api.metered, Some(false));
    assert!(api.scope.is_kind_wide());
    assert_eq!(
        oauth.scope,
        ProviderAccountScope::sub_provider("openai", "oauth")
    );
    assert_eq!(oauth.account_key.as_deref(), Some("acct"));
    assert!(
        account
            .credentials
            .values()
            .all(|credential| !credential.provisional && credential.credentials.is_empty())
    );
    let encoded = serde_json::to_string(&account).unwrap();
    assert!(!encoded.contains("private-"));
    assert_eq!(
        serde_json::from_str::<AgentAccount>(&encoded).unwrap(),
        account
    );
}

#[test]
fn pi_opencode_usage_selection_keeps_scope_expiry_identity_and_adapter_domains() {
    let json = format!(
        r#"{{
            "anthropic":{{"type":"oauth","access":"a","refresh":"stable","expires":{}}},
            "openai":{{"type":"oauth","access":"o","expires":{},"accountId":"acct"}}
        }}"#,
        future_ms(),
        future_ms()
    );
    let pi = select_usage(json.as_bytes(), &config(Adapter::Pi, Some("openai"), b"pi")).unwrap();
    assert_eq!(
        pi.account_key,
        hashed_account_key(b"pi", "access-token", "o")
    );
    assert_eq!(pi.scope, ProviderAccountScope::default());
    let opencode = select_usage(
        json.as_bytes(),
        &config(Adapter::OpenCode, Some("openai"), b"open"),
    )
    .unwrap();
    assert_eq!(opencode.account_key, "acct");
    assert_eq!(
        opencode.scope,
        ProviderAccountScope::sub_provider("openai", "oauth")
    );
    let codex = format!(
        r#"{{"openai-codex":{{"type":"oauth","access":"c","expires":{},"accountId":"codex-acct"}}}}"#,
        future_ms()
    );
    let codex = select_usage(codex.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
    assert_eq!(codex.account_key, "codex-acct");
    assert_eq!(
        codex.scope,
        ProviderAccountScope::sub_provider("openai", "oauth")
    );

    let first = select_usage(
        json.as_bytes(),
        &config(Adapter::Pi, Some("anthropic"), b"pi"),
    )
    .unwrap();
    let rotated = format!(
        r#"{{"anthropic":{{"type":"oauth","access":"rotated","refresh":"stable","expires":{}}}}}"#,
        future_ms()
    );
    let rotated = select_usage(rotated.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
    let access_only = format!(
        r#"{{"anthropic":{{"type":"oauth","access":"access-only","expires":{}}}}}"#,
        future_ms()
    );
    let access_only =
        select_usage(access_only.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
    let other = select_usage(
        json.as_bytes(),
        &config(Adapter::OpenCode, Some("anthropic"), b"opencode"),
    )
    .unwrap();
    assert_eq!(first.provider, "anthropic");
    assert_eq!(first.account_key, rotated.account_key);
    assert_ne!(first.account_key, access_only.account_key);
    assert_ne!(first.account_key, other.account_key);
    assert_eq!(first.account_key.len(), 64);
    assert!(!first.account_key.contains("stable"));

    let expired = br#"{"anthropic":{"type":"oauth","access":"a","expires":1}}"#;
    assert!(matches!(
        select_usage(expired, &config(Adapter::Pi, None, b"pi")),
        Err(Error::TokenExpired { .. })
    ));
}

#[test]
fn pi_opencode_usage_selection_handles_api_key_unsupported_empty_and_malformed_maps() {
    assert!(matches!(
        select_usage(
            br#"{"openai":{"type":"api_key","key":"k"}}"#,
            &config(Adapter::Pi, Some("openai"), b"pi")
        ),
        Err(Error::ApiKeyOnly { .. })
    ));
    assert!(matches!(
        select_usage(br#"{}"#, &config(Adapter::OpenCode, None, b"open")),
        Err(Error::NoCredentials { .. })
    ));
    assert!(matches!(
        select_usage(b"not json", &config(Adapter::Pi, None, b"pi")),
        Err(Error::Parse { .. })
    ));
    let outcomes: Vec<_> = [Adapter::Pi, Adapter::OpenCode]
        .into_iter()
        .flat_map(|adapter| {
            let credentials = serde_json::from_value(serde_json::json!({
                "anthropic": {"type": "oauth", "access": "a", "expires": future_ms()},
                "openai": {"type": "oauth", "access": "o", "expires": future_ms()}
            }))
            .unwrap();
            let config = config(adapter, None, b"test");
            [None, Some("openai"), Some("unlisted")].map(|provider| {
                select_usage_entries(&credentials, &config, provider)
                    .ok()
                    .map(|selected| selected.provider)
            })
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            None,
            Some("openai".to_owned()),
            None,
            None,
            Some("openai".to_owned()),
            None
        ]
    );
}

#[test]
fn pi_opencode_account_probe_handles_files_and_adapter_specific_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut pi = config(Adapter::Pi, Some("openai"), b"pi");
    let auth_path = path.clone();
    pi.load_credentials = Box::new(move || load_auth_file(&auth_path, Adapter::Pi));
    assert!(matches!(probe_account(&pi), AccountProbe::LoggedOut));

    std::fs::write(&path, b"not json").unwrap();
    assert!(matches!(
        probe_account(&pi),
        AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
    ));

    std::fs::write(
        &path,
        br#"{
            "anthropic":{"type":"oauth"},
            "openai":{"type":"api_key"},
            "opencode":{"type":"external"}
        }"#,
    )
    .unwrap();
    let AccountProbe::Found(account) = probe_account(&pi) else {
        panic!("pi account")
    };
    assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
    assert_eq!(account.metered, Some(false));
    assert!(account.credentials_updated_at_ms.is_some());

    std::fs::write(
        &path,
        format!(
            r#"{{"openai":{{"type":"oauth","access":"o","expires":{},"accountId":"acct"}}}}"#,
            future_ms()
        ),
    )
    .unwrap();
    let AccountProbe::Found(account) = probe_account(&pi) else {
        panic!("pi OAuth account")
    };
    assert_eq!(account.plan.as_deref(), Some("OpenAI OAuth"));
    assert_eq!(account.metered, Some(true));
    assert_eq!(account.scope, ProviderAccountScope::default());

    std::fs::write(
        &path,
        br#"{
            "anthropic":{"type":"oauth"},
            "openai":{"type":"api_key"},
            "opencode":{"type":"external"}
        }"#,
    )
    .unwrap();

    let mut opencode = config(Adapter::OpenCode, Some("opencode"), b"open");
    opencode.load_credentials = Box::new(move || load_auth_file(&path, Adapter::OpenCode));
    let AccountProbe::Found(account) = probe_account(&opencode) else {
        panic!("opencode account")
    };
    assert_eq!(account.plan.as_deref(), Some("OpenCode External"));
    assert_eq!(account.metered, None);
}

static RESOLVER_CALLS: AtomicUsize = AtomicUsize::new(0);

fn counted_openai_provider() -> UsedProvider {
    RESOLVER_CALLS.fetch_add(1, Ordering::Relaxed);
    UsedProvider::Named("openai".to_owned())
}

#[test]
fn active_provider_resolves_only_after_valid_nonempty_auth() {
    RESOLVER_CALLS.store(0, Ordering::Relaxed);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut config = config(Adapter::OpenCode, None, b"open");
    let auth_path = path.clone();
    config.load_credentials = Box::new(move || load_auth_file(&auth_path, Adapter::OpenCode));
    config.used_provider = Box::new(counted_openai_provider);

    assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
    std::fs::write(&path, b"not json").unwrap();
    assert!(matches!(
        probe_account(&config),
        AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
    ));
    std::fs::write(&path, b"{}").unwrap();
    assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
    assert_eq!(RESOLVER_CALLS.load(Ordering::Relaxed), 0);

    std::fs::write(
        &path,
        br#"{"anthropic":{"type":"oauth"},"openai":{"type":"key"}}"#,
    )
    .unwrap();
    let AccountProbe::Found(account) = probe_account(&config) else {
        panic!("valid credentials should select an account")
    };
    assert_eq!(account.sub_provider.as_deref(), Some("openai"));
    assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
    assert_eq!(RESOLVER_CALLS.load(Ordering::Relaxed), 1);
}
