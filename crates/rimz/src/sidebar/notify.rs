//! Best-effort notification policy for the elected sidebar producer.
//!
//! The policy is pure over newly opened unread episodes and caller-owned
//! memory: durable unread owns dedupe, while this layer applies user push
//! preferences and returns notifications for the caller to deliver. Side effects
//! stay at the edge (`spawn_notify_handlers` and sidebar event broadcast), so
//! duplicate policy decisions stay tied to the producer election.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use crate::agents::AgentStatus;
use crate::child_process;
use crate::config::{
    NotificationsPrefs, NotifyConditionAgent, RenderMode, TemplateVars, render_template,
};
use crate::ids::{AgentKind, AgentSessionId, AskId, PaneId};
use crate::remote::link::LinkTier;
use crate::sidebar::unread::OpenedUnread;
use crate::store::snapshot::{SidebarLinkFreshness, SidebarLinkHealth, SidebarSnapshot};

pub use crate::config::NotificationKind;

const LINK_DEGRADED_HOLD_MS: u64 = 10_000;
const LINK_RECOVERY_HOLD_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub agents: Vec<NotificationAgent>,
    pub notification_kind: NotificationKind,
    pub title: String,
    pub body: String,
    pub unread_count: Option<usize>,
}

impl Notification {
    pub fn agent_env(&self) -> String {
        self.agents
            .iter()
            .map(|agent| agent.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn handle_env(&self) -> String {
        self.agents
            .iter()
            .map(|agent| agent.handle.as_str())
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
    pub handle: String,
    pub worktree: Option<String>,
    pub task: Option<String>,
    pub pane_id: Option<PaneId>,
    pub root: Option<String>,
    pub ask_id: Option<AskId>,
    /// The status reached by an agent notification; `None` for link/reminder
    /// notifications that name no agent.
    pub new_status: Option<AgentStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkAlert {
    pub tier: LinkTier,
    pub rtt_ms: Option<u32>,
    pub miss_pct: u16,
    pub since_ms: u64,
    pub recovered_after_ms: Option<u64>,
}

/// Tracks remote-link health episodes for the diagnostic record. The badge owns
/// the live user-facing signal while bytes flow; this layer only bounds an
/// episode (degraded held past [`LINK_DEGRADED_HOLD_MS`], recovered past
/// [`LINK_RECOVERY_HOLD_MS`]) and emits a [`LinkAlert`] at each edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkNotificationState {
    seeded: bool,
    degraded_since_ms: Option<u64>,
    active_since_ms: Option<u64>,
    good_since_ms: Option<u64>,
    paused_at_ms: Option<u64>,
}

impl LinkNotificationState {
    pub(crate) fn evaluate(
        &mut self,
        snapshot: &SidebarSnapshot,
        now_ms: u64,
    ) -> Option<LinkAlert> {
        let link = match snapshot.link.as_ref() {
            Some(link) if link.freshness == SidebarLinkFreshness::Fresh => {
                self.resume_timers(now_ms);
                link
            }
            Some(_) => {
                self.seeded = true;
                self.pause_timers(now_ms);
                return None;
            }
            None => {
                let alert = self.expire_episode(now_ms);
                self.seeded = true;
                return alert;
            }
        };
        if !self.seeded {
            self.seeded = true;
            if link.tier >= LinkTier::Degraded {
                self.degraded_since_ms = Some(now_ms);
            }
            return None;
        }

        if link.tier >= LinkTier::Degraded {
            self.observe_degraded(link, now_ms)
        } else {
            self.observe_good(link, now_ms)
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

    fn expire_episode(&mut self, now_ms: u64) -> Option<LinkAlert> {
        let alert = self.active_since_ms.map(|active_since_ms| LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: None,
            miss_pct: 0,
            since_ms: active_since_ms,
            recovered_after_ms: Some(now_ms.saturating_sub(active_since_ms)),
        });
        self.reset_episode();
        alert
    }

    fn observe_degraded(&mut self, link: &SidebarLinkHealth, now_ms: u64) -> Option<LinkAlert> {
        self.good_since_ms = None;
        self.paused_at_ms = None;
        let since_ms = *self.degraded_since_ms.get_or_insert(now_ms);
        if self.active_since_ms.is_some() || now_ms.saturating_sub(since_ms) < LINK_DEGRADED_HOLD_MS
        {
            return None;
        }
        self.active_since_ms = Some(since_ms);
        Some(LinkAlert {
            tier: link.tier,
            rtt_ms: link.rtt_ms,
            miss_pct: link.miss_pct,
            since_ms,
            recovered_after_ms: None,
        })
    }

    fn observe_good(&mut self, link: &SidebarLinkHealth, now_ms: u64) -> Option<LinkAlert> {
        self.degraded_since_ms = None;
        self.paused_at_ms = None;
        let Some(active_since_ms) = self.active_since_ms else {
            self.good_since_ms = None;
            return None;
        };
        let good_since_ms = *self.good_since_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(good_since_ms) < LINK_RECOVERY_HOLD_MS {
            return None;
        }
        self.active_since_ms = None;
        self.good_since_ms = None;
        Some(LinkAlert {
            tier: link.tier,
            rtt_ms: link.rtt_ms,
            miss_pct: link.miss_pct,
            since_ms: active_since_ms,
            recovered_after_ms: Some(now_ms.saturating_sub(active_since_ms)),
        })
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
    fn new(kind: AgentKind, agent_id: AgentSessionId) -> Self {
        Self { kind, agent_id }
    }
}

#[derive(Clone, Debug)]
struct PendingNotification {
    row_id: String,
    key: AgentKey,
    notification_kind: AgentNotificationKind,
    agent: NotificationAgent,
    title: String,
    body: String,
}

#[derive(Clone, Debug, Default)]
pub struct NotificationState {
    last_notified_at_ms: BTreeMap<AgentKey, u64>,
    pending_since_ms: Option<u64>,
    pending: Vec<PendingNotification>,
}

impl NotificationState {
    pub fn evaluate(
        &mut self,
        snapshot: &SidebarSnapshot,
        opened: &[OpenedUnread],
        prefs: &NotificationsPrefs,
        now_ms: u64,
    ) -> Vec<Notification> {
        let live_keys = notification_keys(snapshot);

        self.last_notified_at_ms
            .retain(|key, _| live_keys.contains_key(key));

        self.prune_pending(snapshot);
        if !prefs.enabled {
            self.pending.clear();
            self.pending_since_ms = None;
            return Vec::new();
        }

        let pending_was_empty = self.pending.is_empty();
        for opened in opened {
            if opened.silent || !prefs.triggers_status(opened.status) {
                continue;
            }
            let Some(notification_kind) = AgentNotificationKind::from_status(opened.status) else {
                continue;
            };
            if prefs.suppress_focused && opened.focused {
                continue;
            }
            let key = AgentKey::new(opened.agent_kind.clone(), opened.agent_id.clone());
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
            let mut pending = pending_notification(opened, notification_kind);
            pending.agent.ask_id = snapshot
                .agents
                .iter()
                .find(|agent| agent.kind == opened.agent_kind && agent.agent_id == opened.agent_id)
                .and_then(|agent| agent.open_ask.as_ref())
                .map(|ask| ask.id.clone());
            self.pending.push(pending);
        }

        if pending_was_empty && !self.pending.is_empty() {
            self.pending_since_ms = Some(now_ms);
        }

        let ready = prefs.coalesce_ms == 0
            || self
                .pending_since_ms
                .is_some_and(|since| now_ms.saturating_sub(since) >= prefs.coalesce_ms);
        if ready {
            self.flush_pending(now_ms, prefs)
        } else {
            Vec::new()
        }
    }

    fn prune_pending(&mut self, snapshot: &SidebarSnapshot) {
        self.pending
            .retain(|pending| row_is_unread(snapshot, &pending.row_id));
        if self.pending.is_empty() {
            self.pending_since_ms = None;
        }
    }

    fn flush_pending(&mut self, now_ms: u64, prefs: &NotificationsPrefs) -> Vec<Notification> {
        if self.pending.is_empty() {
            self.pending_since_ms = None;
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.pending);
        self.pending_since_ms = None;
        for item in &pending {
            self.last_notified_at_ms.insert(item.key.clone(), now_ms);
        }
        let notification = if pending.len() == 1 {
            let mut pending = pending;
            let item = pending.remove(0);
            Notification {
                agents: vec![item.agent],
                notification_kind: item.notification_kind.into(),
                title: item.title,
                body: item.body,
                unread_count: None,
            }
        } else {
            coalesced_notification(pending)
        };
        vec![render_notification_text(notification, prefs)]
    }
}

pub fn spawn_notify_handlers(prefs: &NotificationsPrefs, notification: &Notification) -> usize {
    let vars = notification_template_vars(notification);
    let mut spawned = 0;
    for handler in prefs.effective_handlers() {
        if !handler.when.matches(
            notification.notification_kind,
            notification
                .agents
                .iter()
                .map(|agent| NotifyConditionAgent {
                    handle: &agent.handle,
                    worktree: agent.worktree.as_deref(),
                }),
        ) {
            continue;
        }
        let command = match render_template(&handler.command, &vars, RenderMode::Shell) {
            Ok(command) => command,
            Err(err) => {
                tracing::debug!(error = %err, "notify-command template render failed");
                continue;
            }
        };
        match spawn_notify_command(&command, notification) {
            Ok(_) => spawned += 1,
            Err(err) => tracing::debug!(error = %err, "notify-command spawn failed"),
        }
    }
    spawned
}

fn spawn_notify_command(command: &str, notification: &Notification) -> std::io::Result<u32> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("RIMZ_NOTIFY_TITLE", &notification.title)
        .env("RIMZ_NOTIFY_BODY", &notification.body)
        .env("RIMZ_NOTIFY_AGENT", notification.agent_env())
        .env("RIMZ_NOTIFY_KIND", notification.kind_env())
        .env("RIMZ_NOTIFY_ASK", notify_ask_env(notification));
    let (pane, root) = notify_pane_env(notification);
    cmd.env("RIMZ_NOTIFY_PANE", pane)
        .env("RIMZ_NOTIFY_ROOT", root);
    if let Some(unread_count) = notification.unread_count {
        cmd.env("RIMZ_NOTIFY_UNREAD", unread_count.to_string());
    }
    child_process::spawn_detached_reaped(&mut cmd, "notify-command")
}

fn notification_template_vars(notification: &Notification) -> TemplateVars {
    let mut vars = TemplateVars::new();
    vars.insert("kind", notification.kind_env());
    vars.insert("agent", notification.handle_env());
    vars.insert("handle", notification.handle_env());
    vars.insert("count", notification.agents.len().to_string());
    vars.insert(
        "unread",
        notification
            .unread_count
            .map(|count| count.to_string())
            .unwrap_or_default(),
    );
    vars.insert("pane", "");
    vars.insert("root", "");
    if let [agent] = notification.agents.as_slice() {
        vars.insert(
            "status",
            agent
                .new_status
                .map(AgentStatus::as_str)
                .unwrap_or_default(),
        );
        vars.insert("worktree", agent.worktree.clone().unwrap_or_default());
        vars.insert("task", agent.task.clone().unwrap_or_default());
        vars.insert(
            "pane",
            agent
                .pane_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        );
        vars.insert("root", agent.root.clone().unwrap_or_default());
    }
    vars.insert("title", notification.title.clone());
    vars.insert("body", notification.body.clone());
    vars
}

fn render_notification_text(
    mut notification: Notification,
    prefs: &NotificationsPrefs,
) -> Notification {
    if !matches!(
        notification.notification_kind,
        NotificationKind::Waiting
            | NotificationKind::Failed
            | NotificationKind::Paused
            | NotificationKind::Success
            | NotificationKind::Coalesced
    ) {
        return notification;
    }
    let vars = notification_template_vars(&notification);
    if let Some(title) = &prefs.title {
        match render_template(title, &vars, RenderMode::Plain) {
            Ok(title) => notification.title = title,
            Err(err) => tracing::debug!(error = %err, "notification title template render failed"),
        }
    }
    if let Some(body) = &prefs.body {
        match render_template(body, &vars, RenderMode::Plain) {
            Ok(body) => notification.body = body,
            Err(err) => tracing::debug!(error = %err, "notification body template render failed"),
        }
    }
    notification
}

fn notification_keys(snapshot: &SidebarSnapshot) -> BTreeMap<AgentKey, ()> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status().is_some())
        .map(|row| {
            (
                AgentKey::new(
                    AgentKind::new_unchecked(row.name.clone()),
                    AgentSessionId::from(row.id.clone()),
                ),
                (),
            )
        })
        .collect()
}

fn pending_notification(
    opened: &OpenedUnread,
    notification_kind: AgentNotificationKind,
) -> PendingNotification {
    let label = opened.label.clone();
    let title = match notification_kind {
        AgentNotificationKind::Waiting => format!("RimZ: {label} needs you"),
        AgentNotificationKind::Failed => format!("RimZ: {label} failed"),
        AgentNotificationKind::Paused => format!("RimZ: {label} paused"),
        AgentNotificationKind::Success => format!("RimZ: {label} finished"),
    };
    let body = match notification_kind {
        AgentNotificationKind::Waiting => format!("{label} is waiting for input."),
        AgentNotificationKind::Failed => format!("{label} needs a look."),
        AgentNotificationKind::Paused => format!("{label} is parked on a provider limit."),
        AgentNotificationKind::Success => format!("{label} completed successfully."),
    };
    let agent = NotificationAgent {
        kind: opened.agent_kind.clone(),
        agent_id: opened.agent_id.clone(),
        label,
        handle: opened.handle.clone(),
        worktree: opened.worktree.clone(),
        task: opened.task.clone(),
        pane_id: opened.pane_id.clone(),
        root: opened.root.clone(),
        ask_id: None,
        new_status: Some(opened.status),
    };
    PendingNotification {
        row_id: opened.row_id.clone(),
        key: AgentKey::new(opened.agent_kind.clone(), opened.agent_id.clone()),
        notification_kind,
        agent,
        title,
        body,
    }
}

fn notify_pane_env(notification: &Notification) -> (String, String) {
    if let [agent] = notification.agents.as_slice() {
        return (
            agent
                .pane_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            agent.root.clone().unwrap_or_default(),
        );
    }
    (String::new(), String::new())
}

fn notify_ask_env(notification: &Notification) -> String {
    if notification.notification_kind != NotificationKind::Waiting {
        return String::new();
    }
    let [agent] = notification.agents.as_slice() else {
        return String::new();
    };
    agent
        .ask_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default()
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
        title: format!("RimZ: {count} agents need attention"),
        body,
        unread_count: None,
    }
}

fn row_is_unread(snapshot: &SidebarSnapshot, row_id: &str) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| row.id == row_id && row.unread)
}

#[cfg(test)]
mod tests;
