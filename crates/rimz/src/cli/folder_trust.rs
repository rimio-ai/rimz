//! Shared folder-trust collection and consent presentation.

use std::io::Write;

use anyhow::Result;
use rimz::agents::{FolderTrust, FolderTrustRow, RoomLoginSet};
use serde::Serialize;

use super::render;

#[derive(Clone, Copy)]
pub(super) enum Unattended {
    Team,
    Loop,
}

impl std::fmt::Display for Unattended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Team => "team",
            Self::Loop => "loop",
        })
    }
}

pub(super) fn preflight(
    kind: &str,
    trust: Option<FolderTrust>,
    unattended: Option<Unattended>,
) -> Result<()> {
    let message = match trust {
        Some(FolderTrust::Restricted { key }) => {
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
        Some(FolderTrust::Undecided(gap)) => format!(
            "{kind} has no trust decision for `{}`; it will stop at its folder-trust prompt in the pane; {}",
            gap.key.display(),
            gap.fix(kind)
        ),
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
                    fix: gap.fix(row.kind),
                },
            },
        }
    }
}

pub(super) fn preview(rows: &[&FolderTrustRow]) -> Result<Vec<String>> {
    let mut out = render::err();
    let mut grantable = Vec::new();
    for row in rows {
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
            Err(_) => writeln!(out, "  {}", gap.fix(row.kind))?,
        }
    }
    Ok(grantable)
}

pub(super) fn grant(rows: &[&FolderTrustRow], prefix: &str) -> Result<bool> {
    let mut succeeded = true;
    let mut out = render::err();
    for row in rows {
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
                    grant_error(row.kind, &error)
                )?;
            }
        }
    }
    Ok(succeeded)
}

fn grant_error(kind: &str, error: &rimz::agents::FolderTrustErr) -> String {
    match error {
        rimz::agents::FolderTrustErr::Changed { path } => format!(
            "{} changed since the preview; run `rimz trust grant --agents {kind}`",
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
        ] {
            let result = super::preflight(
                "codex",
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
                assert!(text.contains("Open restricted"));
                assert!(!text.contains("rimz trust grant"));
            }
        }
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
        assert!(super::preview(&[&row]).unwrap().is_empty());
        assert!(!super::grant(&[&row], "trust").unwrap());
    }

    #[test]
    fn stale_folder_trust_preview_names_the_grant_command() {
        let error = rimz::agents::FolderTrustErr::Changed {
            path: "/config".into(),
        };
        let advice = super::grant_error("agent", &error);
        assert_eq!(
            advice,
            "/config changed since the preview; run `rimz trust grant --agents agent`"
        );
    }
}
