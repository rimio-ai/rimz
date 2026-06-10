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
use crate::{SidebarSnapshot, child_process};

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
}

impl NotificationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Success => "success",
            Self::Coalesced => "coalesced",
        }
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
mod tests {
    use std::time::{Duration, Instant};

    use jiff::Timestamp;

    use super::*;
    use crate::agents::lifecycle::TurnPhase;
    use crate::feed::PaneRef;
    use crate::ids::{MuxName, PaneId, WorkspaceId};

    fn prefs() -> NotificationsPrefs {
        NotificationsPrefs {
            coalesce_ms: 0,
            ..NotificationsPrefs::default()
        }
    }

    fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-notify")),
            Vec::new(),
            agents,
            Timestamp::now(),
        )
    }

    fn agent(id: &str, status: AgentStatus, focused: bool) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked("claude"),
            status,
            phase: TurnPhase::Idle,
            pane: Some(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Tmux, format!("%{id}")),
                session_name: "rimz-test".to_owned(),
                view_id: Some("view-1".to_owned()),
                view_kind: None,
                view_name: None,
                is_focused: focused,
                command: Some("claude".to_owned()),
                spawn_command: None,
                cwd: Some("/tmp/rimz-notify".to_owned()),
                pane_pid: None,
                pane_process_start: None,
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
            }),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }

    #[test]
    fn first_observation_seeds_without_notifications() {
        let mut state = NotificationState::default();
        let out = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
            &prefs(),
            1,
        );

        assert!(out.is_empty());
    }

    #[test]
    fn configured_transition_edges_fire() {
        let mut state = NotificationState::default();
        let prefs = NotificationsPrefs {
            triggers: vec![crate::config::NotificationTrigger::Failed],
            ..prefs()
        };
        state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs,
            1,
        );

        let waiting = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
            &prefs,
            2,
        );
        assert!(waiting.is_empty(), "waiting is not configured");

        let failed = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Failed, false)]),
            &prefs,
            3,
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].notification_kind, NotificationKind::Failed);
    }

    #[test]
    fn same_agent_is_debounced_within_window() {
        let mut state = NotificationState::default();
        let prefs = NotificationsPrefs {
            debounce_ms: 1_000,
            ..prefs()
        };
        state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs,
            0,
        );
        assert_eq!(
            state
                .evaluate(
                    &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
                    &prefs,
                    100,
                )
                .len(),
            1
        );
        state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs,
            200,
        );

        let debounced = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
            &prefs,
            500,
        );
        assert!(debounced.is_empty());
    }

    #[test]
    fn focused_agent_is_suppressed() {
        let mut state = NotificationState::default();
        state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs(),
            0,
        );

        let out = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Waiting, true)]),
            &prefs(),
            100,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn burst_coalesces_after_window() {
        let mut state = NotificationState::default();
        let prefs = NotificationsPrefs {
            coalesce_ms: 1_000,
            ..NotificationsPrefs::default()
        };
        state.evaluate(
            &snapshot(vec![
                agent("a1", AgentStatus::Running, false),
                agent("a2", AgentStatus::Running, false),
            ]),
            &prefs,
            0,
        );
        assert!(
            state
                .evaluate(
                    &snapshot(vec![
                        agent("a1", AgentStatus::Waiting, false),
                        agent("a2", AgentStatus::Running, false),
                    ]),
                    &prefs,
                    100,
                )
                .is_empty(),
            "the first edge waits for the coalesce window"
        );
        assert!(
            state
                .evaluate(
                    &snapshot(vec![
                        agent("a1", AgentStatus::Waiting, false),
                        agent("a2", AgentStatus::Failed, false),
                    ]),
                    &prefs,
                    500,
                )
                .is_empty(),
            "the second edge joins the same burst"
        );

        let out = state.evaluate(
            &snapshot(vec![
                agent("a1", AgentStatus::Waiting, false),
                agent("a2", AgentStatus::Failed, false),
            ]),
            &prefs,
            1_100,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].notification_kind, NotificationKind::Coalesced);
        assert_eq!(out[0].agents.len(), 2);
    }

    #[test]
    fn notified_agents_are_pruned_when_they_disappear() {
        let mut state = NotificationState::default();
        state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs(),
            0,
        );

        let out = state.evaluate(
            &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
            &prefs(),
            100,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(state.last_notified_at_ms.len(), 1);

        state.evaluate(&snapshot(Vec::new()), &prefs(), 200);
        assert!(state.last_notified_at_ms.is_empty());
    }

    #[test]
    fn command_spawn_receives_notification_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("env.txt");
        let command = format!(
            "printf '%s\\n%s\\n%s\\n%s\\n' \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" \"$RIMZ_NOTIFY_AGENT\" \"$RIMZ_NOTIFY_KIND\" > {}",
            sh_quote(&out)
        );
        let notification = Notification {
            agents: vec![NotificationAgent {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from("sess-1"),
                label: "claude sess-1".to_owned(),
                pane_id: None,
            }],
            notification_kind: NotificationKind::Waiting,
            title: "Rimz: claude needs you".to_owned(),
            body: "claude sess-1 is waiting for input.".to_owned(),
        };

        let pid = spawn_notify_command(&command, &notification).expect("spawn command");
        assert!(pid > 0);

        let deadline = Instant::now() + Duration::from_secs(2);
        while !out.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let text = std::fs::read_to_string(&out).expect("command wrote env file");
        assert_eq!(
            text,
            "Rimz: claude needs you\nclaude sess-1 is waiting for input.\nclaude sess-1\nwaiting\n"
        );
    }

    fn sh_quote(path: &std::path::Path) -> String {
        let raw = path.to_string_lossy();
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}
