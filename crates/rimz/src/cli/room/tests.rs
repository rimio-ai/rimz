use std::path::PathBuf;

use super::{
    RecoveryQuestion, ResumePromptMode, birth_socket_name, blocks_room_start, choose_disposition,
    resume_prompt_mode, tmux_version_preflight, write_project_trust_offer_to,
};
use rimz::harness::rebirth::RebirthDisposition;

use rimz::ids::MuxName;
use rimz::trust::{BirthPromptOffer, SurfaceSummary};

#[test]
fn nested_start_distinguishes_the_current_room_live_elsewhere_and_not_running() {
    use super::{RoomSituation, room_situation};

    for live in [false, true] {
        assert_eq!(room_situation(true, live), RoomSituation::CurrentRoom);
    }
    assert_eq!(room_situation(false, true), RoomSituation::LiveElsewhere);
    assert_eq!(room_situation(false, false), RoomSituation::NotRunning);
}

fn folder_trust_row(kind: &'static str, grantable: bool) -> rimz::agents::FolderTrustRow {
    rimz::agents::FolderTrustRow {
        kind,
        login: rimz::ids::LoginName::default_login(),
        trust: rimz::agents::FolderTrust::Undecided(rimz::agents::FolderTrustGap {
            path: "/config".into(),
            key: "/repo".into(),
            grant: if grantable {
                Ok(rimz::agents::FolderTrustPreview {
                    original: None,
                    candidate: String::new(),
                })
            } else {
                Err("repair config".into())
            },
        }),
    }
}

#[test]
fn folder_trust_offer_filters_decided_and_dismissed_kinds() {
    let mut decided = folder_trust_row("decided", true);
    decided.trust = rimz::agents::FolderTrust::Decided;
    let rows = [
        decided,
        folder_trust_row("dismissed", true),
        folder_trust_row("new", true),
    ];
    let offer = super::folder_trust_offer(&rows, &["dismissed".into()]);
    assert_eq!(
        offer.rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        ["new"]
    );
}

#[test]
fn folder_trust_offer_decline_remembers_every_shown_kind() {
    let rows = [
        folder_trust_row("grantable", true),
        folder_trust_row("broken", false),
    ];
    let offer = super::folder_trust_offer(&rows, &[]);
    assert!(offer.asks);
    assert_eq!(offer.decline, ["grantable", "broken"]);
    assert!(
        super::folder_trust_offer(&rows, &offer.decline)
            .rows
            .is_empty()
    );
}

#[test]
fn folder_trust_offer_without_grantable_rows_asks_nothing_and_remembers_them() {
    let rows = [
        folder_trust_row("home", false),
        folder_trust_row("broken", false),
    ];
    let offer = super::folder_trust_offer(&rows, &[]);
    assert!(!offer.asks);
    assert_eq!(offer.decline, ["home", "broken"]);
    assert!(
        super::folder_trust_offer(&rows, &offer.decline)
            .rows
            .is_empty()
    );
}

#[test]
fn folder_trust_offer_new_kind_reopens_only_its_offer() {
    let rows = [folder_trust_row("old", true), folder_trust_row("new", true)];
    let offer = super::folder_trust_offer(&rows, &["old".into()]);
    assert_eq!(
        offer.rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        ["new"]
    );
    assert_eq!(offer.decline, ["new"]);
}

#[test]
fn zellij_birth_preflights_the_state_dir_name_only_when_the_room_is_dead() {
    assert_eq!(
        birth_socket_name(MuxName::Zellij, false, "repo-abcd"),
        Some("repo-abcd")
    );
    assert_eq!(birth_socket_name(MuxName::Zellij, true, "repo-abcd"), None);
    assert_eq!(birth_socket_name(MuxName::Tmux, false, "repo-abcd"), None);
}

#[test]
fn tmux_version_preflight_names_the_floor_and_release_requirement() {
    let (maj, min, patch) = rimz::mux::tmux::MIN_TMUX_VERSION;
    let mut caps = rimz::mux::tmux::TmuxCapabilities {
        binary_version: "tmux 3.4".to_owned(),
        parsed_version: Some((3, 4, 0)),
        meets_min_version: false,
        popup_supported: false,
    };
    assert_eq!(
        tmux_version_preflight(&caps).unwrap_err().to_string(),
        format!(
            "tmux 3.4 is below RimZ's floor; upgrade tmux to >= {maj}.{min}.{patch}, or run this room with `--mux zellij`."
        )
    );
    for raw in ["tmux next-3.6", "master", ""] {
        caps.binary_version = raw.to_owned();
        caps.parsed_version = None;
        assert_eq!(
            tmux_version_preflight(&caps).unwrap_err().to_string(),
            format!(
                "`tmux -V` output {raw:?} was not recognised; RimZ needs a release build >= {maj}.{min}.{patch}, or run this room with `--mux zellij`."
            )
        );
    }
    caps.meets_min_version = true;
    caps.parsed_version = Some((maj, min, patch));
    caps.binary_version = format!("tmux {maj}.{min}.{patch}");
    assert!(tmux_version_preflight(&caps).is_ok());
}

#[test]
fn machine_config_preflight_blocks_accounts_and_notifications() {
    let path = PathBuf::from("/tmp/config.toml");
    let account_error = rimz::config::ConfigErr::AccountBudget {
        path: path.clone(),
        source: rimz::config::AccountBudgetConfigError::Unsupported {
            kind: "cursor".to_owned(),
        },
    };
    assert!(blocks_room_start(&account_error));
    let login_error = rimz::config::ConfigErr::Account {
        path: path.clone(),
        source: Box::new(rimz::agents::LoginConfigErr::ReservedName {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
        }),
    };
    assert!(blocks_room_start(&login_error));
    let notifications_error = rimz::config::MachineConfig::parse_text(
        &path,
        "[[notifications.handler]]\nname = \"bad\"\ncommand = \"\"\n",
        std::path::Path::new("/tmp/missing-agents-home"),
    )
    .expect_err("invalid notifications");
    assert!(matches!(
        notifications_error,
        rimz::config::ConfigErr::Notifications { .. }
    ));
    assert!(blocks_room_start(&notifications_error));

    let parse_error = rimz::config::MachineConfig::parse_text(
        &path,
        "not = = toml",
        std::path::Path::new("/tmp/missing-agents-home"),
    )
    .expect_err("broken TOML");
    assert!(matches!(parse_error, rimz::config::ConfigErr::Parse { .. }));
    assert!(!blocks_room_start(&parse_error));

    let unrelated = rimz::config::ConfigErr::Io {
        path,
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    };
    assert!(!blocks_room_start(&unrelated));
}

#[test]
fn resume_prompt_mode_uses_tty_or_confirm_flag() {
    assert_eq!(
        resume_prompt_mode(false, true),
        ResumePromptMode::Interactive
    );
    assert_eq!(
        resume_prompt_mode(true, false),
        ResumePromptMode::Interactive
    );
    assert_eq!(resume_prompt_mode(false, false), ResumePromptMode::Silent);
}

#[test]
fn recovery_disposition_follows_who_was_asked_and_what_they_answered() {
    use RebirthDisposition::{Decline, Defer, RecoverDrop, RecoverKeep};
    use RecoveryQuestion::{DropRest, Recover};
    use ResumePromptMode::{Interactive, Silent};
    // (mode, recovery off, resumable, unresumable, recover answer, drop answer)
    //   => (disposition, questions asked)
    let table: [(_, _, _, _, _, _, _, &[RecoveryQuestion]); 13] = [
        (Silent, true, 0, 3, true, true, Defer, &[]),
        (Silent, false, 2, 3, true, true, RecoverKeep, &[]),
        (Silent, false, 0, 3, true, true, RecoverKeep, &[]),
        (Interactive, true, 0, 3, true, true, Decline, &[]),
        (Interactive, true, 0, 0, true, true, Decline, &[]),
        (Interactive, false, 0, 0, true, true, RecoverKeep, &[]),
        (
            Interactive,
            false,
            2,
            0,
            true,
            true,
            RecoverKeep,
            &[Recover],
        ),
        (Interactive, false, 2, 0, false, true, Decline, &[Recover]),
        (Interactive, false, 2, 3, false, true, Decline, &[Recover]),
        (
            Interactive,
            false,
            2,
            3,
            true,
            false,
            RecoverKeep,
            &[Recover, DropRest],
        ),
        (
            Interactive,
            false,
            2,
            3,
            true,
            true,
            RecoverDrop,
            &[Recover, DropRest],
        ),
        (
            Interactive,
            false,
            0,
            3,
            true,
            false,
            RecoverKeep,
            &[DropRest],
        ),
        (
            Interactive,
            false,
            0,
            3,
            false,
            true,
            RecoverDrop,
            &[DropRest],
        ),
    ];
    for (mode, recovery_off, resumable, unresumable, recover, drop_rest, expected, asks) in table {
        let mut asked = Vec::new();
        let disposition =
            choose_disposition(mode, recovery_off, resumable, unresumable, |question| {
                asked.push(question);
                Ok(match question {
                    Recover => recover,
                    DropRest => drop_rest,
                })
            })
            .expect("choose");
        let case = format!("{mode:?} off={recovery_off} {resumable}/{unresumable}");
        assert_eq!(disposition, expected, "{case}");
        assert_eq!(asked, asks, "{case}");
    }
}

#[test]
fn trust_birth_prompt_offer_renders_only_present_summary_lines() {
    let offer = BirthPromptOffer {
        current_hash: "sha256:test".to_owned(),
        summary: SurfaceSummary {
            lsp_servers: vec!["rust: rust-analyzer".to_owned()],
            task_names: vec!["sync".to_owned()],
            profiles: Vec::new(),
            subagent_profiles: Vec::new(),
            teams: Vec::new(),
            env_agents: vec!["claude".to_owned()],
            accounts: vec!["claude=work".to_owned()],
            hooks: 2,
        },
    };
    let mut out = Vec::new();

    write_project_trust_offer_to(&mut out, &offer).expect("render prompt");

    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(
        rendered,
        concat!(
            "This project ships .rimz/config.toml with config that stays inert\n",
            "until you trust it on this machine:\n",
            "  loop tasks: sync\n",
            "  env for: claude\n",
            "  accounts: claude=work\n",
            "  language servers: rust: rust-analyzer\n",
            "  hooks: 2\n",
        )
    );
}
