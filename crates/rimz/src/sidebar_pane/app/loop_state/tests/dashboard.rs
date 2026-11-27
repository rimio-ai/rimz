//! The dashboard tab state an input leaves behind: the frame an input paints already
//! follows the selection it moved, with no fold in between.

use super::*;
use crate::sidebar_pane::app::fixtures::provider;

/// A claude card, a pi card, then a shell, under a claude, codex, pi tab rail, with the
/// claude card focused so the fold seats the baseline there.
fn claude_pi_shell_rig() -> Rig {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    let mut shell =
        snapshot_with_panes(&rig.ws, vec![pane("terminal_10", "tab_0")]).worktree_groups[0].clone();
    shell.key = "/repo/side".to_owned();
    shell.label = "side".to_owned();
    let groups = Arc::make_mut(&mut snapshot.worktree_groups);
    let rows = Arc::make_mut(&mut groups[0].rows);
    let mut pi = rows[0].clone();
    pi.id = "agent-2".to_owned();
    pi.name = "pi".to_owned();
    pi.pane = Some(pane("terminal_11", "tab_0"));
    rows.push(pi);
    groups.push(shell);
    let working_pane_ids: Vec<_> = snapshot
        .rows()
        .flat_map(|row| row.pane_ids())
        .cloned()
        .collect();
    snapshot.viewed_panes = working_pane_ids.clone();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: working_pane_ids.len(),
        working_pane_ids,
        own_view_is_daemon: false,
    });
    snapshot.focused_pane = Some(pane("terminal_9", "tab_0").pane_id);
    snapshot.providers = ["claude", "codex", "pi"].map(provider).into();
    rig.fold(snapshot, SnapshotSource::Produced);
    rig
}

/// The open tab and the rail's order as the view holds them now, and the kinds the last
/// paint hit-mapped on the rail, left to right.
fn dashboard(rig: &mut Rig) -> (Option<String>, Vec<String>, Vec<String>) {
    let painted = rig
        .state
        .pane
        .ui
        .interactions
        .regions()
        .iter()
        .filter_map(|hit| match &hit.target {
            render::HitTarget::ProviderTab(login) => Some(login.kind.as_str().to_owned()),
            _ => None,
        })
        .collect();
    let mut view = rig.view();
    let (ui, ctx) = view.pair();
    let active = render::active_dashboard_tab(&ctx.current, ui).map(|tab| tab.kind.to_string());
    let rail = render::dashboard_rail(&ctx.current, ui)
        .iter()
        .map(|tab| tab.kind.to_string())
        .collect();
    (active, rail, painted)
}

fn kinds(kinds: &[&str]) -> Vec<String> {
    kinds.iter().map(|kind| (*kind).to_owned()).collect()
}

/// A card selected by keys ends a tab pick anchored to another card's provider at that
/// key, so the frame the key paints leads with the new card's provider and opens it.
#[test]
fn a_key_onto_another_providers_card_ends_the_tab_pick_before_its_frame() {
    let mut rig = claude_pi_shell_rig();
    rig.input(KeyAction::Top);
    rig.input(KeyAction::TabNext);
    assert_eq!(dashboard(&mut rig).0.as_deref(), Some("codex"));

    rig.input(KeyAction::Down);

    let (active, rail, painted) = dashboard(&mut rig);
    assert_eq!(active.as_deref(), Some("pi"), "the stale codex pick ended");
    assert_eq!(rail, kinds(&["pi", "claude", "codex"]));
    assert_eq!(painted, rail, "the key's own frame paints the new order");
}

/// Keys walking from a claude card through a pi card onto a shell leave the pi card's
/// provider leading, at the key and after the fold that follows it.
#[test]
fn a_key_walk_onto_a_shell_holds_the_last_card_it_passed() {
    let mut rig = claude_pi_shell_rig();
    rig.input(KeyAction::Top);
    rig.input(KeyAction::Down);
    rig.input(KeyAction::Down);
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(pane("terminal_10", "tab_0").pane_id)
    );

    let (active, rail, _) = dashboard(&mut rig);
    assert_eq!(active.as_deref(), Some("pi"));
    assert_eq!(rail, kinds(&["pi", "claude", "codex"]));

    let snapshot = rig.view().ctx().current.clone();
    rig.fold(snapshot, SnapshotSource::Produced);
    let (active, rail, _) = dashboard(&mut rig);
    assert_eq!(active.as_deref(), Some("pi"), "the fold keeps the pi lead");
    assert_eq!(rail, kinds(&["pi", "claude", "codex"]));
}
