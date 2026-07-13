use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// `loop.toml`: scheduled and automated agent-loop helpers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoopConfig {
    pub tasks: Tasks,
}

impl LoopConfig {
    pub fn is_empty(&self) -> bool {
        self.tasks.0.is_empty()
    }

    pub fn validate_budgets(&self) -> Result<(), TaskBudgetError> {
        for (name, entry) in &self.tasks.0 {
            entry.validate_budget(name)?;
        }
        Ok(())
    }
}

/// Named loop tasks, ordered by name. A map keeps `rimz loop add/remove/run`
/// addressing one task by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Tasks(pub BTreeMap<String, TaskEntry>);

/// One scheduled loop wake-up. The firing time is either a one-shot calendar
/// time, an explicit repeat cadence, or a raw cron escape hatch; `agent`
/// spawns a supervised turn and `wake` delivers to a pinned session.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TaskEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<TaskTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(rename = "prompt-file", skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    #[serde(rename = "max-attempts", skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(rename = "max-strikes", skip_serializing_if = "Option::is_none")]
    pub max_strikes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<CheckOn>,
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(rename = "budget-per-day", skip_serializing_if = "Option::is_none")]
    pub budget_per_day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surplus: Option<String>,
    #[serde(rename = "surplus-after", skip_serializing_if = "Option::is_none")]
    pub surplus_after: Option<String>,
    #[serde(rename = "system-prompt-file", skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Timestamp>,
}

impl TaskEntry {
    /// Root normalized for workspace identity and execution. CLI-added tasks
    /// already store this shape; hand-edited tasks may use `~` or a relative
    /// path.
    pub fn resolved_root(&self) -> PathBuf {
        resolve_root_with(&self.root, home_dir())
    }

    pub fn validate_budget(&self, task: &str) -> Result<(), TaskBudgetError> {
        if let Some(raw) = self.budget.as_deref() {
            raw.parse::<crate::harness::budget::BudgetSpec>()
                .map_err(|source| TaskBudgetError::Invalid {
                    task: task.to_owned(),
                    field: "budget",
                    source,
                })?;
        }
        if let Some(raw) = self.budget_per_day.as_deref() {
            raw.parse::<crate::harness::budget::BudgetSpec>()
                .map_err(|source| TaskBudgetError::Invalid {
                    task: task.to_owned(),
                    field: "budget-per-day",
                    source,
                })?;
            if self.budget.is_none() {
                return Err(TaskBudgetError::MissingRunBudget {
                    task: task.to_owned(),
                });
            }
        }
        if let Some(raw) = self.surplus.as_deref() {
            crate::harness::schedule::parse_surplus(raw).map_err(|detail| {
                TaskBudgetError::InvalidSurplus {
                    task: task.to_owned(),
                    field: "surplus",
                    detail,
                }
            })?;
        }
        if let Some(raw) = self.surplus_after.as_deref() {
            crate::harness::schedule::parse_surplus_after(raw).map_err(|detail| {
                TaskBudgetError::InvalidSurplus {
                    task: task.to_owned(),
                    field: "surplus-after",
                    detail,
                }
            })?;
        }
        if (self.surplus.is_some() || self.surplus_after.is_some())
            && self.agent.is_none()
            && self.wake.is_none()
        {
            return Err(TaskBudgetError::SurplusNeedsAgent {
                task: task.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TaskBudgetError {
    #[error("task `{task}` has invalid `{field}`: {source}")]
    Invalid {
        task: String,
        field: &'static str,
        #[source]
        source: crate::harness::budget::BudgetParseError,
    },
    #[error("task `{task}` sets `budget-per-day` without `budget`; set a per-run budget")]
    MissingRunBudget { task: String },
    #[error("task `{task}` has invalid `{field}`: {detail}")]
    InvalidSurplus {
        task: String,
        field: &'static str,
        detail: String,
    },
    #[error("task `{task}` sets a surplus gate without `agent` or `wake`")]
    SurplusNeedsAgent { task: String },
}

/// A loop delivery target pinned to the exact live agent session that scheduled
/// it. The handle is display-only; `session` is the durable address.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskTarget {
    pub kind: String,
    pub session: String,
    pub handle: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckOn {
    #[default]
    Fail,
    Success,
}

fn resolve_root_with(root: &Path, home: PathBuf) -> PathBuf {
    let raw = root.to_string_lossy();
    let expanded = if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        root.to_path_buf()
    };
    expanded.canonicalize().unwrap_or(expanded)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_root_expands_tilde_prefix() {
        let home = PathBuf::from("/home/dev");
        assert_eq!(
            resolve_root_with(Path::new("~/workspace/app"), home.clone()),
            home.join("workspace/app")
        );
        assert_eq!(resolve_root_with(Path::new("~"), home.clone()), home);
        assert_eq!(
            resolve_root_with(Path::new("~other/app"), PathBuf::from("/home/dev")),
            PathBuf::from("~other/app")
        );
    }

    #[test]
    fn resolve_root_canonicalizes_existing_absolute_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("mkdir nested");
        let dotted = nested.join(".");

        assert_eq!(
            resolve_root_with(&dotted, PathBuf::from("/home/dev")),
            nested.canonicalize().expect("canonical nested")
        );
    }

    #[test]
    fn task_entry_check_fields_round_trip_toml_and_json() {
        let deadline = Timestamp::from_second(1_783_000_000).expect("deadline");
        let entry = TaskEntry {
            wake: Some(TaskTarget {
                kind: "claude".to_owned(),
                session: "sess-1".to_owned(),
                handle: "@claude".to_owned(),
            }),
            prompt: Some("wake".to_owned()),
            check: Some("cargo test".to_owned()),
            verify: Some("cargo xtask gate".to_owned()),
            max_attempts: Some(4),
            max_strikes: Some(5),
            on: Some(CheckOn::Success),
            root: PathBuf::from("/repo"),
            every: Some("reset".to_owned()),
            budget: Some("$5.00".to_owned()),
            budget_per_day: Some("$20.00".to_owned()),
            surplus: Some("1.5x".to_owned()),
            surplus_after: Some("3d".to_owned()),
            deadline: Some(deadline),
            ..TaskEntry::default()
        };
        let tasks = Tasks(BTreeMap::from([("ci".to_owned(), entry.clone())]));
        let loop_config = LoopConfig { tasks };

        let toml = toml::to_string(&loop_config).expect("toml");
        let toml_round: LoopConfig = toml::from_str(&toml).expect("toml round trip");
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.check.as_deref()),
            Some("cargo test")
        );
        assert_eq!(
            toml_round.tasks.0.get("ci").and_then(|entry| entry.on),
            Some(CheckOn::Success)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.verify.as_deref()),
            Some("cargo xtask gate")
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.max_attempts),
            Some(4)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.deadline),
            Some(deadline)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.every.as_deref()),
            Some("reset")
        );
        assert!(
            toml.contains("every = \"reset\""),
            "reset cadence should round-trip through TOML: {toml}"
        );
        assert!(toml.contains("budget = \"$5.00\""), "{toml}");
        assert!(toml.contains("budget-per-day = \"$20.00\""), "{toml}");
        assert!(toml.contains("surplus = \"1.5x\""), "{toml}");
        assert!(toml.contains("surplus-after = \"3d\""), "{toml}");
        assert!(toml.contains("max-attempts = 4"), "{toml}");

        let json = serde_json::to_string(&loop_config.tasks).expect("json");
        let json_round: Tasks = serde_json::from_str(&json).expect("json round trip");
        assert_eq!(json_round.0.get("ci"), Some(&entry));
    }

    #[test]
    fn old_task_keys_are_rejected_by_name() {
        let err = toml::from_str::<Tasks>("[old]\nspec = \"claude\"\nroot = \"/repo\"\n")
            .expect_err("old key rejected");
        assert!(
            err.to_string().contains("unknown field `spec`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn task_budgets_validate_as_one_config_unit() {
        let invalid = TaskEntry {
            budget_per_day: Some("$20.00".to_owned()),
            ..TaskEntry::default()
        };
        assert!(matches!(
            invalid.validate_budget("nightly"),
            Err(TaskBudgetError::MissingRunBudget { task }) if task == "nightly"
        ));

        let malformed = TaskEntry {
            budget: Some("many dollars".to_owned()),
            ..TaskEntry::default()
        };
        assert!(matches!(
            malformed.validate_budget("nightly"),
            Err(TaskBudgetError::Invalid {
                field: "budget",
                ..
            })
        ));

        for (field, entry) in [
            (
                "surplus",
                TaskEntry {
                    surplus: Some("many".to_owned()),
                    ..TaskEntry::default()
                },
            ),
            (
                "surplus-after",
                TaskEntry {
                    surplus_after: Some("soon".to_owned()),
                    ..TaskEntry::default()
                },
            ),
        ] {
            assert!(matches!(
                entry.validate_budget("nightly"),
                Err(TaskBudgetError::InvalidSurplus { field: actual, .. }) if actual == field
            ));
        }

        assert!(matches!(
            TaskEntry {
                surplus: Some("1.5x".to_owned()),
                ..TaskEntry::default()
            }
            .validate_budget("nightly"),
            Err(TaskBudgetError::SurplusNeedsAgent { task }) if task == "nightly"
        ));
    }
}
