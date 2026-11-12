//! `rimz sidebar split` on a scripted tmux: the work pane behind the sidebar
//! splits, and nothing else does.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::common::{CommandTimeoutExt, Env, path_with_front, write_path_shim};

/// A tmux that answers `list-panes` with `RIMZ_TEST_TMUX_PANES` and
/// `display-message` with its target, and logs every argv.
fn tmux_shim(env: &Env) -> (PathBuf, PathBuf) {
    let bin = env.home_root.join("split-bin");
    write_path_shim(
        &bin,
        "tmux",
        r#"if [ "$1" = -S ]; then shift 3; fi
printf '%s\n' "$*" >> "$RIMZ_TEST_TMUX_LOG"
target=
prev=
for arg; do
  if [ "$prev" = "-t" ]; then target=$arg; fi
  prev=$arg
done
case "$1" in
  -V) printf 'tmux 3.5\n' ;;
  list-panes) printf '%b' "$RIMZ_TEST_TMUX_PANES" ;;
  display-message) printf '%s|\n' "$target" ;;
  split-window) printf '%%9\n' ;;
esac
exit 0"#,
    );
    (bin, env.home_root.join("split-tmux.log"))
}

struct Room {
    env: Env,
    session: String,
    bin: PathBuf,
    log: PathBuf,
}

impl Room {
    fn new() -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        let session = env.resolve_workspace(&env.project_root).session_name;
        let (bin, log) = tmux_shim(&env);
        Self {
            env,
            session,
            bin,
            log,
        }
    }

    /// One `list-panes` line in the backend's comma format.
    fn line(&self, pane: &str, window: &str, command: &str, cwd: &Path) -> String {
        let title = if command == "rimz-sidebar" {
            "rimz-sidebar"
        } else {
            "sh"
        };
        format!(
            "{},{window},{pane},{command},{},1,main,{title},0,,1,,,0\\n",
            self.session,
            cwd.display()
        )
    }

    /// Publish a pane frame whose one tab remembers `work_pane`.
    fn remember(&self, panes: &[&str], work_pane: &str) {
        let refs: Vec<_> = panes
            .iter()
            .map(|raw| rimz::pane::PaneRef {
                session_name: self.session.clone(),
                view_id: Some("@1".to_owned()),
                ..crate::common::tmux_pane(raw, "zsh", &self.env.project_root)
            })
            .collect();
        let mut frame = serde_json::to_value(rimz::sidebar::frame::assemble_frame(
            refs,
            1,
            self.session.clone(),
        ))
        .unwrap();
        frame["tabs"][0]["work_pane"] = serde_json::json!(format!("tmux:{work_pane}"));
        let path = self.env.runtime_paths().pane_frame_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&frame).unwrap()).unwrap();
    }

    fn split(&self, panes: &str, from: &str) -> std::process::Output {
        let mut command = self.env.rimz();
        command
            .args(["--mux", "tmux", "sidebar", "split", "down", "--from-pane"])
            .arg(format!("tmux:{from}"))
            .args(["--session-name", &self.session])
            .env("PATH", path_with_front(&self.bin))
            .env("RIMZ_TEST_TMUX_LOG", &self.log)
            .env("RIMZ_TEST_TMUX_PANES", panes);
        command.bounded_output().expect("rimz sidebar split")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }
}

#[test]
fn split_from_the_sidebar_splits_the_work_pane_behind_it() {
    let room = Room::new();
    let (first, remembered) = (
        room.env.project_root.join("a"),
        room.env.project_root.join("b"),
    );
    let panes = [
        room.line("%1", "@1", "rimz-sidebar", &room.env.project_root),
        room.line("%2", "@1", "zsh", &first),
        room.line("%3", "@1", "zsh", &remembered),
        room.line("%4", "@2", "zsh", &room.env.project_root),
    ]
    .concat();
    for (frame, target, cwd) in [(false, "%2", &first), (true, "%3", &remembered)] {
        if frame {
            room.remember(&["%1", "%2", "%3"], "%3");
        }
        std::fs::remove_file(&room.log).ok();
        let output = room.split(&panes, "%1");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = room.calls();
        let focus = calls
            .iter()
            .position(|call| call.starts_with("select-pane") && call.contains(target))
            .unwrap_or_else(|| panic!("focus {target} first: {calls:#?}"));
        let split = calls
            .iter()
            .position(|call| call.starts_with("split-window"))
            .unwrap_or_else(|| panic!("a split: {calls:#?}"));
        assert!(focus < split, "focus moves before the split: {calls:#?}");
        let split = &calls[split];
        assert!(split.contains(&format!("-t {target}")), "{split}");
        assert!(split.contains(" -v"), "split down: {split}");
        assert!(
            split.contains(&format!("-c {}", cwd.display())),
            "the pane's own cwd: {split}"
        );
    }
}

#[test]
fn split_from_a_sidebar_alone_or_from_a_work_pane_changes_nothing() {
    let room = Room::new();
    let alone = [
        room.line("%1", "@1", "rimz-sidebar", &room.env.project_root),
        room.line("%4", "@2", "zsh", &room.env.project_root),
    ]
    .concat();
    let output = room.split(&alone, "%1");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "rimz: sidebar is the only pane in its view; nothing to split"
    );
    let output = room.split(&alone, "%4");
    assert!(!output.status.success(), "a work pane is not a sidebar");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tmux:%4"),
        "names the pane: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = room.calls();
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("split-window") || call.starts_with("select-pane")),
        "{calls:#?}"
    );
}
