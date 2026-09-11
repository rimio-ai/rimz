//! State-derived agent-card templates. Lifecycle facts choose an ordered line
//! skeleton; provider enrichment only fills the chosen slots.

use crate::agents::AgentStatus;
use crate::config::CardDensityMode;
use crate::store::snapshot::SidebarRow;

use super::description::awaiting_first_prompt;

/// The card lifecycle state. Its line set is stable; enrichment only changes
/// the contents of those lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardStage {
    Fresh { labeled: bool },
    Engaged,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardSlot {
    Identity,
    Description,
    AwaitingDots,
    Gauge,
    Tokens,
    /// Standing lifetime child count and cost plus armed one-shot wakes; empty
    /// until either exists.
    Delegation,
    /// Current-turn child entries followed by pending-wait entries.
    DelegationEntries,
}

const IDENTITY: &[CardSlot] = &[CardSlot::Identity];
const IDENTITY_DESCRIPTION: &[CardSlot] = &[CardSlot::Identity, CardSlot::Description];
const IDENTITY_DESCRIPTION_GAUGE: &[CardSlot] =
    &[CardSlot::Identity, CardSlot::Description, CardSlot::Gauge];
const IDENTITY_AWAITING_GAUGE: &[CardSlot] =
    &[CardSlot::Identity, CardSlot::AwaitingDots, CardSlot::Gauge];
const ENGAGED: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Gauge,
    CardSlot::Tokens,
    CardSlot::Delegation,
];
const ENGAGED_EXPANDED: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Gauge,
    CardSlot::Tokens,
    CardSlot::Delegation,
    CardSlot::DelegationEntries,
];

/// The ordered line skeleton for one agent-card state.
pub(super) fn template(
    stage: CardStage,
    status: AgentStatus,
    density: CardDensityMode,
    expanded: bool,
) -> &'static [CardSlot] {
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

    match stage {
        CardStage::Fresh { labeled: false } if !expanded => IDENTITY,
        CardStage::Fresh { labeled: true } if !expanded => IDENTITY_DESCRIPTION,
        CardStage::Fresh { labeled: false } => IDENTITY_AWAITING_GAUGE,
        CardStage::Fresh { labeled: true } => IDENTITY_DESCRIPTION_GAUGE,
        CardStage::Engaged if expanded || density == CardDensityMode::Expanded => ENGAGED_EXPANDED,
        CardStage::Engaged => ENGAGED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGES: [CardStage; 3] = [
        CardStage::Fresh { labeled: false },
        CardStage::Fresh { labeled: true },
        CardStage::Engaged,
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

    fn expected_template(
        stage: CardStage,
        status: AgentStatus,
        density: CardDensityMode,
        expanded: bool,
    ) -> &'static [CardSlot] {
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
        match (stage, expanded, density) {
            (CardStage::Fresh { labeled: false }, false, _) => IDENTITY,
            (CardStage::Fresh { labeled: true }, false, _) => IDENTITY_DESCRIPTION,
            (CardStage::Fresh { labeled: false }, true, _) => IDENTITY_AWAITING_GAUGE,
            (CardStage::Fresh { labeled: true }, true, _) => IDENTITY_DESCRIPTION_GAUGE,
            (CardStage::Engaged, _, CardDensityMode::Expanded) | (CardStage::Engaged, true, _) => {
                ENGAGED_EXPANDED
            }
            (CardStage::Engaged, false, _) => ENGAGED,
        }
    }

    #[test]
    fn table_pins_every_state_status_density_and_expansion_combination() {
        assert!(ENGAGED_EXPANDED.starts_with(ENGAGED));
        for stage in STAGES {
            for status in STATUSES {
                for density in DENSITIES {
                    for expanded in [false, true] {
                        assert_eq!(
                            template(stage, status, density, expanded),
                            expected_template(stage, status, density, expanded),
                            "{stage:?} {status:?} {density:?} expanded={expanded}"
                        );
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
                    for expanded in [false, true] {
                        let template = template(stage, status, density, expanded);
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
                false
            ),
            IDENTITY
        );
    }
}
