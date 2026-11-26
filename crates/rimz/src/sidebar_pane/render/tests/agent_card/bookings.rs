use super::*;

use super::super::budget::sub_agent;
use crate::agents::BackgroundShell;
use crate::config::CardDensityMode;
use crate::sidebar::test_support::bookings_in_each_state;
use crate::sidebar_pane::render::layout::text_width;
use crate::sidebar_pane::render::theme::Component;
use crate::store::snapshot::{
    BookingBand, BookingPr, BookingState, BookingWait, SiblingState, SidebarBooking,
};

/// The fixture's entries named in `tasks`, in the fixture's draw order.
fn pick(tasks: &[&str]) -> Vec<SidebarBooking> {
    bookings_in_each_state(fixed_now())
        .into_iter()
        .filter(|booking| tasks.contains(&booking.task.as_str()))
        .collect()
}

/// Two fired entries running, a sibling waiting after one of them, a raw term, a
/// timed trigger, one waiting on an unfired sibling, and one finished.
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

/// Every entry names what it starts by profile or team and lane, then what it is doing: a
/// running agent its card's text and its runtime on one line, a running team its stage on
/// one line, any other entry the booking's own text over its second line. No task name or
/// handle shows outside the entry needing a fix. Finished entries wait behind the earlier
/// line until paged in, newest end first.
#[test]
fn every_booking_state_draws_in_band_order_and_pages_its_finished_entries() {
    let snapshot = planner_snapshot(bookings_in_each_state(fixed_now()), 60);
    let theme = Theme::fixed(false);
    let current = theme.glyph(GlyphRole::PipelineCurrent);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 60)).join("\n");
    let running_team = format!("team forge#lane-readers · {current} Build");
    let order = [
        "▷ booked (10) · ⚠ broken: invalid",
        "⚠ coder · broken",
        "invalid: unknown term `pr[`",
        "coder · type the workflow language",
        &running_team,
        "▷ writer · document the lane readers",
        "after forge#lane-readers",
        "– coder · try the parser",
        "ended 10m ago · no PR",
        "▷ writer · document the language",
        "pr[workflow-language]: #1640 open",
        "▷ coder ",
        "once at 2026-11-14 03:00",
        "▸ +2 earlier",
    ];
    let mut from = 0;
    for needle in order {
        let at = text[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} after byte {from}:\n{text}"));
        from += at + needle.len();
    }
    let lines: Vec<&str> = text.lines().collect();
    let line_of = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap();
    for one_line in ["type the workflow language", running_team.as_str()] {
        let at = line_of(one_line);
        assert!(
            lines[at].trim_end_matches('▐').trim_end().ends_with('m')
                || lines[at].trim_end_matches('▐').trim_end().ends_with('h'),
            "the runtime pins on line 1: {}",
            lines[at]
        );
        assert!(
            lines[at + 1].contains("▷") || lines[at + 1].contains("team forge"),
            "no second line: {}",
            lines[at + 1]
        );
    }
    let entries = &text[text.find("⚠ coder · broken").unwrap()..];
    for name in [
        "lang-typed",
        "lane-readers-docs",
        "spike",
        "landed",
        "lang-docs",
        "nightly",
        "spike the old parser",
        "fix the store",
        "type the language",
        "@",
    ] {
        assert!(!entries.contains(name), "{name}: {text}");
    }
    let runtime = text
        .lines()
        .find(|line| line.contains("ended 10m ago"))
        .unwrap();
    assert!(
        runtime.trim_end_matches('▐').trim_end().ends_with("30m"),
        "frozen at the end: {runtime}"
    );

    let cost_rolls = CostRolls::default();
    let pages = all_paged(&snapshot);
    let mut ctx = test_row_ctx(&snapshot, &theme, 60, 0, 0, &cost_rolls);
    let unpaged = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let earlier = line_texts(&unpaged.lines)
        .iter()
        .position(|line| line.contains("▸ +2 earlier"))
        .unwrap();
    assert!(matches!(
        unpaged
            .interactions
            .target_at(8, u16::try_from(earlier).unwrap()),
        Some(HitTarget::PageDelegation { .. })
    ));
    ctx.delegation_pages = &pages;
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
    let paged = lines.join("\n");
    let mut from = paged.find("once at 2026-11-14 03:00").unwrap();
    for needle in [
        "– coder · fix the store",
        "ended 5m ago · #1630 merged",
        "spike the old parser",
    ] {
        let at = paged[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} newest finish first:\n{paged}"));
        from += at + needle.len();
    }
    assert!(
        !paged.contains("earlier") && !paged.contains("more"),
        "{paged}"
    );
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

/// The `▷` header wears the tone of the `⧉` and `⧖` headers on every card shape that
/// draws the booked line, its label the body weight, while an unfired entry's `▷` lead
/// stays muted.
#[test]
fn the_booked_header_takes_the_section_header_tone_and_unfired_leads_stay_muted() {
    let theme = Theme::fixed(false);
    let meta = theme.component(Component::WaitHeader);
    assert_eq!(meta, theme.component(Component::SubagentHeader));
    let booked = theme.glyph(GlyphRole::CardBooked);
    let fresh = planner_snapshot_as(healthy_plan(), 60, true);
    let mut compact = planner_snapshot(healthy_plan(), 60);
    compact.theme.display.card_density = CardDensityMode::Compact;
    for (snapshot, selected) in [
        (planner_snapshot(healthy_plan(), 60), usize::MAX),
        (planner_snapshot(healthy_plan(), 172_800), usize::MAX),
        (fresh, usize::MAX),
        (compact, usize::MAX),
        (planner_snapshot(healthy_plan(), 60), 0),
    ] {
        let lines = group_lines_at_width(&snapshot, &theme, selected, 46);
        let line = lines
            .iter()
            .find(|line| line.to_string().contains("booked (7)"))
            .expect("the booked line");
        let at = line
            .spans
            .iter()
            .position(|span| span.content == booked)
            .unwrap();
        assert_eq!(line.spans[at].style.fg, Some(meta), "{line}");
        assert_eq!(line.spans[at + 1].content, " booked (7)");
        assert_eq!(line.spans[at + 1].style.fg, theme.body().fg, "{line}");
    }
    let open = group_lines_at_width(&planner_snapshot(healthy_plan(), 60), &theme, 0, 46);
    let unfired = open
        .iter()
        .find(|line| line.to_string().contains("document the language"))
        .expect("the unfired entry");
    let lead = unfired
        .spans
        .iter()
        .find(|span| span.content == booked)
        .unwrap();
    assert_eq!(lead.style.fg, theme.muted().fg, "{unfired}");
    assert_ne!(lead.style.fg, Some(meta));
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

/// The section line reads the entry next to fire: a timed task, never a task whose every
/// unmet term waits on that unfired sibling, though its name sorts first.
#[test]
fn the_booked_summary_reads_the_next_entry_to_fire_not_one_waiting_on_it() {
    let theme = Theme::fixed(false);
    let plan: Vec<SidebarBooking> = serde_json::from_value(serde_json::json!([
        {
            "task": "a",
            "starts": {"kind": "profile", "name": "writer"},
            "band": "next",
            "state": {
                "state": "waiting",
                "blocker": {
                    "after": {"state": "waiting", "starts": {"kind": "profile", "name": "coder"}},
                    "key": "pr[z]",
                },
                "after_unfired": true,
            },
        },
        {
            "task": "z",
            "starts": {"kind": "profile", "name": "coder"},
            "band": "next",
            "state": {"state": "waiting", "timing": "once at 2026-11-27 03:00"},
        },
    ]))
    .unwrap();
    let snapshot = planner_snapshot(plan, 60);
    let booked = line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, 60))
        .into_iter()
        .find(|line| line.contains('▷'))
        .unwrap();
    assert!(
        booked.contains("once at 2026-11-27 03:00") && !booked.contains("after"),
        "{booked}"
    );
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
            assert!(
                !text.contains("type the workflow"),
                "entries stay closed: {text}"
            );
        }
    }
    let finished = planner_snapshot(pick(&["landed"]), 60);
    for text in shapes(&finished) {
        assert!(!text.contains('▷'), "a finished plan draws nothing: {text}");
    }
    let open = line_texts(&group_lines_at_width(&finished, &theme, 0, 46)).join("\n");
    assert!(
        open.contains("▷ booked (1)") && open.contains("▸ +1 earlier"),
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
    let snapshot = planner_snapshot(plan, 60);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 46)).join("\n");
    assert!(text.contains("forge#lane-readers starting"), "{text}");
    assert!(!text.contains("after forge"), "{text}");
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
    let entry = text
        .find("coder · try the parser")
        .expect("the entry is drawn");
    let second = text[entry..].lines().nth(1).unwrap();
    let second = second.trim_start_matches('▌').trim_start();
    assert!(second.starts_with("starting"), "{text}");
    assert!(!text.contains("ended"), "{text}");
}

/// A sibling is named the way its own entry names it: an agent by its profile, a team by its
/// name and lane without the tag; an ended one reads `<who> ended`.
#[test]
fn a_sibling_is_named_by_its_own_who_text() {
    let theme = Theme::fixed(false);
    let ended = planner_snapshot(plan_with_an_ended_upstream(), 60);
    let text = line_texts(&group_lines_at_width(&ended, &theme, 0, 60)).join("\n");
    assert!(text.contains("forge#lane-readers ended · no PR"), "{text}");
    assert!(text.contains("– team forge#lane-readers"), "{text}");
    let mut plan = pick(&["lang-docs", "lang-release"]);
    plan[1].band = BookingBand::Next;
    let later = planner_snapshot(plan, 60);
    let text = line_texts(&group_lines_at_width(&later, &theme, 0, 60)).join("\n");
    assert!(text.contains("after writer"), "{text}");
}

/// Line 1 of a booking keeps one layout at every width: an agent's text clips from its tail
/// before its profile does, each step at its exact width and one column either side.
#[test]
fn a_booking_line_clips_its_text_before_its_profile_at_every_width() {
    let theme = Theme::fixed(false);
    let snapshot = planner_snapshot(pick(&["lang-docs"]), 60);
    let booked = theme.glyph(GlyphRole::CardBooked);
    let entry = |width: usize| {
        line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains(&format!("{booked} w")))
            .map(|line| {
                line.trim_start_matches('▌')
                    .trim_end_matches('▐')
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default()
    };
    let whole = format!("{booked} writer · document the language");
    let fits = (20..80).find(|width| entry(*width) == whole).unwrap();
    assert_eq!(entry(fits + 1), whole);
    assert_eq!(
        entry(fits - 1),
        format!("{booked} writer · document the languag")
    );
    // The profile alone: one column less clips the profile, and the seam draws only
    // with the headline's first cell.
    let bare = (10..fits)
        .find(|width| entry(*width) == format!("{booked} writer"))
        .unwrap();
    assert_eq!(entry(bare - 1), format!("{booked} write"));
    let first_cell = format!("{booked} writer · d");
    let headed = (bare..fits)
        .find(|width| entry(*width) == first_cell)
        .unwrap();
    assert_eq!(entry(headed - 1), format!("{booked} writer"));
    assert_eq!(entry(headed + 1), format!("{booked} writer · do"));
    for width in bare..fits {
        let line = entry(width);
        assert!(
            whole.starts_with(&line) && !line.ends_with('·'),
            "{width}: one layout, clipped from the tail: {line}"
        );
    }
}

/// Only a running team's stage pins the team's who: a team booking with a text headline
/// keeps its name and lane and clips the text from its tail, as an agent's line does.
#[test]
fn an_unfired_team_booking_clips_its_label_and_keeps_its_name() {
    let theme = Theme::fixed(false);
    let booked = theme.glyph(GlyphRole::CardBooked);
    let mut bookings = pick(&["lane-readers"]);
    bookings[0].band = BookingBand::Next;
    bookings[0].state = BookingState::Waiting(BookingWait::default());
    bookings[0].description =
        Some("read every lane of the workflow language and document each reader".to_owned());
    let snapshot = planner_snapshot(bookings, 60);
    for width in [46, 52] {
        let line = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains("· read"))
            .unwrap_or_else(|| panic!("{width}: no entry"));
        assert!(
            line.contains(&format!("{booked} team forge#lane-readers · read every")),
            "{width}: {line}"
        );
        assert!(!line.contains('…'), "{width}: {line}");
    }
}

/// A sibling's state word stays whole while its long name ellipsizes.
#[test]
fn a_long_sibling_name_clips_before_its_state_word() {
    let theme = Theme::fixed(false);
    let mut bookings = pick(&["lane-readers-docs"]);
    let BookingState::Waiting(wait) = &mut bookings[0].state else {
        panic!("waiting");
    };
    let after = wait.blocker.as_mut().unwrap().after.as_mut().unwrap();
    after.lane = Some("implementation-delegation".to_owned());
    after.state = SiblingState::Ended;
    let snapshot = planner_snapshot(bookings, 60);
    let line = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 40))
        .into_iter()
        .find(|line| line.contains("forge#"))
        .unwrap();
    assert!(line.contains("… ended"), "{line}");
}

/// A team entry's lane clips before its name, and a running team's stage and runtime
/// never clip.
#[test]
fn a_running_team_booking_clips_its_lane_and_keeps_its_stage() {
    let theme = Theme::fixed(false);
    let current = theme.glyph(GlyphRole::PipelineCurrent);
    let snapshot = planner_snapshot(pick(&["lane-readers"]), 60);
    let entry = |width: usize| {
        line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains(&format!("{current} Build")))
            .unwrap_or_else(|| panic!("{width}: no entry"))
    };
    let whole = (28..90)
        .find(|width| entry(*width).contains("team forge#lane-readers · "))
        .unwrap();
    assert!(entry(whole + 1).contains("team forge#lane-readers · "));
    assert!(
        entry(whole - 1).contains("team forge#lane-reade… · "),
        "{}",
        entry(whole - 1)
    );
    // `team forge…`: the lane is gone; one column less clips the name.
    let named = whole - "lane-readers".len();
    assert!(entry(named).contains("team forge… · "), "{}", entry(named));
    assert!(
        entry(named + 1).contains("team forge#… · "),
        "{}",
        entry(named + 1)
    );
    assert!(
        entry(named - 1).contains("team forg… · "),
        "{}",
        entry(named - 1)
    );
    for width in 28..whole {
        let line = entry(width);
        assert!(
            line.contains(&format!("{current} Build")),
            "{width}: {line}"
        );
        assert!(
            line.trim_end_matches('▐').trim_end().ends_with("50m"),
            "{width}: {line}"
        );
    }
}
