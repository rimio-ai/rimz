//! Live RimZ room inventory for browser session selection.

use std::path::PathBuf;

use crate::ids::{MuxName, WorkspaceId};
use crate::room::session::LiveSessions;
use crate::workspace::KnownWorkspace;

use super::{Result, WebErr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRoom {
    pub session_name: String,
    pub mux: MuxName,
    pub project_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub updated_at: jiff::Timestamp,
}

pub fn live_rooms() -> Result<Vec<LiveRoom>> {
    live_rooms_with(&LiveSessions::probe())
}

pub(super) fn live_rooms_with(live: &LiveSessions) -> Result<Vec<LiveRoom>> {
    let known = crate::workspace::known_workspaces()
        .map_err(|source| WebErr::WorkspaceRecords { source })?;
    Ok(live_rooms_from(known, |session| live.mux_of(session)))
}

fn live_rooms_from(
    known: Vec<KnownWorkspace>,
    mux_of: impl Fn(&str) -> Option<MuxName>,
) -> Vec<LiveRoom> {
    let mut rooms = known
        .into_iter()
        .filter_map(|workspace| {
            let mux = mux_of(&workspace.session_name)?;
            Some(LiveRoom {
                session_name: workspace.session_name,
                mux,
                project_root: workspace.project_root,
                workspace_id: workspace.workspace_id,
                updated_at: workspace.updated_at,
            })
        })
        .collect::<Vec<_>>();
    rooms.sort_by(|left, right| left.session_name.cmp(&right.session_name));
    rooms
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::workspace::RootClass;

    use super::*;

    fn known(session: &str, root: &str, updated_at: i64) -> KnownWorkspace {
        KnownWorkspace {
            workspace_id: WorkspaceId::from_project_root(std::path::Path::new(root)),
            project_root: PathBuf::from(root),
            session_name: session.to_owned(),
            root_class: RootClass::Repo,
            rimz_bin: None,
            updated_at: jiff::Timestamp::from_second(updated_at).unwrap(),
        }
    }

    #[test]
    fn live_room_join_filters_stopped_sessions_and_sorts_by_name() {
        let rooms = live_rooms_from(
            vec![
                known("rimz-zulu", "/repo/zulu", 30),
                known("rimz-stopped", "/repo/stopped", 20),
                known("rimz-alpha", "/repo/alpha", 10),
            ],
            |session| match session {
                "rimz-alpha" => Some(MuxName::Tmux),
                "rimz-zulu" => Some(MuxName::Zellij),
                _ => None,
            },
        );

        assert_eq!(
            rooms
                .iter()
                .map(|room| (room.session_name.as_str(), room.mux))
                .collect::<Vec<_>>(),
            vec![
                ("rimz-alpha", MuxName::Tmux),
                ("rimz-zulu", MuxName::Zellij),
            ]
        );
        assert_eq!(rooms[0].project_root, PathBuf::from("/repo/alpha"));
        assert_eq!(
            rooms[0].updated_at,
            jiff::Timestamp::from_second(10).unwrap()
        );
    }
}
