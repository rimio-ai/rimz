use super::*;

fn provider_key(kind: &str) -> crate::ids::LoginKey {
    crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind))
}

fn provider_kinds(snapshot: &SidebarSnapshot) -> Vec<&str> {
    snapshot
        .providers
        .iter()
        .map(|panel| panel.kind.as_str())
        .collect()
}

// ── Provider dashboard aggregation ──────────────────────────────────────────

mod panels;
mod pi;
mod windows;
