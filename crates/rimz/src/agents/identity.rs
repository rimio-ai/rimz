//! Shared root and subagent identity resolution.
//!
//! Adapters pass their native id fields through this module so malformed child
//! payloads quarantine consistently instead of folding onto a parent row.

use serde_json::Value;
use tracing::{debug, error};

use crate::ids::AgentSessionId;

/// The outcome of resolving a subagent event's identity.
pub(crate) enum SubagentIdentity {
    /// A usable child id distinct from its parent — the only case that yields a
    /// child entity.
    Resolved {
        agent_id: AgentSessionId,
        parent_agent_id: AgentSessionId,
    },
    /// Unusable identity (missing child or parent id, or child == parent). The
    /// caller emits no observation, so a malformed subagent event can never
    /// fold onto — and corrupt — its parent's row.
    Quarantined,
}

/// Resolve a subagent event's identity, requiring a non-empty child id, a
/// non-empty parent id, and `child != parent`. This is the one place the rule
/// lives, shared by both adapters; it replaces the unsafe per-adapter
/// `child_id.or_else(|| parent_id)` fallback that silently keyed a child onto
/// its parent. A quarantined identity is logged once with the raw payload so
/// the anomaly is traceable.
pub(crate) fn resolve_subagent_identity(
    kind: &str,
    event_name: &str,
    child_id: Option<&str>,
    parent_id: Option<&str>,
    payload: &Value,
) -> SubagentIdentity {
    if let Some((agent_id, parent_agent_id)) = validated_subagent_identity(child_id, parent_id) {
        return SubagentIdentity::Resolved {
            agent_id,
            parent_agent_id,
        };
    }
    let child = child_id.map(str::trim).filter(|value| !value.is_empty());
    let parent = parent_id.map(str::trim).filter(|value| !value.is_empty());
    error!(
        target: "rimz::agent::lifecycle",
        kind,
        event = event_name,
        child_id = child.unwrap_or(""),
        parent_id = parent.unwrap_or(""),
        payload = %payload,
        "subagent identity unusable — quarantined (need a distinct child and parent id)",
    );
    SubagentIdentity::Quarantined
}

/// Validate the shared child/parent identity rule without logging provider-local discovery failures.
pub(crate) fn validated_subagent_identity(
    child_id: Option<&str>,
    parent_id: Option<&str>,
) -> Option<(AgentSessionId, AgentSessionId)> {
    let child = child_id.map(str::trim).filter(|value| !value.is_empty())?;
    let parent = parent_id.map(str::trim).filter(|value| !value.is_empty())?;
    (child != parent).then(|| (AgentSessionId::from(child), AgentSessionId::from(parent)))
}

/// The outcome of resolving a non-subagent (root-arm) event's identity.
pub(crate) enum RootIdentity {
    /// A normal root event: key on the session id, no parent link.
    Root { agent_id: Option<AgentSessionId> },
    /// The event is stamped with a distinct child `agent_id` — it fired inside
    /// a subagent. Adapters whose child contract is bracket-grained drop it;
    /// adapters with typed child progress resolve that identity before calling
    /// this root-only helper. Folding it onto the parent would advance the
    /// parent's `last_activity` while it remains blocked.
    ForeignChild,
}

/// Resolve a non-subagent event's identity. A payload whose `agent_id` is
/// present, non-empty, and distinct from its `session_id` fired inside a
/// subagent and is the child's, never the root's — the one place the rule
/// lives, shared by the adapters whose providers stamp `agent_id` on every
/// in-subagent payload. A missing or session-equal `agent_id` is a normal root;
/// quarantine stays `Subagent*`-only.
pub(crate) fn resolve_root_identity(
    kind: &str,
    event_name: &str,
    agent_id: Option<&str>,
    session_id: Option<&str>,
) -> RootIdentity {
    let agent = agent_id.map(str::trim).filter(|value| !value.is_empty());
    let session = session_id.map(str::trim).filter(|value| !value.is_empty());
    match (agent, session) {
        (Some(agent), session) if session != Some(agent) => {
            debug!(
                target: "rimz::agent::lifecycle",
                kind,
                event = event_name,
                agent_id = agent,
                session_id = session.unwrap_or(""),
                "foreign-child lifecycle event dropped (rides the child-keyed heartbeat)",
            );
            RootIdentity::ForeignChild
        }
        _ => RootIdentity::Root {
            agent_id: session.map(AgentSessionId::from),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subagent_identity_needs_a_distinct_child_and_parent() {
        match resolve_subagent_identity(
            "claude",
            "SubagentStart",
            Some("child"),
            Some("root"),
            &json!({}),
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => {
                assert_eq!(agent_id, "child");
                assert_eq!(parent_agent_id, "root");
            }
            SubagentIdentity::Quarantined => panic!("expected resolved"),
        }
        // A missing child or parent, equal ids, or a blank child all quarantine —
        // a malformed subagent event can never fold onto its parent's row.
        for (child, parent) in [
            (None, Some("root")),
            (Some("child"), None),
            (Some("same"), Some("same")),
            (Some("  "), Some("root")),
        ] {
            assert!(
                matches!(
                    resolve_subagent_identity("claude", "SubagentStart", child, parent, &json!({})),
                    SubagentIdentity::Quarantined
                ),
                "child={child:?} parent={parent:?}",
            );
        }
    }
}
