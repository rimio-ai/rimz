//! Shared type-led delegation entry, with a pinned right cluster and optional detail.

use super::*;

pub(super) struct Entry {
    pub(super) focused: bool,
    pub(super) lead: Span<'static>,
    /// Who the entry names: a type word, an agent's profile and lane, or a team.
    pub(super) who: Vec<Span<'static>>,
    pub(super) headline: Vec<Span<'static>>,
    /// The who text ellipsizes to the room the headline and right cluster leave, so a
    /// team's stage never clips; otherwise the line clips from its tail.
    pub(super) clip_who: bool,
    pub(super) right: Vec<Span<'static>>,
    pub(super) detail: Option<Line<'static>>,
}

pub(super) fn push_entry(ctx: &RowCtx<'_>, lines: &mut Vec<Line<'static>>, entry: Entry) {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let mut left = entry_indent(theme, entry.focused);
    left.extend([entry.lead, Span::raw(" ")]);
    let seam = Span::styled(value_seam(theme), theme.muted());
    let room = match spans_width(&entry.right) {
        0 => width,
        right => width.saturating_sub(right + 1),
    };
    let who = if entry.clip_who {
        let tail = seam.width() + spans_width(&entry.headline);
        match room.saturating_sub(spans_width(&left) + tail) {
            0 => Vec::new(),
            who_room => ellipsize_spans(entry.who, who_room),
        }
    } else {
        entry.who
    };
    let seam = (spans_width(&who) > 0).then_some(seam);
    left.extend(who);
    // The seam goes with a headline whose first character gets no cells, so it never ends
    // the line; a who with no room takes its seam with it.
    let first = entry
        .headline
        .iter()
        .flat_map(|span| span.content.chars())
        .next()
        .map_or(0, |first| text_width(first.encode_utf8(&mut [0; 4])));
    let seam_width = seam.as_ref().map_or(0, Span::width);
    if first > 0 && spans_width(&left) + seam_width + first <= room {
        left.extend(seam);
        left.extend(entry.headline);
    }
    lines.push(pin_right(left, entry.right, width));
    if let Some(detail) = entry.detail {
        lines.push(detail);
    }
}

/// An agent's who: its name, then `#lane` when it runs in a named lane.
pub(super) fn agent_who(
    theme: &Theme,
    name: Span<'static>,
    lane: Option<&str>,
) -> Vec<Span<'static>> {
    let mut who = vec![name];
    who.extend(lane_span(theme, lane));
    who
}

/// A team's who: the muted `team` tag, then its name and `#lane` when known.
pub(super) fn team_who(
    theme: &Theme,
    team: Option<&str>,
    lane: Option<&str>,
) -> Vec<Span<'static>> {
    let mut who = vec![Span::styled(
        "team",
        theme.styled(Component::EntryTeamTag, Modifier::empty()),
    )];
    if let Some(team) = team {
        who.push(Span::raw(" "));
        who.push(Span::styled(
            team.to_owned(),
            theme.styled(Component::EntryTeamName, Modifier::empty()),
        ));
        who.extend(lane_span(theme, lane));
    }
    who
}

fn lane_span(theme: &Theme, lane: Option<&str>) -> Option<Span<'static>> {
    lane.map(|lane| {
        Span::styled(
            format!("#{lane}"),
            theme.styled(Component::EntryLane, Modifier::empty()),
        )
    })
}

/// A board stage behind its mark: `◉ Build`, or the done mark once the board is `Done`.
pub(super) fn stage_spans(theme: &Theme, stage: &str, done: bool) -> Vec<Span<'static>> {
    let mark = if done {
        GlyphRole::PipelineDone
    } else {
        GlyphRole::PipelineCurrent
    };
    vec![
        Span::styled(
            theme.glyph(mark).to_owned(),
            theme.styled(Component::EntryStage, Modifier::empty()),
        ),
        Span::styled(format!(" {stage}"), theme.body()),
    ]
}

/// `spans` clipped from the tail to `room` cells, ending in `…` in the last kept style.
fn ellipsize_spans(spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= room {
        return spans;
    }
    let mut kept = trim_spans_to_width(spans, room.saturating_sub(1));
    let style = kept.last().map(|span| span.style).unwrap_or_default();
    kept.push(Span::styled("…", style));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::render::odometer::CostRolls;
    use crate::sidebar_pane::render::tests::{snapshot_with, test_row_ctx};

    /// Line 1 of `entry` at `width` pane columns, as text.
    fn drawn(width: usize, entry: impl Fn(&Theme) -> Entry) -> String {
        let snapshot = snapshot_with(Vec::new());
        let theme = Theme::fixed(false);
        let cost_rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, width, 0, 0, &cost_rolls);
        let mut lines = Vec::new();
        push_entry(&ctx, &mut lines, entry(&theme));
        lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn entry(who: Vec<Span<'static>>, headline: Vec<Span<'static>>, clip_who: bool) -> Entry {
        Entry {
            focused: false,
            lead: Span::raw(">"),
            who,
            headline,
            clip_who,
            right: Vec::new(),
            detail: None,
        }
    }

    /// Once the who has no cell left, a live team's stage keeps every cell: the who and its
    /// seam go, never a cell of the stage to an ellipsis.
    #[test]
    fn a_team_stage_keeps_every_cell_once_the_who_has_no_room() {
        let team = |width: usize| {
            drawn(width, |theme| Entry {
                right: vec![Span::raw("50m")],
                ..entry(
                    team_who(theme, Some("forge"), Some("docs")),
                    stage_spans(theme, "Build", false),
                    true,
                )
            })
        };
        let stage = format!(
            "{} Build",
            Theme::fixed(false).glyph(GlyphRole::PipelineCurrent)
        );
        let last = (1..60)
            .find(|width| team(*width).contains(&format!("> … · {stage}")))
            .unwrap();
        assert!(
            team(last + 1).contains(&format!("> t… · {stage}")),
            "{}",
            team(last + 1)
        );
        let gone = team(last - 1);
        assert!(
            gone.contains(&format!("> {stage}")) && gone.trim_end().ends_with("50m"),
            "{gone}"
        );
        assert!(!gone.contains('…') && !gone.contains('·'), "{gone}");
    }

    /// A seam draws only with the headline's whole first character, a wide one included.
    #[test]
    fn a_wide_first_headline_character_brings_its_seam_only_when_it_fits() {
        let line = |width: usize| {
            drawn(width, |theme| {
                entry(
                    agent_who(theme, Span::raw("coder"), None),
                    vec![Span::raw("漢字")],
                    false,
                )
            })
            .trim()
            .to_owned()
        };
        let first = (1..60)
            .find(|width| line(*width).ends_with("· 漢"))
            .unwrap();
        assert_eq!(line(first - 1), "> coder");
        assert!(
            line(first + 1).ends_with("coder · 漢"),
            "{}",
            line(first + 1)
        );
        assert!(
            line(first + 2).ends_with("coder · 漢字"),
            "{}",
            line(first + 2)
        );
    }
}
