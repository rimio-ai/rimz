//! Pane-derived tab labels and the lifetime of a multiplexer's naming claim.

/// Claim pins a launch name and forgets status restoration. Status temporarily
/// disables automatic naming, remembering whether to restore it on Rest.
/// Release restores inherited naming and drops pane pins.
///
/// Status, Rest, and Release are projections from the tab name a producer
/// observed, and apply only while the tab still bears it; Claim applies
/// whatever the tab is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabNameIntent {
    Claim { pane_name: String },
    Status { observed: String },
    Rest { observed: String },
    Release { observed: String },
}

impl TabNameIntent {
    /// The tab name a projection was computed from; a claim has none.
    pub(super) fn observed(&self) -> Option<&str> {
        match self {
            Self::Claim { .. } => None,
            Self::Status { observed } | Self::Rest { observed } | Self::Release { observed } => {
                Some(observed)
            }
        }
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

pub(crate) fn is_named_after_panes(base: &str, names: &[&str]) -> bool {
    if base.starts_with('#') || base.starts_with("team:") {
        return false;
    }
    let mut tokens = base.split('+').filter(|token| *token != "…").peekable();
    tokens.peek().is_some() && tokens.all(|token| !token.is_empty() && names.contains(&token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_bound_the_layout_without_losing_ownership() {
        let names = ["opus", "codex", "pi", "nvim"];
        assert_eq!(label_from_pane_names([]), "");
        assert_eq!(label_from_pane_names(["opus"]), "opus");
        assert_eq!(label_from_pane_names(names), "opus+codex+pi+…");
        assert!(is_named_after_panes(&label_from_pane_names(names), &names));
        assert!(is_named_after_panes("pi+opus", &names));
        for label in ["", "…", "#feat", "team:forge", "my tab", "opus+missing"] {
            assert!(!is_named_after_panes(label, &names), "{label}");
        }
        assert!(!is_named_after_panes("opus", &[]));
        assert!(!is_named_after_panes("#feat", &["#feat"]));
        assert!(!is_named_after_panes("team:forge", &["team:forge"]));
    }
}
