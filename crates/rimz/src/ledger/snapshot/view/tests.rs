//! Projection scenarios over the sidebar view-model, grouped by concern:
//! feed classification, worktree grouping, subagent nesting, pane binding,
//! the lazy-codex bind, the daemon reap, the displayed-status ladder,
//! ranking and caps, the session reaper, and the rate-limit stabilizers.
//!
//! Every scenario builds at the testkit [`epoch`] and projects at that same
//! instant, so window verdicts (stall, compaction expiry, ghost TTLs,
//! rate-limit resets) are exact — the suite never reads the wall clock.

use std::path::{Path, PathBuf};

use super::super::project::reduce_agent_states;
use super::*;
use crate::agents::SpendWindow;
use crate::agents::lifecycle::LifecycleSignal;
use crate::feed::FeedKind;
use crate::feed::{RuntimeOwner, RuntimeOwnerKind};
use crate::ledger::snapshot::testkit::*;

/// A pending agent-hook ask naming `session_id`, homed at `/repo/main` like
/// the agents it joins.
fn agent_ask(kind: FeedKind, source: &str, session_id: &str) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        kind,
        format!("{source} needs attention"),
        source,
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": session_id });
    item
}

// ── Feed classification: which pending items become attention ───────────────

#[test]
fn build_groups_by_surface_and_status() {
    let mut native = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Permission,
        "n",
        "claude",
        "agent-hook",
    );
    let bridge = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "b",
        "rimz",
        "cli",
    );
    let mut answered = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "a",
        "rimz",
        "cli",
    );
    answered.status = FeedStatus::Resolved;
    let mut timed = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "t",
        "rimz",
        "cli",
    );
    timed.status = FeedStatus::TimedOut;
    native.updated_at += std::time::Duration::from_secs(1);

    let snap = room(vec![native, bridge, answered, timed], Vec::new());
    // Pending native + bridge asks surface as attention/working metadata; the
    // resolved and timed-out items are history, so they are dropped. Without a
    // live frame, none of them become rows.
    assert_eq!(snap.needs_attention.len(), 1);
    assert_eq!(snap.resolver_working.len(), 1);
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn pending_cli_native_items_do_not_become_sidebar_attention() {
    let item = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Generic,
        "Should I proceed?",
        "rimz",
        "cli",
    );

    let snap = room(vec![item], Vec::new());

    assert!(snap.needs_attention.is_empty());
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn pending_script_items_wait_for_a_live_frame() {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/repo/rimz".to_owned());
    item.worktree_branch = Some("main".to_owned());

    let snap = room(vec![item], Vec::new());

    assert_eq!(snap.needs_attention.len(), 1);
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn multiple_pending_asks_for_one_session_render_one_row() {
    // The live pile-up: a session held several pending native_ui asks, and
    // the no-panes rollup emitted one row each. Read-time dedup collapses
    // them to a single row keyed by `(source, agent_id)`.
    let session = agent("claude", "sess-1", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let items = vec![
        agent_ask(FeedKind::Permission, "claude", "sess-1"),
        agent_ask(FeedKind::Question, "claude", "sess-1"),
    ];

    let snapshot =
        room(items, vec![session]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows.iter().filter(|row| row.is_agent()).collect();
    assert_eq!(
        agent_rows.len(),
        1,
        "two pending asks for one session collapse to one row: {rows:?}"
    );
    assert_eq!(agent_rows[0].status(), Some(AgentStatus::Waiting));
}

#[test]
fn pending_attention_survives_as_metadata_without_pane_fold_in() {
    let item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );

    let snapshot = room(vec![item], Vec::new());

    assert_eq!(snapshot.needs_attention.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
}

// ── Activity heartbeat fold ─────────────────────────────────────────────────

#[test]
fn activity_heartbeat_updates_last_activity_not_phase() {
    let mut agent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
    agent.phase = TurnPhase::Reasoning;
    let original_seen = agent.last_seen;
    let at = original_seen + std::time::Duration::from_secs(10);
    let touch = AgentActivity {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        at,
    };
    let snap = room(Vec::new(), vec![agent]).with_agent_activity(&[touch]);

    // The heartbeat is latency, not a lifecycle signal — it advances
    // `last_activity` only, never the turn-phase head.
    assert_eq!(snap.agents[0].phase, TurnPhase::Reasoning);
    assert_eq!(snap.agents[0].last_activity, at);
    assert_eq!(snap.agents[0].last_seen, original_seen);
}

// ── Provider dashboard aggregation ──────────────────────────────────────────

#[test]
fn provider_panel_spending_is_attached_and_panels_order_by_kind() {
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let codex = agent("codex", "x1", AgentStatus::Idle, 20);
    let snapshot = room(Vec::new(), vec![claude, codex]);

    let today_tally = |usd: f64| SpendTally {
        today: SpendWindow {
            usd,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert("claude".to_owned(), today_tally(1.0));
    by_provider.insert("codex".to_owned(), today_tally(5.0));

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    // The panels are the dashboard's tabs, so they hold a stable kind order —
    // Codex's larger today spend (5.0) never reorders the row.
    assert_eq!(snapshot.providers[0].kind, "claude");
    assert_eq!(snapshot.providers[1].kind, "codex");
    // Each panel still carries its own spending tally.
    assert_eq!(
        snapshot.providers[0].spending.as_ref().unwrap().today.usd,
        1.0
    );
    assert_eq!(
        snapshot.providers[1].spending.as_ref().unwrap().today.usd,
        5.0
    );
}

#[test]
fn provider_cap_keeps_top_spenders_then_orders_by_kind() {
    // Three providers, room for two: today's spend decides *which* panels
    // survive the cap (claude's 1.0 is dropped), and the survivors render in
    // stable kind order regardless of who outspends whom.
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let codex = agent("codex", "x1", AgentStatus::Idle, 20);
    let pi = agent("pi", "p1", AgentStatus::Idle, 30);
    let mut snapshot = room(Vec::new(), vec![claude, codex, pi]);
    snapshot.sidebar.max_provider_blocks = 2;

    let today_tally = |usd: f64| SpendTally {
        today: SpendWindow {
            usd,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert("claude".to_owned(), today_tally(1.0));
    by_provider.insert("codex".to_owned(), today_tally(5.0));
    by_provider.insert("pi".to_owned(), today_tally(3.0));

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    let kinds: Vec<&str> = snapshot
        .providers
        .iter()
        .map(|panel| panel.kind.as_str())
        .collect();
    assert_eq!(kinds, vec!["codex", "pi"]);
}

#[test]
fn spend_only_provider_does_not_create_a_panel() {
    // No live agents and no probed accounts — only recorded fleet spend for
    // Claude. Spend enriches a discovered provider, but it is not provider
    // presence by itself, so the dashboard stays hidden.
    let snapshot = room(Vec::new(), Vec::new());

    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert(
        "claude".to_owned(),
        SpendTally {
            today: SpendWindow {
                usd: 2.0,
                tokens: 100,
                ..Default::default()
            },
            year: SpendWindow {
                usd: 9.0,
                tokens: 900,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    assert!(
        snapshot.providers.is_empty(),
        "historical spend alone does not create the provider section"
    );
}

#[test]
fn recorded_spend_attaches_to_a_probed_provider_panel() {
    let snapshot = room(Vec::new(), Vec::new());
    let mut probed = BTreeMap::new();
    probed.insert(
        "claude".to_owned(),
        AgentAccount {
            plan: Some("max".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: None,
        },
    );
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert(
        "claude".to_owned(),
        SpendTally {
            today: SpendWindow {
                usd: 2.0,
                tokens: 100,
                ..Default::default()
            },
            year: SpendWindow {
                usd: 9.0,
                tokens: 900,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let snapshot = snapshot.with_provider_aggregates(&probed, &BTreeMap::new(), &by_provider);

    let claude = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel from probed account");
    assert_eq!(claude.spending.as_ref().unwrap().year.usd, 9.0);
}

#[test]
fn default_emblems_keep_every_rows_leading_spaces() {
    // The emblem literals open with a bare newline so the art sits at
    // column 0 in source; the split must keep each row's leading spaces —
    // a `\` continuation once ate the first row's indent and the art
    // drifted a cell left on screen.
    let art = |kind: &str| default_provider_style(kind).1;
    assert_eq!(art("claude"), [" ▐▛███▜▌", "▝▜█████▛▘", "  ▘▘ ▝▝"]);
    assert_eq!(art("codex"), [" ▗▛███▜▖", "▐▜▌ ▚ ▐▛▌", " ▝▀▀▀▀▀▘"]);
    assert_eq!(art("pi"), [" █▜███▛█", "▝▜▛▀▀▀▜▛▘", " ▝▘   ▝▘"]);
}

#[test]
fn provider_without_the_rate_limit_capability_drops_stray_windows() {
    // Pi declares `rate_limit_windows: false`; Claude declares it true. The
    // same stray session reading must paint a budget bar only where the
    // descriptor declares the surface.
    let reading = window(40, 3_600);
    let pi = agent("pi", "p1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let claude = agent("claude", "c1", AgentStatus::Idle, 10).limits(vec![reading]);

    let snapshot = room(Vec::new(), vec![pi, claude]).with_provider_aggregates(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let panel = |kind: &str| {
        snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == kind)
            .unwrap_or_else(|| panic!("{kind} panel present"))
    };
    assert!(
        panel("pi").windows.is_empty(),
        "pi's declared absence drops the stray reading"
    );
    assert_eq!(panel("claude").windows.len(), 1);
}

#[test]
fn pi_on_a_metered_sub_borrows_the_sibling_kinds_windows() {
    // Pi reads no window surface of its own, but an OAuth subscription is the
    // sibling provider's account — `openai` maps to the codex kind — so the
    // Pi panel borrows codex's stable windows: same account, same bars.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);

    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![codex, pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered, "an OAuth sub is metered");
    assert_eq!(
        pi_panel.windows,
        vec![reading],
        "pi borrows the codex account's windows"
    );
}

#[test]
fn pi_sub_without_borrowable_windows_stays_bar_less() {
    // A metered Pi sub whose sibling has no readings (no codex session ever
    // reported), or whose provider maps to no kind, keeps the bar-less block.
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("GitHub Copilot OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("github-copilot".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.windows.is_empty(), "no sibling kind, no bars");
}

#[test]
fn pi_api_key_sub_never_borrows_windows() {
    // An unmetered (API-key) credential has no budget to meter, so even a
    // borrowable sibling leaves the `∞` bar untouched.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI API Key".to_owned()),
            metered: Some(false),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![codex, pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(!pi_panel.metered);
    assert!(pi_panel.windows.is_empty(), "an API key meters nothing");
}

// ── Worktree grouping ────────────────────────────────────────────────────────

#[test]
fn agents_on_different_branches_in_one_path_form_two_groups() {
    // Root cause 5: stale rows put two branches under one path, collapsing
    // into a single mislabeled section. Keying on branch splits them into
    // two correctly-labeled groups.
    let feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    let main = agent("claude", "sess-b", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");

    let snapshot = room_with_agent_panes(Vec::new(), vec![feature, main]);

    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "two branches under one path split into two groups"
    );
    for group in &snapshot.worktree_groups {
        assert_eq!(group.rows.len(), 1);
        assert_eq!(
            group.rows[0].worktree_branch.as_deref(),
            Some(group.label.as_str()),
            "each group's label matches its branch"
        );
    }
}

#[test]
fn one_branch_path_keeps_agent_and_shell_in_one_group() {
    // The common case must not fragment: a process/shell row carries no
    // branch, so it stays with the single-branch agent in its worktree.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .branch("main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "agent and its shell share one worktree group: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "main");
    let rows = &snapshot.worktree_groups[0].rows;
    assert!(rows.iter().any(|row| row.is_agent()));
    assert!(rows.iter().any(|row| row.is_process() && row.name == "zsh"));
}

#[test]
fn is_within_compares_path_components() {
    let root = Path::new("/home/marvin");
    assert!(is_within(root, root));
    assert!(is_within(root, Path::new("/home/marvin/")));
    assert!(is_within(root, Path::new("/home/marvin/sub/dir")));
    // A shared string prefix that is not a component boundary is outside.
    assert!(!is_within(root, Path::new("/home/marvinX")));
    assert!(!is_within(root, Path::new("/home/other")));
    assert!(!is_within(root, Path::new("/")));
}

#[test]
fn out_of_project_process_folds_into_external_catch_all() {
    let root = "/home/marvin/workspace/project-rimz/rimz";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.key, "external");
    assert_eq!(group.label, "external");
    assert_eq!(group.rows[0].name, "zsh");
}

#[test]
fn in_project_worktree_pane_keeps_its_own_group() {
    let root = "/repo/rimz";
    let worktree = "/repo/rimz/.claude/worktrees/featureX";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", worktree)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, worktree);
    assert_eq!(group.label, "featureX");
}

#[test]
fn main_checkout_pane_is_in_project() {
    let root = "/repo/rimz";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root)], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.label, "rimz");
}

#[test]
fn component_boundary_pane_is_external() {
    // cwd shares a string prefix with the root but not a component boundary.
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from("/home/marvin")))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvinX/repo")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.label, "external");
}

#[test]
fn external_worktree_pane_gets_its_own_pod() {
    // A worktree parked outside the project root — captured by `git worktree
    // list` — is project-related and earns its own pod, not the `external`
    // catch-all the `project_root` prefix test alone would give it.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", external)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, external);
    assert_eq!(group.label, "feature-wt");
}

#[test]
fn external_worktree_subdir_stays_with_its_worktree() {
    // A cwd nested under an external worktree root is still that worktree's,
    // never `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/elsewhere/feature-wt/src")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
}

#[test]
fn non_worktree_path_is_the_only_external() {
    // With the worktree set known, a cwd that is neither under the project
    // root nor inside any worktree (a home shell) is all that's left as
    // `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.label, "external");
}

#[test]
fn no_project_root_preserves_per_path_grouping() {
    // With no known root, an outside cwd still gets its own worktree group —
    // the prior behavior, preserved as the safe default.
    let snapshot =
        room(Vec::new(), Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, "/home/marvin");
    assert_eq!(group.label, "marvin");
}

#[test]
fn worktree_subdir_panes_share_the_worktree_pod() {
    // Root-keying: every pane under one enumerated checkout folds into that
    // checkout's pod, so a shell in `feature-wt/src` sits with its worktree
    // instead of minting a `src` pod of its own.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(
            vec![
                pane("%1", "claude", external),
                pane("%2", "zsh", "/elsewhere/feature-wt/src"),
            ],
            None,
        );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, external);
    assert_eq!(group.rows.len(), 2);
}

// ── Fleet rooms: directory/marker roots, child-repo pods, the root pod ───────

#[test]
fn directory_room_groups_panes_by_child_repo() {
    // A directory room (`/srv/agents` holding repos): each enumerated child
    // repo is a group root, so every pane under one child shares one pod keyed
    // on the child's root; panes at the room root take the name-only `Root`
    // pod; a cwd outside the room stays external.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(
            vec![
                pane("%1", "claude", "/srv/agents/query-engine"),
                pane("%2", "zsh", "/srv/agents/query-engine/src"),
                pane("%3", "codex", "/srv/agents/billing"),
                pane("%4", "zsh", "/srv/agents"),
                pane("%5", "zsh", "/tmp/elsewhere"),
            ],
            None,
        );

    let summary: Vec<(SidebarWorktreeKind, &str, &str, usize)> = snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            (
                group.kind,
                group.key.as_str(),
                group.label.as_str(),
                group.rows.len(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (SidebarWorktreeKind::Root, "/srv/agents", "agents", 1),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/billing",
                "billing",
                1
            ),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/query-engine",
                "query-engine",
                2
            ),
            (SidebarWorktreeKind::External, "external", "external", 1),
        ],
    );
}

#[test]
fn depth_two_repo_folds_into_the_root_pod() {
    // The v1 depth rule: enumeration mints pods for depth-1 children only, so
    // a deeper repo's panes belong to the room's root pod.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![PathBuf::from("/srv/agents/billing")])
        .with_live_panes(vec![pane("%1", "zsh", "/srv/agents/org/repo")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.key, "/srv/agents");
}

#[test]
fn scratch_room_is_one_root_pod() {
    // The degenerate fleet room — a marker-less scratch dir: zero child
    // repos, one flat name-only pod.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/tmp/scratch")))
        .with_live_panes(
            vec![
                pane("%1", "claude", "/tmp/scratch"),
                pane("%2", "zsh", "/tmp/scratch/logs"),
            ],
            None,
        );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.label, "scratch");
    assert_eq!(group.rows.len(), 2);
}

#[test]
fn marker_room_root_pod_reads_like_a_directory_room() {
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Marker)
        .with_project_root(Some(PathBuf::from("/srv/app")))
        .with_live_panes(vec![pane("%1", "zsh", "/srv/app/src")], None);

    assert_eq!(snapshot.worktree_groups[0].kind, SidebarWorktreeKind::Root);
    assert_eq!(snapshot.worktree_groups[0].label, "app");
}

#[test]
fn stale_branch_row_never_relabels_the_root_pod() {
    // A row claiming a branch at a non-repo room root is stale by definition
    // (the root has no git story); the pod keeps its directory name.
    let live = pane("%scratch", "rimz-ask", "/tmp/scratch");
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/tmp/scratch".to_owned());
    item.worktree_branch = Some("main".to_owned());
    item.pane = Some(live.clone());

    let snapshot = room(vec![item], Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/tmp/scratch")))
        .with_live_panes(vec![live], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.label, "scratch");
}

#[test]
fn fleet_room_tiering_floats_the_attention_child_repo() {
    // Directory-room ordering rides the same tier ladder: the waiting child
    // repo leads, calm pods follow by label, external tails.
    let q_pane = pane("%q1", "claude", "/srv/agents/query-engine");
    let r_pane = pane("%r1", "claude", "/srv/agents");
    let b_pane = pane("%b1", "claude", "/srv/agents/billing");
    let e_pane = pane("%e1", "claude", "/tmp/outside");
    let mut q1 = agent_in(
        "q1",
        "/srv/agents/query-engine",
        AgentStatus::Waiting,
        1_000,
    );
    q1.pane = Some(q_pane.clone());
    let mut r1 = agent_in("r1", "/srv/agents", AgentStatus::Idle, 1_000);
    r1.pane = Some(r_pane.clone());
    let mut b1 = agent_in("b1", "/srv/agents/billing", AgentStatus::Idle, 1_000);
    b1.pane = Some(b_pane.clone());
    let mut e1 = agent("claude", "e1", AgentStatus::Idle, 1_000);
    e1.pane = Some(e_pane.clone());

    let snapshot = room(Vec::new(), vec![q1, r1, b1, e1])
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(vec![q_pane, r_pane, b_pane, e_pane], None);

    let labels: Vec<&str> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["query-engine", "agents", "billing", "external"]
    );
}

#[test]
fn group_tiering_floats_attention_and_tails_external() {
    let labels_for = |mut agents: Vec<AgentState>| {
        let mut panes = Vec::new();
        for (idx, agent) in agents.iter_mut().enumerate() {
            let raw = format!("%tier-{idx}");
            let mut live = pane(
                &raw,
                agent.kind.as_str(),
                agent.worktree_path.as_deref().unwrap_or("/repo/main"),
            );
            if agent.worktree_path.is_none() {
                live.cwd = None;
            }
            agent.pane = Some(live.clone());
            panes.push(live);
        }

        room(Vec::new(), agents)
            .with_live_panes(panes, None)
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };
    let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

    // A calm external sinks below calm project worktrees; an attention
    // worktree leads regardless of its name.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
            agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Idle),
        ]),
        vec!["alpha", "beta", "external"]
    );

    // The external catch-all rises out of the tail only when it holds an
    // attention agent (waiting or failed).
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["external", "beta"]
    );
}

// ── Remote-control host filtering ────────────────────────────────────────────

#[test]
fn remote_control_host_pane_renders_no_row() {
    // A `claude remote-control` pane (Zellij reports the full command line)
    // is ambient infrastructure: it no longer renders as any row — its
    // presence surfaces as the provider dashboard's `⇅ rc` flag instead.
    // Only the shell pane beside it remains a row.
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(
        vec![
            pane("%1", "zsh", "/repo/main"),
            pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the shell pane is a row: {rows:?}");
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert!(
        rows.iter().all(|row| row.name != "claude"),
        "the host pane must not produce a claude row: {rows:?}",
    );
}

#[test]
fn remote_control_host_pane_filtered_when_detected_by_view_name() {
    // tmux reports only the `claude` basename, but names the window — so the
    // view name marks the host, and that pane is filtered out the same way.
    let mut rc_pane = pane("%2", "claude", "/repo/main");
    rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![rc_pane], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a host-only pane set produces no rows: {rows:?}",
    );
}

// ── Subagent nesting, retention, and enrichment ──────────────────────────────

#[test]
fn sub_agent_nests_under_parent_and_never_top_level() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    // Only the parent built a row; the paneless child attaches onto it.
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(rows.len(), 1, "the child is never its own top-level row");
    assert_eq!(rows[0].sub_agents().len(), 1);
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
}

#[test]
fn orphan_sub_agent_is_dropped() {
    let child = child_state("missing-parent", "child-1", AgentStatus::Running, 5);
    let mut rows: Vec<SidebarRow> = Vec::new();
    attach_sub_agents(&mut rows, &[child], epoch());
    assert!(rows.is_empty(), "a child with no parent row never renders");
}

// ── Child activity folds onto the parent's displayed clock ───────────────────

#[test]
fn child_activity_advances_parent_displayed_clock() {
    // A delegating parent is quiet because the work is its children's: the
    // freshest child activity becomes the row's displayed `last_activity`,
    // while the rollup state keeps the parent's own clock.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100).active_ago(540);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    assert_eq!(row(&snapshot, "sess-root").last_activity, ago(5));
    let rollup = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "sess-root")
        .expect("parent in rollup");
    assert_eq!(
        rollup.last_activity,
        ago(540),
        "the fold is display-only; the rollup keeps the parent's own clock"
    );
}

#[test]
fn recently_finished_child_holds_off_the_stall() {
    // The fold runs before the displayed-status projection, so the stall
    // check reads the folded clock: a parent silent past the stall window
    // whose child finished four minutes ago is alive, not wedged.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100).active_ago(660);
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 240);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let row = row(&snapshot, "sess-root");
    assert_eq!(row.status(), Some(AgentStatus::Running), "not a stall");
    assert_eq!(row.last_activity, ago(240));
}

#[test]
fn waiting_parent_keeps_its_ask_clock() {
    // A `waiting` row's age measures how long the ask has needed a human, so
    // child activity never re-clocks it.
    let parent = agent("claude", "sess-root", AgentStatus::Waiting, 100).active_ago(120);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    assert_eq!(row(&snapshot, "sess-root").last_activity, ago(120));
}

#[test]
fn turn_dead_parent_keeps_the_death_certificate() {
    // A turn that died on a provider API error keeps its own clock: the
    // marker postdates the parent's activity, so the fold abstains and the
    // finished child's fresher activity can never mask the escalation.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100)
        .active_ago(120)
        .turn_error(60, "API Error: Overloaded");
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let row = row(&snapshot, "sess-root");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "the turn death holds"
    );
    assert_eq!(row.last_activity, ago(120), "the fold abstained");
    assert_eq!(row.turn_error_label(), Some("API Error: Overloaded"));
}

#[test]
fn with_subagent_context_folds_onto_child_by_key() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let started = ago(100);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("locate the render seam".to_owned()),
            token_count: Some(12_400),
            started_at: Some(started),
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let child = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        child.subagent_description.as_deref(),
        Some("locate the render seam")
    );
    assert_eq!(child.total_tokens, Some(12_400));
    assert_eq!(child.subagent_started_at, Some(started));

    // A record whose child is absent from the rollup is dropped — the key it
    // is filed under is authority.
    let absent = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "ghost".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("nowhere".to_owned()),
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = folded.with_subagent_context(vec![absent]);
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

#[test]
fn with_subagent_context_back_fills_task_from_agent_type() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // A fork child: parent_agent_id set, task None (no agent_type in SubagentStart).
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let snapshot = room(Vec::new(), vec![parent, fork]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "fork-1".into(),
        context: SubagentContext {
            agent_type: Some("Explore".to_owned()),
            description: Some("search the ledger".to_owned()),
            token_count: Some(5_000),
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let fork = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "fork-1")
        .expect("fork in rollup");
    assert_eq!(
        fork.task.as_deref(),
        Some("Explore"),
        "agent_type back-fills task"
    );
    assert_eq!(
        fork.subagent_description.as_deref(),
        Some("search the ledger")
    );
}

#[test]
fn with_subagent_context_does_not_overwrite_existing_task() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // Typed child: task already set by SubagentStart.
    let mut typed = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    let snapshot = room(Vec::new(), vec![parent, typed]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: Some("SomethingElse".to_owned()),
            description: None,
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let typed = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        typed.task.as_deref(),
        Some("review"),
        "lifecycle-established task must not be overwritten by enrichment",
    );
}

#[test]
fn sub_agent_projection_carries_enrichment_and_freezes_finished_elapsed() {
    let now = epoch();
    let started = ago(100);

    // Running: elapsed counts to `now` (100s), enrichment projects through.
    let mut running = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    running.phase = TurnPhase::Reasoning;
    running.subagent_description = Some("locate the render seam".to_owned());
    running.subagent_started_at = Some(started);
    running.total_tokens = Some(12_400);
    running.model = Some("claude-opus-4-8".to_owned());
    running.effort = Some("high".to_owned());
    let sub = sub_agent_from_state(&running, now);
    assert_eq!(sub.phase, TurnPhase::Reasoning);
    assert_eq!(sub.description.as_deref(), Some("locate the render seam"));
    assert_eq!(sub.total_tokens, Some(12_400));
    assert_eq!(sub.elapsed_secs, Some(100));
    assert_eq!(sub.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(sub.effort.as_deref(), Some("high"));

    // Finished: elapsed freezes at `last_activity` (40s after start), never `now`.
    let mut finished = child_state("sess-root", "child-2", AgentStatus::Success, 0);
    finished.last_activity = ago(60);
    finished.subagent_started_at = Some(started);
    let sub = sub_agent_from_state(&finished, now);
    assert_eq!(sub.elapsed_secs, Some(40));

    // A child with no enrichment (Codex, or pre-first-render) degrades cleanly.
    let bare = child_state("sess-root", "child-3", AgentStatus::Running, 5);
    let sub = sub_agent_from_state(&bare, now);
    assert_eq!(sub.phase, TurnPhase::Idle);
    assert_eq!(sub.description, None);
    assert_eq!(sub.total_tokens, None);
    assert_eq!(sub.elapsed_secs, None);
    assert_eq!(sub.model, None);
    assert_eq!(sub.effort, None);
}

#[test]
fn finished_sub_agent_drops_once_parent_starts_next_turn() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The current turn began AFTER the child finished — a past-turn child.
    parent.turn_started_at = Some(ago(30));
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(rows[0].sub_agents().is_empty());
}

#[test]
fn running_sub_agent_of_current_turn_is_kept() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child's activity — live work of this turn.
    parent.turn_started_at = Some(ago(90));
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 30);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a live child of the current turn is kept"
    );
}

#[test]
fn superseded_running_sub_agent_is_reaped_as_ghost() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The parent moved to a newer turn than the child's last activity: the
    // child never sent `SubagentStop` and is a leftover ghost — reaped so it
    // can't freeze the parent's delegated-wait head.
    parent.turn_started_at = Some(ago(30));
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a running child from a past turn is a ghost"
    );
}

#[test]
fn finished_sub_agent_of_current_turn_is_kept() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child finished — same-turn, so it stays.
    parent.turn_started_at = Some(ago(90));
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 30);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(rows[0].sub_agents().len(), 1);
}

#[test]
fn sub_agents_sort_by_creation_time_ascending() {
    // Spawn order, not activity, keys the list: the child that started
    // first leads however fresh its siblings' activity is, so the list
    // holds still across refreshes. A child with no reported start time
    // sorts after the dated ones, by id.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The youngest-started child is the most recently active — an
    // activity-keyed sort would lead with it; creation order must not.
    let mut first = child_state("sess-root", "c-late-id", AgentStatus::Idle, 40);
    first.subagent_started_at = Some(ago(90));
    let mut second = child_state("sess-root", "c-early-id", AgentStatus::Running, 2);
    second.subagent_started_at = Some(ago(60));
    let undated = child_state("sess-root", "c-undated", AgentStatus::Running, 1);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(
        &mut rows,
        &[parent.clone(), undated, second, first],
        epoch(),
    );
    let ids: Vec<&str> = rows[0].sub_agents().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["c-late-id", "c-early-id", "c-undated"]);
}

#[test]
fn duplicate_children_collapse_to_one_row() {
    // Two reduced states aliasing the same child id must render as one row,
    // so `subagents (N)` never double-counts. Freshest activity wins.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let stale = child_state("sess-root", "child-dup", AgentStatus::Running, 50);
    let fresh = child_state("sess-root", "child-dup", AgentStatus::Running, 5);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), stale, fresh], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the same child can't appear twice"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-dup");
}

#[test]
fn typeless_child_renders_degraded_label_never_the_kind() {
    // A child with no type label must not borrow the provider kind, which
    // would render as a phantom `claude` row. This is the "3 Explore + 3
    // claude" regression.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.task = None;
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    let name = &rows[0].sub_agents()[0].name;
    assert!(name.starts_with("subagent"), "got {name}");
    assert_ne!(name, "claude");
}

#[test]
fn running_child_past_ghost_ttl_is_reaped() {
    // A running child that never sent `SubagentStop` and has been silent past
    // the generous ghost TTL is a leftover — reaped so it can't freeze the
    // parent's delegated-wait head, even with no fresh turn boundary.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Running,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a running child silent past the ghost TTL is reaped"
    );
}

#[test]
fn finished_child_is_kept_however_long_ago_it_finished() {
    // The regression: a finished child used to clear on a 5-minute TTL even
    // mid-turn, vanishing from the list while its siblings still ran. With
    // retention purely turn-scoped, a finished child stays — however stale —
    // until the parent's next turn supersedes it.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 60 * 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a finished child never clears on age, only on the next turn"
    );
}

#[test]
fn newer_subagent_does_not_expire_parent_attention() {
    // A child shares the parent's pane and worktree, so it can be newer than
    // the parent without superseding the parent's human decision surface.
    let item = agent_ask(FeedKind::Permission, "claude", "parent-claude");

    let parent = agent("claude", "parent-claude", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let mut child = agent("claude", "child-claude", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");
    child.parent_agent_id = Some("parent-claude".into());

    let snapshot = room(vec![item.clone()], vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(
        snapshot.needs_attention[0].request_id, item.request_id,
        "the child must not make the parent's ask stale"
    );
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.id, "parent-claude");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id().cloned(), Some(item.request_id));
}

// ── Pane binding: stamped ids, live overlays, one pane = one row ─────────────

#[test]
fn live_panes_add_process_rows_without_attention_counts() {
    let snapshot =
        room(Vec::new(), Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_process());
    assert_eq!(row.name, "zsh");
    assert_eq!(row.status(), None);
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

fn script_ask_for_pane(pane: Option<PaneRef>) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = pane;
    item
}

#[test]
fn standalone_script_ask_renders_only_from_matching_frame_pane() {
    let stale_pane = PaneRef {
        view_id: Some("@stale".to_owned()),
        command: Some("old-deploy".to_owned()),
        cwd: Some("/old".to_owned()),
        ..pane("%7", "old-deploy", "/old")
    };
    let mut frame_pane = pane("%7", "deploy", "/repo/main");
    frame_pane.view_id = Some("@fresh".to_owned());
    frame_pane.is_focused = true;
    let item = script_ask_for_pane(Some(stale_pane));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new()).with_live_panes(vec![frame_pane.clone()], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the ask owns the pane row slot");
    let row = rows[0];
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(row.task(), Some("approve deploy?"));
    assert_eq!(row.worktree_path.as_deref(), Some("/repo/main"));
    assert_eq!(row.pane.as_ref(), Some(&frame_pane));
}

#[test]
fn standalone_script_ask_without_pane_does_not_render() {
    let item = script_ask_for_pane(None);
    let request_id = item.request_id.clone();

    let snapshot =
        room(vec![item], Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(
        rows.len(),
        1,
        "the live shell still renders as a process row"
    );
    assert!(
        rows.iter().all(|row| row.request_id() != Some(&request_id)),
        "the unframed ask remains metadata only"
    );
}

#[test]
fn standalone_script_ask_for_absent_pane_does_not_render() {
    let item = script_ask_for_pane(Some(pane("%7", "deploy", "/repo/main")));
    let request_id = item.request_id.clone();

    let snapshot =
        room(vec![item], Vec::new()).with_live_panes(vec![pane("%8", "zsh", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(
        rows.iter().all(|row| row.request_id() != Some(&request_id)),
        "an ask for a pane absent from the frame is not jumpable"
    );
}

#[test]
fn standalone_script_ask_for_reused_pane_id_does_not_render() {
    let old_start = ago(60);
    let fresh_start = ago(5);
    let item = script_ask_for_pane(Some(pane_started("%7", "/repo/main", old_start)));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new())
        .with_live_panes(vec![pane_started("%7", "/repo/main", fresh_start)], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(
        rows.iter().all(|row| row.request_id() != Some(&request_id)),
        "a reused pane id with a different process start must not route the ask"
    );
}

#[test]
fn standalone_ask_on_an_agents_pane_folds_onto_the_agent_row() {
    // A script ask raised from inside an agent's pane (the agent shelling out
    // to `rimz feed ask`) folds onto the agent's row — identity and capability
    // line kept, the ask's waiting status and request taken — and outranks the
    // session's own pending agent-hook ask: the script blocks the pane's
    // foreground right now.
    let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    claude.model = Some("opus-4".to_owned());
    let script_ask = script_ask_for_pane(Some(pane("%1", "claude", "/repo/main")));
    let request_id = script_ask.request_id.clone();
    let items = vec![
        agent_ask(FeedKind::Permission, "claude", "sess-a"),
        script_ask,
    ];

    let snapshot =
        room(items, vec![claude]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(row.id, "sess-a", "the agent keeps the row identity");
    assert_eq!(row.name, "claude");
    assert_eq!(
        row.model(),
        Some("opus-4"),
        "the capability line survives the fold"
    );
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(
        row.request_id(),
        Some(&request_id),
        "the pane-blocking script ask outranks the agent-hook ask"
    );
    assert_eq!(row.surface(), Some(Surface::Script));
}

#[test]
fn standalone_bridge_ask_renders_its_resolver_from_the_frame() {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = Some(pane("%7", "deploy", "/repo/main"));
    item.chain_active_resolver = Some(crate::ids::ResolverId::new_unchecked("auto-approver"));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new())
        .with_live_panes(vec![pane("%7", "deploy", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the bridge ask owns the pane row slot");
    let row = rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(
        row.resolver()
            .as_ref()
            .map(|resolver| resolver.resolver_id.as_str()),
        Some("auto-approver"),
        "a frame-admitted bridge ask carries its active resolver"
    );
}

#[test]
fn standalone_ask_on_a_wired_idle_lazy_pane_folds_onto_the_idle_row() {
    let item = script_ask_for_pane(Some(pane("term1", "codex", "/repo/main")));
    let request_id = item.request_id.clone();
    let mut snapshot = room(vec![item], Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];

    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(
        row.name, "codex",
        "the idle lazy identity survives the fold"
    );
    assert_eq!(row.id, "tmux:term1");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id(), Some(&request_id));
}

#[test]
fn commandless_unbound_pane_folds_no_row() {
    // A pane whose command is still unknown after frame rotation — mid-birth,
    // or a raced first read — is presence without identity: it folds no row
    // rather than an anonymous `process` under `external`.
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a command-less pane renders no row: {rows:?}"
    );
}

#[test]
fn commandless_pane_with_agent_still_renders_agent_row() {
    // Agent rows bind by stamped pane id, never by command, so a raced read
    // that drops the command never demotes or hides the agent's row.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let raced = PaneRef {
        command: None,
        ..pane("%1", "claude", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the stamped agent row survives: {rows:?}");
    assert!(rows[0].is_agent());
}

#[test]
fn commandless_pane_does_not_form_empty_external_group() {
    // The raced read that drops a command usually drops the cwd too; the
    // filtered pane must not mint a stray `external` header on its way out.
    let root = "/repo/rimz";
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%2", "x", "")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root), raced], None);

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "no external group for the filtered pane: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "rimz");
}

#[test]
fn commandless_pane_keeps_known_process_rows() {
    // The guard is per-pane: a sibling whose command read succeeded keeps
    // its named process row.
    let raced = PaneRef {
        command: None,
        ..pane("%2", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main"), raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "only the named pane is a row: {rows:?}");
    assert_eq!(rows[0].name, "zsh");
}

#[test]
fn live_panes_overlay_matching_agent_rows() {
    let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .branch("main")
        .in_pane("%1");
    let snapshot = room(Vec::new(), vec![codex])
        .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(snapshot.worktree_groups[0].rows.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn stamped_codex_returned_to_shell_renders_process_row() {
    // Codex records lifecycle through the shared app-server daemon, so the
    // session can remain live after the in-pane CLI exits. When the same pane id
    // now reports a shell foreground, the old Codex card must not stay attached.
    let codex = agent("codex", "sess-1", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let snapshot =
        room(Vec::new(), vec![codex]).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_panes_do_not_render_unmatched_ledger_agents() {
    let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");

    let snapshot =
        room(Vec::new(), vec![codex]).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| !row.is_agent()),
        "non-attention agent rows must come from live pane presence"
    );
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.is_process() && row.name == "zsh"),
        "the live shell pane remains a process row"
    );
}

#[test]
fn live_panes_suppress_stale_agent_attention_without_process() {
    let item = agent_ask(FeedKind::Permission, "claude", "stale-claude");

    let snapshot = room(vec![item], Vec::new()).with_live_panes(
        vec![
            pane("%0", "rimz-sidebar", "/repo/main"),
            pane("%1", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| row.is_process() && row.name == "zsh"),
        "a stale agent prompt must not claim the sidebar pane or outlive its agent process: {:?}",
        snapshot.worktree_groups[0].rows,
    );
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

#[test]
fn live_panes_keep_agent_attention_with_process() {
    let item = agent_ask(FeedKind::Permission, "claude", "live-claude");
    // The ask's session is live in the rollup, so it binds to that
    // session's pane and renders as attention.
    let session = agent("claude", "live-claude", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");

    // The pane runs under a `node` wrapper, not a `claude` foreground — the
    // bind is by the session's stamped pane id, so the command is moot.
    let snapshot = room(vec![item], vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.name, "claude");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn answered_native_ui_ask_returns_to_running() {
    // The live bug: a native_ui ask is answered in the agent's own UI and
    // the agent keeps working the same turn. The ask stays pending in the
    // ledger, but the activity heartbeat has advanced `last_activity` past
    // the ask, so the row must read `running`, not stay folded to `waiting`.
    let mut item = agent_ask(FeedKind::Question, "claude", "live-claude");
    // Ask raised long before the agent's recent activity.
    item.updated_at = ago(600);

    // The agent recorded progress after the ask — it has un-blocked and
    // moved on.
    let session = agent("claude", "live-claude", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![item], vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "an answered ask the agent moved past must not pin the row to waiting"
    );
}

#[test]
fn answered_native_ui_ask_without_panes_stays_metadata_only() {
    // With no live frame, the rollup carries the pending ask as metadata but
    // emits no row. The pane-backed path above owns the moved-past display
    // recovery.
    let mut item = agent_ask(FeedKind::Question, "claude", "live-claude");
    item.updated_at = ago(600);
    let session =
        agent("claude", "live-claude", AgentStatus::Running, 2_000).worktree("/repo/main");

    let snapshot = room(vec![item], vec![session]);

    assert_eq!(snapshot.needs_attention.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn two_same_kind_agents_bind_to_their_stamped_panes() {
    // Two claude sessions in one worktree are indistinguishable by name and
    // cwd alone; binding is by the hook-stamped pane id, so each session
    // lands on exactly its own pane instead of cross-wiring the rows.
    let older = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let newer = agent("claude", "sess-b", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%2");

    let snapshot = room(Vec::new(), vec![older, newer]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(
        row(&snapshot, "sess-a")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
    assert_eq!(
        row(&snapshot, "sess-b")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%2"
    );
}

#[test]
fn agent_binds_only_by_stamped_pane_id() {
    // The pane-keyed invariant: an agent stamped `%2`, but only `%1` is
    // live. `%1`'s command and cwd both match the agent — under the old
    // command/cwd fallback it would have bound. Stamped-id binding refuses
    // it, so `%1` stays a process row and the agent simply does not render.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%2");

    let snapshot = room(Vec::new(), vec![claude])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn subagent_never_steals_its_parents_pane() {
    // A subagent runs in its parent's pane, so its lifecycle hooks stamp the
    // parent's pane id — parent and child both claim `%1`. The child here is
    // strictly more recently active than the parked parent, which would let
    // `max_by_key(last_activity)` bind the pane to the child. Panes bind root
    // agents only: `%1` stays the parent's row and the child nests under it.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    // Newer activity than the parent (5s ago vs ~99s ago) — the flip trigger.
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "one pane binds exactly one top-level row");
    assert_eq!(
        rows[0].id, "sess-root",
        "the pane binds the root, not the child"
    );
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the child nests under the parent"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
}

#[test]
fn each_live_pane_yields_exactly_one_row() {
    // One pane = one row, by construction: every live pane produces exactly
    // one row — agent or process — and no pane id is ever duplicated.
    let stamped = |id, raw| {
        agent("claude", id, AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane(raw)
    };

    let snapshot = room(
        Vec::new(),
        vec![stamped("sess-a", "%1"), stamped("sess-b", "%2")],
    )
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
            pane("%3", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 3, "three panes render three rows: {rows:?}");
    let mut pane_ids: Vec<&str> = rows
        .iter()
        .map(|row| row.pane.as_ref().unwrap().pane_id.raw())
        .collect();
    pane_ids.sort_unstable();
    assert_eq!(pane_ids, vec!["%1", "%2", "%3"], "no pane id is duplicated");
    let agents = rows.iter().filter(|row| row.is_agent()).count();
    assert_eq!(agents, 2, "the two stamped panes bound their agents");
}

#[test]
fn live_agent_and_process_rows_are_pane_backed() {
    // In a live-pane fold, every visible top-level row is jumpable: agent
    // rows and process rows both carry a pane. A subagent that shares its
    // parent's pane nests in the parent card instead of becoming a second
    // top-level row with the same pane.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![parent, child]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 2, "root agent + process pane render two rows");
    assert!(
        rows.iter().all(|row| row.pane.is_some()),
        "every visible live-pane row has a pane: {rows:?}",
    );
    assert!(
        rows.iter().all(|row| row.id != "child-1"),
        "the subagent is not a top-level row",
    );
    let parent = rows
        .iter()
        .find(|row| row.id == "sess-root")
        .expect("parent row present");
    assert_eq!(parent.sub_agents().len(), 1);
    assert_eq!(parent.sub_agents()[0].id, "child-1");
}

// ── The lazy-codex bind: cwd fallback and idle synthesis ─────────────────────

fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
    // The app-server daemon fires the hook with no mux pane env, so the
    // agent carries its worktree but never stamps a pane.
    agent("codex", id, AgentStatus::Running, rank).worktree(worktree)
}

#[test]
fn paneless_codex_agent_binds_to_its_worktree_pane() {
    // The daemon exception: a Codex agent the app-server daemon registered
    // has no stamped pane, but its worktree matches the live `codex` pane's
    // cwd, so the cwd fallback binds it as an agent row — not a process row.
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_agent_in_other_worktree_stays_a_process_row() {
    // The cwd fallback never crosses worktrees: a pane-less Codex agent in a
    // different worktree leaves the live `codex` pane a process row.
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/other", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_agent_does_not_capture_a_nested_worktree_pane() {
    // Worktree match is exact, not containment: a session checked out at the
    // parent `/repo` must not capture a `codex` pane running in a nested
    // worktree under it (this repo nests worktrees under `.claude/`).
    let snapshot = room(Vec::new(), vec![paneless_codex("sess-1", "/repo", 1_000)])
        .with_live_panes(vec![pane("term1", "codex", "/repo/sub")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_does_not_bind_a_non_codex_pane() {
    // The pane's own command gates the fallback: a shell the session dropped
    // back to in the worktree stays a process row, never an agent.
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "zsh", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
}

#[test]
fn paneless_claude_agent_is_never_rescued_by_cwd() {
    // Only Codex is daemon-backed and pane-less by construction. A pane-less
    // Claude agent is genuinely gone (Claude always stamps a live pane), so
    // the fallback must leave a matching `claude` pane a process row.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    let snapshot = room(Vec::new(), vec![claude])
        .with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
}

#[test]
fn two_paneless_codex_in_one_worktree_bind_most_recent() {
    // When two pane-less Codex sessions claim one worktree — a lingering
    // closed session and a live one — the most-recently-active binds the
    // single live pane; the stale session does not render.
    let snapshot = room(
        Vec::new(),
        vec![
            paneless_codex("sess-old", "/repo/main", 1_000),
            paneless_codex("sess-new", "/repo/main", 2_000),
        ],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-new");
}

#[test]
fn paneless_codex_and_new_stamped_codex_share_one_worktree_without_idle_row() {
    // Daemon-routed Codex can first bind one session by cwd, then recover a
    // newer session's focused pane at hook ingestion. The older paneless
    // session must survive long enough to bind the other same-cwd pane.
    let newer = paneless_codex("sess-new", "/repo/main", 2_000).in_pane("%2");
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-old", "/repo/main", 1_000), newer],
    )
    .with_live_panes(
        vec![
            pane("%1", "codex", "/repo/main"),
            pane("%2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.is_agent()));
    let old = rows
        .iter()
        .find(|row| row.id == "sess-old")
        .expect("older session renders");
    let new = rows
        .iter()
        .find(|row| row.id == "sess-new")
        .expect("newer session renders");
    assert_eq!(old.pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(new.pane.as_ref().unwrap().pane_id.raw(), "%2");
}

#[test]
fn paneless_codex_predating_pane_start_does_not_bind() {
    // The defensive guard on the cwd fallback: when the backend reports the
    // pane's process start, a pane-less Codex session whose last activity
    // predates it belongs to an older instance that once ran in this worktree,
    // not the process now in the pane. A daemon-mode session records the shared
    // daemon pid, so process liveness can't tell the stale one from the live
    // one — so the bind is refused and the fresh pane stays a process row until
    // its own session reports.
    let stale = paneless_codex("sess-old", "/repo/main", 1_000).active_ago(60);
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![stale]).with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].is_process(),
        "a session predating the pane start must not bind it",
    );
}

#[test]
fn paneless_codex_active_after_pane_start_binds() {
    // The guard never over-blocks: a session whose last activity is at or after
    // the pane's process start is the live occupant and binds normally.
    let live = paneless_codex("sess-1", "/repo/main", 1_000).active_ago(-5);
    let started_pane = PaneRef {
        pane_process_start: Some(epoch()),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![live]).with_live_panes(vec![started_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn fresh_codex_pane_with_proc_start_shows_idle_not_ghost() {
    // The ghost-stats regression. A completed daemon-mode Codex session lingers
    // in the rollup — its owner is the shared, still-alive app-server daemon, so
    // process liveness can never reap it, and the daemon still holds the thread
    // loaded so the loaded-set reap keeps it too. A fresh `codex` then starts in
    // the same worktree. On Zellij the backend reports no pane process start, so
    // the producer stamps the in-pane CLI's `/proc` start; fed that, the guard
    // refuses the stale session and the wired pane renders the synthesized idle
    // row (`○ codex`) — not yesterday's `success` stats — until its own first
    // turn binds a new session.
    let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000).active_ago(12 * 60 * 60);
    ghost.status = AgentStatus::Success;
    ghost.total_tokens = Some(126_621);
    ghost.model = Some("gpt-5.5".to_owned());
    let mut snapshot = room(Vec::new(), vec![ghost]);
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    // The synthesized idle row keys on the pane id, never the stale session, and
    // carries none of its stats.
    assert_eq!(rows[0].id, "tmux:term1");
    assert_ne!(rows[0].id, "sess-old");
    assert_eq!(
        rows[0].total_tokens(),
        None,
        "no ghost tokens on a fresh pane"
    );
    assert_eq!(
        rows[0].model(),
        Some("GPT-5.5"),
        "fresh Codex rows use the provider fallback model, not stale session stats"
    );
    assert_eq!(
        rows[0].context_window(),
        Some(258_000),
        "fresh Codex rows use the provider fallback window, not stale session stats"
    );
}

#[test]
fn wired_unprompted_codex_pane_renders_as_idle_agent() {
    // Codex registers its session lazily — `SessionStart` rides in with the
    // first prompt — so a launched-but-never-prompted `codex` pane has no
    // agent state. When Codex is wired it must read as an idle agent (`○ codex`
    // with its gauge and a cockpit tally), not a bare, dim process row, the
    // moment it opens.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    // No session id exists yet, so the row keys on the pane id (its full
    // mux-qualified form, as `row_from_process` does).
    assert_eq!(rows[0].id, "tmux:term1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    assert_eq!(
        rows[0].model(),
        Some("GPT-5.5"),
        "the card can show Codex's default model before the first session event"
    );
    assert_eq!(
        rows[0].context_window(),
        Some(258_000),
        "the card can show Codex's context tier before the first session event"
    );
}

#[test]
fn wired_unprompted_codex_uses_configured_default_model() {
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    snapshot
        .lazy_agent_default_models
        .insert("codex".to_owned(), "o4-mini".to_owned());
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.model(), Some("o4-mini"));
    assert_eq!(row.context_window(), Some(258_000));
}

#[test]
fn non_lazy_agent_pane_is_never_idle_synthesized() {
    // The idle-instance synthesis is gated on the agent registering lazily
    // (`Capabilities::registers_lazily`), not merely on being wired. Claude
    // stamps a pane on every session, so an unbound `claude` pane stays a
    // process row even when the producer is told claude is a wired lazy kind —
    // the static descriptor gate refuses it. This is what keeps the lifecycle
    // agent-agnostic (a new lazy agent slots in by declaring the capability)
    // without changing how Claude renders.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["claude".to_owned(), "codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
}

#[test]
fn unwired_codex_pane_stays_a_process_row() {
    // The consent invariant: an unwired Codex can report no status, so its
    // live pane stays a process row (agents are invisible until their hooks
    // are wired). `wired_lazy_kinds` left empty reproduces an un-onboarded
    // Codex.
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "codex");
}

#[test]
fn bound_codex_pane_keeps_its_real_agent_over_idle_synthesis() {
    // The idle synthesis is a last resort: a `codex` pane that binds a real
    // (pane-less, cwd-matched) agent keeps that agent's identity and status,
    // never the synthesized idle row — even with Codex wired.
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].id, "sess-1",
        "the real agent binds, not a synthesis"
    );
    assert_eq!(rows[0].status(), Some(AgentStatus::Running));
}

#[test]
fn two_codex_panes_one_agent_yields_one_real_one_idle() {
    // The multi-codex-per-worktree case: one prompted (pane-less) agent plus a
    // second still-unprompted `codex` pane in the same worktree. The agent
    // binds the first codex pane by cwd; the second synthesizes an idle row —
    // no codex pane is ever left as a process row.
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(
        vec![
            pane("term1", "codex", "/repo/main"),
            pane("term2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.is_agent()),
        "neither codex pane is a process row",
    );
    assert!(
        rows.iter().any(|row| row.id == "sess-1"),
        "the prompted session binds one pane",
    );
    assert!(
        rows.iter()
            .any(|row| row.status() == Some(AgentStatus::Idle)),
        "the unprompted pane synthesizes an idle row",
    );
}

#[test]
fn unbound_claude_pane_stays_a_process_row_even_when_codex_wired() {
    // The synthesis is Codex-only: Claude always stamps a live pane, so a
    // `claude` pane with no bound agent is a genuinely-ended session and must
    // read as a process row, never an idle agent — regardless of Codex wiring.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "claude");
}

// ── Stale asks vs live presence ──────────────────────────────────────────────

#[test]
fn stale_session_ask_does_not_render_or_steal_a_pane() {
    // Reproduces the live bug: a pending permission ask whose claude
    // session has ended must not become attention, and must not latch onto
    // a freshly launched codex sharing the worktree.
    let stale = agent_ask(FeedKind::Permission, "claude", "ended-claude");

    // Only a live codex session remains in the rollup.
    let codex = agent("codex", "sess-codex", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![stale], vec![codex])
        .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "stale ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the live codex renders");
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn superseded_zombie_ask_yields_pane_to_the_fresh_session() {
    // Live reproduction: a pidless `SessionStart`-only claude never ends and
    // never gets reaped, so it lingers in the rollup with an old pending
    // ask. A freshly launched claude shares the worktree. The ask must not
    // render as attention or pin the dead session's "permission" task and
    // stale timestamp onto the live pane — the fresh session binds it idle.
    let stale = agent_ask(FeedKind::Permission, "claude", "zombie-claude");

    let zombie = agent("claude", "zombie-claude", AgentStatus::Idle, 1_000).worktree("/repo/main");
    // Only the fresh session stamped the live pane; the zombie holds none.
    let fresh = agent("claude", "fresh-claude", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![stale], vec![zombie, fresh])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "the superseded session's ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the fresh session renders");
    assert_eq!(rows[0].id, "fresh-claude");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_codex_command_does_not_corroborate_claude_attention() {
    // Live reproduction: an old Claude ask still has a ledger session, but
    // the only live pane in the worktree is `node /usr/bin/codex`. The
    // pane must remain Codex-shaped instead of inheriting Claude's model
    // and stale ask age.
    let stale = agent_ask(FeedKind::Permission, "claude", "stale-claude");

    let mut claude =
        agent("claude", "stale-claude", AgentStatus::Idle, 1_000).worktree("/repo/main");
    claude.model = Some("claude-opus-4-7".to_owned());

    let snapshot = room(vec![stale], vec![claude])
        .with_live_panes(vec![pane("%1", "node /usr/bin/codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "codex");
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

/// User's reported scenario: ledger carries a pile of stale claude
/// observations from killed sessions (no SessionEnd ever fired), all
/// claiming the same worktree path. A fresh claude pane lands. The fresh
/// agent must still bind to its pane — stale count does not block live
/// presence.
#[test]
fn live_claude_pane_binds_despite_pile_of_stale_ledger_ghosts() {
    let stale =
        |id: &str, rank: i64| agent("claude", id, AgentStatus::Idle, rank).worktree("/repo/main");
    let live = agent("claude", "live", AgentStatus::Running, i64::from(u32::MAX))
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(
        Vec::new(),
        vec![
            stale("stale-a", 1_000),
            stale("stale-b", 1_001),
            stale("stale-c", 1_002),
            live,
        ],
    )
    .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows.iter().filter(|r| r.is_agent()).collect();
    assert_eq!(agent_rows.len(), 1, "only the live claude renders");
    assert_eq!(agent_rows[0].id, "live");
}

// ── The displayed-status ladder: stall, rate-limit park, turn death ──────────

#[test]
fn stalled_running_agent_recovers_when_activity_resumes() {
    // The stall escalation is self-healing: once the agent's next completed
    // tool touches the activity heartbeat, the fold readvances
    // `last_activity`, `is_stalled` goes false, and the row drops back out
    // of attention with no human action.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        // Silent past the stall window.
        .active_ago(crate::feed::STALL_WINDOW_SECS + 60);

    // A fresh heartbeat lands (the agent's next tool completed).
    let touch = AgentActivity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "live-claude".into(),
        at: epoch(),
    };
    let snapshot = room(Vec::new(), vec![session])
        .with_agent_activity(&[touch])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "a fresh heartbeat readvances last_activity, so the stalled row recovers"
    );
}

#[test]
fn stalled_running_agent_escalates_to_attention() {
    // A running agent that records no activity past the stall window is
    // likely wedged; the displayed row escalates to the attention bucket
    // (`!`) and the rollup keeps the true `running` status.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(crate::feed::STALL_WINDOW_SECS + 60);

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a long-silent running agent escalates to the attention bucket"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the stalled agent counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the true running status; only the display row escalates"
    );
}

#[test]
fn spent_account_parks_every_resting_agent_of_the_kind() {
    // Account-scoped: one claude session reports a spent 5-hour window, so
    // the whole kind is rate-limited — including a *fresh* idle session that
    // carries no context of its own yet (the "launched into a spent account"
    // case). A running session with a spent account also parks: the budget is
    // gone regardless of whether a turn is nominally in progress.
    let reporter = agent("claude", "sess-spent", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)]);
    let fresh = agent("claude", "sess-fresh", AgentStatus::Idle, 1_100).worktree("/repo/main");
    let working = agent("claude", "sess-busy", AgentStatus::Running, 1_200).worktree("/repo/main");

    let snapshot = room_with_agent_panes(Vec::new(), vec![reporter, fresh, working]);
    assert_eq!(
        row(&snapshot, "sess-spent").status(),
        Some(AgentStatus::RateLimited)
    );
    assert_eq!(
        row(&snapshot, "sess-fresh").status(),
        Some(AgentStatus::RateLimited),
        "a fresh idle session inherits the account verdict"
    );
    assert_eq!(
        row(&snapshot, "sess-busy").status(),
        Some(AgentStatus::RateLimited),
        "a running session in a spent account parks — the budget is gone regardless"
    );
    // The rollup keeps the true lifecycle status; only the display projects.
    assert_eq!(
        snapshot
            .agents
            .iter()
            .find(|a| a.agent_id == "sess-fresh")
            .unwrap()
            .status,
        AgentStatus::Idle
    );
}

#[test]
fn a_window_spent_but_already_reset_does_not_park() {
    // A spent reading whose reset has passed is stale, not limiting — the
    // budget has refilled, so a resting agent reads idle, not parked.
    let idle = agent("claude", "sess-1", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, -60)]);

    let snapshot = room_with_agent_panes(Vec::new(), vec![idle]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Idle),
        "a passed reset means the budget refilled — not rate-limited"
    );
}

#[test]
fn running_parent_with_a_live_subagent_waits_instead_of_stalling() {
    // A running parent that has delegated to a live child shows no heartbeat
    // of its own, so the stall window would falsely escalate it. The
    // delegated-wait exemption keeps it `running` while a child runs; the
    // renderer paints the waiting-on-subagents head from `sub_agents`.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        // Silent past the stall window — its heartbeat is quiet because the
        // work is the child's, not a wedge.
        .active_ago(crate::feed::STALL_WINDOW_SECS + 60);
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "a parent delegating to a live child is waiting on it, not stalled"
    );
    assert!(
        row.sub_agents()
            .iter()
            .any(|child| child.status == AgentStatus::Running),
        "the live child is nested so the renderer can paint the wait head"
    );
}

#[test]
fn api_error_turn_escalates_running_to_attention() {
    // A turn that died on a provider API error fires no Stop hook, so the
    // rollup keeps `running` — but the transcript marker postdates the
    // agent's own activity, and the projection escalates at once. The
    // headline: the agent is *inside* the stall window (silent only a
    // minute), so this beats the 10-minute backstop.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "the explicit death certificate escalates without waiting out the stall window"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Overloaded"),
        "the row carries the upstream error text for the card's line 2"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the dead turn counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row escalates"
    );
}

#[test]
fn api_error_self_clears_when_activity_resumes() {
    // Any newer hook event (a prompt, a resume, a rewind) advances
    // `last_activity` past the stale marker and the escalation drops with
    // no human action — the self-clear guard.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(30)
        .turn_error(120, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "activity newer than the marker means the session moved on"
    );
    assert!(
        row.turn_error_label().is_none(),
        "a cleared escalation leaves no stale reason label"
    );
}

#[test]
fn api_error_does_not_override_waiting() {
    // A human-blocked ask outranks every derived state, the dead-turn
    // escalation included.
    let session = agent("claude", "live-claude", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert!(row.turn_error_label().is_none());
}

#[test]
fn dead_parent_with_live_child_keeps_running() {
    // The delegated-wait exemption wins: a live child's heartbeats are the
    // parent's work, so a stale parent marker never escalates over it. If
    // the children also die, the stall window remains the backstop.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Overloaded");
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Running));
    assert!(row.turn_error_label().is_none());
}

// ── The precedence ladder, pinned as an ordering ─────────────────────────────
//
// docs/internals/agent.md commits to a strict order among the derived display
// states: the spent-account park decides first, then the live-subagent
// exemption, then the turn-death marker, then the stall backstop — and a
// human-blocked `waiting` outranks them all. The single-cause cases above
// each prove one rung; this grid pins the *order* by stacking causes and
// asserting which one wins, so a refactor that reorders the chain fails here
// even if every single-cause test still passes.

#[test]
fn displayed_status_precedence_ladder_holds() {
    let spent = || vec![window(100, 3_600)];
    let stalled_secs = crate::feed::STALL_WINDOW_SECS + 60;

    struct Rung {
        name: &'static str,
        agent: AgentState,
        with_live_child: bool,
        expect: AgentStatus,
        expect_error_label: bool,
    }
    let rungs = [
        // The top of the ladder: a human-blocked ask outranks every derived
        // state at once — the human is the blocker, not the provider or a
        // wedged turn, so no projection may repaint the row out from under
        // the pending decision.
        Rung {
            name: "waiting outranks park + child exemption + marker + stall",
            agent: agent("claude", "root", AgentStatus::Waiting, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent())
                .turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            expect: AgentStatus::Waiting,
            expect_error_label: false,
        },
        // Every derived cause at once: park wins over exemption, marker, and stall.
        Rung {
            name: "park beats child exemption + marker + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent())
                .turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            expect: AgentStatus::RateLimited,
            expect_error_label: false,
        },
        // Park beats the marker alone (the limit's own corpse carries no label).
        Rung {
            name: "park beats turn-death marker",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .limits(spent())
                .turn_error(10, "You've hit your usage limit"),
            with_live_child: false,
            expect: AgentStatus::RateLimited,
            expect_error_label: false,
        },
        // Park beats the stall backstop alone.
        Rung {
            name: "park beats stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent()),
            with_live_child: false,
            expect: AgentStatus::RateLimited,
            expect_error_label: false,
        },
        // With the budget back, the exemption decides next: a live child
        // holds the row at running over both the marker and the stall.
        Rung {
            name: "child exemption beats marker + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Overloaded"),
            with_live_child: true,
            expect: AgentStatus::Running,
            expect_error_label: false,
        },
        // No park, no child: the explicit marker beats the stall window.
        Rung {
            name: "turn-death marker beats stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Overloaded"),
            with_live_child: false,
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        // Nothing above holds: the stall backstop escalates on its own.
        Rung {
            name: "stall is the backstop",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs),
            with_live_child: false,
            expect: AgentStatus::Failed,
            expect_error_label: false,
        },
    ];

    for rung in rungs {
        let mut agents = vec![rung.agent.in_pane("%1")];
        if rung.with_live_child {
            agents.push(child_state("root", "child-1", AgentStatus::Running, 5));
        }
        let snapshot =
            room(Vec::new(), agents).with_live_panes(vec![pane("%1", "node", "/repo/main")], None);
        let row = row(&snapshot, "root");
        assert_eq!(
            row.status(),
            Some(rung.expect),
            "precedence rung: {}",
            rung.name
        );
        assert_eq!(
            row.turn_error_label().is_some(),
            rung.expect_error_label,
            "error-label rung: {}",
            rung.name
        );
    }
}

#[test]
fn running_agent_in_spent_account_parks_not_fails() {
    // A running agent that went silent past the stall window AND whose account
    // is spent should surface as RateLimited, not Failed. The rate-limit check
    // takes priority over the stall check so the user sees the real cause.
    let stalled = agent("claude", "stalled-spent", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)])
        .active_ago(crate::feed::STALL_WINDOW_SECS + 60);

    let snapshot = room_with_agent_panes(Vec::new(), vec![stalled]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::RateLimited),
        "rate-limit outranks stall: agent is paused by the account, not wedged"
    );
}

#[test]
fn rate_limit_outranks_the_turn_death_marker() {
    // A rate-limited turn dies on a provider API error (`isApiErrorMessage`)
    // with no `Stop` hook, so the next statusline push delivers the
    // turn-death marker and the spent window *together*. The park wins
    // while the window is spent — the agent is paused by the account, not
    // dead — and the row carries no failure label. Once the window resets,
    // the still-standing marker escalates an agent that failed to resume.
    let session = agent("claude", "limited-dead", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .turn_error(10, "You've hit your usage limit");

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::RateLimited),
        "rate-limit outranks turn-death: the marker is the limit's own corpse"
    );
    assert!(
        row.turn_error_label().is_none(),
        "a parked row carries no failure label"
    );
}

#[test]
fn running_parent_with_live_child_in_spent_account_parks() {
    // Children share the parent's spent account: a window that tips
    // mid-delegation freezes the child with no `SubagentStop` to come, so
    // the delegated-wait exemption must not hold the parent at `running`
    // forever. The park outranks the exemption.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)]);
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::RateLimited),
        "a spent account parks the delegating parent — its children share the budget"
    );
}

// ── Compaction: the transient head and its crash backstop ────────────────────

#[test]
fn compacting_marker_lights_the_head_then_expires() {
    // A fresh compaction marker pulses the head; one older than the window
    // has expired (the crash backstop), so the head returns to its base.
    // The boundary is exact: one second inside the window still pulses, one
    // second past it has expired — a crash mid-compact can never pulse the
    // head forever.
    let fresh = agent("claude", "compacting-now", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .compacting_ago(0);
    let inside = agent("claude", "compacting-inside", AgentStatus::Running, 1_050)
        .worktree("/repo/main")
        .compacting_ago(crate::feed::COMPACTING_WINDOW_SECS - 1);
    let stale = agent("claude", "compacted-long-ago", AgentStatus::Idle, 1_100)
        .worktree("/repo/main")
        .compacting_ago(crate::feed::COMPACTING_WINDOW_SECS + 1);

    let snapshot = room_with_agent_panes(Vec::new(), vec![fresh, inside, stale]);
    assert!(
        row(&snapshot, "compacting-now").compacting(),
        "a fresh marker pulses"
    );
    assert!(
        row(&snapshot, "compacting-inside").compacting(),
        "a marker one second inside the window still pulses"
    );
    assert!(
        !row(&snapshot, "compacted-long-ago").compacting(),
        "a marker past the window has expired"
    );
}

#[test]
fn compaction_event_stamps_then_a_later_event_clears_the_marker() {
    // The reducer treats a `compacting` event as a transient: it stamps
    // `compacting_since` and keeps the prior status (not a transition); the
    // next lifecycle event means compaction is done and clears the marker.
    let ws = workspace();
    let prompt = lifecycle_at(
        &ws,
        "claude",
        "UserPromptSubmit",
        "sess-1",
        LifecycleSignal::TurnStarted,
    );
    let compact = lifecycle_at(
        &ws,
        "claude",
        "PreCompact",
        "sess-1",
        LifecycleSignal::Compacting,
    );
    let after_compact = reduce_agent_states(&[prompt.clone(), compact.clone()]);
    assert!(
        after_compact[0].compacting_since.is_some(),
        "the compaction marker is stamped"
    );
    assert_eq!(
        after_compact[0].status,
        AgentStatus::Running,
        "compaction keeps the prior status — it is not a transition"
    );

    let stop = lifecycle_at(
        &ws,
        "claude",
        "Stop",
        "sess-1",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let after_stop = reduce_agent_states(&[prompt, compact, stop]);
    assert!(
        after_stop[0].compacting_since.is_none(),
        "the next lifecycle event clears the marker"
    );
    assert_eq!(after_stop[0].status, AgentStatus::Success);
}

// ── The daemon-session reap (Codex app-server loaded-thread set) ─────────────

fn daemon_codex(id: &str, worktree: &str, owner_pid: u32) -> AgentState {
    let mut codex = paneless_codex(id, worktree, 1_000);
    codex.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        id,
        owner_pid,
        None,
    ));
    codex.agent_pid = Some(owner_pid);
    codex
}

fn rollup_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn daemon_session_absent_from_loaded_is_reaped() {
    // The shared daemon pid is alive, so process liveness keeps the ghost; the
    // app-server no longer holds the thread, so the loaded-set filter reaps it
    // while keeping the session it still holds.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::from(["t-live".to_owned()]);
    let mut snapshot = room(
        Vec::new(),
        vec![
            daemon_codex("t-live", "/repo/a", 7),
            daemon_codex("t-gone", "/repo/b", 7),
        ],
    );
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-live"]);
}

#[test]
fn unknown_loaded_set_keeps_every_session() {
    // `None` means the daemon was unreachable or its list untrusted — never
    // mass-reap.
    let daemon_pids = BTreeSet::from([7]);
    let mut snapshot = room(Vec::new(), vec![daemon_codex("t-gone", "/repo/b", 7)]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, None);
    assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
}

#[test]
fn empty_daemon_pids_keeps_every_session() {
    // No daemon is running, so every session is standalone — process liveness
    // governs them, not the loaded-thread set.
    let loaded = BTreeSet::new();
    let mut snapshot = room(Vec::new(), vec![daemon_codex("t-gone", "/repo/b", 7)]);
    snapshot.drop_dead_daemon_sessions(&BTreeSet::new(), Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
}

#[test]
fn standalone_codex_is_not_reaped_by_the_loaded_set() {
    // A session whose owner pid is its own in-pane CLI (not a daemon pid) is not
    // daemon-mode, so its absence from the daemon's loaded set means nothing.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::new();
    let mut snapshot = room(
        Vec::new(),
        vec![daemon_codex("t-standalone", "/repo/b", 99)],
    );
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-standalone"]);
}

#[test]
fn daemon_filter_spares_subagents_and_other_kinds() {
    // A codex subagent id is never a root thread, and a non-codex agent is never
    // daemon-mode — neither is reaped even sharing the daemon pid and absent from
    // the loaded set.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::new();
    let mut sub = daemon_codex("sub-1", "/repo/a", 7);
    sub.parent_agent_id = Some("root-1".into());
    let mut claude = daemon_codex("claude-1", "/repo/c", 7);
    claude.kind = AgentKind::new_unchecked("claude");
    let mut snapshot = room(Vec::new(), vec![sub, claude]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["claude-1", "sub-1"]);
}

// ── Ranking, caps, and bucket order ──────────────────────────────────────────

#[test]
fn calm_tail_cap_never_hides_attention_rows() {
    let mut agents = (0..8)
        .map(|i| {
            agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            )
        })
        .collect::<Vec<_>>();
    agents.push(agent_in("failed", "/repo/main", AgentStatus::Failed, 2_000));

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.status() == Some(AgentStatus::Failed)),
        "attention rows remain visible past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn calm_tail_cap_never_hides_focused_rows() {
    let agents = (0..8)
        .map(|i| {
            let mut agent = agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            );
            if i == 0 {
                agent.pane = Some(PaneRef {
                    is_focused: true,
                    ..pane("%99", "codex", "/repo/main")
                });
            }
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "sess-0"),
        "the focused running pane remains visible even past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn bucket_order_puts_attention_first_and_idle_last() {
    // Scrambled input proves the sort, not the insertion order.
    let agents = [
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Idle,
        AgentStatus::Failed,
        AgentStatus::Waiting,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.status())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Failed),
            Some(AgentStatus::Success),
            Some(AgentStatus::Running),
            Some(AgentStatus::Idle),
        ],
        "attention leads; parked idle agents sink to the bottom of the group"
    );
}

#[test]
fn calm_bucket_holds_stable_spawn_order() {
    // Idle agents with distinct spawn times (and one with no pane). The
    // bucket holds spawn order — oldest first — regardless of activity.
    let specs: [(&str, Option<i64>); 4] = [
        ("late", Some(100)),
        ("nopane", None),
        ("early", Some(300)),
        ("mid", Some(200)),
    ];
    let agents = specs
        .into_iter()
        .enumerate()
        .map(|(i, (id, ago_secs))| {
            let mut agent = agent_in(id, "/repo/main", AgentStatus::Idle, 1_000 + i as i64);
            agent.pane =
                ago_secs.map(|secs| pane_started(&format!("%{i}"), "/repo/main", ago(secs)));
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Oldest pane first; the paneless row keys on its `registered_at` — newer
    // than every pane start here — and falls to the bucket tail.
    assert_eq!(order, vec!["early", "mid", "late", "nopane"]);
}

#[test]
fn new_idle_agent_appends_below_calm_work() {
    // A brand-new agent registers idle, so wherever the snapshot catches it —
    // before or after its first prompt — it never lands above finished or
    // working agents: idle is the calm region's bottom bucket.
    let mut done = agent_in("done", "/repo/main", AgentStatus::Success, 1_000);
    done.pane = Some(pane_started("%0", "/repo/main", ago(600)));
    let mut work = agent_in("work", "/repo/main", AgentStatus::Running, 1_001);
    work.pane = Some(pane_started("%1", "/repo/main", ago(500)));
    let mut fresh = agent_in("fresh", "/repo/main", AgentStatus::Idle, 1_002);
    fresh.pane = Some(pane_started("%2", "/repo/main", ago(5)));

    let snapshot = room_with_agent_panes(Vec::new(), vec![fresh, work, done]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["done", "work", "fresh"],
        "the new idle card appends at the bottom of the calm region"
    );
}

#[test]
fn paneless_calm_rows_order_by_registration_not_label() {
    // Zellij reports no pane process start, so calm rows there fall back to
    // the durable `registered_at` spawn key — never a label: the older session
    // leads even though its kind name sorts after its sibling's.
    let mut older = agent("codex", "older", AgentStatus::Idle, 1_000).worktree("/repo/main");
    older.pane = Some(pane("%0", "codex", "/repo/main"));
    let mut newer = agent("claude", "newer", AgentStatus::Idle, 9_000).worktree("/repo/main");
    newer.pane = Some(pane("%1", "claude", "/repo/main"));

    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["older", "newer"],
        "spawn order holds without a pane start; the label never reorders calm rows"
    );
}

#[test]
fn cap_trims_idle_before_running() {
    // Idle ranks last among agents, so the per-worktree cap's calm trim eats
    // the parked idle tail first and a working agent stays visible longer.
    let mut agents = Vec::new();
    for i in 0..4 {
        agents.push(agent_in(
            &format!("run-{i}"),
            "/repo/main",
            AgentStatus::Running,
            1_000 + i,
        ));
    }
    for i in 0..4 {
        agents.push(agent_in(
            &format!("idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            2_000 + i,
        ));
    }

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let visible = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert!(
        (0..4).all(|i| visible.contains(&format!("run-{i}"))),
        "every running agent stays visible; only the idle tail trims: {visible:?}"
    );
    assert_eq!(snapshot.worktree_groups[0].hidden_count, 2);
}

#[test]
fn calm_groups_hold_order_through_member_status_churn() {
    // Calm worktree groups never leapfrog just because a member's calm status
    // flipped: the group tier collapses success/running/idle to one rank, so
    // the stable earliest-pane order decides until genuine attention arises.
    let build = |a_status: AgentStatus, b_status: AgentStatus| {
        let mut a = agent_in("sess-a", "/repo/a", a_status, 1_000);
        a.pane = Some(pane_started("%0", "/repo/a", ago(600)));
        let mut b = agent_in("sess-b", "/repo/b", b_status, 1_001);
        b.pane = Some(pane_started("%1", "/repo/b", ago(500)));
        room_with_agent_panes(Vec::new(), vec![a, b])
    };

    let groups = |snapshot: &SidebarSnapshot| {
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };

    let before = build(AgentStatus::Running, AgentStatus::Success);
    // b's agent finishing a turn while a's keeps working reorders nothing.
    let after = build(AgentStatus::Idle, AgentStatus::Running);
    assert_eq!(groups(&before), groups(&after));
    assert_eq!(groups(&before), vec!["a", "b"]);

    // Genuine attention still floats its group to the top.
    let blocked = build(AgentStatus::Running, AgentStatus::Waiting);
    assert_eq!(groups(&blocked), vec!["b", "a"]);
}

#[test]
fn paneless_calm_groups_order_by_registration_not_label() {
    // Same fallback at the group tier as within a bucket: without a pane
    // start (Zellij), same-tier groups key on their earliest member's
    // `registered_at` — the worktree you opened first stays first, whatever
    // its label.
    let mut older = agent_in("sess-b", "/repo/b", AgentStatus::Idle, 1_000);
    older.pane = Some(pane("%0", "node", "/repo/b"));
    let mut newer = agent_in("sess-a", "/repo/a", AgentStatus::Idle, 9_000);
    newer.pane = Some(pane("%1", "node", "/repo/a"));

    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);

    let groups = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec!["b", "a"],
        "group spawn order holds without pane starts; the label never reorders calm groups"
    );
}

#[test]
fn attention_bucket_sorts_longest_overdue_first() {
    // Scrambled input; a higher rank means more recent activity.
    let agents = vec![
        ("wait-new", AgentStatus::Waiting, 9_000),
        ("wait-old", AgentStatus::Waiting, 1_000),
        ("fail-new", AgentStatus::Failed, 8_000),
        ("fail-old", AgentStatus::Failed, 2_000),
    ]
    .into_iter()
    .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Waiting leads failed; within each, the longest-overdue (oldest activity) rises.
    assert_eq!(order, vec!["wait-old", "wait-new", "fail-old", "fail-new"]);
}

// ── Process liveness and the session reaper ──────────────────────────────────

#[test]
fn liveness_drops_dead_agent_pid_from_rollup() {
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).branch("main");
    codex.agent_pid = Some(424_242);
    codex.agent_process_start = Some("12345".to_owned());

    let mut snapshot = room(Vec::new(), vec![codex]);
    assert_eq!(snapshot.agents.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());

    snapshot.drop_dead_agents_with(|pid, start| {
        assert_eq!(pid, 424_242);
        assert_eq!(start, Some("12345"));
        false
    });

    assert!(snapshot.agents.is_empty());
    assert!(snapshot.worktree_groups.is_empty());
}

/// Build a single-agent rollup at the epoch, run the reap, and return the
/// surviving agent ids. Fixture timestamps are epoch offsets, so the TTL
/// rules are exercised deterministically.
fn reap_survivors(agents: Vec<AgentState>) -> Vec<String> {
    let mut snapshot = room(Vec::new(), agents);
    snapshot.reap_stale_sessions();
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn reap_drops_pidless_session_past_ttl_but_keeps_recent_and_pidful() {
    let stale = agent("claude", "stale", AgentStatus::Idle, 0)
        .worktree("/repo/stale")
        .active_ago(GHOST_SESSION_TTL_SECS + 60);
    let recent = agent("claude", "recent", AgentStatus::Idle, 0)
        .worktree("/repo/recent")
        .active_ago(60);
    // Old but pid-bearing: TTL reaping is for pidless ghosts only.
    let mut pidful = agent("codex", "pidful", AgentStatus::Idle, 0)
        .worktree("/repo/pidful")
        .active_ago(GHOST_SESSION_TTL_SECS * 10);
    pidful.agent_pid = Some(4242);

    assert_eq!(
        reap_survivors(vec![stale, recent, pidful]),
        vec!["pidful".to_owned(), "recent".to_owned()],
        "only the pidless, past-TTL ghost is reaped"
    );
}

#[test]
fn reap_collapses_superseded_paneless_session_to_the_newest() {
    let older = agent("codex", "older", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(120);
    let newer = agent("codex", "newer", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(60);

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned()],
        "the older paneless session on the same path+branch is reaped"
    );
}

#[test]
fn reap_keeps_paneless_older_when_newer_has_distinct_stamped_pane() {
    // A recovered focused-pane stamp on the newer daemon-routed Codex session
    // proves only where the newer session lives. The older paneless session may
    // still bind another same-cwd live pane at projection time, so the reaper
    // must not collapse it as an indistinguishable duplicate.
    let older = agent("codex", "older", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(120);
    let newer = agent("codex", "newer", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .in_pane("%2")
        .active_ago(60);

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned(), "older".to_owned()],
        "a newer distinct pane does not prove the paneless older session is stale"
    );
}

#[test]
fn reap_keeps_concurrent_agents_each_holding_a_distinct_pane() {
    // The one-pane-one-row safety property: two same-branch agents in
    // distinct panes are both live and must both survive supersession.
    let mut older = agent("claude", "older", AgentStatus::Running, 0)
        .worktree("/repo/a")
        .branch("main")
        .in_pane("%1")
        .active_ago(120);
    older.agent_pid = Some(111);
    let mut newer = agent("claude", "newer", AgentStatus::Running, 0)
        .worktree("/repo/a")
        .branch("main")
        .in_pane("%2")
        .active_ago(60);
    newer.agent_pid = Some(222);

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned(), "older".to_owned()],
        "an agent holding its own distinct pane is never reaped"
    );
}

#[test]
fn reaper_never_drops_a_subagent() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 0);
    // A pidless idle child well past the ghost TTL, plus a same-type sibling
    // that would "supersede" it under the root rule — both survive, because
    // children are exempt and leave only when the parent does.
    let old_child = child_state(
        "sess-root",
        "child-old",
        AgentStatus::Idle,
        GHOST_SESSION_TTL_SECS + 600,
    );
    let new_child = child_state("sess-root", "child-new", AgentStatus::Running, 5);
    assert_eq!(
        reap_survivors(vec![parent, old_child, new_child]),
        vec![
            "child-new".to_owned(),
            "child-old".to_owned(),
            "sess-root".to_owned()
        ],
    );
}

// ── Rate-limit window stabilizers (the dashboard bars) ───────────────────────

#[test]
fn stable_window_ignores_passed_resets_and_keeps_the_most_drained() {
    // A stale window (reset already passed) reads low; two live windows
    // report 50% and 80%. The stale one is dropped, and the most-drained
    // live survivor (80%) wins — never over-promising remaining budget.
    let live_50 = window(50, 3_600);
    let live_80 = window(80, 1_800);
    let stale_10 = window(10, -60);

    let pick = stable_window(
        [live_50.clone(), live_80.clone(), stale_10.clone()].into_iter(),
        epoch(),
    )
    .expect("a live window survives");
    assert_eq!(pick.used_percentage, Some(80));

    // Order-independent: the producer must not flicker with session order.
    let reversed = stable_window([stale_10, live_80, live_50].into_iter(), epoch())
        .expect("a live window survives");
    assert_eq!(reversed.used_percentage, Some(80));
}

#[test]
fn stable_window_is_none_when_every_reading_is_stale() {
    assert!(stable_window([window(90, -10), window(40, -3_600)].into_iter(), epoch()).is_none());
}

#[test]
fn stable_window_falls_back_to_an_undated_reading() {
    // A window with no reset instant can't be aged out; it is the last-resort
    // reading only when nothing with a live reset survives.
    let undated = RateLimitWindow {
        used_percentage: Some(33),
        resets_at: None,
        duration_mins: Some(300),
    };
    let pick = stable_window([window(90, -10), undated].into_iter(), epoch())
        .expect("the undated reading backstops the stale one");
    assert_eq!(pick.used_percentage, Some(33));
}

#[test]
fn stable_windows_picks_one_per_duration_sorted_short_to_long() {
    let mk = |used: u8, mins: u32| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(epoch() + std::time::Duration::from_secs(3_600)),
        duration_mins: Some(mins),
    };
    // Two sessions, each reporting a 5h and a 30d window at different drains.
    let readings = [mk(10, 43_800), mk(20, 300), mk(40, 43_800), mk(5, 300)];
    let stable = stable_windows(readings.into_iter(), epoch());
    assert_eq!(stable.len(), 2, "one bar per duration");
    assert_eq!(
        stable[0].duration_mins,
        Some(300),
        "short window sorts first"
    );
    assert_eq!(stable[0].used_percentage, Some(20), "most-drained 5h kept");
    assert_eq!(
        stable[1].duration_mins,
        Some(43_800),
        "long window sorts last"
    );
    assert_eq!(stable[1].used_percentage, Some(40), "most-drained 30d kept");
}

#[test]
fn context_sidecar_ttl_matches_the_ghost_session_ttl() {
    // The agent-context sidecar's missed-tombstone TTL is documented as
    // matched to the rollup's ghost-session TTL, so a vanished session's
    // stale enrichment and its stale row age out together. Pin the parity.
    assert_eq!(
        crate::ledger::agent_context::CONTEXT_TTL_SECS,
        GHOST_SESSION_TTL_SECS,
    );
}

// ── The per-call split: the context line's row-level fallback ────────────────

#[test]
fn call_split_projects_the_lifecycle_rail_composition() {
    // The per-call split a rollout's `last_token_usage` feeds onto the
    // lifecycle rail projects onto the row, and its `filled()` — cache reads +
    // fresh input, exactly the window numerator the `▣` percent scales —
    // stands in for the severity axis's absolute-token read when no rich blob
    // exists.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    let snapshot = room_with_agent_panes(Vec::new(), vec![codex]);

    let projected = row(&snapshot, "sess-1");
    let split = projected
        .call_split()
        .expect("the split projects onto the row");
    assert_eq!(split.cache_read, 120_000);
    assert_eq!(split.fresh_input, 9_200);
    assert_eq!(split.output, 800);
    assert_eq!(split.filled(), 129_200);
    assert_eq!(projected.context_used_tokens(), Some(129_200));
}

#[test]
fn call_split_waits_for_a_known_input_side() {
    // Until the input side of a call is known the row keeps the bare total —
    // a pre-first-turn agent never legends a partial composition.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.total_tokens = Some(5_000);
    codex.cache_read_input_tokens = Some(99);
    let snapshot = room_with_agent_panes(Vec::new(), vec![codex]);

    let projected = row(&snapshot, "sess-1");
    assert_eq!(projected.call_split(), None);
    assert_eq!(projected.context_used_tokens(), None);
}

/// The cockpit's live today-spend rides the published frame across every
/// snapshot wire — `rimz sidebar snapshot` stdout, the plugin rail — so the
/// field must survive a JSON round-trip, and a frame from a pre-overlay
/// producer must read as `None` (version skew degrades to the walked tally,
/// never an error).
#[test]
fn today_spend_live_usd_round_trips_and_defaults_absent() {
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.today_spend_live_usd = Some(12.34);
    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: SidebarSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.today_spend_live_usd, Some(12.34));

    // An old producer's frame carries no field at all (`skip_serializing_if`
    // keeps `None` off the wire symmetrically).
    snapshot.today_spend_live_usd = None;
    let bare = serde_json::to_string(&snapshot).unwrap();
    assert!(!bare.contains("today_spend_live_usd"));
    let parsed: SidebarSnapshot = serde_json::from_str(&bare).unwrap();
    assert_eq!(parsed.today_spend_live_usd, None);
}
