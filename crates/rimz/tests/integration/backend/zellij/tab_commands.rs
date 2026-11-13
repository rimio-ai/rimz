use super::support::*;
use crate::backend::tab_commands::{TabRoom, check_tabs};
use crate::common::{CommandTimeoutExt, Env, ZellijNamespace};

fn command<'a>(
    env: &'a Env,
    room: &'a LiveZellijSession,
) -> impl Fn() -> std::process::Command + 'a {
    move || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("XDG_RUNTIME_DIR", room.path())
            .env("ZELLIJ_CONFIG_DIR", room.path().join(".config/zellij"));
        command
    }
}

#[test]
fn tab_verbs_show_the_dashboard_and_numbered_terminals() {
    require_zellij!();
    let env = Env::new();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let live = LiveZellijSession::from_namespace(ZellijNamespace::new(), &session);
    command(&env, &live)()
        .args(["start", "--no-attach"])
        .assert_success_within_timeout("start tab command room");
    let mut client = AttachedClient::attach(&live, 160, 50);
    client.send_line("true");
    let room = TabRoom {
        env: &env,
        backend: live.backend(),
        session: &session,
        runtime_root: live.path(),
        command: command(&env, &live),
    };
    check_tabs(&room, |name| {
        let tabs = PaneSnapshot::expect(live.path(), &session)
            .panes
            .into_iter()
            .filter(|pane| pane.tab_name.as_deref() == Some(name))
            .map(|pane| pane.tab_id)
            .collect::<std::collections::BTreeSet<_>>();
        for tab in tabs {
            let output = live
                .command()
                .args([
                    "--session",
                    &session,
                    "action",
                    "close-tab-by-id",
                    &tab.to_string(),
                ])
                .bounded_output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
    });
}

#[test]
fn tab_verbs_without_a_client_say_so_and_create_nothing() {
    require_zellij!();
    let env = Env::new();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let live = LiveZellijSession::from_namespace(ZellijNamespace::new(), &session);
    command(&env, &live)()
        .args(["start", "--no-attach"])
        .assert_success_within_timeout("start a clientless tab command room");
    let room = TabRoom {
        env: &env,
        backend: live.backend(),
        session: &session,
        runtime_root: live.path(),
        command: command(&env, &live),
    };
    for (args, verb) in [
        (&["terminal", "3"][..], "terminal"),
        (&["dashboard"][..], "dashboard"),
    ] {
        let output = room.tab(args);
        assert!(
            output.status.success() && output.stdout.is_empty(),
            "{output:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("tab {verb}: no client is attached to session {session}\n"),
        );
    }
    assert!(room.views_named("term-3").is_empty());
    live.namespace().delete_session(&session);
    room.check_dead_session();
}
