//! Pane-derived tab labels and the lifetime of a multiplexer's naming claim.

/// Claim pins a launch name and forgets status restoration. Status temporarily
/// disables automatic naming, remembering whether to restore it on Rest.
/// Release restores inherited naming unconditionally and drops pane pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabNameIntent {
    Claim { pane_name: String },
    Status,
    Rest,
    Release,
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
    }
}
