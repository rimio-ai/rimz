use super::*;

fn reserve(room: &Room, name: &str) {
    use rimz::store::writer::{AgentLaunchName, AgentLaunchRequest, AgentLaunchScope, LaunchLogin};
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    room.env
        .store()
        .begin_agent_launch_batch(
            &[AgentLaunchRequest {
                kind: rimz::ids::AgentKind::new_unchecked("claude"),
                login: LaunchLogin::RoomDefault,
                agent_id: format!("launch_{name}").into(),
                name: AgentLaunchName::Explicit(name.into()),
                launch: Default::default(),
                run_id: None,
                prompt: None,
            }],
            AgentLaunchScope {
                session_name: workspace.session_name,
                cwd: room.env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
}

fn occupant(room: &Room, name: &str, pane: u32) {
    reserve(room, name);
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some(name.into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some(name.into());
    observation.pane_id = Some(rimz::PaneId::parse(&format!("zellij:terminal_{pane}")).unwrap());
    observation.worktree_path = Some(room.env.project_root.display().to_string());
    observation.runtime_owner = Some(rimz::store::runtime::process_owner(
        rimz::pane::RuntimeOwnerKind::Agent,
        name,
        room.owner(name),
    ));
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    room.env
        .store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
}

#[test]
fn a_takeover_that_cannot_stop_its_occupant_launches_nothing() {
    let room = Room::with_kind("admission.occupancy='takeover'\n", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    occupant(&room, "outside", 61);
    let id = room.start();
    let value = until(&room.env, &id, |value| {
        value["record"]["wait"]["outcome"] == "retry"
    });
    assert!(value["record"]["attempt"].is_null(), "{value:#}");
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(
        !trace.contains("new-tab") && !trace.contains("close-pane"),
        "{trace}"
    );
    assert!(rimz::proc::process_is_live(room.owner("outside"), None));
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn a_working_occupant_that_arrives_during_a_stop_is_returned_as_occupancy() {
    let room = Room::with_kind("admission.occupancy='takeover'\n", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    occupant(&room, "first", 61);
    reserve(&room, "late");
    let panes = room.env.project_root.join("panes.json");
    fs::write(
        &panes,
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"claude"}]"#,
    )
    .unwrap();
    let shim = crate::common::write_path_shim(
        &room.env.home_root.join("takeover-bin"),
        "zellij",
        r#""$RIMZ_TEST_TRACE" "$@" || exit "$?"
for arg do
    if [ "$arg" = close-pane ]; then
        printf '%s' '[{"id":62,"is_plugin":false,"tab_id":1,"title":"claude"}]' > "$RIMZ_TEST_LATE_PANES"
        printf '%s' "$RIMZ_TEST_LATE_REGISTER" | RIMZ_AGENT_NAME=late RIMZ_AGENT_PID="$RIMZ_TEST_LATE_PID" "$RIMZ_TEST_RIMZ" hooks feed --source claude || exit "$?"
        printf '%s' "$RIMZ_TEST_LATE_PROMPT" | RIMZ_AGENT_NAME=late RIMZ_AGENT_PID="$RIMZ_TEST_LATE_PID" "$RIMZ_TEST_RIMZ" hooks feed --source claude || exit "$?"
        exec "$RIMZ_TEST_RIMZ" hooks drain --project-root "$RIMZ_TEST_SCOPE" --once
    fi
done
"#,
    );
    let receipt = success(
        room.command(&["workflow", "run", "review", "--prompt", "Review this"])
            .env("RIMZ_ZELLIJ_BIN", shim)
            .env("RIMZ_TEST_TRACE", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_LATE_PANES", &panes)
            .env("RIMZ_TEST_LATE_PID", room.owner("late").to_string())
            .env("RIMZ_TEST_RIMZ", room.env.rimz().get_program())
            .env("RIMZ_TEST_SCOPE", &room.env.project_root)
            .env("ZELLIJ_PANE_ID", "62")
            .env("ZELLIJ", "1")
            .env(
                "RIMZ_TEST_LATE_REGISTER",
                json!({
                    "hook_event_name":"SessionStart", "session_id":"late",
                    "cwd":room.env.project_root
                })
                .to_string(),
            )
            .env(
                "RIMZ_TEST_LATE_PROMPT",
                json!({
                    "hook_event_name":"UserPromptSubmit", "session_id":"late",
                    "cwd":room.env.project_root, "prompt":"working"
                })
                .to_string(),
            )
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    let value = until(&room.env, id, |value| {
        value["record"]["wait"]["outcome"] == "takeover"
    });
    assert!(value["record"]["attempt"].is_null(), "{value:#}");
    assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(trace.contains("close-pane"), "{trace}");
    assert!(!trace.contains("new-tab"), "{trace}");
    assert!(!trace.contains("--pane-id\tterminal_62"), "{trace}");
    assert!(rimz::proc::process_is_live(room.owner("late"), None));
    success(room.command(&["workflow", "cancel", id]).output().unwrap());
}
