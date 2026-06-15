//! The agent registry — the single registration point.
//!
//! Every agent Rimz can wire, in display order. Adding an agent is a new
//! `agents/<name>/` directory plus one line in [`ADAPTERS`]; every dispatch
//! site (the hook CLI, doctor, the sidebar reducer, spending, branding)
//! resolves through here, so no shared file grows a per-agent match arm.

use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::descriptor::AgentDescriptor;
use super::opencode::OpencodeAdapter;
use super::pi::PiAdapter;
use super::{AgentAdapter, AgentErr, Result};

/// Every wired agent, in display order. `&'static dyn` — adapters are
/// zero-sized const values, so resolution never allocates.
pub static ADAPTERS: &[&'static dyn AgentAdapter] =
    &[&ClaudeAdapter, &CodexAdapter, &PiAdapter, &OpencodeAdapter];

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

/// The agent kind whose account a subscription-provider id belongs to —
/// `anthropic` → `claude`, `openai` → `codex` — resolved through each
/// descriptor's [`sub_providers`](AgentDescriptor::sub_providers) declaration.
/// Lets the dashboard borrow budget windows for a multi-provider client (Pi)
/// running on a sibling kind's subscription.
pub fn kind_for_sub_provider(provider: &str) -> Option<&'static str> {
    ADAPTERS
        .iter()
        .map(|adapter| adapter.descriptor())
        .find(|descriptor| descriptor.sub_providers.contains(&provider))
        .map(|descriptor| descriptor.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_kinds_and_sub_providers_without_collisions() {
        // Every kind round-trips through resolution, an unknown kind errors, and
        // no two adapters claim the same kind.
        for adapter in ADAPTERS {
            let kind = adapter.descriptor().kind;
            assert_eq!(
                adapter_by_kind(kind).unwrap().descriptor().kind,
                kind,
                "registry round-trip for {kind}"
            );
        }
        assert!(adapter_by_kind("unknown-agent").is_err());

        let mut kinds: Vec<_> = known_kinds().collect();
        kinds.sort_unstable();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), before, "duplicate kind in ADAPTERS");
    }

    #[test]
    fn every_adapter_exposes_a_manual_compaction_command() {
        // `--auto-compact` types this into the agent's composer; every wired
        // agent supports the `/compact` slash command, so a new adapter that
        // forgets to opt in fails here rather than silently never compacting.
        for adapter in ADAPTERS {
            assert_eq!(
                adapter.compact_command(),
                Some("/compact"),
                "missing compact command for {}",
                adapter.descriptor().kind
            );
        }
    }

    #[test]
    fn sub_providers_are_unique_and_resolve_to_their_metering_kind() {
        // The credential keys Pi's auth file uses, resolved through each
        // descriptor's declaration — the dashboard's window-borrow mapping.
        assert_eq!(kind_for_sub_provider("anthropic"), Some("claude"));
        assert_eq!(kind_for_sub_provider("openai"), Some("codex"));
        assert_eq!(kind_for_sub_provider("openai-codex"), Some("codex"));
        assert_eq!(kind_for_sub_provider("github-copilot"), None);

        let mut providers: Vec<_> = ADAPTERS
            .iter()
            .flat_map(|adapter| adapter.descriptor().sub_providers)
            .collect();
        providers.sort_unstable();
        let before = providers.len();
        providers.dedup();
        assert_eq!(providers.len(), before, "sub provider claimed twice");
    }
}
