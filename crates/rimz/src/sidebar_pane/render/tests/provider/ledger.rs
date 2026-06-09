use super::*;

/// The fleet ledger pinned at the bottom of the dashboard: the static
/// `W:` (week) and `M:` (month) rows, each reading `◎ sessions  ◇ ↘ ↗ ◌
/// $spend` across every provider — precise one-decimal token figures and the
/// bold money-green spend, right-aligned into one aligned grid. Today's
/// headline stays in the cockpit's animated `$`, never repeated here.
#[test]
fn render_fleet_ledger_pins_week_month_rows_under_the_dashboard() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(crate::SpendTally {
        today: crate::SpendWindow {
            usd: 40.23,
            tokens: 3_420_000,
            input: 420_000,
            output: 3_000_000,
            cache_write: 120_000,
            cache_read: 6_800_000,
            sessions: 12,
        },
        week: crate::SpendWindow {
            usd: 312.40,
            tokens: 21_900_000,
            input: 3_200_000,
            output: 18_700_000,
            cache_write: 900_000,
            cache_read: 51_000_000,
            sessions: 92,
        },
        month: crate::SpendWindow {
            usd: 1_240.57,
            tokens: 34_900_000,
            input: 6_200_000,
            output: 28_700_000,
            cache_write: 1_900_000,
            cache_read: 121_000_000,
            sessions: 212,
        },
        year: crate::SpendWindow {
            usd: 4_821.90,
            tokens: 50_200_000,
            input: 10_200_000,
            output: 40_000_000,
            cache_write: 3_000_000,
            cache_read: 210_000_000,
            sessions: 980,
        },
    });
    let rendered = snapshot_to_screen(&snapshot, 60, 34);

    // The `W:` and `M:` rows: each labelled left, the `$` spend pinned right.
    assert!(rendered.contains("W:"), "the week ledger row:\n{rendered}");
    assert!(rendered.contains("M:"), "the month ledger row:\n{rendered}");
    assert!(
        rendered.contains("$312.40"),
        "this week's spend:\n{rendered}"
    );
    assert!(
        rendered.contains("$1,240.57"),
        "this month's spend:\n{rendered}"
    );
    // Session counts and the precise (one-decimal) token total.
    assert!(rendered.contains("212"), "month session count:\n{rendered}");
    assert!(
        rendered.contains("34.9M"),
        "month token total, precise form:\n{rendered}"
    );
    // The `year` window is no longer surfaced — the ledger tops out at month.
    assert!(
        !rendered.contains("$4,821.90"),
        "the year pile is gone from the ledger:\n{rendered}"
    );
    assert_snapshot("fleet_ledger", rendered);
}
