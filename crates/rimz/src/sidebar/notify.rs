//! Best-effort notification policy for the elected sidebar producer.
//!
//! The policy is pure over snapshot state and caller-owned memory: it observes
//! status edges, applies user preferences, and returns notifications for the
//! caller to deliver. Side effects stay at the edge (`spawn_notify_command` and
//! sidebar event broadcast), so duplicate policy decisions stay tied to the
//! producer election.

use std::collections::BTreeMap;
use std::io;
use std::process::{Command, Stdio};

use crate::config::NotificationsPrefs;
use crate::feed::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::remote::link::LinkTier;
use crate::{SidebarLinkFreshness, SidebarLinkHealth, SidebarSnapshot, child_process};

const LINK_DEGRADED_HOLD_MS: u64 = 10_000;
const LINK_RECOVERY_HOLD_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub agents: Vec<NotificationAgent>,
    pub notification_kind: NotificationKind,
    pub title: String,
    pub body: String,
}

impl Notification {
    pub fn agent_env(&self) -> String {
        self.agents
            .iter()
            .map(|agent| agent.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn kind_env(&self) -> &'static str {
        self.notification_kind.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationAgent {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub label: String,
    pub pane_id: Option<PaneId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationKind {
    Waiting,
    Failed,
    Paused,
    Success,
    Coalesced,
    LinkLost,
    LinkRestored,
    LinkDegraded,
    LinkRecovered,
}

impl NotificationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Success => "success",
            Self::Coalesced => "coalesced",
            Self::LinkLost => "link_lost",
            Self::LinkRestored => "link_restored",
            Self::LinkDegraded => "link_degraded",
            Self::LinkRecovered => "link_recovered",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkAlert {
    pub tier: LinkTier,
    pub rtt_ms: Option<u32>,
    pub miss_pct: u16,
    pub since_ms: u64,
    pub recovered_after_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkNotificationState {
    seeded: bool,
    degraded_since_ms: Option<u64>,
    active_since_ms: Option<u64>,
    good_since_ms: Option<u64>,
    paused_at_ms: Option<u64>,
    last_notified_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkNotificationEvaluation {
    pub notification: Option<Notification>,
    pub alert: Option<LinkAlert>,
}

impl LinkNotificationState {
    pub fn evaluate(
        &mut self,
        snapshot: &SidebarSnapshot,
        prefs: &NotificationsPrefs,
        now_ms: u64,
    ) -> LinkNotificationEvaluation {
        let link = match snapshot.link.as_ref() {
            Some(link) if link.freshness == SidebarLinkFreshness::Fresh => {
                self.resume_timers(now_ms);
                link
            }
            Some(_) => {
                self.seeded = true;
                self.pause_timers(now_ms);
                return LinkNotificationEvaluation::default();
            }
            None => {
                let evaluation = self.expire_episode(now_ms);
                self.seeded = true;
                return evaluation;
            }
        };
        if !self.seeded {
            self.seeded = true;
            if link.tier >= LinkTier::Degraded {
                self.degraded_since_ms = Some(now_ms);
            }
            return LinkNotificationEvaluation::default();
        }

        if link.tier >= LinkTier::Degraded {
            self.observe_degraded(link, prefs, now_ms)
        } else {
            self.observe_good(link, prefs, now_ms)
        }
    }

    fn pause_timers(&mut self, now_ms: u64) {
        if self.degraded_since_ms.is_some()
            || self.active_since_ms.is_some()
            || self.good_since_ms.is_some()
        {
            self.paused_at_ms.get_or_insert(now_ms);
        }
    }

    fn resume_timers(&mut self, now_ms: u64) {
        let Some(paused_at_ms) = self.paused_at_ms.take() else {
            return;
        };
        let paused_ms = now_ms.saturating_sub(paused_at_ms);
        shift_timer(&mut self.degraded_since_ms, paused_ms);
        shift_timer(&mut self.active_since_ms, paused_ms);
        shift_timer(&mut self.good_since_ms, paused_ms);
    }

    fn reset_episode(&mut self) {
        self.degraded_since_ms = None;
        self.active_since_ms = None;
        self.good_since_ms = None;
        self.paused_at_ms = None;
    }

    fn expire_episode(&mut self, now_ms: u64) -> LinkNotificationEvaluation {
        let alert = self.active_since_ms.map(|active_since_ms| LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: None,
            miss_pct: 0,
            since_ms: active_since_ms,
            recovered_after_ms: Some(now_ms.saturating_sub(active_since_ms)),
        });
        self.reset_episode();
        LinkNotificationEvaluation {
            notification: None,
            alert,
        }
    }

    fn observe_degraded(
        &mut self,
        link: &SidebarLinkHealth,
        prefs: &NotificationsPrefs,
        now_ms: u64,
    ) -> LinkNotificationEvaluation {
        self.good_since_ms = None;
        self.paused_at_ms = None;
        let since_ms = *self.degraded_since_ms.get_or_insert(now_ms);
        if self.active_since_ms.is_some() || now_ms.saturating_sub(since_ms) < LINK_DEGRADED_HOLD_MS
        {
            return LinkNotificationEvaluation::default();
        }
        self.active_since_ms = Some(since_ms);
        let alert = LinkAlert {
            tier: link.tier,
            rtt_ms: link.rtt_ms,
            miss_pct: link.miss_pct,
            since_ms,
            recovered_after_ms: None,
        };
        LinkNotificationEvaluation {
            notification: self.notify_if_enabled(prefs, now_ms, link_degraded_notification(link)),
            alert: Some(alert),
        }
    }

    fn observe_good(
        &mut self,
        link: &SidebarLinkHealth,
        prefs: &NotificationsPrefs,
        now_ms: u64,
    ) -> LinkNotificationEvaluation {
        self.degraded_since_ms = None;
        self.paused_at_ms = None;
        let Some(active_since_ms) = self.active_since_ms else {
            self.good_since_ms = None;
            return LinkNotificationEvaluation::default();
        };
        let good_since_ms = *self.good_since_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(good_since_ms) < LINK_RECOVERY_HOLD_MS {
            return LinkNotificationEvaluation::default();
        }
        self.active_since_ms = None;
        self.good_since_ms = None;
        let alert = LinkAlert {
            tier: link.tier,
            rtt_ms: link.rtt_ms,
            miss_pct: link.miss_pct,
            since_ms: active_since_ms,
            recovered_after_ms: Some(now_ms.saturating_sub(active_since_ms)),
        };
        LinkNotificationEvaluation {
            notification: self.notify_if_enabled(prefs, now_ms, link_recovered_notification(link)),
            alert: Some(alert),
        }
    }

    fn notify_if_enabled(
        &mut self,
        prefs: &NotificationsPrefs,
        now_ms: u64,
        notification: Notification,
    ) -> Option<Notification> {
        if !prefs.enabled
            || self
                .last_notified_at_ms
                .is_some_and(|last| now_ms.saturating_sub(last) < prefs.debounce_ms)
        {
            return None;
        }
        self.last_notified_at_ms = Some(now_ms);
        Some(notification)
    }
}

fn shift_timer(timer: &mut Option<u64>, by_ms: u64) {
    if let Some(value) = timer {
        *value = value.saturating_add(by_ms);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentNotificationKind {
    Waiting,
    Failed,
    Paused,
    Success,
}

impl AgentNotificationKind {
    const fn from_status(status: AgentStatus) -> Option<Self> {
        match status {
            AgentStatus::Waiting => Some(Self::Waiting),
            AgentStatus::Failed => Some(Self::Failed),
            AgentStatus::Paused => Some(Self::Paused),
            AgentStatus::Success => Some(Self::Success),
            AgentStatus::Running | AgentStatus::Idle => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Success => "success",
        }
    }
}

impl From<AgentNotificationKind> for NotificationKind {
    fn from(kind: AgentNotificationKind) -> Self {
        match kind {
            AgentNotificationKind::Waiting => Self::Waiting,
            AgentNotificationKind::Failed => Self::Failed,
            AgentNotificationKind::Paused => Self::Paused,
            AgentNotificationKind::Success => Self::Success,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AgentKey {
    kind: AgentKind,
    agent_id: AgentSessionId,
}

impl AgentKey {
    fn from_agent(agent: &AgentState) -> Self {
        Self {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct PendingNotification {
    key: AgentKey,
    notification_kind: AgentNotificationKind,
    agent: NotificationAgent,
    title: String,
    body: String,
}

#[derive(Clone, Debug, Default)]
pub struct NotificationState {
    seeded: bool,
    statuses: BTreeMap<AgentKey, AgentStatus>,
    last_notified_at_ms: BTreeMap<AgentKey, u64>,
    pending_since_ms: Option<u64>,
    pending: Vec<PendingNotification>,
}

impl NotificationState {
    pub fn evaluate(
        &mut self,
        snapshot: &SidebarSnapshot,
        prefs: &NotificationsPrefs,
        now_ms: u64,
    ) -> Vec<Notification> {
        let agents = root_agents(snapshot).collect::<Vec<_>>();
        let current_statuses = agents
            .iter()
            .map(|agent| (AgentKey::from_agent(agent), agent.status))
            .collect::<BTreeMap<_, _>>();

        self.last_notified_at_ms
            .retain(|key, _| current_statuses.contains_key(key));

        if !self.seeded {
            self.statuses = current_statuses;
            self.seeded = true;
            self.pending.clear();
            self.pending_since_ms = None;
            return Vec::new();
        }

        self.prune_pending(&current_statuses);
        if !prefs.enabled {
            self.statuses = current_statuses;
            self.pending.clear();
            self.pending_since_ms = None;
            return Vec::new();
        }

        let pending_was_empty = self.pending.is_empty();
        for agent in agents {
            let key = AgentKey::from_agent(agent);
            let status = agent.status;
            if self.statuses.get(&key).copied() == Some(status) {
                continue;
            }
            if !prefs.triggers_status(status) {
                continue;
            }
            let Some(notification_kind) = AgentNotificationKind::from_status(status) else {
                continue;
            };
            if prefs.suppress_focused && agent.pane.as_ref().is_some_and(|pane| pane.is_focused) {
                continue;
            }
            if self
                .last_notified_at_ms
                .get(&key)
                .is_some_and(|last| now_ms.saturating_sub(*last) < prefs.debounce_ms)
            {
                continue;
            }
            if self.pending.iter().any(|pending| pending.key == key) {
                continue;
            }
            self.pending
                .push(pending_notification(agent, notification_kind));
        }

        if pending_was_empty && !self.pending.is_empty() {
            self.pending_since_ms = Some(now_ms);
        }
        self.statuses = current_statuses;

        let ready = prefs.coalesce_ms == 0
            || self
                .pending_since_ms
                .is_some_and(|since| now_ms.saturating_sub(since) >= prefs.coalesce_ms);
        if ready {
            self.flush_pending(now_ms)
        } else {
            Vec::new()
        }
    }

    fn prune_pending(&mut self, current_statuses: &BTreeMap<AgentKey, AgentStatus>) {
        self.pending.retain(|pending| {
            current_statuses
                .get(&pending.key)
                .and_then(|status| AgentNotificationKind::from_status(*status))
                .is_some_and(|kind| kind == pending.notification_kind)
        });
        if self.pending.is_empty() {
            self.pending_since_ms = None;
        }
    }

    fn flush_pending(&mut self, now_ms: u64) -> Vec<Notification> {
        if self.pending.is_empty() {
            self.pending_since_ms = None;
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.pending);
        self.pending_since_ms = None;
        for item in &pending {
            self.last_notified_at_ms.insert(item.key.clone(), now_ms);
        }
        vec![if pending.len() == 1 {
            let mut pending = pending;
            let item = pending.remove(0);
            Notification {
                agents: vec![item.agent],
                notification_kind: item.notification_kind.into(),
                title: item.title,
                body: item.body,
            }
        } else {
            coalesced_notification(pending)
        }]
    }
}

pub fn spawn_notify_command(command: &str, notification: &Notification) -> io::Result<u32> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("RIMZ_NOTIFY_TITLE", &notification.title)
        .env("RIMZ_NOTIFY_BODY", &notification.body)
        .env("RIMZ_NOTIFY_AGENT", notification.agent_env())
        .env("RIMZ_NOTIFY_KIND", notification.kind_env());
    child_process::spawn_detached_reaped(&mut cmd, "notify-command")
}

fn root_agents(snapshot: &SidebarSnapshot) -> impl Iterator<Item = &AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
}

fn pending_notification(
    agent: &AgentState,
    notification_kind: AgentNotificationKind,
) -> PendingNotification {
    let label = agent_label(agent);
    let title = match notification_kind {
        AgentNotificationKind::Waiting => format!("Rimz: {label} needs you"),
        AgentNotificationKind::Failed => format!("Rimz: {label} failed"),
        AgentNotificationKind::Paused => format!("Rimz: {label} paused"),
        AgentNotificationKind::Success => format!("Rimz: {label} finished"),
    };
    let body = match notification_kind {
        AgentNotificationKind::Waiting => format!("{label} is waiting for input."),
        AgentNotificationKind::Failed => format!("{label} needs a look."),
        AgentNotificationKind::Paused => format!("{label} is parked on a provider limit."),
        AgentNotificationKind::Success => format!("{label} completed successfully."),
    };
    PendingNotification {
        key: AgentKey::from_agent(agent),
        notification_kind,
        agent: NotificationAgent {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            label,
            pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
        },
        title,
        body,
    }
}

fn coalesced_notification(pending: Vec<PendingNotification>) -> Notification {
    let count = pending.len();
    let agents = pending
        .iter()
        .map(|item| item.agent.clone())
        .collect::<Vec<_>>();
    let body = pending
        .iter()
        .map(|item| format!("{}: {}", item.agent.label, item.notification_kind.as_str()))
        .collect::<Vec<_>>()
        .join(" | ");
    Notification {
        agents,
        notification_kind: NotificationKind::Coalesced,
        title: format!("Rimz: {count} agents need attention"),
        body,
    }
}

fn link_degraded_notification(link: &SidebarLinkHealth) -> Notification {
    Notification {
        agents: Vec::new(),
        notification_kind: NotificationKind::LinkDegraded,
        title: "Rimz: remote link degraded".to_owned(),
        body: link_notification_body(link),
    }
}

fn link_recovered_notification(link: &SidebarLinkHealth) -> Notification {
    Notification {
        agents: Vec::new(),
        notification_kind: NotificationKind::LinkRecovered,
        title: "Rimz: remote link recovered".to_owned(),
        body: link_notification_body(link),
    }
}

fn link_notification_body(link: &SidebarLinkHealth) -> String {
    let rtt = link
        .rtt_ms
        .map(|rtt| format!("RTT {rtt}ms"))
        .unwrap_or_else(|| "RTT unknown".to_owned());
    format!("{rtt}, {}% loss.", link.miss_pct)
}

fn agent_label(agent: &AgentState) -> String {
    agent
        .task
        .as_deref()
        .or(agent.prompt.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(trim_label)
        .unwrap_or_else(|| format!("{} {}", agent.kind, short_agent_id(&agent.agent_id)))
}

fn trim_label(value: &str) -> String {
    const MAX: usize = 48;
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_owned();
    }
    let mut out = trimmed
        .chars()
        .take(MAX.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn short_agent_id(id: &AgentSessionId) -> &str {
    let raw = id.as_str();
    raw.get(..8).unwrap_or(raw)
}

#[cfg(test)]
mod tests;
