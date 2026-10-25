use super::*;

#[test]
fn go_plan_headers_keep_known_and_unknown_product_names() {
    let labels = ["go", "go-plus", "black-max"]
        .map(|product| crate::store::snapshot::view::format_plan_label("opencode", product));
    assert_eq!(labels, ["Go", "Go Plus", "Black Max"]);
}

fn provider_key(kind: &str) -> crate::ids::LoginKey {
    crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind))
}

/// The logins a fold may build blocks for, by key or bare kind for `default`.
fn shown(logins: &[&str]) -> std::collections::BTreeSet<crate::ids::LoginKey> {
    logins
        .iter()
        .map(|login| {
            if login.contains('@') {
                login.parse().expect("login key")
            } else {
                provider_key(login)
            }
        })
        .collect()
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
