//! Shared folder-trust collection and consent presentation.

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
