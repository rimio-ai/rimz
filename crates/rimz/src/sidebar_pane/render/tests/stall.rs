use super::*;
use crate::ids::LinkTier;
use crate::store::snapshot::{
    SidebarLinkFreshness, SidebarLinkHealth, SidebarPresence, SidebarStallItem,
    SidebarStallPhase::{Active, Fading},
    SidebarStallResource::{Cpu, Io, Memory},
};

fn stalled(
    items: &[(
        crate::store::snapshot::SidebarStallResource,
        u8,
        crate::store::snapshot::SidebarStallPhase,
    )],
) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.stall = items
        .iter()
        .map(|&(resource, percent, phase)| SidebarStallItem {
            resource,
            percent,
            phase,
        })
        .collect();
    snapshot
}

fn remote(rtt: Option<u32>, loss: u16, freshness: SidebarLinkFreshness) -> SidebarLinkHealth {
    SidebarLinkHealth {
        rtt_ms: rtt,
        miss_pct: loss,
        tier: crate::remote::link::link_tier(rtt, loss),
        freshness,
        sampled_at_ms: 1_700_000_000_000,
    }
}

fn footer(snapshot: &SidebarSnapshot, width: usize, nerd: bool) -> Line<'static> {
    let mut config = snapshot.theme.clone();
    config.glyphs = toml::from_str(if nerd {
        "set = 'nerd_font'"
    } else {
        "set = 'unicode'"
    })
    .unwrap();
    let theme = Theme::fixed_for_theme(true, &config);
    super::super::chrome::footer_lines(snapshot, &theme, width).remove(0)
}

fn expected(width: usize, left: &str, help: &str) -> String {
    if help.is_empty() {
        return left.trim_end().to_owned();
    }
    format!(
        "{left}{}{help}",
        " ".repeat(width - layout::text_width(left) - layout::text_width(help))
    )
}

fn transition(
    cases: &mut Vec<(String, String)>,
    snapshot: &SidebarSnapshot,
    first_drop: usize,
    after: (&str, &str),
    before: (&str, &str),
) {
    for (width, (left, help)) in [
        (first_drop - 1, after),
        (first_drop, after),
        (first_drop + 1, before),
    ] {
        cases.push((
            footer(snapshot, width, false)
                .to_string()
                .trim_end()
                .to_owned(),
            expected(width, left, help),
        ));
    }
}

fn assert_cases(cases: Vec<(String, String)>) {
    let (actual, wanted): (Vec<_>, Vec<_>) = cases.into_iter().unzip();
    assert_eq!(actual, wanted);
}

#[test]
fn healthy_anchor_presence_and_one_stall_drop_at_exact_widths() {
    let mut snapshot = stalled(&[(Memory, 34, Active)]);
    snapshot.link = Some(remote(Some(236), 0, SidebarLinkFreshness::Fresh));
    snapshot.presence = Some(SidebarPresence::Detached);
    let full = "⇄ 236ms  ⧗ stall mem 34%";
    let bare = "⇄  ⧗ stall mem 34%";
    let glyph = "⇄  ⧗ mem 34%";
    let mut cases = Vec::new();
    for (width, after, before) in [
        (
            44,
            (full, "? for help"),
            ("⇄ 236ms  ⧗ stall mem 34%  zᶻ away", "? for help"),
        ),
        (35, (full, "?"), (full, "? for help")),
        (26, (bare, "?"), (full, "?")),
        (20, (bare, ""), (bare, "?")),
        (17, (glyph, "?"), (bare, "")),
        (14, (glyph, ""), (glyph, "?")),
    ] {
        transition(&mut cases, &snapshot, width, after, before);
    }
    for (width, wanted) in [
        (10, "⇄  ⧗ mem 3"),
        (11, "⇄  ⧗ mem 34"),
        (12, "⇄  ⧗ mem 34%"),
    ] {
        cases.push((
            footer(&snapshot, width, false).to_string(),
            wanted.to_owned(),
        ));
    }
    assert_cases(cases);
}

#[test]
fn bad_anchor_survives_item_collapse_and_right_edge_clip() {
    let mut snapshot = stalled(&[(Memory, 34, Active), (Cpu, 81, Active)]);
    snapshot.link = Some(remote(Some(620), 12, SidebarLinkFreshness::Fresh));
    assert_eq!(snapshot.link.as_ref().unwrap().tier, LinkTier::Bad);
    let full = "⇄ 620ms 12%  ⧗ stall mem 34% · cpu 81%";
    let named = "⇄ 620ms 12%  ⧗ stall mem 34% +cpu";
    let numbered = "⇄ 620ms 12%  ⧗ stall mem 34% +1";
    let glyph = "⇄ 620ms 12%  ⧗ mem 34% +1";
    let mut cases = Vec::new();
    for (width, after, before) in [
        (49, (full, "?"), (full, "? for help")),
        (40, (full, ""), (full, "?")),
        (37, (named, "?"), (full, "")),
        (35, (named, ""), (named, "?")),
        (32, (numbered, ""), (named, "")),
        (30, (glyph, "?"), (numbered, "")),
        (27, (glyph, ""), (glyph, "?")),
    ] {
        transition(&mut cases, &snapshot, width, after, before);
    }
    for (width, wanted) in [
        (23, "⇄ 620ms 12%  ⧗ mem 34% "),
        (24, "⇄ 620ms 12%  ⧗ mem 34% +"),
        (25, glyph),
    ] {
        cases.push((
            footer(&snapshot, width, false).to_string(),
            wanted.to_owned(),
        ));
    }
    assert_cases(cases);
}

#[test]
fn local_two_and_three_stalls_keep_named_then_numeric_overflow() {
    let mut cases = Vec::new();
    for (items, full, first_help_drop, first_collapse, overflow) in [
        (
            vec![(Memory, 34, Active), (Cpu, 81, Active)],
            "⧗ stall mem 34% · cpu 81%",
            36,
            24,
            "+cpu",
        ),
        (
            vec![(Memory, 34, Active), (Cpu, 81, Active), (Io, 51, Active)],
            "⧗ stall mem 34% · cpu 81% · io 51%",
            45,
            33,
            "+2",
        ),
    ] {
        let snapshot = stalled(&items);
        let collapsed = format!("⧗ stall mem 34% {overflow}");
        transition(
            &mut cases,
            &snapshot,
            first_help_drop,
            (full, "?"),
            (full, "? for help"),
        );
        transition(
            &mut cases,
            &snapshot,
            first_help_drop - 9,
            (full, ""),
            (full, "?"),
        );
        transition(
            &mut cases,
            &snapshot,
            first_collapse,
            (&collapsed, "?"),
            (full, ""),
        );
        if items.len() == 2 {
            transition(
                &mut cases,
                &snapshot,
                22,
                (&collapsed, ""),
                (&collapsed, "?"),
            );
            transition(
                &mut cases,
                &snapshot,
                19,
                ("⧗ stall mem 34% +1", ""),
                (&collapsed, ""),
            );
        } else {
            transition(
                &mut cases,
                &snapshot,
                20,
                (&collapsed, ""),
                (&collapsed, "?"),
            );
        }
        let numbered = format!("⧗ stall mem 34% +{}", items.len() - 1);
        let glyph = format!("⧗ mem 34% +{}", items.len() - 1);
        transition(&mut cases, &snapshot, 17, (&glyph, "?"), (&numbered, ""));
        transition(&mut cases, &snapshot, 14, (&glyph, ""), (&glyph, "?"));
        for (width, wanted) in [
            (10, "⧗ mem 34% "),
            (11, "⧗ mem 34% +"),
            (12, glyph.as_str()),
        ] {
            cases.push((
                footer(&snapshot, width, false).to_string(),
                wanted.to_owned(),
            ));
        }
    }
    assert_cases(cases);
}

#[test]
fn stale_anchor_and_fading_marker_survive_until_the_final_clip() {
    let mut snapshot = stalled(&[(Memory, 6, Fading)]);
    snapshot.link = Some(remote(Some(236), 0, SidebarLinkFreshness::Stale));
    let full = "⇄ stale  ⧗ stall mem 6%↓";
    let glyph = "⇄ stale  ⧗ mem 6%↓";
    let mut cases = Vec::new();
    for (width, after, before) in [
        (35, (full, "?"), (full, "? for help")),
        (26, (full, ""), (full, "?")),
        (23, (glyph, "?"), (full, "")),
        (20, (glyph, ""), (glyph, "?")),
    ] {
        transition(&mut cases, &snapshot, width, after, before);
    }
    for (width, wanted) in [
        (16, "⇄ stale  ⧗ mem 6"),
        (17, "⇄ stale  ⧗ mem 6%"),
        (18, glyph),
    ] {
        cases.push((
            footer(&snapshot, width, false).to_string(),
            wanted.to_owned(),
        ));
    }
    assert_cases(cases);
}

#[test]
fn representative_forty_column_frames_match_in_both_glyph_sets() {
    let cases = [
        (Some((236, 0)), vec![], "⇄ 236ms", "? for help"),
        (
            Some((236, 0)),
            vec![(Memory, 34, Active)],
            "⇄ 236ms  ⧗ stall mem 34%",
            "? for help",
        ),
        (
            Some((236, 0)),
            vec![(Memory, 34, Active), (Cpu, 81, Active)],
            "⇄ 236ms  ⧗ stall mem 34% · cpu 81%",
            "?",
        ),
        (Some((620, 0)), vec![], "⇄ 620ms", "? for help"),
        (
            Some((620, 12)),
            vec![(Memory, 34, Active)],
            "⇄ 620ms 12%  ⧗ stall mem 34%",
            "?",
        ),
        (
            Some((236, 0)),
            vec![(Cpu, 64, Active), (Memory, 6, Fading)],
            "⇄ 236ms  ⧗ stall cpu 64% · mem 6%↓",
            "?",
        ),
        (
            None,
            vec![(Cpu, 92, Active)],
            "⧗ stall cpu 92%",
            "? for help",
        ),
        (
            None,
            vec![(Memory, 6, Fading), (Cpu, 31, Fading)],
            "⧗ stall mem 6%↓ · cpu 31%↓",
            "? for help",
        ),
    ];
    let mut actual = Vec::new();
    let mut wanted = Vec::new();
    for nerd in [false, true] {
        for (link, items, left, help) in &cases {
            let mut snapshot = stalled(items);
            snapshot.link =
                link.map(|(rtt, loss)| remote(Some(rtt), loss, SidebarLinkFreshness::Fresh));
            actual.push(footer(&snapshot, 38, nerd).to_string());
            let left = if nerd {
                left.replace('⇄', "\u{ede3}").replace('⧗', "\u{f0f86}")
            } else {
                (*left).to_owned()
            };
            wanted.push(expected(38, &left, help));
        }
    }
    assert_eq!(actual, wanted);
}

#[test]
fn stall_roles_keep_caution_body_muted_and_faint_with_no_color_weights() {
    let mut snapshot = stalled(&[(Memory, 34, Active), (Cpu, 31, Fading)]);
    snapshot.theme = toml::from_str("caution=1\nmuted=2\nfaint=3\nbody=4\n").unwrap();
    let mut actual = Vec::new();
    let mut wanted = Vec::new();
    for no_color in [false, true] {
        let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
        let full = super::super::chrome::footer_lines(&snapshot, &theme, 60).remove(0);
        let narrow = super::super::chrome::footer_lines(&snapshot, &theme, 25).remove(0);
        for (line, text, color, weight) in [
            (&full, "⧗ stall", 1, Modifier::BOLD),
            (&full, "mem ", 4, Modifier::empty()),
            (&full, "34%", 1, Modifier::BOLD),
            (
                &full,
                " · ",
                3,
                if no_color {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                },
            ),
            (
                &full,
                "cpu 31%↓",
                2,
                if no_color {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                },
            ),
            (
                &narrow,
                " +cpu",
                2,
                if no_color {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                },
            ),
        ] {
            actual.push(
                line.spans
                    .iter()
                    .find(|span| span.content == text)
                    .map(|span| span.style),
            );
            let style = Style::default().add_modifier(weight);
            wanted.push(Some(if no_color {
                style
            } else {
                style.fg(Color::Indexed(color))
            }));
        }
        snapshot.stall[0].phase = Fading;
        snapshot.stall[0].percent = 6;
        let faded = super::super::chrome::footer_lines(&snapshot, &theme, 60).remove(0);
        actual.push(
            faded
                .spans
                .iter()
                .find(|span| span.content == "⧗ stall")
                .map(|span| span.style),
        );
        wanted.push(Some(if no_color {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::Indexed(2))
        }));
        snapshot.stall[0].phase = Active;
        snapshot.stall[0].percent = 34;
    }
    assert_eq!(actual, wanted);
}

#[test]
fn local_empty_footer_keeps_full_help_from_ten_cells_and_short_help_below() {
    let snapshot = stalled(&[]);
    let actual: Vec<_> = (0..=60)
        .map(|width| footer(&snapshot, width, false).to_string())
        .collect();
    let wanted: Vec<_> = (0..=60)
        .map(|width| {
            let help = if width >= 10 {
                "? for help"
            } else if width > 0 {
                "?"
            } else {
                ""
            };
            expected(width, "", help)
        })
        .collect();
    assert_eq!(actual, wanted);
}
