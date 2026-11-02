use super::*;
use crate::config::Team;
use crate::harness::team_notes;
use crate::store::message::{DeliveryGate, HarnessNotice, MessageSender, MessageStatus};
use std::time::Duration;

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: ResolvedWorkspace,
    store: Store,
    request: StallNudgeRequest,
    agents: Vec<AgentState>,
    now: Timestamp,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let id = crate::WorkspaceId::from_project_root(&checkout);
        let state = crate::StatePaths::under(id.clone(), &temp.path().join("home")).unwrap();
        let runtime = crate::RuntimePaths::for_state_under(&state, temp.path());
        let store = Store::open(state, runtime).unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id: id.clone(),
            project_root: checkout.clone(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: checkout.clone(),
            worktree_branch: None,
            session_name: "test".into(),
            mux_hint: None,
        };
        let team = toml::from_str::<Team>(concat!(
            "leader = 'lead'\nstages = ['Build', 'Review']\n",
            "[[roles]]\nrole = 'lead'\nprofile = 'claude'\nowns = ['Review']\n",
            "[[roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Build']",
        ))
        .unwrap();
        team_notes::register(&store, &checkout, "crew", &team, team_notes::Launch::Fresh).unwrap();
        let start: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        let agents = ["lead", "coder"].map(|role| {
            let mut agent = crate::testkit::agent_state("claude", role, start);
            agent.team = Some("crew".into());
            agent.role = Some(role.into());
            agent.name = Some(format!("live-{role}"));
            agent.name_explicit = true;
            agent
        });
        let fixture = Self {
            _temp: temp,
            workspace,
            store,
            request: StallNudgeRequest {
                workspace_id: id,
                checkout,
                team: "crew".into(),
                leader_role: Some("lead".into()),
                stage: "Build".into(),
                visit_started_at: start,
                stalled_since: start,
                target: (agents[1].kind.clone(), agents[1].agent_id.clone()),
                members: agents
                    .iter()
                    .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
                    .collect(),
            },
            agents: agents.into(),
            now: start + Duration::from_secs(3_300),
        };
        fixture
            .board("Stage: Build (@coder)\n## Progress\n- 2026-01-01 00:00 @user: opened Build\n");
        fixture
    }

    fn board(&self, text: &str) {
        std::fs::write(
            crate::harness::scratch::board_path(&self.request.checkout),
            text,
        )
        .unwrap();
    }

    fn queue(&self) -> Option<MessageRecord> {
        queue(
            &self.request,
            &self.workspace,
            &self.store,
            &TimeZone::UTC,
            || Ok((self.agents.clone(), self.now)),
        )
        .unwrap()
    }

    fn seed(&self, notice: Option<HarnessNotice>, status: MessageStatus, at: Timestamp) {
        let mut message = MessageRecord::new(
            self.workspace.workspace_id.clone(),
            &self.agents[0],
            "pending".into(),
            DeliveryGate::Done,
        );
        if let Some(notice) = notice {
            message.sender = MessageSender::Harness { notice };
        }
        message.enqueued_at = at;
        message.status = status;
        self.store
            .queue_message(&message, &self.workspace.session_name)
            .unwrap();
    }
}

#[test]
fn queues_once_and_refuses_a_second_attempt() {
    let fixture = Fixture::new();
    let first = fixture.queue();
    let second = fixture.queue();
    assert_eq!([first.is_some(), second.is_some()], [true, false]);
    let message = first.unwrap();
    assert_eq!(
        serde_json::to_value(&message.sender).unwrap()["notice"],
        "team_stalled"
    );
    assert_eq!(message.gate, DeliveryGate::Done);
    assert_eq!(fixture.store.list_messages().unwrap().len(), 1);
}

#[test]
fn notices_before_a_return_visit_do_not_suppress_its_nudge() {
    let actual = [
        MessageStatus::Delivered,
        MessageStatus::Canceled,
        MessageStatus::Errored,
    ]
    .map(|status| {
        let mut fixture = Fixture::new();
        fixture.board(concat!(
            "Stage: Build (@coder)\n## Progress\n",
            "- 2026-01-01 00:00 @user: opened Build\n",
            "- 2026-01-01 00:01 @coder: Build -> Review\n",
            "- 2026-01-01 00:02 @lead: Review -> Build\n",
        ));
        fixture.request.visit_started_at += Duration::from_secs(120);
        fixture.seed(
            Some(HarnessNotice::TeamStalled),
            status,
            fixture.request.visit_started_at - Duration::from_secs(1),
        );
        fixture.queue().is_some()
    });
    assert_eq!(actual, [true, true, true]);
}

#[test]
fn any_notice_status_in_this_visit_counts_as_the_attempt() {
    let baseline = Fixture::new().queue().is_some();
    let actual = [
        MessageStatus::Queued,
        MessageStatus::Sent,
        MessageStatus::Delivered,
        MessageStatus::Canceled,
        MessageStatus::Errored,
    ]
    .map(|status| {
        let fixture = Fixture::new();
        fixture.seed(
            Some(HarnessNotice::TeamStalled),
            status,
            fixture.request.visit_started_at,
        );
        fixture.queue().is_some()
    });
    assert_eq!((baseline, actual), (true, [false; 5]));
}

#[test]
fn same_stage_reflip_does_not_open_another_nudge() {
    let fixture = Fixture::new();
    let first = fixture.queue().is_some();
    fixture.board(concat!(
        "Stage: Build (@coder)\n## Progress\n",
        "- 2026-01-01 00:00 @user: opened Build\n",
        "- 2026-01-01 00:01 @coder: Build -> Build\n",
    ));
    assert_eq!([first, fixture.queue().is_some()], [true, false]);
}

#[test]
fn activity_after_the_decision_and_nonresting_members_refuse() {
    let baseline = Fixture::new().queue().is_some();
    let mut fixture = Fixture::new();
    fixture.agents[0].last_activity = fixture.request.stalled_since + Duration::from_secs(1);
    let fresh = fixture.queue().is_some();
    let statuses = [
        crate::agents::AgentStatus::Running,
        crate::agents::AgentStatus::Sleeping,
        crate::agents::AgentStatus::Waiting,
        crate::agents::AgentStatus::Paused,
    ]
    .map(|status| {
        let mut fixture = Fixture::new();
        fixture.agents[0].status = status;
        fixture.queue().is_some()
    });
    assert_eq!((baseline, fresh, statuses), (true, false, [false; 4]));
}

#[test]
fn pending_real_messages_refuse_but_cache_pings_do_not() {
    let actual = [None, Some(HarnessNotice::CacheKeepalive)].map(|notice| {
        let fixture = Fixture::new();
        fixture.seed(notice, MessageStatus::Queued, fixture.now);
        fixture.queue().is_some()
    });
    assert_eq!(actual, [false, true]);
}

#[test]
fn done_or_changed_visits_refuse() {
    let baseline = Fixture::new().queue().is_some();
    let fixture = Fixture::new();
    fixture.board("Stage: Done\n## Progress\n- 2026-01-01 00:00 @user: opened Done\n");
    let done = fixture.queue().is_some();
    fixture.board("Stage: Build (@coder)\n## Progress\n- 2026-01-01 00:01 @user: opened Build\n");
    assert_eq!(
        [baseline, done, fixture.queue().is_some()],
        [true, false, false]
    );
}

#[test]
fn text_routes_nonleaders_to_the_live_leader_and_leaders_to_the_user() {
    let fixture = Fixture::new();
    let message = fixture.queue();
    assert!(message.is_some(), "an eligible teammate receives a notice");
    let text = message.unwrap().text;
    for required in [
        "Build",
        "55m",
        "board",
        "stage file",
        "@lead",
        "once per stage visit",
    ] {
        assert!(text.contains(required), "{text}");
    }
    let mut fixture = Fixture::new();
    fixture.request.target = fixture.request.members[0].clone();
    let text = fixture.queue().unwrap().text;
    assert!(text.contains("ask the user"), "{text}");

    let mut fixture = Fixture::new();
    fixture.request.leader_role = Some("project-lead".into());
    let text = fixture.queue().unwrap().text;
    assert!(text.contains("message @project-lead"), "{text}");
}
