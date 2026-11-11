//! The host's image permit, alone in its file.

/// The room host's leave for one paint pass to take a pixel slot and write kitty
/// graphics. Every exit that does either requires one; a pass mints it, and nothing
/// stores it. It has this file to itself so that no sibling can name its private
/// field: `cargo xtask invariants` holds the rest of the boundary.
pub(in crate::sidebar_pane) struct ImagePermit(());

impl ImagePermit {
    /// The host's grant, for a pane on a client's screen or a jump landing on it.
    /// `cargo xtask invariants` admits this call only in the app's permit mint.
    pub(in crate::sidebar_pane) fn grant_by_host() -> Self {
        Self(())
    }

    /// For painter and pixel unit tests and the scenario player, which paints the
    /// contributor's own terminal.
    #[cfg(any(test, feature = "testkit"))]
    pub(in crate::sidebar_pane) const fn for_test() -> Self {
        Self(())
    }
}
