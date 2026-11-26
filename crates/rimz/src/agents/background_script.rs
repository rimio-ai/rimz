//! Provider-neutral background scripts: a code-mode script an agent started
//! that keeps running past the call that dispatched it.
//!
//! An adapter reports what one hook proves as a [`BackgroundScriptReport`] on
//! its lifecycle observation; the rollup folds reports into the session's
//! durable script list with [`BackgroundScriptReport::apply`]. A script counts
//! as background work once it has run [`BACKGROUND_SCRIPT_SHOWN_AFTER`] since
//! RimZ first saw it. Nothing here knows a provider.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// How long a script runs, from first sighting, before it shows as background
/// work: the age at which a code-mode harness lets later scripts run beside one
/// that has not settled. A script that settles sooner never shows.
pub const BACKGROUND_SCRIPT_SHOWN_AFTER: SignedDuration = SignedDuration::from_secs(30);

/// One script a session is running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundScript {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When RimZ first saw the script, not when the provider started it.
    pub started_at: Timestamp,
    /// The native key of an ask a call inside a script raised while this one was
    /// listed. The script waits on that ask while it stays open, so it is not
    /// running work then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
}

/// What one hook proves about a session's background scripts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BackgroundScriptReport {
    /// A script started; the list gains it.
    Started { script: BackgroundScript },
    /// This script's result reached the agent; the list drops it.
    Finished { id: String },
    /// The session runs no script; the list empties.
    NoneRunning,
}

impl BackgroundScriptReport {
    /// Fold this report onto `scripts`. A script already listed keeps its
    /// first sighting, so a repeated start never resets its age.
    pub(crate) fn apply(&self, scripts: &mut Vec<BackgroundScript>) {
        match self {
            Self::Started { script } => {
                if !scripts.iter().any(|known| known.id == script.id) {
                    scripts.push(script.clone());
                }
            }
            Self::Finished { id } => scripts.retain(|known| known.id != *id),
            Self::NoneRunning => scripts.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(id: &str, started_at: i64) -> BackgroundScript {
        BackgroundScript {
            id: id.to_owned(),
            description: Some(format!("run {id}")),
            started_at: Timestamp::from_second(started_at).unwrap(),
            held_by: None,
        }
    }

    #[test]
    fn reports_fold_start_finish_and_none_running_keeping_first_sighting() {
        let mut scripts = Vec::new();
        for (id, at) in [("a", 10), ("a", 20), ("b", 30)] {
            BackgroundScriptReport::Started {
                script: script(id, at),
            }
            .apply(&mut scripts);
        }
        assert_eq!(scripts, vec![script("a", 10), script("b", 30)]);

        BackgroundScriptReport::Finished { id: "a".to_owned() }.apply(&mut scripts);
        assert_eq!(scripts, vec![script("b", 30)]);

        BackgroundScriptReport::NoneRunning.apply(&mut scripts);
        assert!(scripts.is_empty());
    }
}
