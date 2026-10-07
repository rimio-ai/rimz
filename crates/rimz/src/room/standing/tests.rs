use super::*;
use crate::agents::{AgentState, AgentStatus, LoginCatalog};
use crate::trust::TrustState;

fn kind(value: &str) -> AgentKind {
    AgentKind::new_unchecked(value)
}

fn name(value: &str) -> LoginName {
    value.parse().expect("login name")
}

fn logins(pairs: &[(&str, &str)]) -> RoomLogins {
    pairs
        .iter()
        .map(|(kind_name, login)| (kind(kind_name), name(login)))
        .collect()
}

fn standing(
    recorded: Option<RoomLogins>,
    project: ProjectLogins,
    machine: RoomLogins,
) -> AccountStanding {
    let config = MachineConfig {
        accounts: toml::from_str("[claude.work]\nhome = \"/srv/claude-work\"\n[codex.team]\nhome = \"/srv/codex-team\"\n").unwrap(),
        ..Default::default()
    };
    let config = MachineConfig {
        accounts: crate::config::AccountsConfig {
            use_accounts: machine.clone(),
            ..config.accounts
        },
        ..config
    };
    AccountStanding {
        accounts: Some(crate::agents::RoomAccounts::from_layers(
            &recorded.unwrap_or_default(),
            project,
            &config,
        )),
        machine,
    }
}

fn applied(project: &RoomLogins) -> ProjectLogins {
    if project.is_empty() {
        ProjectLogins::Unconfigured
    } else {
        ProjectLogins::Apply(project.clone())
    }
}

#[test]
fn active_account_is_what_birth_selects_without_a_request() {
    let catalog = LoginCatalog::from_config(
        &toml::from_str(
            "[claude.work]\nhome = \"/srv/claude-work\"\n[codex.team]\nhome = \"/srv/codex-team\"\n",
        )
        .unwrap(),
    )
    .unwrap();
    let layers = [
        logins(&[]),
        logins(&[("claude", "work")]),
        logins(&[("claude", "default")]),
        logins(&[("codex", "team")]),
        logins(&[("claude", "work"), ("codex", "team")]),
    ];
    let mut compared = 0;
    for recorded in std::iter::once(None).chain(layers.iter().cloned().map(Some)) {
        for project in &layers {
            for machine in &layers {
                let Ok(born) = catalog.birth_selection(
                    &recorded.clone().unwrap_or_default(),
                    project,
                    machine,
                ) else {
                    continue;
                };
                let standing = standing(recorded.clone(), applied(project), machine.clone());
                for kind_name in ["claude", "codex"] {
                    let kind = kind(kind_name);
                    assert_eq!(
                        standing.active(&kind),
                        Some(born.get(&kind).cloned().unwrap_or_default()),
                        "{kind} under recorded {recorded:?}, project {project:?}, machine {machine:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 6 * 5 * 5 * 2);
}

#[test]
fn scopes_join_coinciding_layers_in_order_and_default_fills_unnamed_ones() {
    let claude = kind("claude");
    let default = LoginName::default_login();
    let unnamed = standing(None, ProjectLogins::Unconfigured, RoomLogins::new());
    assert_eq!(unnamed.scopes(&claude, &default).label(), "new rooms");
    assert!(unnamed.scopes(&claude, &default).contains(Scope::NewRooms));
    assert_eq!(unnamed.scopes(&claude, &name("work")).label(), "-");
    assert!(
        !unnamed
            .scopes(&claude, &name("work"))
            .contains(Scope::NewRooms)
    );

    let everywhere = standing(
        Some(logins(&[("claude", "work")])),
        ProjectLogins::Apply(logins(&[("claude", "work")])),
        logins(&[("claude", "work")]),
    );
    let scopes = everywhere.scopes(&claude, &name("work"));
    assert_eq!(scopes.label(), "this room, this project, new rooms");
    assert_eq!(
        serde_json::to_value(&scopes).unwrap(),
        serde_json::json!(["this_room", "this_project", "new_rooms"])
    );
    assert_eq!(everywhere.scopes(&claude, &default).label(), "-");

    let other_kind_recorded = standing(
        Some(logins(&[("codex", "team")])),
        ProjectLogins::Unconfigured,
        logins(&[("claude", "work")]),
    );
    assert_eq!(other_kind_recorded.scopes(&claude, &default).label(), "-");
    assert_eq!(
        other_kind_recorded.scopes(&claude, &name("work")).label(),
        "new rooms"
    );
}

#[test]
fn blocked_project_leaves_no_active_account_unless_the_kind_is_pinned() {
    let claude = kind("claude");
    let blocked = standing(
        None,
        ProjectLogins::Blocked(TrustState::Untrusted),
        logins(&[("claude", "work")]),
    );
    for kind_name in ["claude", "codex"] {
        assert_eq!(blocked.active(&kind(kind_name)), None);
        assert_eq!(blocked.deciding(&kind(kind_name)), None);
    }
    let warning = blocked.blocked().expect("a blocked project warns");
    assert!(warning.contains("rimz trust grant"), "{warning}");
    assert_eq!(blocked.scopes(&claude, &name("work")).label(), "new rooms");

    let recorded = standing(
        Some(logins(&[("claude", "work")])),
        ProjectLogins::Blocked(TrustState::Stale),
        RoomLogins::new(),
    );
    assert_eq!(recorded.active(&claude), Some(name("work")));
    assert_eq!(recorded.deciding(&claude), Some(Deciding::Room));
    assert_eq!(recorded.active(&kind("codex")), None);
    assert!(recorded.blocked().unwrap().contains("stale"));
}

#[test]
fn an_undeclared_selection_at_any_layer_leaves_its_kind_without_an_active_account() {
    let claude = kind("claude");
    let gone = logins(&[("claude", "gone")]);
    for (layer, standing) in [
        (
            "room",
            standing(
                Some(gone.clone()),
                ProjectLogins::Unconfigured,
                RoomLogins::new(),
            ),
        ),
        (
            "project",
            standing(None, ProjectLogins::Apply(gone.clone()), RoomLogins::new()),
        ),
        (
            "machine",
            standing(None, ProjectLogins::Unconfigured, gone.clone()),
        ),
    ] {
        assert_eq!(standing.active(&claude), None, "{layer}");
        assert_eq!(standing.deciding(&claude), None, "{layer}");
        assert_ne!(
            standing.scopes(&claude, &name("gone")).label(),
            "-",
            "{layer}"
        );
        assert_eq!(
            standing.active(&kind("codex")),
            Some(LoginName::default_login()),
            "{layer}: other kinds still resolve"
        );
    }
}

#[test]
fn an_unread_position_has_no_active_account_but_keeps_machine_scopes() {
    let machine = MachineConfig {
        accounts: toml::from_str("[claude.work]\nhome = \"/srv/w\"\n[use]\nclaude = \"work\"\n")
            .unwrap(),
        ..Default::default()
    };
    let unread = AccountStanding::unread(&machine);
    for kind_name in ["claude", "codex"] {
        assert_eq!(unread.active(&kind(kind_name)), None);
        assert_eq!(unread.deciding(&kind(kind_name)), None);
    }
    assert_eq!(
        unread.scopes(&kind("claude"), &name("work")).label(),
        "new rooms"
    );
    assert_eq!(unread.blocked(), None);
    assert_eq!(
        AccountStanding::machine_only(&machine).active(&kind("claude")),
        Some(name("work"))
    );
}

#[test]
fn deciding_layer_names_who_moves_the_active_account() {
    let claude = kind("claude");
    let project = standing(
        None,
        ProjectLogins::Apply(logins(&[("codex", "team")])),
        RoomLogins::new(),
    );
    assert_eq!(project.deciding(&kind("codex")), Some(Deciding::Project));
    assert_eq!(project.deciding(&claude), Some(Deciding::Machine));
    let mixed = standing(
        Some(logins(&[("codex", "default")])),
        ProjectLogins::Unconfigured,
        logins(&[("claude", "work")]),
    );
    assert_eq!(mixed.active(&claude), Some(name("work")));
    assert_eq!(mixed.deciding(&claude), Some(Deciding::Machine));
    assert_eq!(mixed.deciding(&kind("codex")), Some(Deciding::Room));
}

#[cfg(unix)]
#[test]
fn the_agents_column_is_the_refusal_count_with_nothing_launching() {
    use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
    let agent = |login: Option<&str>| {
        let mut agent = AgentState::seed(
            kind("codex"),
            "root".into(),
            AgentStatus::Idle,
            jiff::Timestamp::UNIX_EPOCH,
        );
        agent.login = login.map(name);
        agent.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::MuxName::Tmux, "%1"),
        ));
        agent
    };
    let mut paneless = agent(Some("team"));
    paneless.pane = None;
    let mut dead_owner = agent(Some("team"));
    dead_owner.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "root",
        u32::MAX,
        None,
    ));
    let mut ended = agent(Some("team"));
    ended.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let mut native_child = agent(Some("team"));
    native_child.parent_agent_id = Some("root".into());
    // A remote conversation the account's daemon serves: its owner lives and
    // it has no pane, so the daemon's own record speaks for it.
    let mut daemon_session = agent(Some("team"));
    daemon_session.pane = None;
    daemon_session.runtime_owner = Some(crate::store::runtime::current_process_owner(
        RuntimeOwnerKind::Daemon,
        "root",
    ));
    let room = [
        agent(Some("team")),
        agent(None),
        paneless,
        dead_owner,
        ended,
        native_child,
        daemon_session,
    ];
    let other_room = [agent(Some("team"))];

    let mut counts = BTreeMap::new();
    count_live_logins(&mut counts, &room);
    count_live_logins(&mut counts, &other_room);
    let team = LoginKey::new(kind("codex"), name("team"));
    let native = LoginKey::new(kind("codex"), LoginName::default_login());
    assert_eq!(
        counts,
        BTreeMap::from([(native.clone(), 1), (team.clone(), 2)])
    );
    for (account, column) in counts {
        let refusal: usize = [&room[..], &other_room[..]]
            .iter()
            .map(|agents| count_others_on(agents, &account, &[]))
            .sum();
        assert_eq!(column, refusal);
    }
}

#[test]
fn an_account_counts_its_other_live_agents_and_never_the_one_launching() {
    let agent = |id: &str, login: Option<&str>| {
        let mut agent = AgentState::seed(
            kind("codex"),
            id.into(),
            AgentStatus::Idle,
            jiff::Timestamp::UNIX_EPOCH,
        );
        agent.login = login.map(name);
        agent
    };
    let mut ended = agent("ended", Some("team"));
    ended.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let agents = [
        agent("launching", Some("team")),
        agent("peer", Some("team")),
        agent("native", None),
        ended,
    ];
    let team = LoginKey::new(kind("codex"), name("team"));
    // A seat of the same batch, or a row a rebirth recovered: no pane, so no
    // provider that could be writing.
    assert_eq!(count_others_on(&agents, &team, &[]), 0);
    let agents = agents.map(|mut agent| {
        agent.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::MuxName::Tmux, "%1"),
        ));
        agent
    });
    assert_eq!(count_others_on(&agents, &team, &[]), 2);
    assert_eq!(count_others_on(&agents, &team, &["launching".into()]), 1);
    assert_eq!(
        count_others_on(&agents, &team, &["launching".into(), "peer".into()]),
        0
    );
    assert_eq!(
        count_others_on(&agents, &LoginKey::new(kind("claude"), name("team")), &[]),
        0
    );
}

#[cfg(unix)]
#[test]
fn an_account_stops_counting_a_row_whose_owner_process_is_dead() {
    use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
    let agent = |owner: Option<RuntimeOwner>| {
        let mut agent = AgentState::seed(
            kind("codex"),
            "peer".into(),
            AgentStatus::Idle,
            jiff::Timestamp::UNIX_EPOCH,
        );
        agent.login = Some(name("team"));
        agent.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::MuxName::Tmux, "%1"),
        ));
        agent.runtime_owner = owner;
        agent
    };
    let team = LoginKey::new(kind("codex"), name("team"));
    let count = |owner| count_others_on(&[agent(owner)], &team, &[]);
    // A launch that failed after its pane was bound, or a wrapper killed
    // outright: the row is not ended, and nothing behind it can write.
    let dead = RuntimeOwner::new(RuntimeOwnerKind::Agent, "peer", u32::MAX, None);
    assert_eq!(count(Some(dead)), 0);
    let live = crate::store::runtime::current_process_owner(RuntimeOwnerKind::Agent, "peer");
    assert_eq!(count(Some(live)), 1);
    assert_eq!(count(None), 1);
}
