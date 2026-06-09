use super::*;

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
fn wired_unprompted_supervised_codex_pane_renders_as_idle_agent() {
    // `rimz agents --worktree` launches Codex through the hidden cleanup
    // wrapper. During the pre-session window the pane command is still that
    // wrapper, but it is semantically a Codex pane and must take the same idle
    // lazy-agent path as a bare `codex` command.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(
        vec![pane(
            "term1",
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main",
            "/repo/main",
        )],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].id, "tmux:term1");
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
