//! The team prompt layer: the consensus and team-level prompt files a staged
//! team composes after every role's own system prompt.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Team;

/// The consensus every staged team runs under unless `consensus-file` replaces it.
pub const BUILT_IN_CONSENSUS: &str = include_str!("team_consensus.md");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consensus {
    BuiltIn,
    File(PathBuf),
}

/// Composed after the role's base prompt and fragments, in field order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamPrompt {
    pub consensus: Consensus,
    pub files: Vec<PathBuf>,
}

impl TeamPrompt {
    /// The layer one role of `team` receives. Replacement is the only typed
    /// prompt channel, so a role without a base prompt receives none; an
    /// unstaged team has no pipeline for a consensus to govern.
    pub(crate) fn for_role(team: &Team, role_has_base: bool) -> Option<Self> {
        (team.staged() && role_has_base).then(|| Self {
            consensus: team
                .consensus_file
                .clone()
                .map_or(Consensus::BuiltIn, Consensus::File),
            files: team.append_system_prompt_files.clone(),
        })
    }
}

/// Whether the team configures its layer explicitly, which makes an
/// undeliverable layer a refusal instead of a silent omission.
pub(crate) fn declared(team: &Team) -> bool {
    team.consensus_file.is_some() || !team.append_system_prompt_files.is_empty()
}
