//! State-derived agent-card templates. Lifecycle, quiet age, density, and reach choose an ordered slot skeleton; provider enrichment only fills the chosen slots. Read idle and done history can shrink to one line.

use crate::agents::AgentStatus;
use crate::config::CardDensityMode;
use crate::sidebar_pane::render::fmt::age_secs;
use crate::store::snapshot::SidebarRow;

use super::CardExpansion;
use super::description::awaiting_first_prompt;

/// The card lifecycle state. Its slot set is stable; enrichment only changes
/// the contents of those slots, including a waiting description's line count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardStage {
    Fresh { labeled: bool },
    Engaged,
    Stale,
}

impl CardStage {
    pub(super) fn of(row: &SidebarRow) -> Self {
        let Some(agent) = row.as_agent() else {
            return Self::Engaged;
        };
        if matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
            && agent.prompt.is_none()
            && !agent.has_session_history()
            && row.context_gauge_percent().unwrap_or(0) == 0
        {
            Self::Fresh {
                labeled: !awaiting_first_prompt(row),
            }
        } else {
            Self::Engaged
        }
    }

    /// The stage every `template` caller draws from: a read card quiet past
    /// `stale_after_secs` is `Stale` unless live or held selection, an open
    /// delegation section, or `expanded` density keeps it open.
    pub(super) fn resolve(
        row: &SidebarRow,
        density: CardDensityMode,
        expansion: CardExpansion,
        stale_after_secs: u64,
        now: jiff::Timestamp,
    ) -> Self {
        if !row.unread
            && !expansion.reach.opens_card()
            && !expansion.delegation
            && density != CardDensityMode::Expanded
            && stale_after_secs > 0
            && age_secs(row.last_activity, now).unsigned_abs() > stale_after_secs
        {
            Self::Stale
        } else {
            Self::of(row)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardSlot {
    Stale,
    Identity,
    Description,
    Question,
    Gauge,
    Tokens,
    Cache,
    /// The current turn's child count and cost plus armed one-shot waits;
    /// empty until either exists.
    Delegation,
    /// Unfinished and newest finished child entries, a count of the other
    /// finished children, and pending waits.
    DelegationEntries,
}

const IDENTITY: &[CardSlot] = &[CardSlot::Identity];
const IDENTITY_DESCRIPTION: &[CardSlot] = &[CardSlot::Identity, CardSlot::Description];
const IDENTITY_DESCRIPTION_GAUGE: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Question,
    CardSlot::Gauge,
];
const ENGAGED: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Question,
    CardSlot::Gauge,
    CardSlot::Tokens,
    CardSlot::Cache,
    CardSlot::Delegation,
];
const ENGAGED_EXPANDED: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Question,
    CardSlot::Gauge,
    CardSlot::Tokens,
    CardSlot::Cache,
    CardSlot::Delegation,
    CardSlot::DelegationEntries,
];
const ENGAGED_RESTING: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Question,
    CardSlot::Gauge,
    CardSlot::Delegation,
];
const ENGAGED_RESTING_EXPANDED: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Question,
    CardSlot::Gauge,
    CardSlot::Delegation,
    CardSlot::DelegationEntries,
];

/// Whether `row` draws as a card that has not started its first turn.
pub(in crate::sidebar_pane::render) fn is_fresh(row: &SidebarRow) -> bool {
    matches!(CardStage::of(row), CardStage::Fresh { .. })
}

/// The lines a selected Engaged card always draws, one per slot that is never empty: what
/// a birth reveal reserves for a card still Fresh.
pub(in crate::sidebar_pane::render) fn engaged_base_lines() -> usize {
    ENGAGED
        .iter()
        .filter(|slot| {
            !matches!(
                slot,
                CardSlot::Question | CardSlot::Cache | CardSlot::Delegation
            )
        })
        .count()
}

/// The ordered slot skeleton for one agent-card state.
pub(super) fn template(
    stage: CardStage,
    status: AgentStatus,
    density: CardDensityMode,
    expansion: CardExpansion,
) -> &'static [CardSlot] {
    if let CardStage::Fresh { labeled } = stage {
        return if labeled && density != CardDensityMode::Compact {
            IDENTITY_DESCRIPTION
        } else {
            IDENTITY
        };
    }
    let expanded = expansion.reach.opens_card();
    if stage == CardStage::Stale
        && matches!(status, AgentStatus::Idle | AgentStatus::Success)
        && !expanded
        && density != CardDensityMode::Expanded
    {
        return &[CardSlot::Stale];
    }
    if density == CardDensityMode::Compact && !expanded {
        return match status {
            AgentStatus::Idle => IDENTITY,
            AgentStatus::Running | AgentStatus::Waiting => IDENTITY_DESCRIPTION_GAUGE,
            AgentStatus::Paused
            | AgentStatus::Success
            | AgentStatus::Failed
            | AgentStatus::Sleeping => IDENTITY_DESCRIPTION,
        };
    }
    match (
        expanded || density == CardDensityMode::Expanded,
        expansion.delegation,
    ) {
        (true, true) => ENGAGED_EXPANDED,
        (true, false) => ENGAGED,
        (false, true) => ENGAGED_RESTING_EXPANDED,
        (false, false) => ENGAGED_RESTING,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::SelectionReach;
    use super::*;

    #[test]
    fn ended_idle_child_with_a_carried_prompt_is_not_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = crate::disk::paths::StatePaths::under(workspace, dir.path()).unwrap();
        std::fs::create_dir_all(paths.agents_carryover.parent().unwrap()).unwrap();
        let now = jiff::Timestamp::from_second(1_750_000_000).unwrap();
        let mut child = crate::testkit::agent_state("claude", "child", now);
        child.parent_agent_id = Some("parent".into());
        child.launch_depth = Some(1);
        child.ended_at = Some(now);
        child.prompt = Some("finish\n the task".to_owned());
        std::fs::write(
            &paths.agents_carryover,
            serde_json::to_vec(&serde_json::json!({"agents": [child]})).unwrap(),
        )
        .unwrap();
        let (_, agents, _) = crate::store::snapshot::RollupCursor::new()
            .fold(&paths)
            .unwrap();
        let child = agents.iter().next().unwrap();
        let row = SidebarRow {
            id: child.agent_id.to_string(),
            name: "child".to_owned(),
            pane: None,
            worktree_path: None,
            checkout: None,
            worktree_branch: None,
            channel: None,
            lane: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            attention_at_ms: None,
            last_activity: now,
            card: crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
                crate::store::snapshot::AgentCard::from_agent(child, now),
            )),
        };
        assert_eq!(CardStage::of(&row), CardStage::Engaged);
    }

    const STAGES: [CardStage; 4] = [
        CardStage::Fresh { labeled: false },
        CardStage::Fresh { labeled: true },
        CardStage::Engaged,
        CardStage::Stale,
    ];
    const STATUSES: [AgentStatus; 7] = [
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Paused,
        AgentStatus::Sleeping,
        AgentStatus::Success,
        AgentStatus::Failed,
    ];
    const DENSITIES: [CardDensityMode; 3] = [
        CardDensityMode::Auto,
        CardDensityMode::Expanded,
        CardDensityMode::Compact,
    ];
    const REACHES: [SelectionReach; 3] = [
        SelectionReach::Unreached,
        SelectionReach::Held,
        SelectionReach::Selected,
    ];

    fn expected_template(
        stage: CardStage,
        status: AgentStatus,
        density: CardDensityMode,
        expansion: CardExpansion,
    ) -> Vec<CardSlot> {
        if let CardStage::Fresh { labeled } = stage {
            return if labeled && density != CardDensityMode::Compact {
                IDENTITY_DESCRIPTION
            } else {
                IDENTITY
            }
            .to_vec();
        }
        if stage == CardStage::Stale
            && matches!(status, AgentStatus::Idle | AgentStatus::Success)
            && !expansion.reach.opens_card()
            && density != CardDensityMode::Expanded
        {
            return vec![CardSlot::Stale];
        }
        if density == CardDensityMode::Compact && !expansion.reach.opens_card() {
            return match status {
                AgentStatus::Idle => IDENTITY,
                AgentStatus::Running | AgentStatus::Waiting => IDENTITY_DESCRIPTION_GAUGE,
                AgentStatus::Paused
                | AgentStatus::Success
                | AgentStatus::Failed
                | AgentStatus::Sleeping => IDENTITY_DESCRIPTION,
            }
            .to_vec();
        }
        let mut slots = match (stage, expansion.reach.opens_card(), expansion.delegation) {
            (CardStage::Fresh { .. }, _, _) => unreachable!("fresh cards returned above"),
            (CardStage::Engaged | CardStage::Stale, _, true) => ENGAGED_EXPANDED,
            (CardStage::Engaged | CardStage::Stale, _, false) => ENGAGED,
        }
        .to_vec();
        if !expansion.reach.opens_card() && density != CardDensityMode::Expanded {
            slots.retain(|slot| !matches!(slot, CardSlot::Tokens | CardSlot::Cache));
        }
        slots
    }

    /// Engaged card shape follows live or held selection; delegation entries
    /// follow the resolved open state alone, so a selected card can close its
    /// entries and an unselected one can open them.
    #[test]
    fn table_pins_every_state_status_density_and_expansion_combination() {
        assert!(ENGAGED_EXPANDED.starts_with(ENGAGED));
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for stage in STAGES {
            for status in STATUSES {
                for density in DENSITIES {
                    for reach in REACHES {
                        for delegation in [false, true] {
                            let expansion = CardExpansion { reach, delegation };
                            actual.push((
                                stage,
                                status,
                                density,
                                expansion,
                                template(stage, status, density, expansion).to_vec(),
                            ));
                            expected.push((
                                stage,
                                status,
                                density,
                                expansion,
                                expected_template(stage, status, density, expansion),
                            ));
                        }
                    }
                }
            }
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn fresh_templates_never_gain_session_stats() {
        for stage in [
            CardStage::Fresh { labeled: false },
            CardStage::Fresh { labeled: true },
        ] {
            for status in STATUSES {
                for density in DENSITIES {
                    for reach in REACHES {
                        let template = template(
                            stage,
                            status,
                            density,
                            CardExpansion {
                                reach,
                                delegation: reach == SelectionReach::Selected,
                            },
                        );
                        assert!(!template.contains(&CardSlot::Tokens));
                        assert!(!template.contains(&CardSlot::Delegation));
                        assert!(!template.contains(&CardSlot::DelegationEntries));
                    }
                }
            }
        }
        assert_eq!(
            template(
                CardStage::Fresh { labeled: false },
                AgentStatus::Idle,
                CardDensityMode::Auto,
                CardExpansion::default()
            ),
            IDENTITY
        );
    }
}
