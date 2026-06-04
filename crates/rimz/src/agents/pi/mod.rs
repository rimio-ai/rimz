//! Pi adapter — spend surface today, hook surface next.
//!
//! Pi sessions are discovered and priced fleet-wide through [`spend`], and the
//! descriptor declares Pi's identity, branding, and capabilities, so spending,
//! the provider dashboard, and doctor all resolve Pi through the registry. The
//! hook surface — Pi's in-process extension API, blocking returns, and the
//! one-file extension install — is mirrored and feasibility-checked in
//! `docs/internals/adapter/pi-reference.md` and lands here next; until then
//! every capability it would unlock is declared off, so the absence renders
//! deliberately.

pub(crate) mod spend;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, PlanLabel, ThreadKey, ToolClassification,
};
use super::pricing::PriceBook;
use super::spending::CachedEntry;
use super::{AgentAdapter, AgentErr, ClassifiedHook, Result, classify_agent_hook};
use crate::feed::{FeedItem, Resolution};

/// Everything `const` about Pi, in one place. See [`AgentDescriptor`] for the
/// descriptor-vs-trait split.
static PI_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "pi",
    display_name: "Pi",
    brand: Brand {
        emblem: &[" ▗▛████▜▖", "  ▐▌  ▐▌", "  ▝▘  ▝▘"],
        color: 28,
    },
    // Pi sessions span whatever provider account the user wired, so no single
    // brand prefix is honest — the tier renders bare.
    plan_label: PlanLabel::TitleCaseOnly,
    // No hook surface yet, so no tool events reach the lifecycle channel.
    tools: ToolClassification {
        mutating: &[],
        editing: &[],
    },
    capabilities: Capabilities {
        blocking_feed: false,
        rate_limit_windows: false,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        hook_install: false,
    },
    // Placeholder until the extension adapter lands — nothing blocks on it
    // while `blocking_feed` is off.
    hook_cap: Duration::from_secs(60),
    process_names: &["pi"],
    hook_install_unavailable: Some("the Pi extension adapter has not landed yet"),
    thread_key: ThreadKey::PerFile,
};

#[derive(Clone, Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &PI_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        // No hook surface yet: every event classifies Unknown.
        classify_agent_hook(event_name, None, &[])
    }

    fn render_decision(&self, _item: &FeedItem, _resolution: &Resolution) -> Result<Value> {
        Err(AgentErr::Render {
            agent: "pi",
            reason: "pi has no blocking feed surface yet".to_owned(),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::pi_session_files()
    }

    /// Pi logs `costUSD` directly, so the price book is unused.
    fn parse_spend(&self, path: &Path, _prices: &PriceBook) -> Vec<CachedEntry> {
        spend::parse_pi_jsonl(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentHookClass;

    #[test]
    fn pi_classifies_every_event_unknown_until_the_hook_surface_lands() {
        let classified = PiAdapter.classify_hook("SessionStart", &Value::Null);
        assert_eq!(classified.class, AgentHookClass::Unknown);
    }

    #[test]
    fn pi_declares_its_absent_surfaces() {
        let capabilities = PiAdapter.descriptor().capabilities;
        assert!(!capabilities.blocking_feed);
        assert!(!capabilities.rate_limit_windows);
        assert!(!capabilities.hook_install);
        assert!(PI_DESCRIPTOR.hook_install_unavailable.is_some());
    }
}
