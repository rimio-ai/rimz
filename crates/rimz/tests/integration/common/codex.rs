//! Codex trust setup for tests that drive a real Codex launch: the hook
//! trust entries Codex writes after the user trusts via `/hooks`, and the
//! project trust level Codex records per checkout.

use std::path::Path;

use super::Env;

/// Write native trust hashes for every RimZ-installed Codex event.
pub fn trust_codex_hooks(env: &Env) {
    rimz::agents::trust_hook_config_for_test(&env.agent_config_path("codex"), &[])
        .expect("trust installed hooks");
}

/// Trust every hook but the forward-compatible Interrupt event, so preflight
/// sees the advisory it reports for an untrusted Interrupt hook.
pub fn trust_codex_preflight_hooks(env: &Env) {
    rimz::agents::trust_hook_config_for_test(&env.agent_config_path("codex"), &["Interrupt"])
        .expect("trust installed preflight hooks");
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
