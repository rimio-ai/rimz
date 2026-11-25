use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::agents::state::project_display;
use crate::agents::{AgentStatus, DelegatedWait, ParkDemotion};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::row::SidebarRow;

use super::{AgentKey, AgentProjectionIndex};

/// Project each agent row's *displayed* status from its seeded card, its
/// child-folded clock, its live subagents, delegated parks, and waiting
/// subagents, and its source agent, through the rung order
/// `agents::state::project_display` holds.
#[expect(
    clippy::too_many_arguments,
    reason = "projection inputs stay explicit rather than adding a one-shot parameter struct"
)]
pub(super) fn project_display_status(
    rows: &mut [SidebarRow],
    index: &AgentProjectionIndex<'_>,
    demotion: &ParkDemotion,
    delegated_parks: &BTreeMap<AgentKey, (AgentStatus, String)>,
    delegated_waits: &BTreeMap<AgentKey, DelegatedWait>,
    now: Timestamp,
    stalled_after_secs: u32,
    tool_repeat_attention_after: u32,
) {
    for row in rows.iter_mut() {
        let last_activity = row.last_activity;
        let key: AgentKey = (
            AgentKind::new_unchecked(row.name.clone()),
            AgentSessionId::from(row.id.as_str()),
        );
        let source_agent = index.root(&key);
        let Some(agent) = row.as_agent_mut() else {
            continue;
        };
        let has_live_child = agent
            .sub_agents
            .iter()
            .any(|child| child.holds_parent_turn());
        let delegated_park = delegated_parks
            .get(&key)
            .map(|(status, label)| (*status, label.as_str()));
        let delegated_wait = delegated_waits.get(&key);
        // A launched child rendered as its own row has no root source; only its turn start and
        // the refusal step read its own state, and every other rung keeps what it sees without one.
        project_display(
            source_agent,
            source_agent.or_else(|| index.launched(&key)),
            &mut agent.status,
            &mut agent.phase,
            &mut agent.turn_error_label,
            &mut agent.overlay_raised_at,
            &mut agent.ask_detail,
            &mut agent.extra_questions,
            &mut agent.delegated_wait,
            agent.context.as_ref(),
            last_activity,
            agent.tool_repeat.as_ref(),
            has_live_child,
            delegated_park,
            delegated_wait,
            demotion,
            now,
            stalled_after_secs,
            tool_repeat_attention_after,
        );
    }
}
