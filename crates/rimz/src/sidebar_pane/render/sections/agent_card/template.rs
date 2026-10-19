//! State-derived agent-card templates. Lifecycle, quiet age, density, and reach choose an ordered line skeleton; provider enrichment only fills the chosen slots. Read idle and done history can shrink to one line, and closed resting history omits the delegation summary.

use crate::agents::AgentStatus;
use crate::config::CardDensityMode;
use crate::sidebar_pane::render::fmt::age_secs;
use crate::store::snapshot::SidebarRow;

use super::super::SelectionReach;
use super::CardExpansion;
use super::description::awaiting_first_prompt;

/// The card lifecycle state. Its line set is stable; enrichment only changes
/// the contents of those lines.
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
    /// `stale_after_secs` is `Stale` unless selection, teammate reach, an
    /// open delegation section, or `expanded` density keeps it open.
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
    AwaitingDots,
    Gauge,
    Tokens,
    Cache,
    /// Standing lifetime child count and cost plus armed one-shot waits; empty
    /// until either exists.
    Delegation,
    /// Live and recent child entries, pending waits, and the `+K older` tail
    /// that lists the rest.
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
const IDENTITY_AWAITING_GAUGE: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::AwaitingDots,
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

/// The ordered line skeleton for one agent-card state.
pub(super) fn template(
    stage: CardStage,
    status: AgentStatus,
    density: CardDensityMode,
    expansion: CardExpansion,
) -> &'static [CardSlot] {
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
    if let CardStage::Fresh { labeled } = stage
        && expansion.reach != SelectionReach::Selected
    {
        return if labeled {
            IDENTITY_DESCRIPTION
        } else {
            IDENTITY
        };
    }

    match stage {
        CardStage::Fresh { labeled: false } => IDENTITY_AWAITING_GAUGE,
        CardStage::Fresh { labeled: true } => IDENTITY_DESCRIPTION_GAUGE,
        CardStage::Engaged | CardStage::Stale => match (
            matches!(
                expansion.reach,
                SelectionReach::Selected | SelectionReach::Held
            ) || density == CardDensityMode::Expanded,
            expansion.delegation,
        ) {
            (true, true) => ENGAGED_EXPANDED,
            (true, false) => ENGAGED,
            (false, true) => ENGAGED_RESTING_EXPANDED,
            (false, false)
                if expansion.reach == SelectionReach::Unreached
                    && matches!(status, AgentStatus::Idle | AgentStatus::Success) =>
            {
                IDENTITY_DESCRIPTION_GAUGE
            }
            (false, false) => ENGAGED_RESTING,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_unlabeled_card_keeps_the_question_slot() {
        for status in [AgentStatus::Running, AgentStatus::Waiting] {
            let slots = template(
                CardStage::Fresh { labeled: false },
                status,
                CardDensityMode::Expanded,
                CardExpansion {
                    reach: SelectionReach::Selected,
                    ..Default::default()
                },
            );
            assert!(slots.contains(&CardSlot::Question));
        }
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
    const REACHES: [SelectionReach; 4] = [
        SelectionReach::Unreached,
        SelectionReach::Teammate,
        SelectionReach::Held,
        SelectionReach::Selected,
    ];

    fn expected_template(
        stage: CardStage,
        status: AgentStatus,
        density: CardDensityMode,
        expansion: CardExpansion,
    ) -> Vec<CardSlot> {
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
        if let CardStage::Fresh { labeled } = stage
            && expansion.reach != SelectionReach::Selected
        {
            return if labeled {
                IDENTITY_DESCRIPTION
            } else {
                IDENTITY
            }
            .to_vec();
        }
        let mut slots = match (stage, expansion.reach.opens_card(), expansion.delegation) {
            (CardStage::Fresh { labeled: false }, false, _) => IDENTITY,
            (CardStage::Fresh { labeled: true }, false, _) => IDENTITY_DESCRIPTION,
            (CardStage::Fresh { labeled: false }, true, _) => IDENTITY_AWAITING_GAUGE,
            (CardStage::Fresh { labeled: true }, true, _) => IDENTITY_DESCRIPTION_GAUGE,
            (CardStage::Engaged | CardStage::Stale, _, true) => ENGAGED_EXPANDED,
            (CardStage::Engaged | CardStage::Stale, _, false) => ENGAGED,
        }
        .to_vec();
        if !matches!(
            expansion.reach,
            SelectionReach::Selected | SelectionReach::Held
        ) && density != CardDensityMode::Expanded
        {
            slots.retain(|slot| !matches!(slot, CardSlot::Tokens | CardSlot::Cache));
        }
        if expansion.reach == SelectionReach::Unreached
            && !expansion.delegation
            && density != CardDensityMode::Expanded
            && matches!(status, AgentStatus::Idle | AgentStatus::Success)
        {
            slots.retain(|slot| *slot != CardSlot::Delegation);
        }
        slots
    }

    /// The card shape follows live or held selection and the delegation entries
    /// follow the resolved open state alone, so a selected card can close its
    /// entries and an unselected one can open them.
    #[test]
    fn table_pins_every_state_status_density_and_expansion_combination() {
        assert!(ENGAGED_EXPANDED.starts_with(ENGAGED));
        for stage in STAGES {
            for status in STATUSES {
                for density in DENSITIES {
                    for reach in REACHES {
                        for delegation in [false, true] {
                            let expansion = CardExpansion {
                                reach,
                                delegation,
                                history: false,
                            };
                            assert_eq!(
                                template(stage, status, density, expansion),
                                expected_template(stage, status, density, expansion),
                                "{stage:?} {status:?} {density:?} {expansion:?}"
                            );
                        }
                    }
                }
            }
        }
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
                                history: false,
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
