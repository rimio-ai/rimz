use super::support::*;
use crate::backend::card_commands::{check_cards, write_rimz_style};
use crate::common::{CommandTimeoutExt, Env, ZellijNamespace};

#[test]
fn walk_slot_last_close_and_reopen_follow_the_attached_view() {
    require_zellij!();
    let env = Env::new();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let room = LiveZellijSession::from_namespace(ZellijNamespace::new(), &session);
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("XDG_RUNTIME_DIR", room.path())
            .env("ZELLIJ_CONFIG_DIR", room.path().join(".config/zellij"));
        command
    };
    write_rimz_style(&env);
    command()
        .args(["start", "--no-attach"])
        .assert_success_within_timeout("start card command room");
    let mut client = AttachedClient::attach(&room, 160, 50);
    client.send_line("true");
    // RimZ's keymap binds normal-mode keys in Zellij's locked mode, and an attach to a live
    // session starts in normal mode: Ctrl g takes it to locked.
    client.send(b"\x07");
    let mut press_close = || client.press_alt('x');
    check_cards(
        &env,
        room.backend(),
        &session,
        room.path(),
        command,
        &mut press_close,
    );
}
