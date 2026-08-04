use serde::{Deserialize, Serialize, Serializer};
use std::str::FromStr;

/// Git-worktree launch defaults. Per-machine by design: it names where this
/// machine stores sibling worktrees and which base ref it prefers for new ones.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorktreeConfig {
    /// Directory template for RimZ-owned worktrees. Relative paths resolve from
    /// the repository root; `{repo}` expands to the root directory basename.
    pub dir: String,
    /// Base ref for new worktrees: local `HEAD`, remote `origin/HEAD`, or an
    /// explicit ref string.
    pub base: WorktreeBase,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            dir: "../{repo}-worktrees".to_owned(),
            base: WorktreeBase::Head,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorktreeBase {
    #[default]
    Head,
    Fresh,
    Explicit(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("worktree base cannot be empty")]
pub struct WorktreeBaseParseError;

impl Serialize for WorktreeBase {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_config_value())
    }
}

impl WorktreeBase {
    pub fn as_refspec(&self) -> &str {
        match self {
            Self::Head => "HEAD",
            Self::Fresh => "origin/HEAD",
            Self::Explicit(value) => value,
        }
    }

    fn as_config_value(&self) -> &str {
        match self {
            Self::Head => "head",
            Self::Fresh => "fresh",
            Self::Explicit(value) => value,
        }
    }
}

impl FromStr for WorktreeBase {
    type Err = WorktreeBaseParseError;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(WorktreeBaseParseError);
        }
        Ok(match trimmed {
            "head" => Self::Head,
            "fresh" => Self::Fresh,
            other => Self::Explicit(other.to_owned()),
        })
    }
}

impl<'de> Deserialize<'de> for WorktreeBase {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}
