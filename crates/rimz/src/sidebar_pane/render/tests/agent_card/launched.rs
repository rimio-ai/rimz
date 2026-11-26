use super::*;

use crate::agents::BackgroundShell;
use crate::sidebar_pane::render::layout::text_width;
use crate::sidebar_pane::render::theme::Component;
use crate::store::snapshot::{
    BookingLive, BookingPr, BookingStage, BookingStarts, SidebarBooking, SidebarLaunched,
};

fn ago(secs: u64) -> Timestamp {
    fixed_now() - Duration::from_secs(secs)
}

fn live(status: AgentStatus, pane: &str, description: Option<&str>) -> BookingLive {
    BookingLive {
        status,
        phase: crate::agents::TurnPhase::Acting,
        stage: None,
        description: description.map(ToOwned::to_owned),
        pane: Some(PaneId::from_parts(MuxName::Tmux, pane)),
    }
}

fn entry(starts: BookingStarts, lane: Option<&str>, at: u64) -> SidebarLaunched {
    SidebarLaunched {
        starts,
        lane: lane.map(ToOwned::to_owned),
        description: None,
        launches: vec![format!("launch-{at}")],
        at: ago(at),
        live: None,
        starting: false,
        ended_at: None,
        pr: BookingPr::Unread,
        team: None,
    }
}

fn coder() -> BookingStarts {
    BookingStarts::Profile("coder".to_owned())
}

/// A running peer, a running team at its board stage, a reviewer that ended ten minutes ago
/// with its pull request open, and a writer that ended two hours ago.
fn launches() -> Vec<SidebarLaunched> {
    vec![
        SidebarLaunched {
            live: Some(live(AgentStatus::Running, "%9", Some("store refactor"))),
            ..entry(coder(), Some("feat-x"), 25 * 60)
        },
        SidebarLaunched {
            live: Some(BookingLive {
                stage: Some(BookingStage {
                    stage: "Build".to_owned(),
                    done: false,
                }),
                ..live(AgentStatus::Running, "%10", None)
            }),
            ..entry(
                BookingStarts::Team("forge".to_owned()),
                Some("lane-readers"),
                3 * 3_600,
            )
        },
        SidebarLaunched {
            description: Some("review the parser".to_owned()),
            ended_at: Some(ago(10 * 60)),
            pr: BookingPr::Open { number: Some(12) },
            ..entry(BookingStarts::Profile("reviewer".to_owned()), None, 40 * 60)
        },
        SidebarLaunched {
            description: Some("document the lanes".to_owned()),
            ended_at: Some(ago(2 * 3_600)),
            ..entry(BookingStarts::Profile("writer".to_owned()), None, 4 * 3_600)
        },
    ]
}

fn lead_snapshot(launched: Vec<SidebarLaunched>) -> SidebarSnapshot {
    let mut lead = agent(
        "lead",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("Split the store"),
    );
    lead.prompt = Some("split the store".to_owned());
    lead.model = Some("opus".to_owned());
    lead.usage.context_pct = Some(1);
    lead.usage.context_window = Some(200_000);
    let mut snapshot = snapshot_with(vec![lead]);
    let row = snapshot.rows_mut().next().unwrap();
    row.unread = false;
    row.last_activity = fixed_now() - Duration::from_secs(60);
    row.as_agent_mut().unwrap().launched = launched;
    snapshot
}

fn screen(snapshot: &SidebarSnapshot, ui: &ViewState, width: u16) -> String {
    snapshot_to_screen_with_alert_and_ui(snapshot, None, ui, &UiState::default(), width, 34)
}

#[test]
fn launched_cards_render_at_46_40_and_28_columns() {
    let snapshot = lead_snapshot(launches());
    let rest = ViewState {
        selected_index: Some(usize::MAX),
        ..Default::default()
    };
    let selected = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    for (name, ui) in [
        ("rest", rest),
        ("selected", selected),
        ("selected_paged", paged_view(&snapshot, 0)),
    ] {
        for width in [46, 40, 28] {
            assert_snapshot(
                &format!("launched_{name}_{width}"),
                screen(&snapshot, &ui, width),
            );
        }
    }
}

/// A running entry draws on one line, named by profile and lane or by team and lane, with
/// its card's text or its board stage and its runtime; it sits after the waits and before
/// the booked entries, and a click on it focuses its pane. A finished one waits behind the
/// earlier line, and paged in draws its own text over when it ended and its pull request,
/// newest end first among every group's finished entries.
#[test]
fn launched_entries_list_between_waits_and_booked_and_page_when_finished() {
    let mut snapshot = lead_snapshot(launches());
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.background_shells.push(BackgroundShell {
        id: "shell".into(),
        command: Some("cargo test".into()),
        description: None,
        started_at: fixed_now(),
    });
    let mut booked: SidebarBooking = serde_json::from_value(serde_json::json!({
        "task": "docs",
        "starts": {"kind": "profile", "name": "scribe"},
        "band": "next",
        "state": {"state": "waiting", "timing": "once at 2026-11-27 03:00"},
    }))
    .unwrap();
    booked.description = Some("write the docs".to_owned());
    card.bookings = vec![booked];
    let theme = Theme::fixed(false);
    let current = theme.glyph(GlyphRole::PipelineCurrent);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 60)).join("\n");
    let team = format!("team forge#lane-readers · {current} Build");
    let mut from = 0;
    for needle in [
        "▶ launched (4)",
        "cargo test",
        "coder#feat-x · store refactor",
        &team,
        "scribe · write the docs",
        "▸ +2 earlier",
    ] {
        let at = text[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} after byte {from}:\n{text}"));
        from += at + needle.len();
    }
    for hidden in ["reviewer", "writer"] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    let runtime = text
        .lines()
        .find(|line| line.contains("store refactor"))
        .unwrap();
    assert!(
        runtime.trim_end_matches('▐').trim_end().ends_with("25m"),
        "the runtime pins on line 1: {runtime}"
    );

    let cost_rolls = CostRolls::default();
    let pages = all_paged(&snapshot);
    let mut ctx = test_row_ctx(&snapshot, &theme, 60, 0, 0, &cost_rolls);
    ctx.delegation_pages = &pages;
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let lines = line_texts(&block.lines);
    let target = |needle: &str| {
        let line = lines.iter().position(|line| line.contains(needle)).unwrap();
        block
            .interactions
            .target_at(8, u16::try_from(line).unwrap())
    };
    assert_eq!(
        target("store refactor"),
        Some(HitTarget::SubAgentPane(PaneId::from_parts(
            MuxName::Tmux,
            "%9"
        )))
    );
    let paged = lines.join("\n");
    let mut from = paged.find("scribe · write the docs").unwrap();
    for needle in [
        "– reviewer · review the parser",
        "ended 10m ago · #12 open",
        "– writer · document the lanes",
        "ended 2h ago",
    ] {
        let at = paged[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} newest end first:\n{paged}"));
        from += at + needle.len();
    }
    assert!(!paged.contains("earlier"), "{paged}");
    let selected = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    assert_eq!(
        selected.entry_stops(&snapshot, 0),
        [
            EntryCursor::SubAgent(PaneId::from_parts(MuxName::Tmux, "%9")),
            EntryCursor::SubAgent(PaneId::from_parts(MuxName::Tmux, "%10")),
            EntryCursor::DelegationPage,
        ]
    );
}

/// The `▶` header wears the section headers' tone. On an unselected card's closed section
/// the half draws while an entry runs and counts every entry; with every entry finished the
/// line drops it until the card is selected.
#[test]
fn the_launched_half_draws_closed_only_while_an_entry_runs() {
    let theme = Theme::fixed(false);
    let half = |snapshot: &SidebarSnapshot, selected: usize| {
        line_texts(&group_lines_at_width(snapshot, &theme, selected, 46))
            .into_iter()
            .find(|line| line.contains('▶'))
    };
    let running = lead_snapshot(launches());
    assert!(
        half(&running, usize::MAX).is_some_and(|line| line.contains("▶ launched (4)")),
        "{:?}",
        half(&running, usize::MAX)
    );
    let finished = lead_snapshot(launches().split_off(2));
    assert_eq!(half(&finished, usize::MAX), None);
    let open = half(&finished, 0).expect("the open section's line");
    assert!(open.contains("▶ launched (2)"), "{open}");
    let lines = group_lines_at_width(&running, &theme, usize::MAX, 46);
    let header = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == theme.glyph(GlyphRole::CardLaunched))
        .unwrap();
    assert_eq!(
        header.style.fg,
        Some(theme.component(Component::LaunchedHeader))
    );
    assert_eq!(
        theme.component(Component::LaunchedHeader),
        theme.component(Component::WaitHeader)
    );
}

/// A pin closing a selected card's section keeps its line and the toggle that reopens it
/// while the card holds only finished launched entries or only finished bookings.
#[test]
fn a_pin_closed_selected_section_keeps_its_line_over_finished_entries() {
    let theme = Theme::fixed(false);
    let rolls = CostRolls::default();
    let launched = lead_snapshot(launches().split_off(2));
    let mut booked = lead_snapshot(Vec::new());
    booked
        .rows_mut()
        .next()
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .bookings = crate::sidebar::test_support::bookings_in_each_state(fixed_now())
        .into_iter()
        .filter(|booking| booking.task == "landed")
        .collect();
    for (snapshot, needle) in [(&launched, "▶ launched (2)"), (&booked, "▷ booked (1)")] {
        let row = snapshot.worktree_groups[0].rows[0].id.clone();
        let pins = std::collections::BTreeMap::from([(
            row.clone(),
            DelegationPin {
                open: false,
                turn: None,
            },
        )]);
        let mut ctx = test_row_ctx(snapshot, &theme, 46, 0, 0, &rolls);
        ctx.delegation_overrides = &pins;
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        let lines = line_texts(&block.lines);
        let text = lines.join("\n");
        let at = lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle}:\n{text}"));
        assert_eq!(
            block.interactions.target_at(8, u16::try_from(at).unwrap()),
            Some(HitTarget::ToggleDelegation { row, open: true }),
            "{text}"
        );
        assert!(
            !text.contains("earlier"),
            "the section stays closed:\n{text}"
        );
    }
}

/// A team the card launched and waits on lists once, under launched, and its wait neither
/// draws nor counts in `⧖ waits`; a wait on a team the card did not launch still does.
#[test]
fn a_team_the_card_launched_and_waits_on_lists_once_under_launched() {
    let theme = Theme::fixed(false);
    let mut launched = launches();
    launched[1].team = Some("forge#lane-readers".parse().unwrap());
    let mut snapshot = lead_snapshot(launched);
    let team_wait = |name: &str| crate::agents::PendingWait {
        class: crate::agents::PendingWaitClass::Working,
        name: name.to_owned(),
        armed_at: Some(ago(60)),
        trigger: crate::agents::PendingWaitTrigger::Team {
            stage: Some("Build".to_owned()),
        },
    };
    let text = |snapshot: &SidebarSnapshot| {
        line_texts(&group_lines_at_width(snapshot, &theme, 0, 60)).join("\n")
    };
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.pending_waits = vec![team_wait("forge#lane-readers")];
    let once = text(&snapshot);
    assert!(!once.contains('⧖'), "{once}");
    assert_eq!(once.matches("forge#lane-readers").count(), 1, "{once}");
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.pending_waits.push(team_wait("forge#other"));
    let other = text(&snapshot);
    assert!(
        other.contains("⧖ waits (1)") && other.contains("forge#other"),
        "{other}"
    );
}

/// The section line's halves in order, `⧉ · ⧖ · ▶ · ▷`, take their long labels exactly
/// where the whole line fits and their short forms one column narrower.
#[test]
fn the_launched_half_sits_between_waits_and_booked_and_shortens_at_its_drop_width() {
    let theme = Theme::fixed(false);
    let mut snapshot = lead_snapshot(launches());
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.sub_agents = (0..2)
        .map(|index| super::super::budget::sub_agent("lead", index))
        .collect();
    card.background_shells.push(BackgroundShell {
        id: "shell".into(),
        command: Some("cargo test".into()),
        description: None,
        started_at: fixed_now(),
    });
    card.bookings = vec![
        serde_json::from_value(serde_json::json!({
            "task": "docs",
            "starts": {"kind": "profile", "name": "scribe"},
            "band": "next",
            "state": {"state": "waiting"},
        }))
        .unwrap(),
    ];
    let line = |width: usize| {
        line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))
            .into_iter()
            .find(|line| line.contains('⧉'))
            .unwrap()
    };
    let long = "  ⧉ subagents (2) · ⧖ waits (1) · ▶ launched (4) · ▷ booked (1)";
    let drop = text_width(long) + 2;
    assert!(line(drop + 1).contains(long), "{}", line(drop + 1));
    assert!(line(drop).contains(long), "{}", line(drop));
    let short = line(drop - 1);
    assert!(
        short.contains("  ⧉ 2 · ⧖ 1 · ▶ 4 · ▷ 1 "),
        "short labels one column narrower: {short}"
    );
    assert_eq!(drop, 65, "{long}");
}
