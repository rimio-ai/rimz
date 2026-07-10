//! Best-effort Qwen provider and credential-source presence probe.

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

#[derive(Default, Deserialize)]
#[serde(default)]
struct Settings {
    security: Security,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Security {
    auth: Auth,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Auth {
    selected_type: Option<String>,
}

fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

pub(crate) fn probe() -> AccountProbe {
    let Ok(path) = super::install::qwen_settings_path() else {
        return AccountProbe::Unavailable;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return AccountProbe::Unavailable;
    };
    let Ok(settings) = serde_json::from_slice::<Settings>(&bytes) else {
        return AccountProbe::LoggedOut;
    };
    let Some(provider) = settings
        .security
        .auth
        .selected_type
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return AccountProbe::LoggedOut;
    };
    let present = match provider {
        "openai" => env_present("OPENAI_API_KEY"),
        "anthropic" => env_present("ANTHROPIC_API_KEY"),
        "gemini" | "vertex-ai" => env_present("GEMINI_API_KEY") || env_present("GOOGLE_API_KEY"),
        "bailian" | "qwen" | "qwen-coding-plan" => env_present("BAILIAN_CODING_PLAN_API_KEY"),
        _ => false,
    };
    if !present {
        return AccountProbe::LoggedOut;
    }
    AccountProbe::Found(AgentAccount {
        plan: Some(provider.to_owned()),
        account_id: None,
        metered: None,
        version: None,
        sub_provider: Some(provider.to_owned()),
    })
}
