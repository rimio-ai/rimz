use std::path::PathBuf;

use super::{
    ResumePromptMode, preflight_account_budgets, resume_prompt_mode, write_project_trust_offer_to,
};

use rimz::trust::{BirthPromptOffer, SurfaceSummary};

#[test]
fn account_budget_preflight_propagates_only_unsupported_caps() {
    let path = PathBuf::from("/tmp/config.toml");
    let account_error = rimz::config::ConfigErr::AccountBudget {
        path: path.clone(),
        source: rimz::config::AccountBudgetConfigError::Unsupported {
            kind: "cursor".to_owned(),
        },
    };
    assert!(preflight_account_budgets(Err(account_error)).is_err());

    let unrelated = rimz::config::ConfigErr::Io {
        path,
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    };
    assert!(preflight_account_budgets(Err(unrelated)).is_ok());
}

#[test]
fn resume_prompt_mode_uses_tty_or_confirm_flag() {
    assert_eq!(
        resume_prompt_mode(false, true),
        ResumePromptMode::Interactive
    );
    assert_eq!(
        resume_prompt_mode(true, false),
        ResumePromptMode::Interactive
    );
    assert_eq!(resume_prompt_mode(false, false), ResumePromptMode::Silent);
}

#[test]
fn trust_birth_prompt_offer_renders_only_present_summary_lines() {
    let offer = BirthPromptOffer {
        current_hash: "sha256:test".to_owned(),
        summary: SurfaceSummary {
            task_names: vec!["sync".to_owned()],
            env_agents: vec!["claude".to_owned()],
            hooks: 2,
            ..SurfaceSummary::default()
        },
    };
    let mut out = Vec::new();

    write_project_trust_offer_to(&mut out, &offer).expect("render prompt");

    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(
        rendered,
        concat!(
            "This project ships .rimz/config.toml with config that stays inert\n",
            "until you trust it on this machine:\n",
            "  loop tasks: sync\n",
            "  env for: claude\n",
            "  hooks: 2\n",
        )
    );
}
