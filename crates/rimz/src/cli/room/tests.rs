use super::{
    RecoveryQuestion, ResumePromptMode, birth_socket_name, choose_disposition, resume_prompt_mode,
    tmux_version_preflight, write_project_trust_offer_to,
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
fn folder_trust_offer_never_asks_or_declines_restricted() {
    let rows = [rimz::agents::FolderTrustRow {
        kind: "codex",
        login: rimz::ids::LoginName::default_login(),
        trust: rimz::agents::FolderTrust::Restricted {
            key: "/repo".into(),
        },
    }];
    let offer = super::folder_trust_offer(&rows, &[]);
    assert!(offer.rows.is_empty());
    assert!(offer.decline.is_empty());
    assert!(!offer.asks);
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
    let error = tmux_version_preflight(&caps, None).unwrap_err();
    let blocker = error
        .downcast_ref::<rimz::blocker::Blocker>()
        .expect("typed refusal");
    assert_eq!(blocker.problem, "tmux 3.4 is below RimZ's floor");
    assert!(
        blocker
            .fixes
            .iter()
            .any(|fix| fix.summary.contains(&format!(">= {maj}.{min}.{patch}")))
    );
    assert!(
        blocker.fixes.iter().any(|fix| fix.command.is_none()
            && fix.summary == "Run this room under zellij (`--mux zellij`)")
    );
    let rerun = "rimz start --mux zellij /w";
    for raw in ["tmux next-3.6", "master", ""] {
        caps.binary_version = raw.to_owned();
        caps.parsed_version = None;
        let error = tmux_version_preflight(&caps, Some(rerun.into())).unwrap_err();
        let blocker = error
            .downcast_ref::<rimz::blocker::Blocker>()
            .expect("typed refusal");
        assert_eq!(
            blocker.problem,
            format!("`tmux -V` output {raw:?} was not recognised")
        );
        assert!(
            blocker
                .fixes
                .iter()
                .any(|fix| fix.command.as_deref() == Some(rerun))
        );
    }
    caps.meets_min_version = true;
    caps.parsed_version = Some((maj, min, patch));
    caps.binary_version = format!("tmux {maj}.{min}.{patch}");
    assert!(tmux_version_preflight(&caps, None).is_ok());
}

#[test]
fn start_under_reruns_the_same_room_and_accounts_from_any_cwd() {
    let accounts = [
        crate::cli::accounts::parse_account_flag("claude=work").unwrap(),
        crate::cli::accounts::parse_account_flag("codex=home").unwrap(),
    ];
    assert_eq!(
        super::start_under(
            MuxName::Tmux,
            std::path::Path::new("/home/u/my room"),
            &accounts
        ),
        "rimz start --mux tmux '/home/u/my room' --account 'claude=work' --account 'codex=home'"
    );
}

#[test]
fn mux_timeout_refusals_name_the_session_wide_reset_and_other_backend() {
    for (mux, reset, fallback) in [
        (
            MuxName::Zellij,
            "zellij kill-all-sessions",
            "rimz start --mux tmux /w",
        ),
        (
            MuxName::Tmux,
            "tmux kill-server",
            "rimz start --mux zellij /w",
        ),
    ] {
        let blocker = super::mux_not_responding_message(
            mux,
            std::time::Duration::from_secs(3),
            &rimz::mux::MuxErr::Timeout {
                program: mux.to_string(),
                args: "list-sessions".into(),
                timeout: std::time::Duration::from_secs(3),
            },
            Some(fallback.into()),
        );
        assert!(
            blocker
                .fixes
                .iter()
                .any(|fix| fix.command.as_deref() == Some(fallback)),
            "{blocker}"
        );
        let fix = blocker
            .fixes
            .iter()
            .find(|fix| fix.summary.starts_with("End every"))
            .expect("reset warns that it ends every room");
        let argv = shlex::split(fix.command.as_deref().unwrap()).unwrap();
        if mux == MuxName::Tmux {
            assert_eq!(
                argv,
                [
                    "tmux",
                    "-S",
                    &rimz::mux::tmux::managed_server_socket_path().to_string_lossy(),
                    "kill-server"
                ]
            );
        } else {
            assert_eq!(argv, shlex::split(reset).unwrap());
        }
    }
}

#[test]
fn socket_budget_blocker_names_the_short_runtime_export() {
    let blocker = rimz::blocker::Blocker::from(rimz::sock::SocketPathTooLong {
        path: "/long/runtime/socket".into(),
        used: 120,
        limit: 108,
    });
    assert!(blocker.problem.contains("120 bytes"));
    assert!(
        blocker
            .fixes
            .iter()
            .any(|fix| { fix.command.as_deref() == Some(rimz::sock::XDG_REMEDY) }),
        "{blocker:?}"
    );
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
