//! Agent pane display titles and the short labels used by tabs.

use super::{AgentState, LaunchParams};
use crate::ids::{AgentKind, AgentSessionId};

/// Borrowed identity of a stored or about-to-launch agent seat.
pub struct Seat<'a> {
    kind: &'a AgentKind,
    profile: Option<&'a str>,
    team: Option<&'a str>,
    role: Option<&'a str>,
    name: Option<&'a str>,
    parent_id: Option<&'a AgentSessionId>,
    parent_kind: Option<&'a AgentKind>,
}

impl<'a> Seat<'a> {
    pub fn from_launch(
        kind: &'a AgentKind,
        name: Option<&'a str>,
        launch: &'a LaunchParams,
    ) -> Self {
        Self {
            kind,
            profile: launch.profile.as_deref(),
            team: launch.team.as_deref(),
            role: launch.role.as_deref(),
            name,
            parent_id: launch.parent_agent_id.as_ref(),
            parent_kind: launch.parent_agent_kind.as_ref(),
        }
    }
}

impl<'a> From<&'a AgentState> for Seat<'a> {
    fn from(agent: &'a AgentState) -> Self {
        Self {
            kind: &agent.kind,
            profile: agent.profile.as_deref(),
            team: agent.team.as_deref(),
            role: agent.role.as_deref(),
            name: agent.name.as_deref(),
            parent_id: agent.parent_agent_id.as_ref(),
            parent_kind: agent.parent_agent_kind.as_ref(),
        }
    }
}

/// The profile, else the provider kind, without instance or ancestry context.
pub fn short_label<'a>(profile: Option<&'a str>, kind: &'a AgentKind) -> &'a str {
    profile.unwrap_or_else(|| kind.as_str())
}

/// Compose a pane's identity, following at most four parent links.
pub fn title(seat: Seat<'_>, agents: &[AgentState]) -> String {
    title_with_parents(seat, agents, 4)
}

fn title_with_parents(seat: Seat<'_>, agents: &[AgentState], remaining: u8) -> String {
    let short = short_label(seat.profile, seat.kind);
    let mut own = if seat.parent_id.is_some() {
        format!("{short} subagent")
    } else if let Some(team) = seat.team {
        format!("{team} · {}", seat.role.unwrap_or(short))
    } else {
        short.to_owned()
    };
    if let Some(name) = seat.name.filter(|name| !name.is_empty()) {
        own.push_str(&format!(" (@{name})"));
    }
    if remaining == 0 {
        return own;
    }
    let Some(parent_id) = seat.parent_id else {
        return own;
    };
    let parent_kind = seat.parent_kind.unwrap_or(seat.kind);
    let Some(parent) = agents
        .iter()
        .filter(|agent| &agent.kind == parent_kind && agent.answers_to(parent_id))
        .min_by_key(|agent| agent.ended_at.is_some())
    else {
        return own;
    };
    let parent = title_with_parents(Seat::from(parent), agents, remaining - 1);
    format!("{parent} › {own}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn agent(kind: &str, id: &str, name: &str, profile: Option<&str>) -> AgentState {
        let mut agent = crate::testkit::agent_state(kind, id, Timestamp::UNIX_EPOCH);
        agent.name = Some(name.to_owned());
        agent.profile = profile.map(str::to_owned);
        agent
    }

    #[test]
    fn root_titles_show_profile_or_kind_and_the_addressable_name() {
        let mut seat = agent("claude", "root", "brave-fox", Some("coder"));
        assert_eq!(title(Seat::from(&seat), &[]), "coder (@brave-fox)");
        seat.profile = None;
        assert_eq!(title(Seat::from(&seat), &[]), "claude (@brave-fox)");
        seat.name_explicit = true;
        seat.name = Some("reviewer".into());
        assert_eq!(title(Seat::from(&seat), &[]), "claude (@reviewer)");
    }

    #[test]
    fn nameless_seats_omit_the_parenthetical() {
        let mut seat = agent("claude", "root", "", Some("coder"));
        assert_eq!(title(Seat::from(&seat), &[]), "coder");
        seat.name = None;
        assert_eq!(title(Seat::from(&seat), &[]), "coder");
    }

    #[test]
    fn team_titles_lead_with_the_role_or_short_label() {
        let mut seat = agent("claude", "root", "swift-otter", Some("coder"));
        seat.team = Some("recon".into());
        seat.role = Some("planner".into());
        assert_eq!(
            title(Seat::from(&seat), &[]),
            "recon · planner (@swift-otter)"
        );
        seat.role = None;
        assert_eq!(
            title(Seat::from(&seat), &[]),
            "recon · coder (@swift-otter)"
        );
        seat.team = None;
        seat.role = Some("planner".into());
        assert_eq!(title(Seat::from(&seat), &[]), "coder (@swift-otter)");
    }

    #[test]
    fn children_prefix_the_parent_title_and_ignore_their_own_team() {
        let mut parent = agent("claude", "parent", "swift-otter", Some("coder"));
        parent.team = Some("recon".into());
        parent.role = Some("planner".into());
        let mut child = agent("codex", "child", "gold-tower", Some("explorer"));
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.team = Some("ignored".into());
        child.role = Some("ignored".into());
        assert_eq!(
            title(Seat::from(&child), &[parent]),
            "recon · planner (@swift-otter) › explorer subagent (@gold-tower)"
        );
        let parent = agent("claude", "parent", "brave-fox", Some("coder"));
        child.kind = parent.kind.clone();
        child.profile = None;
        assert_eq!(
            title(Seat::from(&child), &[parent]),
            "coder (@brave-fox) › claude subagent (@gold-tower)"
        );
    }

    #[test]
    fn a_missing_parent_keeps_the_subagent_marker() {
        let mut child = agent("codex", "child", "gold-tower", Some("explorer"));
        child.parent_agent_id = Some("missing".into());
        assert_eq!(
            title(Seat::from(&child), &[]),
            "explorer subagent (@gold-tower)"
        );
    }

    #[test]
    fn parents_match_launch_aliases_with_same_kind_as_the_legacy_default() {
        let mut parent = agent("claude", "native-session", "brave-fox", Some("coder"));
        parent.launch_id = Some("launch".into());
        parent.prior_launch_ids = vec!["earlier-launch".into()];
        let mut child = agent("claude", "child", "gold-tower", None);
        for id in ["native-session", "launch", "earlier-launch"] {
            child.parent_agent_id = Some(id.into());
            assert_eq!(
                title(Seat::from(&child), std::slice::from_ref(&parent)),
                "coder (@brave-fox) › claude subagent (@gold-tower)"
            );
        }
        child.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
        assert_eq!(
            title(Seat::from(&child), &[parent]),
            "claude subagent (@gold-tower)"
        );
    }

    #[test]
    fn an_unended_parent_wins_over_a_historical_alias_match() {
        let mut ended = agent("claude", "old", "old-fox", Some("stale"));
        ended.launch_id = Some("launch".into());
        ended.ended_at = Some(Timestamp::UNIX_EPOCH);
        let mut live = agent("claude", "new", "brave-fox", Some("coder"));
        live.launch_id = Some("launch".into());
        let mut child = agent("claude", "child", "gold-tower", None);
        child.parent_agent_id = Some("launch".into());
        assert_eq!(
            title(Seat::from(&child), &[ended, live]),
            "coder (@brave-fox) › claude subagent (@gold-tower)"
        );
    }

    #[test]
    fn synthetic_nested_children_compose_parent_first() {
        let a = agent("claude", "a", "first", Some("a"));
        let mut b = agent("claude", "b", "second", Some("b"));
        b.parent_agent_id = Some(a.agent_id.clone());
        let mut c = agent("claude", "c", "third", Some("c"));
        c.parent_agent_id = Some(b.agent_id.clone());
        assert_eq!(
            title(Seat::from(&c), &[a, b]),
            "a (@first) › b subagent (@second) › c subagent (@third)"
        );
    }

    #[test]
    fn a_self_parent_terminates_with_a_bounded_title() {
        let mut seat = agent("claude", "self", "gold-tower", Some("explorer"));
        seat.parent_agent_id = Some(seat.agent_id.clone());
        let rendered = title(Seat::from(&seat), std::slice::from_ref(&seat));
        assert!(rendered.ends_with("explorer subagent (@gold-tower)"));
        assert!(rendered.len() < 200, "{rendered}");
    }

    #[test]
    fn launch_and_stored_seats_share_the_title_rule() {
        let kind = AgentKind::new_unchecked("claude");
        let launch = LaunchParams {
            profile: Some("opus-4.5".into()),
            team: Some("recon".into()),
            role: Some("planner".into()),
            ..Default::default()
        };
        assert_eq!(
            title(Seat::from_launch(&kind, Some("swift-otter"), &launch), &[]),
            "recon · planner (@swift-otter)"
        );
        assert_eq!(short_label(launch.profile.as_deref(), &kind), "opus-4.5");
        assert_eq!(short_label(None, &kind), "claude");
    }
}
