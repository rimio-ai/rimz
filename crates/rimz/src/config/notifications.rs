use std::collections::BTreeMap;

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::agents::AgentStatus;

pub const KNOWN_TEMPLATE_VARS: &[&str] = &[
    "kind", "agent", "status", "worktree", "task", "count", "unread", "title", "body",
];
const KNOWN_TEMPLATE_VARS_LIST: &str =
    "kind, agent, status, worktree, task, count, unread, title, body";

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
    pub remind_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handler: Vec<NotifyHandler>,
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
            remind_secs: 60,
            title: None,
            body: None,
            command: None,
            handler: Vec::new(),
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

    pub fn effective_handlers(&self) -> Vec<NotifyHandler> {
        let mut handlers = self.handler.clone();
        if let Some(command) = self.command() {
            handlers.push(NotifyHandler {
                name: Some("command".to_owned()),
                when: NotifyCondition::default(),
                command: command.to_owned(),
            });
        }
        handlers
    }

    pub fn has_handlers(&self) -> bool {
        self.command().is_some() || !self.handler.is_empty()
    }

    pub fn triggers_status(&self, status: AgentStatus) -> bool {
        NotificationTrigger::from_status(status)
            .is_some_and(|trigger| self.triggers.contains(&trigger))
    }

    pub fn validate(&self) -> Result<(), NotificationsConfigErr> {
        validate_template("[notifications].title", self.title.as_deref(), false)?;
        validate_template("[notifications].body", self.body.as_deref(), false)?;
        validate_template("[notifications].command", self.command.as_deref(), true)?;
        for (index, handler) in self.handler.iter().enumerate() {
            let label = handler_label(index, handler.name.as_deref());
            if handler.command.trim().is_empty() {
                return Err(NotificationsConfigErr::EmptyCommand {
                    field: format!("{label}.command"),
                });
            }
            validate_template(&format!("{label}.command"), Some(&handler.command), true)?;
            handler.when.validate(&label)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotifyHandler {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub when: NotifyCondition,
    pub command: String,
}

impl Default for NotifyHandler {
    fn default() -> Self {
        Self {
            name: None,
            when: NotifyCondition::default(),
            command: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotifyCondition {
    pub kind: Vec<NotificationKind>,
    pub worktree: Vec<String>,
    pub handle: Vec<String>,
}

impl NotifyCondition {
    pub fn matches<'a>(
        &self,
        kind: NotificationKind,
        agents: impl IntoIterator<Item = NotifyConditionAgent<'a>>,
    ) -> bool {
        if !self.kind.is_empty() && !self.kind.contains(&kind) {
            return false;
        }
        let agents = agents.into_iter().collect::<Vec<_>>();
        let worktree_matches = self.worktree.is_empty()
            || agents.iter().any(|agent| {
                agent
                    .worktree
                    .is_some_and(|worktree| any_pattern_matches(&self.worktree, worktree))
            });
        let handle_matches = self.handle.is_empty()
            || agents
                .iter()
                .any(|agent| any_pattern_matches(&self.handle, agent.handle));
        worktree_matches && handle_matches
    }

    fn validate(&self, label: &str) -> Result<(), NotificationsConfigErr> {
        validate_patterns(format!("{label}.when.worktree"), &self.worktree)?;
        validate_patterns(format!("{label}.when.handle"), &self.handle)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyConditionAgent<'a> {
    pub handle: &'a str,
    pub worktree: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateVars {
    values: BTreeMap<&'static str, String>,
}

impl TemplateVars {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: &'static str, value: impl Into<String>) {
        self.values.insert(key, value.into());
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Plain,
    Shell,
}

pub fn render_template(
    template: &str,
    vars: &TemplateVars,
    mode: RenderMode,
) -> Result<String, shlex::QuoteError> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = after_open[..end].trim();
        let value = vars.get(name).unwrap_or_default();
        match mode {
            RenderMode::Plain => out.push_str(value),
            RenderMode::Shell => out.push_str(&shlex::try_quote(value)?),
        }
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn referenced_vars(template: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            break;
        };
        vars.push(after_open[..end].trim().to_owned());
        rest = &after_open[end + 2..];
    }
    vars
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Waiting,
    Failed,
    Paused,
    Success,
    Coalesced,
    LinkLost,
    LinkRestored,
    Reminder,
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
            Self::Reminder => "reminder",
        }
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NotificationsConfigErr {
    #[error(
        "unknown notification template variable `{{{{{var}}}}}` in {field}; supported variables: {known}"
    )]
    UnknownTemplateVar {
        field: String,
        var: String,
        known: &'static str,
    },
    #[error("notification template variable `{{{{{var}}}}}` is not available in {field}")]
    ReservedTemplateVar { field: String, var: String },
    #[error("notification command in {field} must not be empty")]
    EmptyCommand { field: String },
    #[error("invalid notification glob `{pattern}` in {field}: {error}")]
    InvalidGlob {
        field: String,
        pattern: String,
        error: String,
    },
}

fn validate_template(
    field: &str,
    template: Option<&str>,
    allow_rendered_text_vars: bool,
) -> Result<(), NotificationsConfigErr> {
    let Some(template) = template else {
        return Ok(());
    };
    for var in referenced_vars(template) {
        if !KNOWN_TEMPLATE_VARS.contains(&var.as_str()) {
            return Err(NotificationsConfigErr::UnknownTemplateVar {
                field: field.to_owned(),
                var,
                known: KNOWN_TEMPLATE_VARS_LIST,
            });
        }
        if !allow_rendered_text_vars && matches!(var.as_str(), "title" | "body") {
            return Err(NotificationsConfigErr::ReservedTemplateVar {
                field: field.to_owned(),
                var,
            });
        }
    }
    Ok(())
}

fn validate_patterns(field: String, patterns: &[String]) -> Result<(), NotificationsConfigErr> {
    for pattern in patterns {
        Pattern::new(pattern).map_err(|source| NotificationsConfigErr::InvalidGlob {
            field: field.clone(),
            pattern: pattern.clone(),
            error: source.to_string(),
        })?;
    }
    Ok(())
}

fn any_pattern_matches(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|pattern| {
        Pattern::new(pattern)
            .map(|pattern| pattern.matches(value))
            .unwrap_or(false)
    })
}

fn handler_label(index: usize, name: Option<&str>) -> String {
    match name {
        Some(name) if !name.trim().is_empty() => {
            format!("[notifications.handler #{} `{}`]", index + 1, name.trim())
        }
        _ => format!("[notifications.handler #{}]", index + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> TemplateVars {
        let mut vars = TemplateVars::new();
        vars.insert("kind", "waiting");
        vars.insert("agent", "codex");
        vars.insert("title", "Rimz: codex needs you");
        vars.insert("body", "codex is waiting; touch nothing");
        vars.insert("task", "\"; rm -rf /");
        vars
    }

    #[test]
    fn templates_render_plain_and_shell_values() {
        assert_eq!(
            render_template(
                "{{ kind }}: {{agent}} {{missing}}",
                &vars(),
                RenderMode::Plain
            )
            .expect("render"),
            "waiting: codex "
        );
        assert_eq!(
            render_template("before {{", &vars(), RenderMode::Plain).expect("render"),
            "before {{"
        );

        let rendered =
            render_template("notify {{task}}", &vars(), RenderMode::Shell).expect("render");
        assert_eq!(rendered, "notify '\"; rm -rf /'");
        assert_eq!(
            shlex::split(&rendered).expect("shell split"),
            vec!["notify".to_owned(), "\"; rm -rf /".to_owned()]
        );
    }

    #[test]
    fn condition_matches_kind_worktree_and_handle_globs() {
        let condition = NotifyCondition {
            kind: vec![NotificationKind::Waiting],
            worktree: vec!["feat/*".to_owned()],
            handle: vec!["@planner".to_owned(), "codex-*".to_owned()],
        };
        assert!(condition.matches(
            NotificationKind::Waiting,
            [NotifyConditionAgent {
                handle: "codex-1",
                worktree: Some("feat/ntfy"),
            }]
        ));
        assert!(!condition.matches(
            NotificationKind::Failed,
            [NotifyConditionAgent {
                handle: "codex-1",
                worktree: Some("feat/ntfy"),
            }]
        ));
        assert!(!condition.matches(
            NotificationKind::Waiting,
            [NotifyConditionAgent {
                handle: "codex-1",
                worktree: Some("fix/ntfy"),
            }]
        ));
        assert!(NotifyCondition::default().matches(
            NotificationKind::Reminder,
            [NotifyConditionAgent {
                handle: "anything",
                worktree: None,
            }]
        ));
        assert!(condition.matches(
            NotificationKind::Waiting,
            [
                NotifyConditionAgent {
                    handle: "reviewer",
                    worktree: Some("main"),
                },
                NotifyConditionAgent {
                    handle: "@planner",
                    worktree: Some("feat/ntfy"),
                },
            ]
        ));
    }

    #[test]
    fn handlers_deserialize_validate_and_desugar_legacy_command() {
        let prefs: NotificationsPrefs = toml::from_str(
            r#"
enabled = true
title = "Rimz: {{agent}} {{kind}}"
body = "{{task}}"
command = "ntfy publish rimz"

[[handler]]
name = "urgent"
command = "notify {{title}} {{body}}"
when = { kind = ["waiting"], worktree = ["feat/*"], handle = ["@planner"] }
"#,
        )
        .expect("parse");
        prefs.validate().expect("validate");
        assert_eq!(prefs.title.as_deref(), Some("Rimz: {{agent}} {{kind}}"));
        assert_eq!(prefs.handler.len(), 1);
        assert_eq!(prefs.effective_handlers().len(), 2);
        assert!(prefs.has_handlers());
    }

    #[test]
    fn validation_rejects_unknown_vars_bad_globs_and_title_body_self_reference() {
        let unknown = NotificationsPrefs {
            handler: vec![NotifyHandler {
                name: Some("bad".to_owned()),
                command: "notify {{nope}}".to_owned(),
                ..NotifyHandler::default()
            }],
            ..NotificationsPrefs::default()
        };
        assert!(matches!(
            unknown.validate(),
            Err(NotificationsConfigErr::UnknownTemplateVar { var, .. }) if var == "nope"
        ));

        let bad_glob = NotificationsPrefs {
            handler: vec![NotifyHandler {
                command: "notify".to_owned(),
                when: NotifyCondition {
                    worktree: vec!["[".to_owned()],
                    ..NotifyCondition::default()
                },
                ..NotifyHandler::default()
            }],
            ..NotificationsPrefs::default()
        };
        assert!(matches!(
            bad_glob.validate(),
            Err(NotificationsConfigErr::InvalidGlob { pattern, .. }) if pattern == "["
        ));

        let self_reference = NotificationsPrefs {
            title: Some("{{title}}".to_owned()),
            ..NotificationsPrefs::default()
        };
        assert!(matches!(
            self_reference.validate(),
            Err(NotificationsConfigErr::ReservedTemplateVar { var, .. }) if var == "title"
        ));
    }
}
