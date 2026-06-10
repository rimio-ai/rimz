use serde::{Deserialize, Serialize};

use crate::feed::AgentStatus;

/// Best-effort attention delivery preferences. These are per-machine because
/// they describe how this terminal or host should reach this user; a clone never
/// inherits them and they do not enter project trust.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotificationsPrefs {
    pub enabled: bool,
    pub triggers: Vec<NotificationTrigger>,
    pub desktop: DesktopNotificationMode,
    pub sound: NotificationSoundMode,
    pub suppress_focused: bool,
    pub debounce_ms: u64,
    pub coalesce_ms: u64,
    #[serde(default)]
    pub command: Option<String>,
}

impl Default for NotificationsPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            triggers: NotificationTrigger::all().to_vec(),
            desktop: DesktopNotificationMode::Auto,
            sound: NotificationSoundMode::Bell,
            suppress_focused: true,
            debounce_ms: 5_000,
            coalesce_ms: 1_000,
            command: None,
        }
    }
}

impl NotificationsPrefs {
    pub fn command(&self) -> Option<&str> {
        self.command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    pub fn triggers_status(&self, status: AgentStatus) -> bool {
        NotificationTrigger::from_status(status)
            .is_some_and(|trigger| self.triggers.contains(&trigger))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTrigger {
    Waiting,
    Failed,
    Paused,
    Success,
}

impl NotificationTrigger {
    pub const ALL: [Self; 4] = [Self::Waiting, Self::Failed, Self::Paused, Self::Success];

    pub const fn all() -> &'static [Self; 4] {
        &Self::ALL
    }

    pub const fn from_status(status: AgentStatus) -> Option<Self> {
        match status {
            AgentStatus::Waiting => Some(Self::Waiting),
            AgentStatus::Failed => Some(Self::Failed),
            AgentStatus::Paused => Some(Self::Paused),
            AgentStatus::Success => Some(Self::Success),
            AgentStatus::Running | AgentStatus::Idle => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Success => "success",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopNotificationMode {
    #[default]
    Auto,
    Osc,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSoundMode {
    #[default]
    Bell,
    Off,
}
