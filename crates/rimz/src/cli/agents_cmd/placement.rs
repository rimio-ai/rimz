//! Shared mux placement for fresh, resumed, and forked agent launches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use rimz::harness::plan::Placement;
use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    LayoutPanes, MuxBackend, SidebarPaneOptions, SplitPaneOptions, TabOptions, own_pane_id,
};
use rimz::store::AgentLaunchBatch;

pub(super) struct PlacementRequest {
    pub placement: Placement,
    pub mux: MuxName,
    pub session_name: String,
    pub cwd: PathBuf,
    pub title: String,
    pub panes: LayoutPanes,
    pub sidebar: SidebarPaneOptions,
    pub identity_env: BTreeMap<String, String>,
    pub background: bool,
    pub errors: PlacementErrors,
}

#[derive(Clone, Copy)]
pub(super) struct PlacementErrors {
    pub new_tab: &'static str,
    pub new_pane: &'static str,
    pub same_pane: &'static str,
}

enum PreparedPlacement {
    NewTab(TabOptions),
    NewPane(SplitPaneOptions),
    SamePane {
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
}

pub(super) fn execute(
    backend: &dyn MuxBackend,
    store: &rimz::Store,
    batch: &AgentLaunchBatch,
    request: PlacementRequest,
) -> Result<()> {
    let errors = request.errors;
    let context = match request.placement {
        Placement::NewTab => errors.new_tab,
        Placement::NewPane => errors.new_pane,
        Placement::SamePane => errors.same_pane,
    };
    let result = prepare(request).and_then(|prepared| match prepared {
        PreparedPlacement::NewTab(options) => backend.open_tab(&options).map_err(Into::into),
        PreparedPlacement::NewPane(options) => backend.split_pane(options).map_err(Into::into),
        PreparedPlacement::SamePane { argv, env, cwd } => {
            Err(exec_wrapper_in_place(&argv, env, &cwd))
        }
    });
    if let Err(err) = result {
        let _ = store.fail_agent_launch_batch(batch);
        return Err(err).context(context);
    }
    Ok(())
}

fn prepare(request: PlacementRequest) -> Result<PreparedPlacement> {
    let detected_size = rimz::mux::detect_terminal_size();
    let target_pane_id = (request.placement == Placement::NewPane)
        .then(|| own_pane_id(request.mux))
        .flatten();
    prepare_resolved(request, detected_size, target_pane_id)
}

fn prepare_resolved(
    request: PlacementRequest,
    detected_size: Option<(u16, u16)>,
    target_pane_id: Option<PaneId>,
) -> Result<PreparedPlacement> {
    let direction = detected_size
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let PlacementRequest {
        placement,
        session_name,
        cwd,
        title,
        panes,
        sidebar,
        identity_env,
        background,
        ..
    } = request;
    Ok(match placement {
        Placement::NewTab => PreparedPlacement::NewTab(TabOptions {
            session_name,
            title,
            cwd,
            panes,
            focus: !background,
            dock_sidebar: true,
            sidebar,
        }),
        Placement::NewPane => PreparedPlacement::NewPane(SplitPaneOptions {
            session_name: None,
            target_view_id: None,
            target_pane_id,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command: Some(single_pane_argv(&panes)?),
            title: None,
            env: identity_env,
            stacked: false,
            direction,
            focus: !background,
        }),
        Placement::SamePane => PreparedPlacement::SamePane {
            argv: single_pane_argv(&panes)?,
            env: identity_env,
            cwd,
        },
    })
}

/// The one pane command of an in-pane launch (the resolver guarantees a single
/// cell before this is reached).
fn single_pane_argv(panes: &LayoutPanes) -> Result<Vec<String>> {
    panes
        .columns
        .first()
        .and_then(|column| column.panes.first())
        .map(|pane| pane.argv.clone())
        .context("in-pane launch produced no pane command")
}

#[cfg(unix)]
fn exec_wrapper_in_place(
    argv: &[String],
    env: BTreeMap<String, String>,
    cwd: &Path,
) -> anyhow::Error {
    use std::os::unix::process::CommandExt;

    let Some((program, rest)) = argv.split_first() else {
        return anyhow::anyhow!("in-place launch produced no command");
    };
    let mut command = Command::new(program);
    command.args(rest).envs(&env).current_dir(cwd);
    command.exec().into()
}

#[cfg(not(unix))]
fn exec_wrapper_in_place(
    _argv: &[String],
    _env: BTreeMap<String, String>,
    _cwd: &Path,
) -> anyhow::Error {
    anyhow::anyhow!("in-place launch is only supported on Unix")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::WorkspaceId;
    use rimz::mux::{LayoutColumn, PaneCmd, SidebarWidth};

    fn request(placement: Placement) -> PlacementRequest {
        let width = SidebarWidth::default();
        PlacementRequest {
            placement,
            mux: MuxName::Tmux,
            session_name: "room".to_owned(),
            cwd: PathBuf::from("/work"),
            title: "#lane".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["rimz".to_owned(), "agents".to_owned()],
                    }],
                    stacked: false,
                }],
            },
            sidebar: SidebarPaneOptions {
                session_name: "room".to_owned(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/work")),
                project_root: PathBuf::from("/work"),
                extra_env: BTreeMap::new(),
                cwd: PathBuf::from("/work"),
                width,
                birth_size: width.birth_size(None),
                detected_view_size: None,
                width_override: None,
                rimz_bin: PathBuf::from("/bin/rimz"),
                replace_existing: false,
                pristine_birth: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            identity_env: BTreeMap::from([("RIMZ_PROJECT_MODE".to_owned(), "1".to_owned())]),
            background: true,
            errors: PlacementErrors {
                new_tab: "tab context",
                new_pane: "pane context",
                same_pane: "same context",
            },
        }
    }

    #[test]
    fn prepares_new_tab_options() {
        let PreparedPlacement::NewTab(options) =
            prepare_resolved(request(Placement::NewTab), Some((120, 40)), None).unwrap()
        else {
            panic!("new tab placement");
        };
        assert_eq!(options.session_name, "room");
        assert_eq!(options.title, "#lane");
        assert_eq!(options.cwd, Path::new("/work"));
        assert_eq!(options.panes.columns[0].panes[0].argv, ["rimz", "agents"]);
        assert!(!options.focus);
        assert!(options.dock_sidebar);
        assert_eq!(options.sidebar.session_name, "room");
    }

    #[test]
    fn prepares_new_pane_options() {
        let target = PaneId::from_parts(MuxName::Tmux, "%7");
        let PreparedPlacement::NewPane(options) = prepare_resolved(
            request(Placement::NewPane),
            Some((120, 40)),
            Some(target.clone()),
        )
        .unwrap() else {
            panic!("new pane placement");
        };
        assert_eq!(options.target_pane_id, Some(target));
        assert_eq!(options.cwd.as_deref(), Some("/work"));
        assert_eq!(
            options.command.as_deref(),
            Some(&["rimz".to_owned(), "agents".to_owned()][..])
        );
        assert_eq!(options.env["RIMZ_PROJECT_MODE"], "1");
        assert!(!options.stacked);
        assert_eq!(options.direction, rimz::mux::SplitDirection::Right);
        assert!(!options.focus);
    }

    #[test]
    fn prepares_same_pane_exec() {
        let PreparedPlacement::SamePane { argv, env, cwd } =
            prepare_resolved(request(Placement::SamePane), Some((40, 120)), None).unwrap()
        else {
            panic!("same pane placement");
        };
        assert_eq!(argv, ["rimz", "agents"]);
        assert_eq!(env["RIMZ_PROJECT_MODE"], "1");
        assert_eq!(cwd, Path::new("/work"));
    }

    #[test]
    fn single_pane_extraction_reports_empty_layout() {
        let mut request = request(Placement::NewPane);
        request.panes.columns.clear();
        let err = prepare_resolved(request, None, None)
            .err()
            .expect("empty layout must fail");
        assert_eq!(err.to_string(), "in-pane launch produced no pane command");
    }

    #[test]
    fn failure_context_follows_placement() {
        for (placement, expected) in [
            (Placement::NewTab, "tab context"),
            (Placement::NewPane, "pane context"),
            (Placement::SamePane, "same context"),
        ] {
            let request = request(placement);
            let actual = match placement {
                Placement::NewTab => request.errors.new_tab,
                Placement::NewPane => request.errors.new_pane,
                Placement::SamePane => request.errors.same_pane,
            };
            assert_eq!(actual, expected);
        }
    }
}
