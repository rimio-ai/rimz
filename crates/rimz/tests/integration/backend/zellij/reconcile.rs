use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{SidebarLiveness, SidebarPaneOptions, SidebarWidth};
use tempfile::TempDir;

use crate::common::CommandTimeoutExt;

use super::support::*;

/// An in-place add on a *detached* session is deferred, never attempted:
/// Zellij's screen thread drops a `new-pane` mount when no client is attached
/// while the spawned process keeps running, so the only safe move is to wait
/// for an attached client. Regression test for the reload loop that leaked
/// (and then reaped) one serve pair per run against a detached session.
#[test]
fn reconcile_defers_the_add_on_a_detached_session() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("defer");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A detached background session with a working pane and no sidebar.
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let before = wait_for_pane_count(xdg.path(), &name, 1);
    assert!(
        !before.is_empty(),
        "plain session should have a pane before reconcile: {before:?}",
    );

    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    // A freshly born --create-background session whose only pane is still
    // materializing is the case most prone to reconcile's transient-empty read,
    // so retry until reconcile actually observes the working pane.
    let report = reconcile_until_observed(xdg.path(), &opts, &SidebarLiveness::default());

    assert_eq!(report.deferred, 1, "the detached session's add is deferred");
    assert_eq!(report.recovered, 0, "nothing is added without a client");
    assert_eq!(report.failed, 0, "a deferral is not a failure");
    // Poll rather than read once: a recovered add would already have tripped the
    // `recovered == 0` assertion above, so here a single empty `list-panes` answer
    // under load would only flake a settled result. `before` polls for the same reason.
    let after = wait_for_pane_count(xdg.path(), &name, 1);
    assert_eq!(after.len(), 1, "no pane was added detached: {after:?}");
    assert_eq!(
        serve_processes_for(&name),
        0,
        "no serve pair leaked for the deferred add",
    );
}
/// A claimed live sidebar sitting off the layout's dock — the residue of the
/// pre-discovery mis-mount (right side, ~50%) — is converged in place by
/// reconcile: moved to the left column and resized toward the fixed birth width,
/// with the renderer's pane (and so the renderer) untouched.
#[test]
fn reconcile_redocks_an_off_spec_claimed_sidebar() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("redock");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A background session with a deterministic right-side sidebar, matching the
    // shape a raced live add used to leave behind.
    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(
        cwd.path(),
        &stub,
        "right-sidebar.kdl",
        |cwd_kdl, stub_kdl| {
            format!(
                r#"layout {{
    pane split_direction="vertical" {{
        pane cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
        pane name="rimz-sidebar" cwd={cwd_kdl} {{
            command {stub_kdl}
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
            )
        },
    );
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "right-sidebar");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 2);
    assert_eq!(
        initial.len(),
        2,
        "layout should birth a sidebar and one work pane: {initial:?}",
    );

    // A wide client: the 50% mis-mount must exceed the `max_cols` cap (72) to
    // trip the tolerant width trigger — at 240 columns it lands at ~120.
    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = sidebar_id_from(&before);
    assert!(
        before.get("pane_x").and_then(|value| value.as_u64()) > Some(0),
        "the recreated mis-mount starts off the left column: {before}",
    );

    let project_root = std::env::temp_dir();
    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-redock",
        &project_root,
        &project_root,
        stub,
        240,
    );
    let liveness = claimed_liveness(sidebar_id);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(report.redocked, 1, "the off-spec claimed sidebar converges");
    assert_eq!(report.closed, 0, "the renderer's pane is never closed");
    assert_eq!(report.recovered, 0, "nothing needed adding");
    assert_eq!(report.failed, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
    assert_sidebar_identity(
        &xdg,
        &name,
        sidebar_id,
        "the same pane survived the move — the renderer was never replaced",
    );
}
/// A claimed sidebar can sit at `x=0` while still not being a full-height left
/// column: a work pane spans the whole tab below it. Reconcile detects that
/// nested row, preserves the running renderer, and moves the work panes into a
/// right-side stack so the sidebar owns the full left column.
#[test]
fn reconcile_repairs_a_nested_sidebar_into_a_full_height_left_column() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nested");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "nested.kdl", |cwd_kdl, stub_kdl| {
        format!(
            r#"layout {{
    pane split_direction="horizontal" {{
        pane split_direction="vertical" {{
            pane name="rimz-sidebar" cwd={cwd_kdl} {{
                command {stub_kdl}
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
        }}
        pane cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
        )
    });
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "nested");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 3);
    assert_eq!(
        initial.len(),
        3,
        "layout should birth a sidebar and two work panes: {initial:?}",
    );

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 160, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);

    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = sidebar_id_from(&before);
    assert_eq!(
        before.get("pane_x").and_then(|value| value.as_u64()),
        Some(0),
        "the nested sidebar starts in the left row band: {before}",
    );
    let sidebar_cols = before
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns before");
    let before_work = work_pane_geometry(&xdg, &name);
    let before_right_xs: BTreeSet<u64> = before_work
        .iter()
        .filter(|pane| pane.x >= sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert!(
        before_work.len() == 2
            && before_work.iter().any(|pane| pane.x == 0)
            && before_right_xs.len() == 1,
        "fixture should start as a repairable nested sidebar: \
         sidebar={before}, work={before_work:?}",
    );
    let original_id = before_work
        .iter()
        .find(|pane| pane.x >= sidebar_cols)
        .map(|pane| pane.id)
        .expect("right-side work pane");
    let tab_id = before
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    focus_nonplugin_pane_until(
        &xdg,
        &name,
        tab_id,
        original_id,
        "original work pane before reconcile",
    );

    let opts = reconcile_opts(&name, "/tmp/rimz-nested", cwd.path(), cwd.path(), stub, 160);
    let liveness = claimed_liveness(sidebar_id);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(report.redocked, 1, "the nested sidebar converges");
    assert_eq!(report.closed, 0, "geometry repair is not duplicate cleanup");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
    assert_sidebar_identity(
        &xdg,
        &name,
        sidebar_id,
        "the renderer pane survives the nested-row repair",
    );
    assert_eq!(
        focused_nonplugin_id_in_tab(&xdg, &name, tab_id),
        Some(original_id),
        "in-place nested repair restores the tab focus that existed before reconcile",
    );
}
/// A nested sidebar beside a user-made multi-column work layout is detected but
/// not rewritten: stacking every work pane would preserve processes while
/// collapsing the user's right-side columns.
#[test]
fn reconcile_reports_nested_multicolumn_sidebar_without_stacking_work_area() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nestedwide");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "nested-wide.kdl", |cwd_kdl, stub_kdl| {
        format!(
            r#"layout {{
    pane split_direction="horizontal" {{
        pane split_direction="vertical" {{
            pane name="rimz-sidebar" cwd={cwd_kdl} {{
                command {stub_kdl}
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
        }}
        pane cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
        )
    });
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "nested-wide");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 4);
    assert_eq!(
        initial.len(),
        4,
        "layout should birth four panes: {initial:?}"
    );

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);

    let before_sidebar = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = sidebar_id_from(&before_sidebar);
    let before_sidebar_cols = before_sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns before");
    let before_work = work_pane_geometry(&xdg, &name);
    let before_ids: BTreeSet<u64> = before_work.iter().map(|pane| pane.id).collect();
    let before_right_xs: BTreeSet<u64> = before_work
        .iter()
        .filter(|pane| pane.x >= before_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert!(
        before_work.iter().any(|pane| pane.x == 0) && before_right_xs.len() >= 2,
        "fixture should start as a nested sidebar with a multi-column work area: \
         sidebar={before_sidebar}, work={before_work:?}",
    );

    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-nestedwide",
        cwd.path(),
        cwd.path(),
        stub,
        240,
    );
    let liveness = claimed_liveness(sidebar_id);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(
        report.misdocked, 1,
        "the nested sidebar is reported for operator visibility",
    );
    assert_eq!(
        report.redocked, 0,
        "the arbitrary work layout is not repaired"
    );
    assert_eq!(report.closed, 0, "the claimed renderer pane is preserved");
    assert_eq!(report.failed, 0);

    let after_sidebar = raw_sidebar_pane(&xdg, &name);
    let after_sidebar_cols = after_sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns after");
    let after_work = work_pane_geometry(&xdg, &name);
    let after_ids: BTreeSet<u64> = after_work.iter().map(|pane| pane.id).collect();
    let after_right_xs: BTreeSet<u64> = after_work
        .iter()
        .filter(|pane| pane.x >= after_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert_eq!(after_ids, before_ids, "work panes are not replaced");
    assert!(
        after_work.iter().any(|pane| pane.x == 0) && after_right_xs.len() >= 2,
        "reconcile must not collapse the user's multi-column work area: \
         sidebar={after_sidebar}, work={after_work:?}",
    );
    assert_sidebar_identity(&xdg, &name, sidebar_id, "the renderer pane is not rebuilt");
}
/// Adding a sidebar to a tab whose work panes are already row-stacked used to
/// birth the sidebar into only one row. The verified add path now repairs that
/// nested shape before reporting success.
#[test]
fn reconcile_add_ends_docked_in_a_row_stacked_tab() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("rowadd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    create_plain_background_session(xdg_dir.path(), &name, cwd.path(), "600");
    wait_for_pane_count(xdg_dir.path(), &name, 1);

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 160, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let down = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "down",
            "--",
            "sleep",
            "600",
        ])
        .bounded_output()
        .expect("new-pane down");
    assert!(
        down.status.success(),
        "new-pane down failed: {}",
        String::from_utf8_lossy(&down.stderr),
    );
    wait_for_pane_count(&xdg, &name, 2);
    let (_stub_dir, stub) = sidebar_command_stub();

    let opts = reconcile_opts(&name, "/tmp/rimz-rowadd", cwd.path(), cwd.path(), stub, 160);
    let report = reconcile_until_converged(&xdg, &opts, &rimz::mux::SidebarLiveness::default());

    assert_eq!(report.recovered, 1, "the missing sidebar is added");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
}

fn write_kdl_layout(
    cwd: &Path,
    stub: &Path,
    file_name: &str,
    render: impl FnOnce(&str, &str) -> String,
) -> PathBuf {
    let stub_kdl = serde_json::to_string(&stub.to_string_lossy()).expect("stub kdl string");
    let cwd_kdl = serde_json::to_string(&cwd.to_string_lossy()).expect("cwd kdl string");
    let layout = cwd.join(file_name);
    std::fs::write(&layout, render(&cwd_kdl, &stub_kdl)).expect("write kdl layout");
    layout
}

fn birth_kdl_session(xdg: &Path, name: &str, cwd: &Path, layout: &Path, label: &str) {
    let created = scoped_zellij(xdg)
        .args(["attach", "--create-background", name, "options"])
        .arg("--default-cwd")
        .arg(cwd)
        .arg("--default-layout")
        .arg(layout)
        .bounded_status()
        .unwrap_or_else(|err| panic!("create {label} session failed to run: {err}"));
    assert!(created.success(), "create-background failed for {name}");
}

fn reconcile_opts(
    name: &str,
    workspace_root: &str,
    project_root: &Path,
    cwd: &Path,
    stub: PathBuf,
    detected_cols: u16,
) -> SidebarPaneOptions {
    SidebarPaneOptions {
        session_name: name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new(workspace_root)),
        project_root: project_root.to_path_buf(),
        cwd: cwd.to_path_buf(),
        birth_size: SidebarWidth::default().birth_size(Some(detected_cols)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

fn claimed_liveness(raw_sidebar_id: u64) -> SidebarLiveness {
    let mut liveness = SidebarLiveness::default();
    liveness.claimed_panes.insert(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{raw_sidebar_id}"),
    ));
    liveness
}

fn sidebar_id_from(sidebar: &serde_json::Value) -> u64 {
    sidebar
        .get("id")
        .and_then(|value| value.as_u64())
        .expect("sidebar id")
}

fn assert_sidebar_identity(xdg: &Path, name: &str, sidebar_id: u64, message: &str) {
    let after = raw_sidebar_pane(xdg, name);
    assert_eq!(
        after.get("id").and_then(|value| value.as_u64()),
        Some(sidebar_id),
        "{message}",
    );
}
