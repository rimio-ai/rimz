//! Shared folder- and hook-trust launch verdicts.

use crate::agents::FolderTrust;
use anyhow::Result;

#[derive(Debug, PartialEq, Eq)]
pub struct TrustNotice(String);

impl std::fmt::Display for TrustNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const AGENT_RELAUNCH: &str = "launch from a directory the account already trusts: on `rimz agents`, `--cwd <DIR>` chooses one and dropping `--cwd` uses your own checkout";

#[derive(Clone, Copy)]
pub enum Unattended {
    Team,
    Loop,
    Workflow,
    Subagent,
    Agent,
}

impl std::fmt::Display for Unattended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Team => "team",
            Self::Loop => "loop",
            Self::Workflow => "workflow",
            Self::Subagent => "subagent resume",
            Self::Agent => "agent-started",
        })
    }
}

pub fn preflight_hooks(
    health: Result<(), crate::agents::BirthLoginErr>,
    unattended: impl FnOnce() -> Option<Unattended>,
    fix_command: impl FnOnce() -> String,
    noticed: &mut std::collections::HashSet<crate::ids::LoginKey>,
) -> Result<Option<TrustNotice>> {
    let Err(error) = health else {
        return Ok(None);
    };
    let crate::agents::BirthLoginErr::HooksUntrusted {
        kind,
        name,
        choice: Some(choice),
        ..
    } = error
    else {
        if matches!(
            error,
            crate::agents::BirthLoginErr::HooksMissing { .. }
                | crate::agents::BirthLoginErr::HooksDisabled { .. }
                | crate::agents::BirthLoginErr::HooksUntrusted { .. }
        ) && matches!(unattended(), Some(Unattended::Workflow))
        {
            anyhow::bail!("workflow launch refused: {error}");
        }
        return Err(error.into());
    };
    if let Some(context) = unattended() {
        let fix_command = fix_command();
        anyhow::bail!(
            "{context} launch refused: {kind} account `{name}` has untrusted RimZ hooks, and only the user can trust them.\nIf this blocks your task, ask the user to run `{fix_command}` once and {choice}."
        );
    }
    if noticed.insert(crate::ids::LoginKey {
        kind: kind.clone(),
        name: name.clone(),
    }) {
        let provider = crate::agents::find_definition(kind.as_str())
            .map_or(kind.as_str(), |adapter| adapter.spec().display_name);
        return Ok(Some(TrustNotice(format!(
            "rimz: {kind} account `{name}` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent."
        ))));
    }
    Ok(None)
}

pub fn preflight(
    kind: &str,
    cwd: &std::path::Path,
    trust: Option<FolderTrust>,
    unattended: Option<Unattended>,
) -> Result<Option<TrustNotice>> {
    let message = match trust {
        Some(FolderTrust::Restricted { key }) => {
            if matches!(unattended, Some(Unattended::Agent)) {
                anyhow::bail!(
                    "agent-started launch refused: {kind} folder `{}` is saved untrusted; ask the user to trust the folder in Codex; {AGENT_RELAUNCH}; RimZ leaves the saved decision alone",
                    key.display()
                );
            }
            let message = format!(
                "{kind} folder `{}` is saved untrusted; it will stop at its Folder access prompt on every launch; {}",
                key.display(),
                FolderTrust::RESTRICTED_FIX
            );
            if let Some(context) = unattended {
                anyhow::bail!(
                    "{context} launch refused: {message}; RimZ leaves the saved decision alone"
                );
            }
            message
        }
        Some(FolderTrust::Undecided(gap)) => {
            let fix =
                if crate::disk::paths::is_under_root(cwd, &crate::disk::paths::empty_dir_root()) {
                    gap.empty_dir_fix(kind)
                } else {
                    gap.fix(kind, "rimz trust grant")
                };
            if matches!(unattended, Some(Unattended::Agent)) {
                anyhow::bail!(
                    "agent-started launch refused: {kind} has no trust decision for `{}`; the pane would stop at its folder-trust prompt with nobody watching; ask the user to {}; {AGENT_RELAUNCH}",
                    gap.key.display(),
                    fix
                );
            }
            let message = format!(
                "{kind} has no trust decision for `{}`; it will stop at its folder-trust prompt in the pane; {}",
                gap.key.display(),
                fix
            );
            if matches!(unattended, Some(Unattended::Subagent)) {
                anyhow::bail!("subagent resume refused: {message}");
            }
            message
        }
        Some(FolderTrust::Decided) | None => return Ok(None),
    };
    Ok(Some(TrustNotice(format!("rimz: {message}"))))
}

#[cfg(test)]
mod tests {

    fn untrusted_hooks(choice: Option<&'static str>) -> crate::agents::BirthLoginErr {
        crate::agents::BirthLoginErr::HooksUntrusted {
            kind: crate::ids::AgentKind::new_unchecked("codex"),
            name: "team-1".parse().unwrap(),
            home: Box::new("/private/codex-home".into()),
            env_key: Some("CODEX_HOME"),
            hooks: "SessionStart, UserPromptSubmit".into(),
            fix: "run /hooks inside codex and trust the RimZ hooks".into(),
            choice,
        }
    }

    #[test]
    fn hook_trust_verdict_depends_on_the_command() {
        for (kind, provider, choice) in [
            ("hermes", "Hermes", "answer y to each RimZ hook prompt"),
            ("codex", "Codex", "choose \"Trust all and continue\""),
        ] {
            for unattended in [
                None,
                Some(super::Unattended::Team),
                Some(super::Unattended::Loop),
                Some(super::Unattended::Subagent),
                Some(super::Unattended::Agent),
            ] {
                let mut error = untrusted_hooks(Some(choice));
                if let crate::agents::BirthLoginErr::HooksUntrusted {
                    kind: agent_kind, ..
                } = &mut error
                {
                    *agent_kind = crate::ids::AgentKind::new_unchecked(kind);
                }
                let result = super::preflight_hooks(
                    Err(error),
                    || unattended,
                    || {
                        assert!(unattended.is_some(), "watched launches need no fix lookup");
                        "rimz agents sol".into()
                    },
                    &mut Default::default(),
                );
                if let Some(context) = unattended {
                    let text = result
                        .expect_err("an unwatched trust prompt must refuse")
                        .to_string();
                    for expected in [
                        context.to_string().as_str(),
                        "only the user can trust them",
                        "ask the user to run `rimz agents sol` once",
                        choice,
                    ] {
                        assert!(text.contains(expected), "{text}");
                    }
                    for excluded in [
                        "/private/codex-home",
                        "SessionStart",
                        "UserPromptSubmit",
                        "CODEX_HOME",
                    ] {
                        assert!(!text.contains(excluded), "{text}");
                    }
                } else {
                    assert!(result.is_ok(), "{result:?}");
                    assert_eq!(
                        result.unwrap().unwrap().to_string(),
                        format!(
                            "rimz: {kind} account `team-1` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent."
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn hook_notice_is_once_per_account_per_command() {
        let mut notices = Vec::new();
        let mut noticed = Default::default();
        for name in ["team-1", "team-1", "team-2"] {
            let mut error = untrusted_hooks(Some("choose \"Trust all and continue\""));
            if let crate::agents::BirthLoginErr::HooksUntrusted { name: account, .. } = &mut error {
                *account = name.parse().unwrap();
            }
            let notice = super::preflight_hooks(
                Err(error),
                || None,
                || panic!("watched launches need no fix lookup"),
                &mut noticed,
            )
            .unwrap();
            notices.extend(notice);
        }
        let text = notices
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("`team-1`"));
        assert!(text.contains("`team-2`"));
    }

    #[test]
    fn hook_trust_without_a_pane_choice_and_other_errors_pass_through() {
        super::preflight_hooks(
            Ok(()),
            || panic!("healthy launches need no caller lookup"),
            || panic!("healthy launches need no fix lookup"),
            &mut Default::default(),
        )
        .unwrap();
        for unattended in [None, Some(super::Unattended::Agent)] {
            let error = untrusted_hooks(None);
            let expected = error.to_string();
            let result = super::preflight_hooks(
                Err(error),
                || unattended,
                || panic!("non-opted-in providers keep their original error"),
                &mut Default::default(),
            );
            assert_eq!(result.unwrap_err().to_string(), expected);
        }
        let error = crate::agents::BirthLoginErr::MissingHome {
            kind: crate::ids::AgentKind::new_unchecked("codex"),
            name: crate::ids::LoginName::default_login(),
            home: "/missing".into(),
        };
        let expected = error.to_string();
        assert_eq!(
            super::preflight_hooks(
                Err(error),
                || panic!("other health errors need no caller lookup"),
                || panic!("other health errors need no fix lookup"),
                &mut Default::default(),
            )
            .unwrap_err()
            .to_string(),
            expected
        );
    }

    #[test]
    fn every_workflow_hook_refusal_keeps_its_fix_and_names_the_workflow() {
        use crate::agents::BirthLoginErr;
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let name = crate::ids::LoginName::default_login();
        for error in [
            BirthLoginErr::HooksMissing {
                kind: kind.clone(),
                name: name.clone(),
                home: "/codex".into(),
            },
            BirthLoginErr::HooksDisabled {
                kind,
                name,
                reason: "hooks disabled".into(),
                fix: "enable hooks".into(),
            },
            untrusted_hooks(None),
        ] {
            let expected = format!("workflow launch refused: {error}");
            let error = super::preflight_hooks(
                Err(error),
                || Some(super::Unattended::Workflow),
                || panic!("the original error carries its fix"),
                &mut Default::default(),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn restricted_launch_verdict_depends_on_the_command() {
        for unattended in [
            None,
            Some(super::Unattended::Team),
            Some(super::Unattended::Loop),
            Some(super::Unattended::Subagent),
            Some(super::Unattended::Agent),
        ] {
            let result = super::preflight(
                "codex",
                std::path::Path::new("/repo"),
                Some(crate::agents::FolderTrust::Restricted {
                    key: "/repo".into(),
                }),
                unattended,
            );
            assert_eq!(result.is_err(), unattended.is_some());
            if let Err(error) = result {
                let text = error.to_string();
                assert!(text.contains(&unattended.unwrap().to_string()));
                assert!(text.contains("saved untrusted"));
                if matches!(unattended, Some(super::Unattended::Agent)) {
                    assert!(text.contains("ask the user to trust the folder in Codex"));
                    assert!(!text.contains("in the pane"));
                } else {
                    assert!(text.contains("Open restricted"));
                }
                assert!(!text.contains("rimz trust grant"));
            }
        }
    }

    #[test]
    fn empty_dir_undecided_fix_is_machine_wide() {
        if std::env::var_os("RIMZ_TEST_TRUST_LINKED_HOME").is_none() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real");
            let link = dir.path().join("link");
            std::fs::create_dir_all(real.join("empty/child")).unwrap();
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launcher::trust::tests::empty_dir_undecided_fix_is_machine_wide",
                    "--nocapture",
                ])
                .env("RIMZ_HOME", &link)
                .env("RIMZ_TEST_TRUST_LINKED_HOME", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let cwd = crate::disk::paths::empty_dir_root().join("child");
        let resolved = cwd.canonicalize().unwrap();
        for cwd in [&cwd, &resolved] {
            for unattended in [
                None,
                Some(super::Unattended::Subagent),
                Some(super::Unattended::Agent),
            ] {
                let gap = crate::agents::FolderTrustGap {
                    key: cwd.to_path_buf(),
                    path: "/config".into(),
                    grant: Ok(crate::agents::FolderTrustPreview {
                        original: None,
                        candidate: String::new(),
                    }),
                };
                let result = super::preflight(
                    "claude",
                    cwd,
                    Some(crate::agents::FolderTrust::Undecided(gap)),
                    unattended,
                );
                assert_eq!(result.is_err(), unattended.is_some());
                let text = match result {
                    Ok(notice) => notice.unwrap().to_string(),
                    Err(error) => error.to_string(),
                };
                assert!(
                    text.contains("run `rimz trust grant --empty-dir`"),
                    "{text}"
                );
                assert!(!text.contains("--agents"), "{text}");
                assert!(!text.contains(" in `"), "{text}");
            }
        }
    }

    #[test]
    fn agent_undecided_refusal_names_the_key_and_both_relaunch_exits() {
        let gap = crate::agents::FolderTrustGap {
            key: "/target".into(),
            path: "/config".into(),
            grant: Ok(crate::agents::FolderTrustPreview {
                original: None,
                candidate: String::new(),
            }),
        };
        let error = super::preflight(
            "claude",
            std::path::Path::new("/target"),
            Some(crate::agents::FolderTrust::Undecided(gap)),
            Some(super::Unattended::Agent),
        )
        .expect_err("agent caller must not leave an unwatched prompt");
        let text = error.to_string();
        for expected in [
            "claude",
            "/target",
            "nobody watching",
            "ask the user to run `rimz trust grant --agents claude`",
            "--cwd <DIR>",
            "dropping `--cwd` uses your own checkout",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn agent_restricted_refusal_leaves_the_saved_decision_alone() {
        let error = super::preflight(
            "codex",
            std::path::Path::new("/target"),
            Some(crate::agents::FolderTrust::Restricted {
                key: "/target".into(),
            }),
            Some(super::Unattended::Agent),
        )
        .unwrap_err();
        let text = error.to_string();
        for expected in [
            "/target",
            "ask the user to trust the folder in Codex",
            "--cwd <DIR>",
            "RimZ leaves the saved decision alone",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("rimz trust grant"));
        assert!(!text.contains("in the pane"));
    }
}
