//! Shared folder-trust collection and consent presentation.

use std::io::Write;

use anyhow::Result;
use rimz::agents::{FolderTrust, FolderTrustRow, RoomLoginSet};
use serde::Serialize;

use super::render;

pub(super) fn collect(workspace: &rimz::ResolvedWorkspace) -> Result<Vec<FolderTrustRow>> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    let machine = rimz::config::MachineConfig::load()?;
    let agents = super::open_existing_store(workspace)?
        .map(|store| store.snapshot_cached())
        .transpose()?
        .map(|snapshot| snapshot.agents)
        .unwrap_or_default();
    let logins = RoomLoginSet::resolve(&paths.workspace_record, &machine).with_agents(&agents);
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
