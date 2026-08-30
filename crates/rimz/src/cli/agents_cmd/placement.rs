//! Shared mux placement for fresh, resumed, and forked agent launches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rimz::config::LaunchPlacement;
use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, SplitPaneOptions, SplitPlacement,
    SplitTarget, TabOptions, own_pane_id,
};
use rimz::store::writer::AgentLaunchBatch;

/// Where a launch lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    SamePane,
    NewPane,
    NewTab,
}

/// Resolve launch placement from explicit flags, config policy, and current
/// pane feasibility.
pub(super) fn resolve_placement(
    new_tab: bool,
    new_pane: bool,
    policy: LaunchPlacement,
    is_worktree: bool,
    single_cell: bool,
    has_launching_pane: bool,
) -> Result<Placement> {
    if new_tab {
        return Ok(Placement::NewTab);
    }
    if new_pane {
        if !single_cell {
            bail!(
                "--new-pane opens a single agent cell; a multi-cell layout opens a new tab — drop --new-pane or pass --new-tab"
            );
        }
        if !has_launching_pane {
            bail!(
                "--new-pane splits the current pane, so run it from inside the room; drop it to open a new tab"
            );
        }
        return Ok(Placement::NewPane);
    }
    Ok(match policy {
        LaunchPlacement::Tab => Placement::NewTab,
        LaunchPlacement::Pane if is_worktree => Placement::NewTab,
        LaunchPlacement::Pane => {
            feasible_or_new(Placement::NewPane, single_cell, has_launching_pane)
        }
        LaunchPlacement::Auto if is_worktree => Placement::NewTab,
        LaunchPlacement::Auto => {
            feasible_or_new(Placement::SamePane, single_cell, has_launching_pane)
        }
    })
}

pub(super) fn resolve_fork_placement(
    new_tab: bool,
    new_pane: bool,
    bg: bool,
    has_launching_pane: bool,
) -> Result<Placement> {
    let placement = resolve_placement(
        new_tab,
        new_pane,
        LaunchPlacement::Auto,
        false,
        true,
        has_launching_pane,
    )?;
    Ok(apply_in_place_downgrade(placement, bg, true))
}

pub(super) fn apply_in_place_downgrade(
    placement: Placement,
    bg: bool,
    allow_in_place: bool,
) -> Placement {
    if placement == Placement::SamePane && (bg || !allow_in_place) {
        Placement::NewPane
    } else {
        placement
    }
}

fn feasible_or_new(target: Placement, single_cell: bool, has_launching_pane: bool) -> Placement {
    if single_cell && has_launching_pane {
        target
    } else {
        Placement::NewTab
    }
}

pub(super) struct PlacementRequest {
    pub placement: Placement,
    pub mux: MuxName,
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
            title,
            panes,
            focus: !background,
            dock_sidebar: true,
            after: None,
            sidebar,
        }),
        Placement::NewPane => {
            let pane = single_pane(&panes)?;
            PreparedPlacement::NewPane(SplitPaneOptions {
                target: target_pane_id.map_or(SplitTarget::Ambient, SplitTarget::Pane),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                command: Some(pane.argv.clone()),
                title: pane.name.clone(),
                close_on_exit: false,
                env: identity_env,
                placement: SplitPlacement::Directional(direction),
                focus: !background,
            })
        }
        Placement::SamePane => PreparedPlacement::SamePane {
            argv: single_pane(&panes)?.argv.clone(),
            env: identity_env,
            cwd,
        },
    })
}

/// The one pane command of an in-pane launch (the resolver guarantees a single
/// cell before this is reached).
fn single_pane(panes: &LayoutPanes) -> Result<&PaneCmd> {
    panes
        .columns
        .first()
        .and_then(|column| column.panes.first())
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
    use rimz::mux::{LayoutColumn, PaneCmd};

    fn request(placement: Placement) -> PlacementRequest {
        PlacementRequest {
            placement,
            mux: MuxName::Tmux,
            cwd: PathBuf::from("/work"),
            title: "#lane".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["rimz".to_owned(), "agents".to_owned()],
                        name: Some("codex".to_owned()),
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
                target: rimz::mux::SidebarTarget {
                    share: rimz::mux::WidthPermille::from_percent(25),
                    max_cols: std::num::NonZeroU16::new(72).expect("nonzero test width"),
                    pinned: false,
                },
                detected_view_size: None,
                rimz_bin: PathBuf::from("/bin/rimz"),
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
        assert_eq!(options.sidebar.session_name, "room");
        assert_eq!(options.title, "#lane");
        assert_eq!(options.sidebar.cwd, Path::new("/work"));
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
        assert_eq!(options.target, SplitTarget::Pane(target));
        assert_eq!(options.cwd.as_deref(), Some("/work"));
        assert_eq!(
            options.command.as_deref(),
            Some(&["rimz".to_owned(), "agents".to_owned()][..])
        );
        assert_eq!(options.env["RIMZ_PROJECT_MODE"], "1");
        assert_eq!(options.title.as_deref(), Some("codex"));
        assert_eq!(options.placement, SplitPlacement::default());
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

    #[test]
    fn resolves_launch_placement_matrix() {
        use Placement::{NewPane, NewTab, SamePane};

        for (policy, is_worktree, single_cell, has_pane, expected) in [
            (LaunchPlacement::Auto, false, true, true, SamePane),
            (LaunchPlacement::Auto, true, true, true, NewTab),
            (LaunchPlacement::Auto, false, false, true, NewTab),
            (LaunchPlacement::Auto, false, true, false, NewTab),
            (LaunchPlacement::Pane, false, true, true, NewPane),
            (LaunchPlacement::Pane, true, true, true, NewTab),
            (LaunchPlacement::Tab, false, true, true, NewTab),
        ] {
            assert_eq!(
                resolve_placement(false, false, policy, is_worktree, single_cell, has_pane)
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            resolve_placement(true, false, LaunchPlacement::Auto, false, true, true).unwrap(),
            NewTab
        );
        assert_eq!(
            resolve_placement(false, true, LaunchPlacement::Auto, true, true, true).unwrap(),
            NewPane
        );
        assert_eq!(apply_in_place_downgrade(SamePane, true, true), NewPane);
        assert_eq!(apply_in_place_downgrade(SamePane, false, false), NewPane);

        let multi =
            resolve_placement(false, true, LaunchPlacement::Auto, false, false, true).unwrap_err();
        assert!(multi.to_string().contains("single agent cell"));
        let outside =
            resolve_placement(false, true, LaunchPlacement::Auto, false, true, false).unwrap_err();
        assert!(outside.to_string().contains("inside the room"));
    }

    #[test]
    fn fork_defaults_to_launching_pane() {
        use Placement::{NewPane, NewTab, SamePane};

        for (new_tab, new_pane, bg, has_pane, expected) in [
            (false, false, false, true, SamePane),
            (false, true, false, true, NewPane),
            (true, false, false, true, NewTab),
            (false, false, true, true, NewPane),
            (false, false, false, false, NewTab),
        ] {
            assert_eq!(
                resolve_fork_placement(new_tab, new_pane, bg, has_pane).unwrap(),
                expected
            );
        }
    }
}
