use super::support::*;
use crate::backend::tab_commands::{TabRoom, check_tabs};
use crate::common::{CommandTimeoutExt, Env};

#[test]
fn tab_verbs_show_the_dashboard_and_numbered_terminals() {
    require_tmux!();
    let env = Env::new();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let command = || {
        let mut command = env.rimz();
        command.args(["--mux", "tmux"]);
        command
    };
    command()
        .args(["start", "--no-attach"])
        .assert_success_within_timeout("start tab command room");
    let _client = AttachedTmuxClient::attach(&server.socket, &session, 160, 50);
    let room = TabRoom {
        env: &env,
        backend: &server.backend,
        session: &session,
        runtime_root: &env.runtime_root,
        command,
    };
    check_tabs(&room, |name| {
        for view in room.views_named(name) {
            server.tmux(&["kill-window", "-t", &view]);
        }
    });
    server.tmux(&["kill-session", "-t", &session]);
    room.check_dead_session();
}
