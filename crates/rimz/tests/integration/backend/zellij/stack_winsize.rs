use std::path::Path;
use std::time::Duration;

use rimz::harness::launch::ExecAction;
use rimz::mux::{MuxBackend, SplitPaneOptions, SplitPlacement, SplitTarget};

use crate::common::{Env, write_path_shim};

use super::support::*;

#[test]
fn background_stacked_wrapper_pane_starts_sized_without_moving_client_focus() {
    require_zellij!();

    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let workspace = env.resolve_workspace(&env.project_root);
    let room = LiveZellijSession::from_namespace(
        crate::common::ZellijNamespace::new(),
        workspace.session_name.clone(),
    );
    room.create_plain_background(&env.project_root, "600");
    let mut client = AttachedClient::attach(&room, 120, 40);
    client.send_line("winsize focus probe");
    let first = wait_for_pane_count(room.path(), room.name(), 1)[0]
        .pane_id
        .clone();
    room.backend()
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: room.name().to_owned(),
                pane_id: first.clone(),
            },
            command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
            focus: false,
            ..Default::default()
        })
        .expect("split anchor");
    let anchor = wait_for_pane_count(room.path(), room.name(), 2)
        .into_iter()
        .find(|pane| pane.pane_id != first)
        .expect("listed anchor")
        .pane_id;
    client.wait_until_focused(&first, "initial pane before stacked insert");
    let focused = wait_for_human_client_count(room.backend(), room.name(), 1).viewed_panes;
    assert_eq!(focused, vec![first]);

    let agent_bin = env.home_root.join("winsize-agent-bin");
    write_path_shim(
        &agent_bin,
        "claude",
        r#"
case "$1" in
  --version) printf '99.0.0 (Claude Code)\n'; exit 0 ;;
  auth) printf '{"loggedIn":true}\n'; exit 0 ;;
esac
trap 'exit 0' HUP TERM INT
while :; do stty size >> "$RIMZ_TEST_WINSIZE_FILE"; sleep 0.2; done
"#,
    );
    let mut sizes: Vec<std::path::PathBuf> = Vec::new();
    for index in 0..2 {
        let panes_before = wait_for_pane_count(room.path(), room.name(), 2 + index);
        let file = env.home_root.join(format!("winsize-{index}"));
        let command = zellij_agent_exec_command(
            &env,
            room.path(),
            &agent_bin,
            &file,
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        room.backend()
            .split_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: room.name().to_owned(),
                    pane_id: anchor.clone(),
                },
                placement: SplitPlacement::Stacked,
                cwd: Some(env.project_root.display().to_string()),
                command: Some(command),
                env: std::collections::BTreeMap::from([(
                    "RIMZ_TEST_WINSIZE_FILE".to_owned(),
                    file.display().to_string(),
                )]),
                focus: false,
                ..Default::default()
            })
            .expect("stack wrapper");
        let reported = poll_until(
            Duration::from_secs(10),
            || read_sizes(&file),
            |lines| !lines.is_empty(),
            "provider's first winsize",
        );
        let (rows, cols) = reported[0];
        assert!(
            rows > 0 && cols > 0,
            "provider started unsized: {reported:?}"
        );
        let snapshot = expect_list_panes(room.path(), room.name());
        let listed = snapshot
            .panes
            .iter()
            .find(|candidate| {
                !candidate.is_plugin
                    && !panes_before
                        .iter()
                        .any(|before| before.pane_id.creation_ordinal() == Some(candidate.id))
            })
            .expect("listed wrapper");
        let pane = listed.pane_ref(room.name()).pane_id;
        assert_eq!(
            (rows, cols),
            (listed.pane_content_rows, listed.pane_content_columns),
            "provider size versus live content rectangle"
        );
        let repairs: Vec<_> = env
            .diag_records(room.name())
            .into_iter()
            .map(|record| serde_json::to_value(record.event).expect("diagnostic JSON"))
            .filter(|event| event["kind"] == "pane_winsize_repaired")
            .collect();
        assert_eq!(
            repairs.len(),
            index + 1,
            "{}",
            env.diag_tail(room.name(), 20)
        );
        let repair = repairs
            .iter()
            .find(|event| event["pane"] == pane.to_string())
            .expect("one repair for this pane");
        assert_eq!(
            (repair["rows"].as_u64(), repair["cols"].as_u64()),
            (Some(rows), Some(cols))
        );
        client.wait_until_focused(&focused[0], "client focus after stacked insert");
        assert_eq!(snapshot.pane_refs().len(), 3 + index);
        sizes.push(file);
    }
    let samples_before_read = read_sizes(&sizes[0]).expect("first provider sizes").len();
    let collapsed = poll_until(
        Duration::from_secs(10),
        || read_sizes(&sizes[0]),
        |lines| lines.len() > samples_before_read,
        "collapsed provider's current winsize",
    );
    let (rows, cols) = collapsed.last().copied().expect("current collapsed size");
    assert!(
        rows > 0 && cols > 0,
        "collapsed member lost its size: {collapsed:?}"
    );
}

fn read_sizes(path: &Path) -> Result<Vec<(u64, u64)>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    text.lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let rows = fields.next().and_then(|value| value.parse().ok());
            let cols = fields.next().and_then(|value| value.parse().ok());
            match (rows, cols, fields.next()) {
                (Some(rows), Some(cols), None) => Ok((rows, cols)),
                _ => Err(format!("invalid stty size: {line:?}")),
            }
        })
        .collect()
}
