//! The agent registry — the single registration point.
//!
//! Every agent Rimz can wire, in display order. Adding an agent is a new
//! `agents/<name>/` directory plus one line in [`ADAPTERS`]; every dispatch
//! site (the hook CLI, doctor, the sidebar reducer, spending, branding)
//! resolves through here, so no shared file grows a per-agent match arm.

use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::descriptor::AgentDescriptor;
use super::pi::PiAdapter;
use super::{AgentAdapter, AgentErr, Result};

/// Every wired agent, in display order. `&'static dyn` — adapters are
/// zero-sized const values, so resolution never allocates.
pub static ADAPTERS: &[&'static dyn AgentAdapter] = &[&ClaudeAdapter, &CodexAdapter, &PiAdapter];

/// Resolve an adapter for the `--source <agent>` CLI tag.
pub fn adapter_by_kind(kind: &str) -> Result<&'static dyn AgentAdapter> {
    find_adapter(kind).ok_or_else(|| AgentErr::Unknown(kind.to_owned()))
}

/// [`adapter_by_kind`] for callers that treat an unknown kind as absence.
pub fn find_adapter(kind: &str) -> Option<&'static dyn AgentAdapter> {
    ADAPTERS
        .iter()
        .copied()
        .find(|adapter| adapter.descriptor().kind == kind)
}

/// The static descriptor for `kind`, for sites that need only const data
/// (branding, capabilities, tool tables) without the behavioral trait.
pub fn descriptor_by_kind(kind: &str) -> Option<&'static AgentDescriptor> {
    find_adapter(kind).map(AgentAdapter::descriptor)
}

/// Display-order kinds — the walk doctor and the wiring probes iterate.
pub fn known_kinds() -> impl Iterator<Item = &'static str> {
    ADAPTERS.iter().map(|adapter| adapter.descriptor().kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_each_kind_to_its_own_descriptor() {
        for adapter in ADAPTERS {
            let kind = adapter.descriptor().kind;
            assert_eq!(
                adapter_by_kind(kind).unwrap().descriptor().kind,
                kind,
                "registry round-trip for {kind}"
            );
        }
        assert!(adapter_by_kind("unknown-agent").is_err());
    }

    #[test]
    fn kinds_are_unique() {
        let mut kinds: Vec<_> = known_kinds().collect();
        kinds.sort_unstable();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), before, "duplicate kind in ADAPTERS");
    }
}
