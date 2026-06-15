use super::*;

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
