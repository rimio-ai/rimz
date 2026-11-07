use super::support::*;
use crate::backend::card_commands::check_cards;

#[test]
fn walk_slot_last_close_and_reopen_follow_the_attached_view() {
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
        .assert_success_within_timeout("start card command room");
    let _client = AttachedTmuxClient::attach(&server.socket, &session, 160, 50);
    check_cards(&env, &server.backend, &session, &env.runtime_root, command);
}
