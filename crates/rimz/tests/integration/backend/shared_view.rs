use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd};
use rimz::pane::PaneRef;
use rimz::store::{event::EventEnvelope, event_log};

pub(super) fn agent_layout() -> LayoutPanes {
    LayoutPanes {
        columns: (0..3)
            .map(|_| LayoutColumn {
                panes: (0..4)
                    .map(|_| PaneCmd {
                        argv: vec![
                            "sh".to_owned(),
                            "-c".to_owned(),
                            "printf 'VIEW-AGENTS\\n'; exec sleep 600".to_owned(),
                        ],
                        name: None,
                    })
                    .collect(),
                stacked: false,
            })
            .collect(),
        focused_pane: 0,
    }
}

pub(super) fn seed_agents(state: &rimz::StatePaths, panes: &[PaneRef]) {
    state.ensure_dirs().expect("agent state dirs");
    for (slot, pane) in panes.iter().enumerate() {
        let id = format!("view-agent-{slot:02}");
        let mut observation =
            AgentLifecycleObservation::new(Some(id.clone().into()), LifecycleSignal::Registered);
        observation.agent_name = Some(format!("view-card-{slot:02}"));
        observation.task = Some(format!("view-card-{slot:02}"));
        observation.pane_id = Some(pane.pane_id.clone());
        observation.worktree_path = pane.cwd.clone();
        observation.worktree_branch = Some("main".to_owned());
        event_log::append(
            &state.events_log,
            &EventEnvelope::agent_lifecycle(
                state.workspace_id.clone(),
                &id,
                "claude",
                "SessionStart",
                &observation,
            ),
        )
        .expect("seed live card");
        observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
        event_log::append(
            &state.events_log,
            &EventEnvelope::agent_lifecycle(
                state.workspace_id.clone(),
                id,
                "claude",
                "UserPromptSubmit",
                &observation,
            ),
        )
        .expect("start live card");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SidebarView {
    pub(super) cards: Vec<(String, bool)>,
    pub(super) selection_visible: bool,
}

pub(super) fn sidebar_view(screen: &str) -> SidebarView {
    let mut view = SidebarView {
        cards: Vec::new(),
        selection_visible: false,
    };
    for line in screen.lines() {
        let sidebar = line.split('│').next().unwrap().trim_start();
        let selected = sidebar.starts_with('▌');
        view.selection_visible |= selected;
        if let Some(start) = sidebar.find("view-card-") {
            view.cards
                .push((sidebar[start..].chars().take(12).collect(), selected));
        }
    }
    view
}

pub(super) fn wait_frame(
    runtime: &rimz::RuntimePaths,
    ready: impl Fn(&rimz::sidebar::frame::PaneFrame) -> bool,
    label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let bytes = match std::fs::read(runtime.pane_frame_path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                assert!(
                    Instant::now() < deadline,
                    "{label}: no host frame published"
                );
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            result => result.expect("read host frame"),
        };
        let sample: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let resolved = sample["client_view_fresh"] == true;
        let frame = serde_json::from_value::<rimz::sidebar::frame::PaneFrame>(sample).unwrap();
        if resolved && ready(&frame) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: {:?}, {:?}",
            frame.client_views,
            frame.presence
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn wait_screen(
    mut screen: impl FnMut() -> String,
    ready: impl Fn(&str) -> bool,
    label: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let current = screen();
        if ready(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "{label}:\n{current}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
