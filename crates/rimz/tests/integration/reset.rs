//! Integration coverage for `rimz reset`.

use std::fs;

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::workspace::WorkspaceResolver;

use crate::common::Env;

/// `rimz reset --no-start --yes` deletes the room's serialized-session cache and
/// reports what it removed, without trying to rebirth or attach. `--mux zellij`
/// forces the Zellij backend so the cache purge runs regardless of which mux the
/// host has installed; the purge itself is filesystem-only.
#[test]
fn reset_purges_the_resurrection_cache() {
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");

    // Plant a serialized-session cache the way Zellij would, under HOME/.cache
    // (the harness pins HOME and leaves XDG_CACHE_HOME unset, so `cache_home()`
    // resolves there).
    let session_info = env
        .project_root
        .join(".cache/zellij/contract_version_1/session_info");
    fs::create_dir_all(&session_info).expect("mkdir cache");
    let cache_entry = session_info.join(&workspace.session_name);
    fs::write(&cache_entry, b"serialized").expect("write cache");

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("cache entr"))
        .stderr(contains("Run `rimz start`"));

    assert!(
        !cache_entry.exists(),
        "reset should purge the serialized-session cache"
    );
}

/// Without a terminal to confirm and without `--yes`, `rimz reset` refuses rather
/// than destroying a session unattended — the fail-fast-with-the-fix contract.
#[test]
fn reset_without_a_tty_or_yes_refuses() {
    let env = Env::new();
    env.rimz()
        .args(["reset", "--no-start"])
        .assert()
        .failure()
        .stderr(contains("pass --yes"));
}
