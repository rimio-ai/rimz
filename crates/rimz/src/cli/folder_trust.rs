//! Shared folder- and hook-trust launch policy and consent presentation.

use std::io::Write;

use anyhow::Result;
use rimz::agents::{FolderTrust, FolderTrustRow, RoomLoginSet};
use serde::Serialize;

use super::render;

const AGENT_RELAUNCH: &str = "launch from a directory the account already trusts: on `rimz agents`, `--cwd <DIR>` chooses one and dropping `--cwd` uses your own checkout";

#[derive(Clone, Copy)]
pub(super) enum Unattended {
    Team,
    Loop,
    Subagent,
    Agent,
}

impl std::fmt::Display for Unattended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Team => "team",
            Self::Loop => "loop",
            Self::Subagent => "subagent resume",
            Self::Agent => "agent-started",
        })
    }
}

pub(super) fn preflight_hooks(
    health: Result<(), rimz::agents::BirthLoginErr>,
    unattended: impl FnOnce() -> Option<Unattended>,
    fix_command: impl FnOnce() -> String,
    noticed: &mut std::collections::HashSet<rimz::ids::LoginKey>,
    out: &mut impl Write,
) -> Result<()> {
    let Err(error) = health else {
        return Ok(());
    };
    let rimz::agents::BirthLoginErr::HooksUntrusted {
        kind,
        name,
        choice: Some(choice),
        ..
    } = error
    else {
        return Err(error.into());
    };
    if let Some(context) = unattended() {
        let fix_command = fix_command();
        anyhow::bail!(
            "{context} launch refused: {kind} account `{name}` has untrusted RimZ hooks, and only the user can trust them.\nIf this blocks your task, ask the user to run `{fix_command}` once and {choice}."
        );
    }
    if noticed.insert(rimz::ids::LoginKey {
        kind: kind.clone(),
        name: name.clone(),
    }) {
        let provider = rimz::agents::find_definition(kind.as_str())
            .map_or(kind.as_str(), |adapter| adapter.spec().display_name);
        writeln!(
            out,
            "rimz: {kind} account `{name}` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent."
        )?;
    }
    Ok(())
}

pub(super) fn preflight(
    kind: &str,
    cwd: &std::path::Path,
    trust: Option<FolderTrust>,
    unattended: Option<Unattended>,
) -> Result<()> {
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
            let subject = if cwd.starts_with(rimz::disk::paths::empty_dir_root()) {
                "rimz trust grant --empty-dir"
            } else {
                "rimz trust grant"
            };
            if matches!(unattended, Some(Unattended::Agent)) {
                anyhow::bail!(
                    "agent-started launch refused: {kind} has no trust decision for `{}`; the pane would stop at its folder-trust prompt with nobody watching; ask the user to {}; {AGENT_RELAUNCH}",
                    gap.key.display(),
                    gap.fix(kind, subject)
                );
            }
            let message = format!(
                "{kind} has no trust decision for `{}`; it will stop at its folder-trust prompt in the pane; {}",
                gap.key.display(),
                gap.fix(kind, subject)
            );
            if matches!(unattended, Some(Unattended::Subagent)) {
                anyhow::bail!("subagent resume refused: {message}");
            }
            message
        }
        Some(FolderTrust::Decided) | None => return Ok(()),
    };
    writeln!(render::err(), "rimz: {message}")?;
    Ok(())
}

pub(super) fn collect(workspace: &rimz::ResolvedWorkspace) -> Result<Vec<FolderTrustRow>> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    let machine = rimz::config::MachineConfig::load()?;
    let agents = super::open_existing_store(workspace)?
        .map(|store| store.snapshot_cached())
        .transpose()?
        .map(|snapshot| snapshot.agents)
        .unwrap_or_default();
    let logins = RoomLoginSet::resolve(
        &paths.workspace_record,
        Some(&workspace.project_root),
        &machine,
    )
    .with_agents(&agents);
    Ok(rimz::agents::folder_trust_rows(
        &logins,
        &workspace.worktree_root,
        workspace.launch_repo_root(),
    ))
}

#[derive(Debug, Serialize)]
pub(super) struct Row {
    pub(super) kind: &'static str,
    pub(super) login: rimz::ids::LoginName,
    #[serde(flatten)]
    pub(super) state: State,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum State {
    Decided,
    Restricted {
        key: String,
        fix: String,
    },
    Undecided {
        key: String,
        path: String,
        fix: String,
    },
}

impl From<&FolderTrustRow> for Row {
    fn from(row: &FolderTrustRow) -> Self {
        Self::for_subject(row, "rimz trust grant")
    }
}

impl Row {
    pub(super) fn for_subject(row: &FolderTrustRow, subject: &str) -> Self {
        Self {
            kind: row.kind,
            login: row.login.clone(),
            state: match &row.trust {
                FolderTrust::Decided => State::Decided,
                FolderTrust::Restricted { key } => State::Restricted {
                    key: key.display().to_string(),
                    fix: FolderTrust::RESTRICTED_FIX.to_owned(),
                },
                FolderTrust::Undecided(gap) => State::Undecided {
                    key: gap.key.display().to_string(),
                    path: gap.path.display().to_string(),
                    fix: gap.fix(row.kind, subject),
                },
            },
        }
    }
}

fn unique_grants<'a>(rows: &[&'a FolderTrustRow]) -> Vec<&'a FolderTrustRow> {
    let mut seen = std::collections::HashSet::new();
    rows.iter()
        .copied()
        .filter(|row| match &row.trust {
            FolderTrust::Undecided(gap) => seen.insert((
                row.kind,
                gap.path.canonicalize().unwrap_or_else(|_| gap.path.clone()),
            )),
            _ => true,
        })
        .collect()
}

pub(super) fn preview(rows: &[&FolderTrustRow], subject: &str) -> Result<Vec<String>> {
    let mut out = render::err();
    let mut grantable = Vec::new();
    for row in unique_grants(rows) {
        let FolderTrust::Undecided(gap) = &row.trust else {
            continue;
        };
        writeln!(
            out,
            "{} ({})",
            render::paint(render::palette::identity(row.kind), row.kind),
            row.login
        )?;
        match &gap.grant {
            Ok(preview) => {
                render::diff::preview_file_diff(
                    &mut out,
                    &gap.path,
                    preview.original.as_deref(),
                    &preview.candidate,
                )?;
                grantable.push(row.kind.to_owned());
            }
            Err(_) => writeln!(out, "  {}", gap.fix(row.kind, subject))?,
        }
    }
    Ok(grantable)
}

pub(super) fn grant(rows: &[&FolderTrustRow], prefix: &str, subject: &str) -> Result<bool> {
    let mut succeeded = true;
    let mut out = render::err();
    for row in unique_grants(rows) {
        if let FolderTrust::Restricted { key } = &row.trust {
            succeeded = false;
            writeln!(
                out,
                "{prefix}: {}: RimZ does not change a saved untrusted decision for `{}`; {}",
                row.kind,
                key.display(),
                FolderTrust::RESTRICTED_FIX
            )?;
            continue;
        }
        let FolderTrust::Undecided(gap) = &row.trust else {
            writeln!(out, "{prefix}: {} already decided", row.kind)?;
            continue;
        };
        match rimz::agents::grant_folder_trust(gap) {
            Ok(()) => writeln!(
                out,
                "{prefix}: {} trusted → {}",
                row.kind,
                gap.path.display()
            )?,
            Err(error) => {
                succeeded = false;
                writeln!(
                    out,
                    "{prefix}: {}: {}",
                    row.kind,
                    grant_error(row.kind, &error, subject)
                )?;
            }
        }
    }
    Ok(succeeded)
}

fn grant_error(kind: &str, error: &rimz::agents::FolderTrustErr, subject: &str) -> String {
    match error {
        rimz::agents::FolderTrustErr::Changed { path } => format!(
            "{} changed since the preview; run `{subject} --agents {kind}`",
            path.display()
        ),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    fn untrusted_hooks(choice: Option<&'static str>) -> rimz::agents::BirthLoginErr {
        rimz::agents::BirthLoginErr::HooksUntrusted {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
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
                let mut out = Vec::new();
                let mut error = untrusted_hooks(Some(choice));
                if let rimz::agents::BirthLoginErr::HooksUntrusted {
                    kind: agent_kind, ..
                } = &mut error
                {
                    *agent_kind = rimz::ids::AgentKind::new_unchecked(kind);
                }
                let result = super::preflight_hooks(
                    Err(error),
                    || unattended,
                    || {
                        assert!(unattended.is_some(), "watched launches need no fix lookup");
                        "rimz agents sol".into()
                    },
                    &mut Default::default(),
                    &mut out,
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
                    assert!(out.is_empty());
                } else {
                    assert!(result.is_ok(), "{result:?}");
                    assert_eq!(
                        String::from_utf8(out).unwrap(),
                        format!(
                            "rimz: {kind} account `team-1` has untrusted RimZ hooks. {provider} will ask you to trust them in the new pane: {choice}, or RimZ cannot track this agent.\n"
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn hook_notice_is_once_per_account_per_command() {
        let mut out = Vec::new();
        let mut noticed = Default::default();
        for name in ["team-1", "team-1", "team-2"] {
            let mut error = untrusted_hooks(Some("choose \"Trust all and continue\""));
            if let rimz::agents::BirthLoginErr::HooksUntrusted { name: account, .. } = &mut error {
                *account = name.parse().unwrap();
            }
            super::preflight_hooks(
                Err(error),
                || None,
                || panic!("watched launches need no fix lookup"),
                &mut noticed,
                &mut out,
            )
            .unwrap();
        }
        let text = String::from_utf8(out).unwrap();
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
            &mut Vec::new(),
        )
        .unwrap();
        for unattended in [None, Some(super::Unattended::Agent)] {
            let error = untrusted_hooks(None);
            let expected = error.to_string();
            let mut out = Vec::new();
            let result = super::preflight_hooks(
                Err(error),
                || unattended,
                || panic!("non-opted-in providers keep their original error"),
                &mut Default::default(),
                &mut out,
            );
            assert_eq!(result.unwrap_err().to_string(), expected);
            assert!(out.is_empty());
        }
        let error = rimz::agents::BirthLoginErr::MissingHome {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            name: rimz::ids::LoginName::default_login(),
            home: "/missing".into(),
        };
        let expected = error.to_string();
        assert_eq!(
            super::preflight_hooks(
                Err(error),
                || panic!("other health errors need no caller lookup"),
                || panic!("other health errors need no fix lookup"),
                &mut Default::default(),
                &mut Vec::new()
            )
            .unwrap_err()
            .to_string(),
            expected
        );
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
                Some(rimz::agents::FolderTrust::Restricted {
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
    fn agent_undecided_refusal_names_the_key_and_both_relaunch_exits() {
        let gap = rimz::agents::FolderTrustGap {
            key: "/target".into(),
            path: "/config".into(),
            grant: Ok(rimz::agents::FolderTrustPreview {
                original: None,
                candidate: String::new(),
            }),
        };
        let error = super::preflight(
            "claude",
            std::path::Path::new("/target"),
            Some(rimz::agents::FolderTrust::Undecided(gap)),
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
            Some(rimz::agents::FolderTrust::Restricted {
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

    #[test]
    fn restricted_rows_have_no_grant_and_report_the_saved_decision() {
        let row = rimz::agents::FolderTrustRow {
            kind: "codex",
            login: rimz::ids::LoginName::default_login(),
            trust: rimz::agents::FolderTrust::Restricted {
                key: "/repo".into(),
            },
        };
        let json = serde_json::to_value(super::Row::from(&row)).unwrap();
        assert_eq!(json["state"], "restricted");
        assert_eq!(json["key"], "/repo");
        assert!(
            super::preview(&[&row], "rimz trust grant")
                .unwrap()
                .is_empty()
        );
        assert!(!super::grant(&[&row], "trust", "rimz trust grant").unwrap());
    }

    #[test]
    fn stale_folder_trust_preview_names_the_grant_command() {
        let error = rimz::agents::FolderTrustErr::Changed {
            path: "/config".into(),
        };
        let advice = super::grant_error("agent", &error, "rimz trust grant");
        assert_eq!(
            advice,
            "/config changed since the preview; run `rimz trust grant --agents agent`"
        );
    }
}
