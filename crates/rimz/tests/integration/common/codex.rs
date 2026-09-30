//! Codex trust setup for tests that drive a real Codex launch: the hook
//! trust entries Codex writes after the user trusts via `/hooks`, and the
//! project trust level Codex records per checkout.

use std::path::Path;

use super::Env;

/// Every Codex hook event RimZ installs, as the token Codex keys trust by.
const CODEX_HOOK_EVENTS: [&str; 11] = [
    "session_start",
    "user_prompt_submit",
    "subagent_start",
    "subagent_stop",
    "stop",
    "interrupt",
    "permission_request",
    "pre_tool_use",
    "post_tool_use",
    "pre_compact",
    "post_compact",
];

/// Append `[hooks.state]` trust entries for every RimZ-installed Codex event,
/// key-shaped exactly as Codex writes them after the user trusts via /hooks.
pub fn trust_codex_hooks(env: &Env) {
    trust_codex_hooks_except(env, None);
}

/// Trust every hook but the forward-compatible Interrupt event, so preflight
/// sees the advisory it reports for an untrusted Interrupt hook.
pub fn trust_codex_preflight_hooks(env: &Env) {
    trust_codex_hooks_except(env, Some("interrupt"));
}

fn trust_codex_hooks_except(env: &Env, excluded: Option<&str>) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    for token in CODEX_HOOK_EVENTS {
        if Some(token) == excluded {
            continue;
        }
        text.push_str(&format!(
            "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
            config.display(),
        ));
    }
    std::fs::write(&config, text).expect("write trust state");
}

/// Record `project` as trusted in the Codex config, replacing any
/// `[projects]` table already there.
pub fn trust_codex_project(env: &Env, project: &Path) {
    let config = env.agent_config_path("codex");
    let mut table: toml::Table = std::fs::read_to_string(&config)
        .expect("read codex config")
        .parse()
        .expect("parse codex config");
    table.insert(
        "projects".to_owned(),
        toml::Value::Table(toml::Table::from_iter([(
            project.display().to_string(),
            toml::Value::Table(toml::Table::from_iter([(
                "trust_level".to_owned(),
                toml::Value::String("trusted".to_owned()),
            )])),
        )])),
    );
    std::fs::write(&config, toml::to_string(&table).expect("serialize trust"))
        .expect("write project trust");
}
