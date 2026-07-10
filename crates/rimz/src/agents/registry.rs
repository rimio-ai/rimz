//! The agent registry — the single registration point.
//!
//! Built-ins live in [`ADAPTERS`]; validated machine-tier process plugins join
//! them through [`all_adapters`]. Every behavioral dispatch site resolves
//! through this module, so no consumer grows a per-agent match arm.

use super::amp::AmpAdapter;
use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::copilot::CopilotAdapter;
use super::cursor::CursorAdapter;
use super::descriptor::AgentDescriptor;
use super::droid::DroidAdapter;
use super::gemini::GeminiAdapter;
use super::kiro::KiroAdapter;
use super::opencode::OpencodeAdapter;
use super::pi::PiAdapter;
use super::qwen::QwenAdapter;
use super::{AgentAdapter, AgentErr, Result};

/// Every wired agent, in display order. `&'static dyn` — adapters are
/// zero-sized const values, so resolution never allocates.
pub static ADAPTERS: &[&'static dyn AgentAdapter] = &[
    &ClaudeAdapter,
    &CodexAdapter,
    &AmpAdapter,
    &CopilotAdapter,
    &GeminiAdapter,
    &PiAdapter,
    &OpencodeAdapter,
    &CursorAdapter,
    &DroidAdapter,
    &KiroAdapter,
    &QwenAdapter,
];

/// Every built-in and valid machine-tier plugin adapter, in display order.
pub fn all_adapters() -> impl Iterator<Item = &'static dyn AgentAdapter> {
    ADAPTERS
        .iter()
        .copied()
        .chain(super::plugin::loaded().adapters.iter().copied())
}

/// Resolve an adapter for the `--source <agent>` CLI tag.
pub fn adapter_by_kind(kind: &str) -> Result<&'static dyn AgentAdapter> {
    find_adapter(kind).ok_or_else(|| AgentErr::Unknown(kind.to_owned()))
}

/// [`adapter_by_kind`] for callers that treat an unknown kind as absence.
pub fn find_adapter(kind: &str) -> Option<&'static dyn AgentAdapter> {
    all_adapters().find(|adapter| adapter.descriptor().kind == kind)
}

/// The static descriptor for `kind`, for sites that need only const data
/// (branding, capabilities, tool tables) without the behavioral trait.
pub fn descriptor_by_kind(kind: &str) -> Option<&'static AgentDescriptor> {
    find_adapter(kind).map(AgentAdapter::descriptor)
}

/// Display-order kinds — the walk doctor and the wiring probes iterate.
pub fn known_kinds() -> impl Iterator<Item = &'static str> {
    all_adapters().map(|adapter| adapter.descriptor().kind)
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
        // `--smart-compact` types this into the agent's composer; every wired
        // agent exposes a slash command (`/compact`, Gemini's `/compress`,
        // Cursor's `/summarize`), so a new adapter that forgets to opt in fails
        // here rather than silently never compacting.
        for adapter in ADAPTERS {
            // Amp compacts automatically and exposes no manual compact command;
            // see docs/externals/agent-adapter/amp-reference.md.
            if adapter.descriptor().kind == "amp" {
                continue;
            }
            let command = adapter.compact_command().unwrap_or_else(|| {
                panic!("missing compact command for {}", adapter.descriptor().kind)
            });
            assert!(!command.is_empty() && command.starts_with('/'));
        }
    }

    #[test]
    fn sub_providers_are_unique() {
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
