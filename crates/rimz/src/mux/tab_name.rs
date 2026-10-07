//! Pane-derived tab labels and the lifetime of a multiplexer's naming claim.

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, ViewKind};

/// What the multiplexer records about who names a view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewNaming {
    #[serde(default)]
    pub automatic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TabOwnerRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabOwnerRecord {
    pub base: String,
    #[serde(default)]
    pub founders: Vec<PaneId>,
}

/// Claim pins a launch name and records its founder. Status and Rest rename
/// only; Rebuild also updates the recorded base. Release restores inherited
/// naming and drops ownership and pane pins.
///
/// Status, Rest, Rebuild, and Release are projections from the tab name a producer
/// observed, and apply only while the tab still bears it; Claim applies
/// whatever the tab is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabNameIntent {
    Claim { pane_name: String },
    Status { observed: String },
    Rest { observed: String },
    Rebuild { observed: String, base: String },
    Release { observed: String },
}

impl TabNameIntent {
    /// The tab name a projection was computed from; a claim has none.
    pub(super) fn observed(&self) -> Option<&str> {
        match self {
            Self::Claim { .. } => None,
            Self::Status { observed }
            | Self::Rest { observed }
            | Self::Rebuild { observed, .. }
            | Self::Release { observed } => Some(observed),
        }
    }
}

pub(crate) fn is_scoped_label(base: &str) -> bool {
    base.starts_with('#') || base.starts_with("team:") || base.starts_with("team-")
}

pub(super) fn tmux_window_name(raw: &str) -> String {
    raw.replace([':', '.'], "-")
}

/// A label in the form the view's backend reads it back, so a stored label
/// compares equal to the one that was written.
pub(crate) fn stored_label(kind: ViewKind, label: &str) -> String {
    match kind {
        ViewKind::Window => tmux_window_name(label.trim()).replace(',', "_"),
        ViewKind::Tab => label.to_owned(),
    }
}

pub(crate) fn label_from_pane_names<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut names = names.into_iter();
    let mut label = names.by_ref().take(3).collect::<Vec<_>>().join("+");
    if names.next().is_some() {
        label.push_str("+…");
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_intents_carry_the_observed_name() {
        for intent in [
            TabNameIntent::Status {
                observed: "before".to_owned(),
            },
            TabNameIntent::Rest {
                observed: "before".to_owned(),
            },
            TabNameIntent::Rebuild {
                observed: "before".to_owned(),
                base: "after".to_owned(),
            },
            TabNameIntent::Release {
                observed: "before".to_owned(),
            },
        ] {
            assert_eq!(intent.observed(), Some("before"));
        }
        assert_eq!(
            TabNameIntent::Claim {
                pane_name: "opus".to_owned()
            }
            .observed(),
            None
        );
    }

    #[test]
    fn scoped_labels_include_sanitized_tmux_team_names() {
        for label in ["#feat", "team:forge", "team-forge"] {
            assert!(is_scoped_label(label), "{label}");
        }
        for label in ["", "opus", "opus+codex", "my tab"] {
            assert!(!is_scoped_label(label), "{label}");
        }
    }

    #[test]
    fn labels_bound_the_layout() {
        let names = ["opus", "codex", "pi", "nvim"];
        assert_eq!(label_from_pane_names([]), "");
        assert_eq!(label_from_pane_names(["opus"]), "opus");
        assert_eq!(label_from_pane_names(names), "opus+codex+pi+…");
    }
}
