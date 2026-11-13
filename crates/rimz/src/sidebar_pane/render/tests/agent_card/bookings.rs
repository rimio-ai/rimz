use super::*;

use super::super::budget::sub_agent;
use crate::agents::BackgroundShell;
use crate::config::CardDensityMode;
use crate::sidebar::test_support::bookings_in_each_state;
use crate::sidebar_pane::render::layout::text_width;
use crate::sidebar_pane::render::theme::Component;
use crate::store::snapshot::{BookingBand, BookingPr, BookingState, SiblingState, SidebarBooking};

/// The fixture's entries named in `tasks`, in the fixture's draw order.
fn pick(tasks: &[&str]) -> Vec<SidebarBooking> {
    bookings_in_each_state(fixed_now())
        .into_iter()
        .filter(|booking| tasks.contains(&booking.task.as_str()))
        .collect()
}

/// Two fired entries running, a sibling waiting after one of them, a raw term, a
/// timed trigger, one counted `later`, and one counted `older`.
fn healthy_plan() -> Vec<SidebarBooking> {
    pick(&[
        "lang-typed",
        "lane-readers",
        "lane-readers-docs",
        "lang-docs",
        "nightly",
        "lang-release",
        "old-spike",
    ])
}

fn plan_needing_a_fix() -> Vec<SidebarBooking> {
    let mut plan = pick(&["broken"]);
    plan.extend(healthy_plan());
    plan
}

/// The team stage ended 53 minutes ago with no pull request while its sibling still
/// waits after it, beside another stage that ended the same way.
fn plan_with_an_ended_upstream() -> Vec<SidebarBooking> {
    let mut plan = healthy_plan();
    plan.extend(pick(&["spike"]));
    plan.sort_by_key(|booking| booking.band != BookingBand::Fired);
    for booking in &mut plan {
        match (&booking.task[..], &mut booking.state) {
            ("lane-readers", BookingState::Fired(fire)) => {
                fire.live = None;
                fire.at = fixed_now() - Duration::from_secs(53 * 60 + 3 * 3_600);
                fire.ended_at = Some(fixed_now() - Duration::from_secs(53 * 60));
                fire.pr = BookingPr::None;
                fire.depended_on = true;
            }
            ("lane-readers-docs", BookingState::Waiting(wait)) => {
                let blocker = wait.blocker.as_mut().unwrap();
                blocker.after.as_mut().unwrap().state = SiblingState::Ended;
                blocker.reading = Some("none".to_owned());
            }
            _ => {}
        }
    }
    plan
}

fn planner_snapshot(bookings: Vec<SidebarBooking>, quiet_secs: u64) -> SidebarSnapshot {
    planner_snapshot_as(bookings, quiet_secs, false)
}

/// The planner card, or with `fresh` one that has not started its first turn.
fn planner_snapshot_as(
    bookings: Vec<SidebarBooking>,
    quiet_secs: u64,
    fresh: bool,
) -> SidebarSnapshot {
    let mut planner = agent(
        "planner",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("Workflow language redesign plan"),
    );
    planner.prompt = Some("plan the redesign".to_owned());
    planner.model = Some("opus".to_owned());
    planner.effort = Some("high".to_owned());
    planner.usage.context_pct = Some(1);
    planner.usage.context_window = Some(200_000);
    if fresh {
        planner.status = AgentStatus::Idle;
        planner.prompt = None;
        planner.usage.context_pct = None;
    }
    let mut snapshot = snapshot_with(vec![planner]);
    let row = snapshot.rows_mut().next().unwrap();
    row.unread = false;
    row.last_activity = fixed_now() - Duration::from_secs(quiet_secs);
    row.as_agent_mut().unwrap().bookings = bookings;
    snapshot
}

fn screen(snapshot: &SidebarSnapshot, selected: bool, width: u16) -> String {
    let ui = ViewState {
        selected_index: selected.then_some(0).or(Some(usize::MAX)),
        ..Default::default()
    };
    snapshot_to_screen_with_alert_and_ui(snapshot, None, &ui, &UiState::default(), width, 34)
}

#[test]
fn booked_cards_render_at_46_40_and_28_columns() {
    let cases = [
        ("rest_healthy", healthy_plan(), false, 60),
        ("rest_fix", plan_needing_a_fix(), false, 60),
        ("selected_healthy", healthy_plan(), true, 60),
        (
            "selected_ended_upstream",
            plan_with_an_ended_upstream(),
            true,
            60,
        ),
        ("stale", healthy_plan(), false, 172_800),
    ];
    for (name, plan, selected, quiet) in cases {
        let snapshot = planner_snapshot(plan, quiet);
        for width in [46, 40, 28] {
            assert_snapshot(
                &format!("booked_{name}_{width}"),
                screen(&snapshot, selected, width),
            );
        }
    }
}

#[test]
fn every_booking_state_draws_in_band_order_with_its_count_rows() {
    let snapshot = planner_snapshot(bookings_in_each_state(fixed_now()), 60);
    let theme = Theme::fixed(false);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 46)).join("\n");
    let order = [
        "▷ booked (10) · ⚠ broken: invalid",
        "⚠ coder · broken",
        "invalid: unknown term `pr[`",
        "coder · lang-typed",
        "@coder",
        "forge · lane-readers",
        "Build · @coder",
        "▷ writer · lane-readers-docs",
        "after lane-readers",
        "– coder · spike",
        "ended 10m ago · no PR",
        "– coder · landed",
        "ended 5m ago · #1630 merged",
        "▷ writer · lang-docs",
        "pr[workflow-language]: #1640 open",
        "▷ coder · nightly",
        "once at 2026-11-14 03:00",
        "+1 later",
        "1 more ended",
    ];
    let mut from = 0;
    for needle in order {
        let at = text[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} after byte {from}:\n{text}"));
        from += at + needle.len();
    }
    assert!(!text.contains("lang-release"), "{text}");
    assert!(!text.contains("old-spike"), "{text}");
    let runtime = text
        .lines()
        .find(|line| line.contains("ended 10m ago"))
        .unwrap();
    assert!(
        runtime.trim_end_matches('▐').trim_end().ends_with("30m"),
        "frozen at the end: {runtime}"
    );

    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 46, 0, 0, &cost_rolls);
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let lines = line_texts(&block.lines);
    let target = |needle: &str| {
        let line = lines.iter().position(|line| line.contains(needle)).unwrap();
        block
            .interactions
            .target_at(8, u16::try_from(line).unwrap())
    };
    let card = target("once at 2026-11-14 03:00");
    assert!(matches!(card, Some(HitTarget::Row(_))), "{card:?}");
    for count in ["+1 later", "1 more ended"] {
        assert_eq!(target(count), card, "{count} is static: the card's own row");
    }
}

#[test]
fn only_the_fix_segment_of_the_booked_line_takes_the_warning_tone() {
    let theme = Theme::fixed(false);
    let warn = theme.component(Component::BookingFix);
    for (plan, segment) in [
        (
            plan_needing_a_fix(),
            "⚠ broken: invalid: unknown term `pr[`",
        ),
        (
            {
                let mut plan = plan_needing_a_fix();
                plan[1].needs_fix = Some("checkout gone".to_owned());
                plan[1].band = BookingBand::NeedsFix;
                plan
            },
            "⚠ 2 need a fix",
        ),
    ] {
        let snapshot = planner_snapshot(plan, 60);
        let lines = group_lines_at_width(&snapshot, &theme, usize::MAX, 60);
        let line = lines
            .iter()
            .find(|line| line.to_string().contains("booked ("))
            .expect("the resting booked line");
        let warned: Vec<_> = line
            .spans
            .iter()
            .filter(|span| span.style.fg == Some(warn))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(warned, [segment], "{line}");
        let header = line
            .spans
            .iter()
            .find(|span| span.content == theme.glyph(GlyphRole::CardBooked))
            .unwrap();
        assert_eq!(
            header.style.fg,
            Some(theme.component(Component::BookedHeader))
        );
    }
}

#[test]
fn the_resting_summary_gives_way_running_first_and_the_fix_never_drops() {
    let theme = Theme::fixed(false);
    let ended = planner_snapshot(plan_with_an_ended_upstream(), 60);
    let booked = |snapshot: &SidebarSnapshot, width: usize| {
        line_texts(&group_lines_at_width(snapshot, &theme, usize::MAX, width))
            .into_iter()
            .find(|line| line.contains('▷'))
            .unwrap()
    };
    assert!(booked(&ended, 60).ends_with("1 running · 2 ended, no PR "));
    assert!(booked(&ended, 34).ends_with("  ▷ 8 1 running · 2 ended, no PR "));
    let dropped = booked(&ended, 33);
    assert!(dropped.ends_with("2 ended, no PR ") && !dropped.contains("running"));
    assert!(booked(&ended, 28).ends_with("1 ⣾ · 2 no PR "));
    let healthy = planner_snapshot(healthy_plan(), 60);
    assert!(booked(&healthy, 46).contains("▷ booked (7)"));
    assert!(booked(&healthy, 46).ends_with("2 running "));
    let waiting = planner_snapshot(pick(&["lang-docs", "lang-release"]), 60);
    assert!(booked(&waiting, 46).ends_with("#1640 open "));
    let fix = planner_snapshot(plan_needing_a_fix(), 60);
    for width in [24, 28, 32, 46] {
        assert!(booked(&fix, width).contains('⚠'), "{width}");
    }
    assert!(booked(&fix, 28).contains("▷ 8 · ⚠ broken"));
}

#[test]
fn the_booked_line_holds_on_every_resting_shape_until_the_plan_finishes() {
    let theme = Theme::fixed(false);
    let shapes = |snapshot: &SidebarSnapshot| {
        let mut density = snapshot.clone();
        density.theme.display.card_density = CardDensityMode::Compact;
        [snapshot.clone(), density].map(|snapshot| {
            line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, 46)).join("\n")
        })
    };
    for snapshot in [
        planner_snapshot(healthy_plan(), 60),
        planner_snapshot(healthy_plan(), 172_800),
        planner_snapshot_as(healthy_plan(), 60, true),
    ] {
        for text in shapes(&snapshot) {
            assert!(text.contains("▷ booked (7)"), "{text}");
            assert!(!text.contains("lang-typed"), "entries stay closed: {text}");
        }
    }
    let finished = planner_snapshot(pick(&["landed"]), 60);
    for text in shapes(&finished) {
        assert!(!text.contains('▷'), "a finished plan draws nothing: {text}");
    }
    let open = line_texts(&group_lines_at_width(&finished, &theme, 0, 46)).join("\n");
    assert!(
        open.contains("▷ booked (1)") && open.contains("landed"),
        "{open}"
    );
}

/// Subagents, waits and bookings on one line at 46 columns: the labels take their short
/// forms before anything is trimmed, and the `⚠` segment lands.
#[test]
fn a_crowded_section_line_shortens_its_labels_before_the_fix_clips() {
    let theme = Theme::fixed(false);
    let mut snapshot = planner_snapshot(plan_needing_a_fix(), 60);
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.sub_agents = (0..3).map(|index| sub_agent("planner", index)).collect();
    card.background_shells.push(BackgroundShell {
        id: "shell".into(),
        command: Some("cargo test".into()),
        description: None,
        started_at: fixed_now(),
    });
    let line = |width: usize| {
        line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains('▷'))
            .unwrap()
    };
    let crowded = line(46);
    assert!(
        crowded.contains(" 3 · ") && crowded.contains("▷ 8 · ⚠ broken"),
        "short labels, then the fix: {crowded}"
    );
    assert!(!crowded.contains("subagents ("), "{crowded}");
    let wide = line(100);
    assert!(
        wide.contains("subagents (3)")
            && wide.contains("booked (8)")
            && wide.contains("⚠ broken: invalid: unknown term `pr[`"),
        "long labels where the whole line fits: {wide}"
    );
}

/// The review's card: 3 subagents, 1 wait, 8 bookings with one needing a fix, and a
/// subagent cost. At every width from 28 columns and at a one- and a three-digit cost, the
/// `⚠` segment keeps its glyph and at least one cell of its words; the cost gives way
/// first. Without bookings the line is the plain subagents and waits line.
#[test]
fn the_fix_keeps_its_glyph_and_a_word_cell_at_every_width_and_cost() {
    let theme = Theme::fixed(false);
    let card = |plan: Vec<SidebarBooking>, cost: f64| {
        let mut snapshot = planner_snapshot(plan, 60);
        let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
        card.sub_agents = (0..3).map(|index| sub_agent("planner", index)).collect();
        card.sub_agents[0].cost_usd = Some(cost);
        card.background_shells.push(BackgroundShell {
            id: "shell".into(),
            command: Some("cargo test".into()),
            description: None,
            started_at: fixed_now(),
        });
        snapshot
    };
    let section = |snapshot: &SidebarSnapshot, width: usize| {
        line_texts(&group_lines_at_width(snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains('⧉'))
            .unwrap()
    };
    for cost in [1.0, 123.45] {
        let snapshot = card(plan_needing_a_fix(), cost);
        for width in 28..=72 {
            let line = section(&snapshot, width);
            let fix = line
                .split_once("⚠ ")
                .map(|(_, words)| words)
                .unwrap_or_else(|| panic!("{width} ${cost}: no `⚠ `: {line:?}"));
            assert!(
                fix.chars()
                    .next()
                    .is_some_and(|cell| cell != '…' && cell != ' '),
                "{width} ${cost}: a cell of the fix's words: {line:?}"
            );
            assert!(text_width(&line) <= width, "{width}: {line:?}");
        }
    }
    let review = section(&card(plan_needing_a_fix(), 1.0), 28);
    assert!(review.contains("▷ 8 · ⚠ b"), "{review:?}");

    let plain = card(Vec::new(), 1.0);
    assert_eq!(
        section(&plain, 46),
        "▌  ⧉ subagents (3) · ⧖ waits (1)        $1.00▐"
    );
    assert_eq!(section(&plain, 28), "▌  ⧉ 3 · ⧖ 1          $1.00▐");
}

/// A sibling whose fire has not launched its agent yet reads `starting` in the `after`
/// text of the entry waiting on it.
#[test]
fn a_sibling_still_starting_reads_starting_in_the_after_text() {
    let theme = Theme::fixed(false);
    let mut plan = pick(&["lane-readers-docs"]);
    let BookingState::Waiting(wait) = &mut plan[0].state else {
        panic!("lane-readers-docs waits");
    };
    let sibling = wait.blocker.as_mut().unwrap().after.as_mut().unwrap();
    sibling.state = SiblingState::Starting;
    let task = sibling.task.clone();
    let snapshot = planner_snapshot(plan, 60);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 46)).join("\n");
    assert!(text.contains(&format!("{task} starting")), "{text}");
    assert!(!text.contains(&format!("after {task}")), "{text}");
}

/// A fire whose launch has no row yet and no end stamp reads `starting`, never `ended`.
#[test]
fn a_starting_fire_reads_starting_on_its_second_line() {
    let theme = Theme::fixed(false);
    let mut plan = pick(&["spike"]);
    let BookingState::Fired(fire) = &mut plan[0].state else {
        panic!("spike is fired");
    };
    fire.ended_at = None;
    fire.pr = BookingPr::Unread;
    fire.starting = true;
    let snapshot = planner_snapshot(plan, 60);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 46)).join("\n");
    let entry = text.find("coder · spike").expect("the entry is drawn");
    let second = text[entry..].lines().nth(1).unwrap();
    let second = second.trim_start_matches('▌').trim_start();
    assert!(second.starts_with("starting"), "{text}");
    assert!(!text.contains("ended"), "{text}");
}
