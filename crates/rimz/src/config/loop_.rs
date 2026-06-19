use serde::{Deserialize, Serialize};

use super::AutoPingConfig;

/// `[agents.loop]`: scheduled and automated agent-loop helpers. The umbrella
/// starts with autoping so future loop automation stays grouped by intent.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoopConfig {
    pub autoping: AutoPingConfig,
}
