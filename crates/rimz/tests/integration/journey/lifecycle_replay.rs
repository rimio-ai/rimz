use rimz::ids::MuxName;
use serde_json::{Value, json};

use super::{
    RoomHarness, SETTLE, compacting_row, pi_agent_end, pi_before_agent_start,
    pi_session_before_compact, pi_session_compact, pi_session_shutdown, pi_session_start,
    pi_tool_execution_end, post_compact, post_tool_use, pre_compact, running_row, session_end,
    session_start, session_start_compact, stop_failure, stop_turn, subagent_start, subagent_stop,
    thinking_row, user_prompt_submit,
};
use crate::common::Env;

#[derive(Clone, Copy)]
enum ReplayAgent {
    Claude,
    Codex,
    Pi,
}

enum ContextVia {
    Payload,
    Statusline,
    None,
}

impl ReplayAgent {
    fn source(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "coder",
            Self::Pi => "pi",
        }
    }

    fn session(self) -> &'static str {
        match self {
            Self::Claude => "claude-life",
            Self::Codex => "codex-life",
            Self::Pi => "pi-life",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Self::Claude => "Opus",
            Self::Codex | Self::Pi => "GPT-5.5",
        }
    }

    fn model_label(self) -> &'static str {
        match self {
            Self::Claude => "Opus",
            Self::Codex | Self::Pi => "GPT 5.5",
        }
    }

    fn effort(self) -> &'static str {
        match self {
            Self::Claude => "xhigh",
            Self::Codex | Self::Pi => "high",
        }
    }

    fn branch(self) -> &'static str {
        "main"
    }

    fn context_via(self) -> ContextVia {
        match self {
            Self::Claude => ContextVia::Statusline,
            Self::Codex => ContextVia::None,
            Self::Pi => ContextVia::Payload,
        }
    }

    fn register(self) -> Value {
        match self {
            Self::Pi => pi_session_start(
                self.session(),
                self.model(),
                self.effort(),
                self.branch(),
                54,
            ),
            Self::Claude | Self::Codex => {
                session_start(self.session(), self.model(), self.effort(), self.branch())
            }
        }
    }

    fn prompt(self, prompt: &str) -> Value {
        match self {
            Self::Pi => pi_before_agent_start(self.session(), prompt),
            Self::Claude | Self::Codex => user_prompt_submit(self.session(), prompt),
        }
    }

    fn command_tool(self) -> Value {
        match self {
            Self::Claude => post_tool_use(self.session(), "Bash"),
            Self::Codex => post_tool_use(self.session(), "shell"),
            Self::Pi => pi_tool_execution_end(self.session(), "bash", false),
        }
    }

    fn edit_tool(self) -> Value {
        match self {
            Self::Claude => post_tool_use(self.session(), "Edit"),
            Self::Codex => post_tool_use(self.session(), "apply_patch"),
            Self::Pi => pi_tool_execution_end(self.session(), "edit", true),
        }
    }

    fn compact_pre(self) -> Value {
        match self {
            Self::Pi => pi_session_before_compact(self.session()),
            Self::Claude | Self::Codex => pre_compact(self.session()),
        }
    }

    fn compact_post(self) -> Value {
        match self {
            Self::Claude => post_compact(self.session()),
            Self::Codex => {
                session_start_compact(self.session(), self.model(), self.effort(), self.branch())
            }
            Self::Pi => pi_session_compact(self.session()),
        }
    }

    fn turn_end_clean(self) -> Value {
        match self {
            Self::Pi => pi_agent_end(self.session(), false),
            Self::Claude | Self::Codex => stop_turn(self.session()),
        }
    }

    fn turn_end_errored(self) -> Value {
        match self {
            Self::Claude => stop_failure(self.session()),
            Self::Codex => {
                let mut payload = stop_turn(self.session());
                payload["status"] = json!("failed");
                payload
            }
            Self::Pi => pi_agent_end(self.session(), true),
        }
    }

    fn session_end(self) -> Option<Value> {
        match self {
            Self::Claude => Some(session_end(self.session())),
            Self::Codex => None,
            Self::Pi => Some(pi_session_shutdown(self.session())),
        }
    }
}

#[test]
fn claude_full_lifecycle() {
    replay_full_lifecycle(ReplayAgent::Claude);
}

#[test]
fn codex_full_lifecycle() {
    replay_full_lifecycle(ReplayAgent::Codex);
}

#[test]
fn pi_full_lifecycle() {
    replay_full_lifecycle(ReplayAgent::Pi);
}

fn replay_full_lifecycle(agent: ReplayAgent) {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&[agent.source()]);

    room.agent_hook(agent.source(), &agent.register());
    let screen = room.wait_for(
        |s| s.contains(&format!("○ {}", agent.role())) && s.contains(agent.model_label()),
        SETTLE,
    );
    assert!(
        screen.contains(&format!("○ {}", agent.role())),
        "registered agent renders idle:\n{screen}"
    );
    assert!(
        screen.contains(agent.model_label()),
        "registered agent renders its model:\n{screen}"
    );

    let task = format!("fix {} lifecycle", agent.source());
    room.agent_hook(agent.source(), &agent.prompt(&task));
    let screen = room.wait_for(
        |s| thinking_row(s, agent.role()) && s.contains(&task),
        SETTLE,
    );
    assert!(
        thinking_row(&screen, agent.role()),
        "prompt opens the thinking head:\n{screen}"
    );
    assert!(screen.contains(&task), "task text renders:\n{screen}");

    room.agent_hook(agent.source(), &agent.command_tool());
    let screen = room.wait_for(|s| thinking_row(s, agent.role()), SETTLE);
    assert!(
        thinking_row(&screen, agent.role()),
        "command-only tool keeps the thinking head:\n{screen}"
    );

    room.agent_hook(agent.source(), &agent.edit_tool());
    let screen = room.wait_for(|s| running_row(s, agent.role()), SETTLE);
    assert!(
        running_row(&screen, agent.role()),
        "first edit flips the row to working:\n{screen}"
    );

    match agent.context_via() {
        ContextVia::Payload => {
            let screen = room.wait_for(has_context_gauge, SETTLE);
            assert!(
                has_context_gauge(&screen),
                "payload context renders the gauge:\n{screen}"
            );
        }
        ContextVia::Statusline => {
            let payload = format!(
                r#"{{
                    "session_id": "{}",
                    "model": {{ "id": "claude-opus-4-8", "display_name": "Opus" }},
                    "context_window": {{ "context_window_size": 200000, "used_percentage": 42 }}
                }}"#,
                agent.session()
            );
            let out = room.run_statusline_feed(agent.source(), &payload);
            assert!(
                out.status.success(),
                "statusline feed failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let screen = room.wait_for(has_context_gauge, SETTLE);
            assert!(
                has_context_gauge(&screen),
                "statusline context renders the gauge:\n{screen}"
            );
        }
        ContextVia::None => {}
    }

    room.agent_hook(agent.source(), &agent.compact_pre());
    let screen = room.wait_for(|s| compacting_row(s, agent.role()), SETTLE);
    assert!(
        compacting_row(&screen, agent.role()),
        "compaction pre-hook overlays the compacting head:\n{screen}"
    );

    room.agent_hook(agent.source(), &agent.compact_post());
    let screen = room.wait_for(|s| running_row(s, agent.role()), SETTLE);
    assert!(
        running_row(&screen, agent.role()),
        "compaction close restores the working head:\n{screen}"
    );

    room.agent_hook(agent.source(), &agent.turn_end_clean());
    let screen = room.wait_for(
        |s| s.contains(&format!("✓ {}", agent.role())) && s.contains(&task),
        SETTLE,
    );
    assert!(
        screen.contains(&format!("✓ {}", agent.role())),
        "clean turn end renders success:\n{screen}"
    );
    assert!(
        screen.contains(&task),
        "success row keeps the turn prompt:\n{screen}"
    );

    if let Some(payload) = agent.session_end() {
        room.agent_hook(agent.source(), &payload);
        let screen = room.wait_for(|s| !s_contains_role(s, agent.role()), SETTLE);
        assert!(
            !s_contains_role(&screen, agent.role()),
            "session end retires the agent row:\n{screen}"
        );
    }
}

#[test]
fn errored_turn_renders_failed() {
    for agent in [ReplayAgent::Claude, ReplayAgent::Codex] {
        let env = Env::new();
        if env.skip_if_sandboxed() {
            return;
        }
        let room = RoomHarness::launch(&env, MuxName::Tmux);
        room.onboard(&[agent.source()]);
        room.agent_hook(agent.source(), &agent.register());
        room.wait_for(|s| s.contains(&format!("○ {}", agent.role())), SETTLE);

        let task = format!("break {} turn", agent.source());
        room.agent_hook(agent.source(), &agent.prompt(&task));
        room.wait_for(
            |s| thinking_row(s, agent.role()) && s.contains(&task),
            SETTLE,
        );

        let errored = agent.turn_end_errored();
        match agent {
            ReplayAgent::Claude => room.agent_hook_in_room_runtime(agent.source(), &errored),
            ReplayAgent::Codex | ReplayAgent::Pi => room.agent_hook(agent.source(), &errored),
        }
        let screen = room.wait_for(
            |s| {
                s.contains(&format!("! {}", agent.role())) && s.contains("! 1") && s.contains(&task)
            },
            SETTLE,
        );
        assert!(
            screen.contains(&format!("! {}", agent.role())),
            "errored turn renders failed row:\n{screen}"
        );
        assert!(
            screen.contains("! 1"),
            "failed row counts in tally:\n{screen}"
        );
        assert!(
            screen.contains(&task),
            "failed row keeps the triggering task:\n{screen}"
        );
    }
}

#[test]
fn subagent_child_row_appears_and_clears() {
    for agent in [ReplayAgent::Claude, ReplayAgent::Codex] {
        let env = Env::new();
        if env.skip_if_sandboxed() {
            return;
        }
        let room = RoomHarness::launch(&env, MuxName::Tmux);
        room.onboard(&[agent.source()]);
        room.agent_hook(agent.source(), &agent.register());
        room.agent_hook(agent.source(), &agent.prompt("parent task"));
        room.wait_for(|s| thinking_row(s, agent.role()), SETTLE);

        let child_id = format!("{}-child", agent.source());
        room.agent_hook(agent.source(), &subagent_start(agent.session(), &child_id));
        let screen = room.wait_for(
            |s| s.contains("subagents (1)") && s.contains("review"),
            SETTLE,
        );
        assert!(
            screen.contains("subagents (1)") && screen.contains("review"),
            "subagent start renders the child under the selected parent:\n{screen}"
        );

        room.agent_hook(
            agent.source(),
            &subagent_stop(agent.session(), &child_id, false),
        );
        let screen = room.wait_for(|s| s.contains("✓ review"), SETTLE);
        assert!(
            screen.contains("✓ review"),
            "subagent stop settles the child to success for the current turn:\n{screen}"
        );

        room.agent_hook(agent.source(), &agent.prompt("next parent turn"));
        let screen = room.wait_for(
            |s| s.contains("next parent turn") && !s.contains("subagents (1)"),
            SETTLE,
        );
        assert!(
            !screen.contains("subagents (1)") && !screen.contains("✓ review"),
            "next parent turn retires the finished child:\n{screen}"
        );
    }
}

fn has_context_gauge(screen: &str) -> bool {
    screen.contains('▣') && screen.contains('━')
}

fn s_contains_role(screen: &str, role: &str) -> bool {
    screen
        .lines()
        .any(|line| line.contains(&format!(" {role}")))
}
