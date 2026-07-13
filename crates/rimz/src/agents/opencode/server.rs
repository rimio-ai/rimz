//! OpenCode embedded-server HTTP reader.
//!
//! The in-process plugin owns the only discovery handle for a TUI launch's
//! random-port server: `PluginInput.serverUrl`. The hook envelope carries that
//! URL to `rimz opencode refresh-context`, and this module performs read-only,
//! best-effort HTTP reads against it. Failures omit fields; they never fail the
//! hook helper.

use std::collections::HashMap;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::AgentContext;

const TIMEOUT_SECS: u64 = 2;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const PASSWORD_ENV: &str = "OPENCODE_SERVER_PASSWORD";
const USERNAME_ENV: &str = "OPENCODE_SERVER_USERNAME";

pub fn observe(
    server_url: &str,
    session_id: Option<&str>,
    model_hint: Option<&str>,
    observed_at: Timestamp,
) -> AgentContext {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build()
        .new_agent();
    let auth = basic_auth_header();
    let health = get_json(
        &agent,
        &endpoint(server_url, "global/health"),
        auth.as_deref(),
    );
    let providers = get_json(
        &agent,
        &endpoint(server_url, "config/providers"),
        auth.as_deref(),
    );
    let session = session_id.filter(|id| !id.is_empty()).and_then(|id| {
        get_json(
            &agent,
            &endpoint(server_url, &format!("session/{}", path_segment(id))),
            auth.as_deref(),
        )
    });
    into_context(
        health.as_ref(),
        providers.as_ref(),
        session.as_ref(),
        model_hint,
        observed_at,
    )
}

fn get_json(agent: &ureq::Agent, url: &str, auth: Option<&str>) -> Option<Value> {
    let mut request = agent.get(url).header("Accept", "application/json");
    if let Some(auth) = auth {
        request = request.header("Authorization", auth);
    }
    let mut response = request.call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
        .ok()?;
    serde_json::from_str(&body).ok()
}

fn endpoint(server_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        server_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn basic_auth_header() -> Option<String> {
    let password = std::env::var(PASSWORD_ENV)
        .ok()
        .filter(|value| !value.is_empty())?;
    let username = std::env::var(USERNAME_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "opencode".to_owned());
    Some(format!(
        "Basic {}",
        base64(&format!("{username}:{password}"))
    ))
}

fn base64(input: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex(byte >> 4));
            out.push(hex(byte & 0x0f));
        }
    }
    out
}

fn hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

fn into_context(
    health: Option<&Value>,
    providers: Option<&Value>,
    session: Option<&Value>,
    model_hint: Option<&str>,
    observed_at: Timestamp,
) -> AgentContext {
    AgentContext {
        source: "opencode".to_owned(),
        session_name: session.and_then(session_name),
        session_preview: None,
        model_id: None,
        model_display_name: providers.and_then(|body| model_display_name(body, model_hint)),
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: health.and_then(agent_version),
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        turn_interrupted: None,
        observed_at,
    }
}

#[derive(Debug, Default, Deserialize)]
struct Health {
    #[serde(default)]
    version: Option<String>,
}

fn agent_version(body: &Value) -> Option<String> {
    let body = body.get("data").unwrap_or(body);
    let parsed: Health = serde_json::from_value(body.clone()).ok()?;
    nonempty(parsed.version)
}

#[derive(Debug, Default, Deserialize)]
struct Providers {
    #[serde(default)]
    providers: Vec<Provider>,
}

#[derive(Debug, Default, Deserialize)]
struct Provider {
    #[serde(default)]
    id: String,
    #[serde(default)]
    models: HashMap<String, Model>,
}

#[derive(Debug, Default, Deserialize)]
struct Model {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "providerID")]
    provider_id: String,
    #[serde(default)]
    name: String,
}

fn model_display_name(body: &Value, model_hint: Option<&str>) -> Option<String> {
    let hint = model_hint.map(str::trim).filter(|hint| !hint.is_empty())?;
    let body = body.get("data").unwrap_or(body);
    let parsed: Providers = serde_json::from_value(body.clone()).ok()?;
    parsed.providers.into_iter().find_map(|provider| {
        provider.models.into_iter().find_map(|(key, model)| {
            let provider_id = nonempty_ref(&model.provider_id).unwrap_or(provider.id.as_str());
            if model_matches(hint, provider_id, &key, &model.id) {
                nonempty(Some(model.name))
            } else {
                None
            }
        })
    })
}

fn model_matches(hint: &str, provider_id: &str, key: &str, model_id: &str) -> bool {
    let model_id = nonempty_ref(model_id).unwrap_or(key);
    if let Some((hint_provider, hint_model)) = hint.split_once('/') {
        return hint_provider == provider_id && (hint_model == key || hint_model == model_id);
    }
    hint == key || hint == model_id
}

#[derive(Debug, Default, Deserialize)]
struct Session {
    #[serde(default)]
    title: Option<String>,
}

fn session_name(body: &Value) -> Option<String> {
    let body = body.get("data").unwrap_or(body);
    let parsed: Session = serde_json::from_value(body.clone()).ok()?;
    nonempty(parsed.title)
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn nonempty_ref(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn into_context_projects_server_bodies() {
        let observed_at = Timestamp::from_second(1_700_000_000).unwrap();
        let context = into_context(
            Some(&json!({ "healthy": true, "version": "1.17.9" })),
            Some(&json!({
                "providers": [
                    {
                        "id": "openai",
                        "models": {
                            "gpt-5": {
                                "id": "gpt-5",
                                "providerID": "openai",
                                "name": "GPT-5"
                            }
                        }
                    }
                ],
                "default": {}
            })),
            Some(&json!({
                "id": "ses_1",
                "title": "Fix auth"
            })),
            Some("openai/gpt-5"),
            observed_at,
        );

        assert_eq!(context.source, "opencode");
        assert_eq!(context.agent_version.as_deref(), Some("1.17.9"));
        assert_eq!(context.model_display_name.as_deref(), Some("GPT-5"));
        assert_eq!(context.session_name.as_deref(), Some("Fix auth"));
        assert!(context.session_preview.is_none());
        assert_eq!(context.observed_at, observed_at);
    }

    #[test]
    fn into_context_omits_missing_or_garbage_fields() {
        let observed_at = Timestamp::from_second(1_700_000_000).unwrap();
        let context = into_context(
            Some(&json!("bad")),
            Some(&json!({ "providers": [] })),
            Some(&json!({ "title": "  " })),
            Some("gpt-5"),
            observed_at,
        );

        assert!(context.agent_version.is_none());
        assert!(context.model_display_name.is_none());
        assert!(context.session_name.is_none());
        assert!(context.session_preview.is_none());
        assert_eq!(context.observed_at, observed_at);
    }

    #[test]
    fn matching_accepts_bare_model_ids() {
        let body = json!({
            "providers": [
                {
                    "id": "openai",
                    "models": {
                        "gpt-5": { "id": "gpt-5", "name": "GPT-5" }
                    }
                }
            ]
        });
        assert_eq!(
            model_display_name(&body, Some("gpt-5")).as_deref(),
            Some("GPT-5")
        );
    }

    #[test]
    fn auth_and_path_encoding_are_pinned() {
        assert_eq!(base64("opencode:secret"), "b3BlbmNvZGU6c2VjcmV0");
        assert_eq!(path_segment("ses 1/a"), "ses%201%2Fa");
    }
}
