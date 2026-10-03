//! One spend service across the panes of two shared accounts.
//!
//! A pane on a named account exports that account's home, so each caller's
//! ambient env differs. Every account of one history pool must still reach
//! the one warm service a held `rimz stats --refresh` owns.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rimz::agents::spending::{unix_secs_now, utc_date};

use super::stats_refresh_resize::{
    StatsRefreshHarness, screen, short_tempdir, wait_for_tagline_col,
};
use crate::common::ScrubSessionEnvExt;

const ROOT_KEYS: [&str; 10] = [
    "HOME",
    "RIMZ_HOME",
    "TMPDIR",
    "TMUX_TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "ZELLIJ_CONFIG_DIR",
];

#[test]
fn accounts_of_one_pool_share_the_held_spend_service() {
    let root = short_tempdir();
    let base = root.path().to_path_buf();
    let rimz = |account_home: Option<&Path>| {
        let mut command =
            Command::new(crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")));
        command.scrub_session_env();
        for key in ROOT_KEYS {
            command.env(key, &base);
        }
        command
            .env("RIMZ_PRICING_OFFLINE", "1")
            .env_remove("RUST_LOG");
        if let Some(home) = account_home {
            command.env("CLAUDE_CONFIG_DIR", home);
        }
        command
    };
    let run = |command: &mut Command| -> String {
        let output = command.output().expect("run rimz");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let turn = |id: u32| {
        let now = unix_secs_now();
        let tod = now % 86_400;
        format!(
            r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","costUSD":0.25,"requestId":"req-{id}","message":{{"id":"msg-{id}","usage":{{"input_tokens":1200,"output_tokens":80}}}}}}"#,
            utc_date(now),
            tod / 3_600,
            (tod % 3_600) / 60,
            tod % 60
        ) + "\n"
    };

    let project = base.join(".claude/projects/repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("first.jsonl"), turn(1)).unwrap();
    let one = base.join("one");
    let two = base.join("two");
    for (name, home) in [("one", &one), ("two", &two)] {
        run(rimz(None).args([
            "accounts",
            "add",
            "claude",
            name,
            "--home",
            home.to_str().unwrap(),
        ]));
        assert_eq!(
            std::fs::read_link(home.join("projects")).unwrap(),
            base.join(".claude/projects")
        );
    }

    let harness = StatsRefreshHarness::launch_in(80, root, &[("CLAUDE_CONFIG_DIR", one.as_path())]);
    wait_for_tagline_col(&harness.parser, |_| true, Duration::from_secs(5))
        .unwrap_or_else(|| panic!("held stats never rendered:\n{}", screen(&harness.parser)));
    let publication = wait_for_file(&base, "provider-spending.json");
    wait_for_file(&base, ".sock");

    // New spend through the second account's link, then a publication too old
    // to serve: only the warm owner's walk can report the new total.
    std::fs::write(two.join("projects/repo/second.jsonl"), turn(2)).unwrap();
    let mut published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&publication).unwrap()).unwrap();
    published["refreshed_at_ms"] = ((unix_secs_now() - 3_600) * 1_000).into();
    std::fs::write(&publication, serde_json::to_vec(&published).unwrap()).unwrap();

    // A sidebar refresh is a one-shot service caller: it reaches a live owner
    // in its namespace and otherwise serves the publication, however old.
    let panes = base.join("panes.json");
    std::fs::write(&panes, b"[]").unwrap();
    run(rimz(Some(&two))
        .current_dir(&base)
        .args(["sidebar", "snapshot", "--json"])
        .env("RIMZ_TEST_PANE_LIST", &panes));
    let stats: serde_json::Value =
        serde_json::from_str(&run(rimz(None).args(["stats", "--json"]))).unwrap();
    assert_eq!(stats["windows"]["week"]["usd"], 0.5, "{stats}");
    let sockets = files_under(&base)
        .into_iter()
        .filter(|path| path.to_string_lossy().ends_with(".sock"))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("spending."))
        })
        .collect::<Vec<_>>();
    assert_eq!(sockets.len(), 1, "{sockets:?}");
    drop(harness);
}

fn wait_for_file(root: &Path, suffix: &str) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(path) = files_under(root)
            .into_iter()
            .find(|path| path.to_string_lossy().ends_with(suffix))
        {
            return path;
        }
        assert!(
            Instant::now() < deadline,
            "no `{suffix}` under {}",
            root.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if !kind.is_symlink() {
                found.push(entry.path());
            }
        }
    }
    found
}
