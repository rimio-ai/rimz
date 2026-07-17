//! State-derived agent-card templates. Lifecycle facts choose an ordered line
//! skeleton; provider enrichment only fills the chosen slots.

use crate::SidebarRow;
use crate::agents::AgentStatus;
use crate::config::CardDensityMode;

use super::description::awaiting_first_prompt;
use super::gauge::gauge_percent;

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
            && gauge_percent(row).unwrap_or(0) == 0
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
    Subagents,
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
];
const ENGAGED_WITH_SUBAGENTS: &[CardSlot] = &[
    CardSlot::Identity,
    CardSlot::Description,
    CardSlot::Gauge,
    CardSlot::Tokens,
    CardSlot::Subagents,
];

/// The ordered line skeleton for one agent-card state.
pub(super) fn template(
    stage: CardStage,
    status: AgentStatus,
    density: CardDensityMode,
    selected: bool,
) -> &'static [CardSlot] {
    if density == CardDensityMode::Compact && !selected {
        return match status {
            AgentStatus::Idle => IDENTITY,
            AgentStatus::Running | AgentStatus::Waiting => IDENTITY_DESCRIPTION_GAUGE,
            AgentStatus::Paused | AgentStatus::Success | AgentStatus::Failed => {
                IDENTITY_DESCRIPTION
            }
        };
    }

    match stage {
        CardStage::Fresh { labeled: false } if !selected => IDENTITY,
        CardStage::Fresh { labeled: true } if !selected => IDENTITY_DESCRIPTION,
        CardStage::Fresh { labeled: false } => IDENTITY_AWAITING_GAUGE,
        CardStage::Fresh { labeled: true } => IDENTITY_DESCRIPTION_GAUGE,
        CardStage::Engaged if selected || density == CardDensityMode::Expanded => {
            ENGAGED_WITH_SUBAGENTS
        }
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
    const STATUSES: [AgentStatus; 6] = [
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Paused,
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
        selected: bool,
    ) -> &'static [CardSlot] {
        if density == CardDensityMode::Compact && !selected {
            return match status {
                AgentStatus::Idle => IDENTITY,
                AgentStatus::Running | AgentStatus::Waiting => IDENTITY_DESCRIPTION_GAUGE,
                AgentStatus::Paused | AgentStatus::Success | AgentStatus::Failed => {
                    IDENTITY_DESCRIPTION
                }
            };
        }
        match (stage, selected, density) {
            (CardStage::Fresh { labeled: false }, false, _) => IDENTITY,
            (CardStage::Fresh { labeled: true }, false, _) => IDENTITY_DESCRIPTION,
            (CardStage::Fresh { labeled: false }, true, _) => IDENTITY_AWAITING_GAUGE,
            (CardStage::Fresh { labeled: true }, true, _) => IDENTITY_DESCRIPTION_GAUGE,
            (CardStage::Engaged, _, CardDensityMode::Expanded) | (CardStage::Engaged, true, _) => {
                ENGAGED_WITH_SUBAGENTS
            }
            (CardStage::Engaged, false, _) => ENGAGED,
        }
    }

    #[test]
    fn table_pins_every_state_status_density_and_selection_combination() {
        for stage in STAGES {
            for status in STATUSES {
                for density in DENSITIES {
                    for selected in [false, true] {
                        assert_eq!(
                            template(stage, status, density, selected),
                            expected_template(stage, status, density, selected),
                            "{stage:?} {status:?} {density:?} selected={selected}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fresh_templates_never_gain_token_stats() {
        for stage in [
            CardStage::Fresh { labeled: false },
            CardStage::Fresh { labeled: true },
        ] {
            for status in STATUSES {
                for density in DENSITIES {
                    for selected in [false, true] {
                        assert!(
                            !template(stage, status, density, selected).contains(&CardSlot::Tokens)
                        );
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
