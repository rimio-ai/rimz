//! Provider-neutral background shells: a command an agent launched to keep
//! running past the tool call that started it.
//!
//! An adapter reports what one hook proves as a [`BackgroundShellReport`] on
//! its lifecycle observation; the rollup folds reports into the session's
//! durable shell list with [`BackgroundShellReport::apply`]. Nothing here knows
//! a provider.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// One background shell a session is running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundShell {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When RimZ first saw the shell, not when the provider spawned it.
    pub started_at: Timestamp,
}

/// What one hook proves about a session's background shells.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BackgroundShellReport {
    /// A shell launched; the list gains it.
    Started { shell: BackgroundShell },
    /// The authoritative list of shells still running; it replaces the list.
    Snapshot { shells: Vec<BackgroundShell> },
    /// These shells stopped running; the list drops them.
    Finished { ids: Vec<String> },
}

impl BackgroundShellReport {
    /// Fold this report onto `shells`. A shell already listed keeps its first
    /// `started_at`, so a later report never resets its elapsed time.
    pub fn apply(&self, shells: &mut Vec<BackgroundShell>) {
        match self {
            Self::Started { shell } => {
                if !shells.iter().any(|known| known.id == shell.id) {
                    shells.push(shell.clone());
                }
            }
            Self::Snapshot { shells: reported } => {
                let next = reported
                    .iter()
                    .map(|shell| {
                        let started_at = shells
                            .iter()
                            .find(|known| known.id == shell.id)
                            .map_or(shell.started_at, |known| known.started_at);
                        BackgroundShell {
                            started_at,
                            ..shell.clone()
                        }
                    })
                    .collect();
                *shells = next;
            }
            Self::Finished { ids } => shells.retain(|known| !ids.contains(&known.id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(id: &str, started_at: i64) -> BackgroundShell {
        BackgroundShell {
            id: id.to_owned(),
            command: Some(format!("run {id}")),
            description: None,
            started_at: Timestamp::from_second(started_at).unwrap(),
        }
    }

    #[test]
    fn reports_fold_launch_snapshot_and_finish_keeping_first_sighting() {
        let mut shells = Vec::new();
        BackgroundShellReport::Started {
            shell: shell("a", 10),
        }
        .apply(&mut shells);
        BackgroundShellReport::Started {
            shell: shell("a", 20),
        }
        .apply(&mut shells);
        assert_eq!(shells, vec![shell("a", 10)]);

        BackgroundShellReport::Snapshot {
            shells: vec![shell("a", 30), shell("b", 30)],
        }
        .apply(&mut shells);
        assert_eq!(shells, vec![shell("a", 10), shell("b", 30)]);

        BackgroundShellReport::Finished {
            ids: vec!["a".to_owned()],
        }
        .apply(&mut shells);
        assert_eq!(shells, vec![shell("b", 30)]);

        BackgroundShellReport::Snapshot { shells: Vec::new() }.apply(&mut shells);
        assert!(shells.is_empty());
    }
}
