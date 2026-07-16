//! The pinned provider dashboard — per-provider header, brand emblem, stats and
//! budget bars — and the W/M fleet store rows.

use crate::agents::{ExtraCredits, RateLimitWindow};
use crate::config::{BudgetBarConfig, GlyphRole};
use crate::sidebar_pane::pets::{PetBody, PetView};
use crate::{RemoteControlBadge, SidebarProviderPanel, SpendTally, SpendWindow};
use jiff::{SignedDuration, Timestamp};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::{
    dollars_cap, dollars2, reset_countdown, tokens_int, tokens_short, window_label,
};
use crate::sidebar_pane::render::labels::{
    TokenColumns, TokenDetail, mana_bar_spans, mana_style, pace_reading, pace_style,
    token_breakdown_spans, unknown_mana_bar_spans,
};
use crate::sidebar_pane::render::layout::{clip, pad_line_to, spans_width, text_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::sidebar_pane::render::{HitRegion, HitTarget};

use super::{pin_right, trim_spans_to_width};

/// The provider dashboard's fixed art column width: the brand emblem is padded
/// to this many cells so the bar column to its right starts at one shared cell
/// for every provider block — the bars align across providers by construction.
/// Dropped (bars run full-width) below [`PROVIDER_ART_MIN_WIDTH`].
const PROVIDER_ART_WIDTH: usize = 9;

/// Emblem rows the body art column holds; an earlier row (the crest) rides the
/// chrome line directly above the body — the header when it reserves the art
/// column, the wide spacer otherwise — so four-row art stays height-neutral.
const PROVIDER_ART_BODY_ROWS: usize = 3;

/// Narrowest sidebar that still affords the art column beside a bar; below it
/// the emblem is dropped so the bar keeps a legible length.
const PROVIDER_ART_MIN_WIDTH: usize = 34;

/// Widths that choose the provider body density. Wide keeps the historical
/// one-line stats row. Normal keeps that row in the narrower provider column.
/// Narrow also drops input/output token splits.
const PROVIDER_WIDE_MIN_WIDTH: usize = 52;
const PROVIDER_NORMAL_MIN_WIDTH: usize = 40;

/// Blank cells between the active provider block and the pet column when the
/// pet rides beside the dashboard.
const PET_COLUMN_GAP: usize = 1;

/// Pet captions leave three trailing cells so their right edge lines up with the
/// sprite body's inner gap.
const PET_CAPTION_RIGHT_PAD: usize = 3;

/// The provider bar's label slot (`5h` / `7d` / `30d` / `ex` / `api`) and value
/// column, shared by every provider bar so they align front and back. The label
/// fits three cells (`30d`); the value holds `↻ ` plus a six-cell reset countdown
/// slot (`↻  4h50m` / `↻ 20h20m`), an unknown slot, or a compact paid-usage
/// value.
const PROVIDER_LABEL_WIDTH: usize = 3;
const PROVIDER_VALUE_WIDTH: usize = 8;
const PROVIDER_BAR_ROW_FRAME_WIDTH: usize = 13;
const PROVIDER_RESET_COUNTDOWN_WIDTH: usize = 6;
const PROVIDER_RESET_MARKER_PAD: usize =
    PROVIDER_VALUE_WIDTH.saturating_sub(3 + PROVIDER_RESET_COUNTDOWN_WIDTH);
const RESET_RED_H: f64 = 24.0;
const RESET_AMBER_H: f64 = 48.0;
const RESET_YELLOW_H: f64 = 72.0;
const RESET_GREEN_H: f64 = 168.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderLayout {
    Wide,
    Normal,
    Narrow,
}

impl ProviderLayout {
    fn for_width(width: usize, allow_wide: bool) -> Self {
        if allow_wide && width >= PROVIDER_WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= PROVIDER_NORMAL_MIN_WIDTH {
            Self::Normal
        } else {
            Self::Narrow
        }
    }

    fn inline_art(self) -> bool {
        self != Self::Wide
    }
}

/// The fallback fleet store rows pinned below no-table dashboards: the trailing
/// week (`W:`) and month (`M:`), each reading `◎ sessions  ◇ ↘ ↗ ◌  $spend`
/// across every provider. The token figures read the precise one-decimal form
/// (`16.5k`) at the soft tier — the store is the exact record next to the cockpit's
/// coarse live read — each marker in its one shared color (the sky-blue window
/// tag, the teal `◎`, the blue `◇`/`↗`, the deep-red `↘`, and the green `◌`)
/// and the `$` bold dollar green; the
/// spend deliberately does **not** animate (only the headline does). Both
/// rows share one set of right-aligned column widths so the labels stack and
/// every number column lines up. Always present once the store is rendered;
/// empty history reads `$0.00`.
pub(in crate::sidebar_pane::render) fn fleet_store_lines(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
) -> Vec<Line<'static>> {
    total_store_rows(theme, tally, width, ProviderLayout::Wide)
}

pub(in crate::sidebar_pane::render) fn fleet_total_lines(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
) -> Vec<Line<'static>> {
    let layout = ProviderLayout::for_width(width, true);
    total_spend_lines(theme, tally, width, layout)
}

fn total_spend_lines(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
    layout: ProviderLayout,
) -> Vec<Line<'static>> {
    let rows = total_store_rows(theme, tally, width, layout);
    let has_spacer = matches!(layout, ProviderLayout::Wide | ProviderLayout::Normal);
    let mut lines = Vec::with_capacity(rows.len() + 1 + usize::from(has_spacer));
    if has_spacer {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(total_delimiter_row(theme, width)));
    lines.extend(rows);
    lines
}

fn total_store_rows(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
    layout: ProviderLayout,
) -> Vec<Line<'static>> {
    let zero = SpendTally::default();
    let tally = tally.unwrap_or(&zero);
    let token_format: fn(u64) -> String = tokens_short;
    let cols = WmColumns::measure([(&tally.week, token_format), (&tally.month, token_format)]);
    match layout {
        ProviderLayout::Wide => vec![
            wm_row(
                theme,
                "W",
                &tally.week,
                tokens_short,
                TokenDetail::Full,
                &cols,
                width,
            ),
            wm_row(
                theme,
                "M",
                &tally.month,
                tokens_short,
                TokenDetail::Full,
                &cols,
                width,
            ),
        ],
        ProviderLayout::Normal | ProviderLayout::Narrow => {
            let detail = total_token_detail(theme, tally, tokens_short, layout, &cols, width);
            vec![
                split_spend_token_row(theme, "W", &tally.week, tokens_short, detail, &cols, width),
                split_spend_token_row(theme, "M", &tally.month, tokens_short, detail, &cols, width),
                total_usd_row(theme, &tally.week, &tally.month, width),
            ]
        }
    }
}

fn total_token_detail(
    theme: &Theme,
    tally: &SpendTally,
    token_format: fn(u64) -> String,
    layout: ProviderLayout,
    cols: &WmColumns,
    width: usize,
) -> TokenDetail {
    if layout == ProviderLayout::Narrow {
        return TokenDetail::Summary;
    }
    let full_fits = [("W", &tally.week), ("M", &tally.month)]
        .into_iter()
        .all(|(label, window)| {
            spend_token_row_width(theme, label, window, token_format, TokenDetail::Full, cols)
                <= width
        });
    if full_fits {
        TokenDetail::Full
    } else {
        TokenDetail::Summary
    }
}

/// The shared right-aligned column widths for store rows, measured across every
/// rendered window so a 2- and a 3-digit figure stack on one right edge.
struct WmColumns {
    sessions: usize,
    total: usize,
    input: usize,
    output: usize,
    cache_read: usize,
    usd: usize,
}

impl WmColumns {
    fn measure<'a>(
        windows: impl IntoIterator<Item = (&'a SpendWindow, fn(u64) -> String)>,
    ) -> Self {
        let mut cols = Self {
            sessions: 1,
            total: 1,
            input: 1,
            output: 1,
            cache_read: 1,
            usd: 1,
        };
        for (window, token_format) in windows {
            cols.sessions = cols
                .sessions
                .max(window.sessions.to_string().chars().count());
            cols.total = cols.total.max(token_format(window.tokens).chars().count());
            cols.input = cols.input.max(token_format(window.input).chars().count());
            cols.output = cols.output.max(token_format(window.output).chars().count());
            cols.cache_read = cols
                .cache_read
                .max(token_format(window.cache_read).chars().count());
            cols.usd = cols.usd.max(dollars2(window.usd).chars().count());
        }
        cols
    }

    fn token_columns(&self) -> TokenColumns {
        TokenColumns {
            total: self.total,
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
        }
    }
}

fn total_delimiter_row(theme: &Theme, width: usize) -> Vec<Span<'static>> {
    let hairline = theme.glyph(GlyphRole::ChromeHairline);
    let label = format!("{hairline}{hairline} Total: ");
    let used = 1 + text_width(&label);
    let fill = width.saturating_sub(used);
    trim_spans_to_width(
        vec![
            Span::raw(" "),
            Span::styled(
                label,
                theme.styled(Component::StoreLabel, Modifier::empty()),
            ),
            Span::styled(hairline.repeat(fill), theme.faint()),
        ],
        width,
    )
}

/// One store row — `W: ◎ {sessions}  ◇ {total} ↘ {in} ↗ {out} ◌ {cache_read}`
/// left-clustered, the `$ {spend}` pinned to the right edge. A one-cell lead
/// pad sets the `W:`/`M:` tags a hair off the chrome edge. The window tag wears
/// sky blue — distinct from the teal `◎` beside it — and each token marker its
/// one shared color (`↗` blue, `◌` green), with the figures at the soft tier
/// ([`Theme::soft`]).
/// Every numeric field is right-aligned to the shared [`WmColumns`] width, so
/// the `W:` and `M:` rows stack into one tidy grid. Cache-write is folded into
/// the `↘` input column, so the store keeps to the four headline token figures
/// the all-time read needs.
fn wm_row(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    token_format: fn(u64) -> String,
    token_detail: TokenDetail,
    cols: &WmColumns,
    width: usize,
) -> Line<'static> {
    let left = spend_token_spans(theme, label, window, token_format, token_detail, cols);
    let right = vec![Span::styled(
        format!("{:>w$}", dollars2(window.usd), w = cols.usd),
        theme.money_style(Modifier::BOLD),
    )];
    pin_right(left, right, width)
}

fn split_spend_token_row(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    token_format: fn(u64) -> String,
    token_detail: TokenDetail,
    cols: &WmColumns,
    width: usize,
) -> Line<'static> {
    pin_right(
        spend_session_spans(theme, label, window, cols),
        spend_token_metric_spans(theme, window, token_format, token_detail, cols),
        width,
    )
}

fn total_usd_row(
    theme: &Theme,
    week: &SpendWindow,
    month: &SpendWindow,
    width: usize,
) -> Line<'static> {
    let label = theme.style(theme.component(Component::StoreLabel), Modifier::empty());
    let left = vec![
        Span::raw(" "),
        Span::styled("W: ".to_owned(), label),
        Span::styled(dollars2(week.usd), theme.money_style(Modifier::BOLD)),
    ];
    let right = vec![
        Span::styled("M: ".to_owned(), label),
        Span::styled(dollars2(month.usd), theme.money_style(Modifier::BOLD)),
    ];
    pin_right(left, right, width)
}

fn spend_token_row_width(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    token_format: fn(u64) -> String,
    token_detail: TokenDetail,
    cols: &WmColumns,
) -> usize {
    spans_width(&spend_session_spans(theme, label, window, cols))
        + 1
        + spans_width(&spend_token_metric_spans(
            theme,
            window,
            token_format,
            token_detail,
            cols,
        ))
}

fn spend_session_spans(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    cols: &WmColumns,
) -> Vec<Span<'static>> {
    let value = theme.body();
    let marker = |color: Color| theme.style(color, Modifier::empty());
    vec![
        Span::raw(" "),
        Span::styled(
            format!("{label}: "),
            marker(theme.component(Component::StoreLabel)),
        ),
        Span::styled(
            theme.glyph(GlyphRole::CockpitSessions).to_owned(),
            marker(theme.component(Component::Sessions)),
        ),
        Span::styled(
            format!(" {:>w$}", window.sessions, w = cols.sessions),
            value,
        ),
    ]
}

fn spend_token_metric_spans(
    theme: &Theme,
    window: &SpendWindow,
    token_format: fn(u64) -> String,
    token_detail: TokenDetail,
    cols: &WmColumns,
) -> Vec<Span<'static>> {
    token_breakdown_spans(
        theme,
        window.tokens,
        window.input,
        window.output,
        window.cache_read,
        token_format,
        token_detail,
        &cols.token_columns(),
    )
}

fn spend_token_spans(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    token_format: fn(u64) -> String,
    token_detail: TokenDetail,
    cols: &WmColumns,
) -> Vec<Span<'static>> {
    let mut spans = spend_session_spans(theme, label, window, cols);
    spans.push(Span::raw("  "));
    spans.extend(spend_token_metric_spans(
        theme,
        window,
        token_format,
        token_detail,
        cols,
    ));
    spans
}

/// The pinned per-provider dashboard. In stacked mode every account paints its
/// own block — the dashboard's top hairline, then each provider header and
/// brand/budget/spend body separated from the next by a blank row. In
/// tabbed mode the top hairline becomes a [tab rail](provider_tab_rail) — each
/// account set into the rule, the active one a brand-filled bold chip — over
/// the active provider's block alone, so the budgets read one account at a time;
/// the header then drops the name the rail carries and sits beside the emblem's
/// first row. A metered account drains one "mana" bar per included budget window and
/// may swap in an `ex` paid-usage row when an included cap is spent; an
/// unmetered API-key account shows one `api` budget row. The bars share one start
/// and one end column across every block, so the dashboard reads as one aligned
/// grid.
#[allow(clippy::too_many_arguments)]
pub(in crate::sidebar_pane::render) fn dashboard_panel_lines_with_footer(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    active_tab: Option<&String>,
    tabbed: bool,
    fleet_tally: Option<&SpendTally>,
    pet: Option<&PetView>,
    pets_enabled: bool,
    folded_footer: Option<super::super::chrome::FooterParts>,
    width: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
) -> (Vec<Line<'static>>, Vec<HitRegion>) {
    let mut lines = Vec::new();
    let first = providers.first();
    if first.is_none() && !pets_enabled {
        return (lines, Vec::new());
    }
    if first.is_none() {
        return (super::pets::pet_panel_lines(pet, theme, width), Vec::new());
    }
    if !tabbed {
        let mut blocks = Vec::new();
        for (index, panel) in providers.iter().enumerate() {
            if index == 0 {
                blocks.push(super::super::hairline_rule(theme, width));
            } else {
                blocks.push(Line::from(""));
            }
            blocks.extend(single_block_lines(theme, panel, width, zones, now));
        }
        return (blocks, Vec::new());
    }

    let active_kind = active_tab
        .filter(|kind| providers.iter().any(|panel| panel.kind == kind.as_str()))
        .cloned()
        .or_else(|| first.map(|panel| panel.kind.clone()))
        .unwrap_or_default();
    let (rail, hits) = provider_tab_rail(theme, providers, &active_kind, width);
    let active = providers
        .iter()
        .find(|panel| panel.kind == active_kind)
        .or(first);
    lines.push(rail);
    if let Some(active) = active {
        if pets_enabled {
            if let Some(pet_w) = pet_column_width(theme, pet)
                && pet_w + PET_COLUMN_GAP < width
            {
                let block_w = width.saturating_sub(pet_w + PET_COLUMN_GAP);
                lines.push(provider_pet_caption_line(theme, pet, width));
                let block = active_provider_block_lines(
                    theme,
                    active,
                    block_w,
                    zones,
                    now,
                    ActiveProviderBlockOptions {
                        fleet_tally,
                        allow_wide: false,
                        include_totals: true,
                        folded_footer: None,
                    },
                );
                let pet_lines = super::pets::dashboard_pet_grid_lines(pet, theme, pet_w);
                let footer_rows = usize::from(folded_footer.is_some());
                let block_rows = block.len() + footer_rows;
                let pet_rows = pet_lines.len();
                let rows = block_rows.max(pet_rows);
                let pet_top = rows.saturating_sub(pet_rows);
                let footer = folded_footer
                    .clone()
                    .map(|footer| folded_footer_line(footer, block_w));
                let layout = ProviderPetZipLayout {
                    pet_top,
                    rows,
                    block_w,
                    pet_w,
                    width,
                };
                lines.extend(zip_provider_pet_lines(block, pet_lines, footer, layout));
                return (lines, hits);
            } else {
                // A blank line below the rail sets the tabs apart from the active
                // account's tall spend block when the pet column is unavailable.
                lines.push(Line::from(""));
                lines.extend(active_provider_block_lines(
                    theme,
                    active,
                    width,
                    zones,
                    now,
                    ActiveProviderBlockOptions {
                        fleet_tally,
                        allow_wide: false,
                        include_totals: true,
                        folded_footer: folded_footer.clone(),
                    },
                ));
            }
        } else {
            // A blank line below the rail sets the tabs apart from the active
            // account's main block, matching the cockpit's breathing room.
            lines.push(Line::from(""));
            lines.extend(active_provider_block_lines(
                theme,
                active,
                width,
                zones,
                now,
                ActiveProviderBlockOptions {
                    fleet_tally: None,
                    allow_wide: true,
                    include_totals: false,
                    folded_footer: None,
                },
            ));
        }
    }
    (lines, hits)
}

fn single_block_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let layout = ProviderLayout::for_width(width, true);
    let mut lines = vec![provider_header_line(
        theme,
        panel,
        width,
        false,
        layout.inline_art(),
        now,
    )];
    if layout == ProviderLayout::Wide {
        // Wide stacked blocks keep the historical identity/body breathing room.
        let crest = art_crest_row(panel)
            .filter(|_| width >= PROVIDER_ART_MIN_WIDTH)
            .map(|row| pad_line_to(Line::from(art_row_spans(theme, panel, row)), width))
            .unwrap_or_else(|| Line::from(""));
        lines.push(crest);
    }
    lines.extend(provider_body_lines(theme, panel, width, layout, zones, now));
    lines
}

struct ActiveProviderBlockOptions<'a> {
    fleet_tally: Option<&'a SpendTally>,
    allow_wide: bool,
    include_totals: bool,
    folded_footer: Option<super::super::chrome::FooterParts>,
}

fn active_provider_block_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
    options: ActiveProviderBlockOptions<'_>,
) -> Vec<Line<'static>> {
    let layout = ProviderLayout::for_width(width, options.allow_wide);
    let mut lines = vec![provider_header_line(
        theme,
        panel,
        width,
        true,
        layout.inline_art(),
        now,
    )];
    lines.extend(provider_body_lines(theme, panel, width, layout, zones, now));
    if options.include_totals {
        lines.extend(total_spend_lines(theme, options.fleet_tally, width, layout));
    }
    if let Some(footer) = options.folded_footer {
        lines.push(folded_footer_line(footer, width));
    }
    lines
}

fn folded_footer_line(parts: super::super::chrome::FooterParts, width: usize) -> Line<'static> {
    pin_right(parts.left, vec![parts.help], width)
}

fn pet_column_width(theme: &Theme, pet: Option<&PetView>) -> Option<usize> {
    theme
        .pet_body_enabled()
        .then(|| {
            pet.and_then(|view| match view.body.as_ref()? {
                PetBody::Pixel(pixel) => Some(usize::from(pixel.size.cols)),
                PetBody::Cell(grid) => grid.iter().map(Vec::len).max(),
            })
        })
        .flatten()
        .filter(|width| *width > 0)
}

fn provider_pet_caption_line(theme: &Theme, pet: Option<&PetView>, width: usize) -> Line<'static> {
    let Some(caption) = super::pets::dashboard_pet_caption(pet) else {
        return Line::from("");
    };
    let caption_w = width.saturating_sub(PET_CAPTION_RIGHT_PAD);
    let clipped = clip(caption, caption_w);
    let lead = caption_w.saturating_sub(text_width(&clipped));
    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::raw(" ".repeat(lead)));
    }
    if !clipped.is_empty() {
        spans.push(Span::styled(clipped, theme.muted()));
    }
    let trail = width.saturating_sub(caption_w);
    if trail > 0 {
        spans.push(Span::raw(" ".repeat(trail)));
    }
    Line::from(spans)
}

struct ProviderPetZipLayout {
    pet_top: usize,
    rows: usize,
    block_w: usize,
    pet_w: usize,
    width: usize,
}

fn zip_provider_pet_lines(
    block: Vec<Line<'static>>,
    pet: Vec<Line<'static>>,
    footer: Option<Line<'static>>,
    layout: ProviderPetZipLayout,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(layout.rows);
    for index in 0..layout.rows {
        let footer_row = footer.is_some() && index + 1 == layout.rows;
        let mut block_line = pad_line_to(
            if footer_row {
                footer.clone().unwrap_or_else(|| Line::from(""))
            } else {
                block.get(index).cloned().unwrap_or_else(|| Line::from(""))
            },
            layout.block_w,
        );
        block_line.spans.push(Span::raw(" ".repeat(PET_COLUMN_GAP)));
        let pet_index = index.checked_sub(layout.pet_top);
        let pet_line = pad_line_to(
            pet_index
                .and_then(|index| pet.get(index).cloned())
                .unwrap_or_else(|| Line::from("")),
            layout.pet_w,
        );
        block_line.spans.extend(pet_line.spans);
        block_line.spans = trim_spans_to_width(block_line.spans, layout.width);
        lines.push(block_line);
    }
    lines
}

/// The dashboard's tab rail — the top hairline with every account set into it:
/// a leading `──` stub, then each tab in order, the active one a brand-filled
/// bold chip (dark ink on the brand color) and the rest brand-colored labels
/// at full strength resting in the line, separated and trailed by `─` fill in
/// the hairline's soft gray. Every rail glyph is identical whichever tab is
/// active — each tab reserves one rail cell and one pad space on each side of
/// its name, and the pick moves as fill and weight alone — so a click changes
/// color without a single cell of glyph motion. Under `NO_COLOR` the chip
/// fill drops, and `┤ ├` caps paint into the active tab's reserved rail cells
/// as the pick's shape instead. Labels are the kind slugs
/// first-char-capitalized — the rail carries the product-name role the tabbed
/// header drops. Holds to one screen row: a tab that would overflow `width` is
/// dropped whole (label and hit together), except that the active tab reserves
/// its footprint first so the selected block always has a visible chip. The
/// hit map stays in lockstep with the frame however many kinds register or
/// however narrow the pane.
/// Returns the line plus one typed [`HitRegion`] per rendered tab (line index
/// 0, columns over the full edge-to-edge footprint, so the click target holds
/// still too) for the mouse hit-test.
fn provider_tab_rail(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    active_kind: &str,
    width: usize,
) -> (Line<'static>, Vec<HitRegion>) {
    let rail = theme.body();
    let hairline = theme.glyph(GlyphRole::ChromeHairline).to_owned();
    let fill = |cells: usize| Span::styled(hairline.repeat(cells), rail);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut hits = Vec::new();
    let mut col: usize = 0;
    let stub = RAIL_STUB.min(width);
    spans.push(fill(stub));
    col += stub;

    let tab_cells = |panel: &SidebarProviderPanel| tab_label(&panel.kind).chars().count() + 4;
    let active_index = providers.iter().position(|panel| panel.kind == active_kind);
    let mut selected = vec![false; providers.len()];
    let mut used = stub;
    if let Some(index) = active_index {
        let cells = tab_cells(&providers[index]);
        if used < width {
            selected[index] = true;
            used = (used + cells).min(width);
        }
    }
    for (index, panel) in providers.iter().enumerate() {
        if Some(index) == active_index {
            continue;
        }
        let cells = tab_cells(panel);
        let gap = if selected.iter().any(|selected| *selected) {
            RAIL_STUB
        } else {
            0
        };
        if used + gap + cells <= width {
            selected[index] = true;
            used += gap + cells;
        }
    }

    let mut rendered = 0;
    for (index, panel) in providers.iter().enumerate() {
        if !selected[index] {
            continue;
        }
        let gap = if rendered > 0 { RAIL_STUB } else { 0 };
        let active = panel.kind == active_kind;
        // Kind labels are registry-fixed ASCII slugs, so chars == cells; the
        // footprint adds the two pad spaces and the two reserved rail cells.
        let label = tab_label(&panel.kind);
        let cells = label.chars().count() + 4;
        if gap > 0 {
            spans.push(fill(gap));
            col += gap;
        }
        if active {
            // The brand fill and bold are the pick; the reserved rail cells
            // keep their `─` so a click moves color alone, never a glyph.
            // When `NO_COLOR` drops the fill, the `┤ ├` caps paint into
            // those cells instead as the pick's shape.
            let brand = theme.brand_tone(panel);
            let chip = theme.chip(brand, Modifier::BOLD);
            let (left, right) = if chip.bg.is_none() {
                (
                    Span::styled(theme.glyph(GlyphRole::ChromeTabCapLeft).to_owned(), rail),
                    Span::styled(theme.glyph(GlyphRole::ChromeTabCapRight).to_owned(), rail),
                )
            } else {
                (fill(1), fill(1))
            };
            spans.push(left);
            spans.push(Span::styled(format!(" {label} "), chip));
            spans.push(right);
        } else {
            spans.push(fill(1));
            spans.push(Span::styled(
                format!(" {label} "),
                theme.style(theme.brand_tone(panel), Modifier::empty()),
            ));
            spans.push(fill(1));
        }
        hits.push(HitRegion::line(
            0,
            col as u16..(col + cells).min(width) as u16,
            HitTarget::ProviderTab(panel.kind.clone()),
        ));
        col += cells;
        rendered += 1;
    }
    if col < width {
        spans.push(fill(width - col));
    }
    (Line::from(trim_spans_to_width(spans, width)), hits)
}

/// Width of the tab rail's leading stub and inter-tab gaps.
const RAIL_STUB: usize = 2;

/// A tab's display label: the registry kind slug with its first ASCII char
/// capitalized (`claude` → `Claude`) — the rail names the product, so the
/// tabbed header doesn't have to. Hits keep the raw slug. Kind slugs are
/// registry-fixed ASCII — the rail's cell math counts on it — so a non-ASCII
/// first char (a mid-codepoint `get_mut` range) is left uncapitalized rather
/// than split.
fn tab_label(kind: &str) -> String {
    let mut label = kind.to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

/// The block's header line, with the health-colored `⇅ rc` flag pinned to the
/// top-right corner when remote control is on for the provider. Untabbed it
/// reads `Claude v2.1.158 · Claude Max` — the product name in the brand color,
/// version and plan dim. Tabbed, the rail already names the account, so the
/// name drops and the line reads `Claude Max · v2.1.158` — plan first, the
/// version demoted to trailing trivia — beside the emblem's first row. Plan
/// drops out when unknown; version renders as `v?` until an out-of-band probe or
/// live context fills it.
fn provider_header_line(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    tabbed: bool,
    inline_art: bool,
    now: Timestamp,
) -> Line<'static> {
    let mut right = reset_header_spans(theme, panel, now);
    let remote_control = match panel.remote_control {
        RemoteControlBadge::Hidden => None,
        RemoteControlBadge::Healthy => Some(Component::RemoteControl),
        RemoteControlBadge::Down => Some(Component::RemoteControlDown),
    };
    if let Some(component) = remote_control {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            format!("{} rc", theme.glyph(GlyphRole::ChromeRemoteControl)),
            theme.styled(component, Modifier::BOLD),
        ));
    }
    let full = provider_header_left(theme, panel, width, tabbed, inline_art, true);
    let left = if spans_width(&full) + spans_width(&right) <= width {
        full
    } else {
        provider_header_left(theme, panel, width, tabbed, inline_art, false)
    };
    pin_right(left, right, width)
}

/// Heat amount (0.0 green … 1.0 red) for the reset marker at `hours` until the
/// soonest credit expires, or `None` at/after 7d for the default grey.
pub(in crate::sidebar_pane::render) fn reset_expiry_heat_amount(hours: f64) -> Option<f32> {
    let lerp = |h: f64, a: f64, b: f64, amt_a: f32, amt_b: f32| -> f32 {
        let t = ((a - h) / (a - b)).clamp(0.0, 1.0) as f32;
        amt_a + (amt_b - amt_a) * t
    };
    if hours >= RESET_GREEN_H {
        None
    } else if hours < RESET_RED_H {
        Some(1.0)
    } else if hours < RESET_AMBER_H {
        Some(lerp(hours, RESET_AMBER_H, RESET_RED_H, 2.0 / 3.0, 1.0))
    } else if hours < RESET_YELLOW_H {
        Some(lerp(
            hours,
            RESET_YELLOW_H,
            RESET_AMBER_H,
            1.0 / 3.0,
            2.0 / 3.0,
        ))
    } else {
        Some(lerp(hours, RESET_GREEN_H, RESET_YELLOW_H, 0.0, 1.0 / 3.0))
    }
}

fn reset_header_spans(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    now: Timestamp,
) -> Vec<Span<'static>> {
    if panel.kind != "codex" {
        return Vec::new();
    }
    let Some(reset_credits) = panel.reset_credits.as_ref() else {
        return Vec::new();
    };
    if reset_credits.count == 0 {
        return Vec::new();
    }
    let style = reset_credits
        .soonest_expiry
        .map(|at| at.duration_since(now).as_secs() as f64 / 3600.0)
        .and_then(reset_expiry_heat_amount)
        .map(|amount| theme.style(theme.heat_tone(amount), Modifier::empty()))
        .unwrap_or_else(|| theme.body());
    vec![
        Span::styled(theme.glyph(GlyphRole::MeterReset).to_owned(), style),
        Span::styled(format!(" {}", reset_credits.count), theme.body()),
    ]
}

fn provider_header_left(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    tabbed: bool,
    inline_art: bool,
    include_version: bool,
) -> Vec<Span<'static>> {
    let mut left = Vec::new();
    let art_fits = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    if (tabbed || inline_art) && art_fits {
        if let Some(row) = art_crest_row(panel) {
            left.extend(art_row_spans(theme, panel, row));
            left.push(Span::raw(" "));
        } else {
            left.push(Span::raw(" ".repeat(PROVIDER_ART_WIDTH + 1)));
        }
    }
    let version = panel
        .version
        .as_deref()
        .map(|version| format!("v{version}"))
        .unwrap_or_else(|| "v?".to_owned());
    if tabbed {
        if let Some(plan) = panel.plan.as_deref() {
            left.push(Span::styled(plan.to_owned(), theme.muted()));
            if include_version {
                left.push(Span::styled(" · ", theme.faint()));
                left.push(Span::styled(version, theme.muted()));
            }
        } else if include_version {
            left.push(Span::styled(version, theme.muted()));
        }
    } else {
        left.push(Span::styled(
            panel.product_name.clone(),
            theme.style(theme.brand_tone(panel), Modifier::BOLD),
        ));
        if include_version {
            left.push(Span::styled(format!(" {version}"), theme.muted()));
        }
        if let Some(plan) = panel.plan.as_deref() {
            left.push(Span::styled(" · ", theme.faint()));
            left.push(Span::styled(plan.to_owned(), theme.muted()));
        }
    }
    left
}

/// The provider body: the brand emblem in a fixed left column zipped against the
/// right column. The stats stay on one row; normal and narrow use the same row
/// in the smaller provider column and drop input/output splits when it would
/// otherwise clip.
fn provider_body_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    layout: ProviderLayout,
    zones: &BudgetBarConfig,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_column = if show_art { PROVIDER_ART_WIDTH + 1 } else { 0 };
    let bar_region = width.saturating_sub(art_column);
    let art_start = panel.art.len().saturating_sub(PROVIDER_ART_BODY_ROWS);

    let mut rights = provider_stats_rows(theme, panel, bar_region, layout);
    rights.extend(provider_bar_rows(theme, panel, bar_region, zones, now));

    let art_rows = if show_art {
        panel.art.len().saturating_sub(art_start)
    } else {
        0
    };
    let rows = art_rows.max(rights.len());
    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_art {
            spans.extend(art_row_spans(theme, panel, art_start.saturating_add(index)));
            spans.push(Span::raw(" "));
        }
        if let Some(right) = rights.get(index) {
            spans.extend(right.iter().cloned());
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));
    }
    lines
}

fn art_crest_row(panel: &SidebarProviderPanel) -> Option<usize> {
    panel
        .art
        .len()
        .saturating_sub(PROVIDER_ART_BODY_ROWS)
        .checked_sub(1)
}

fn art_row_spans(theme: &Theme, panel: &SidebarProviderPanel, row: usize) -> Vec<Span<'static>> {
    let clipped = clip(
        panel.art.get(row).map(String::as_str).unwrap_or(""),
        PROVIDER_ART_WIDTH,
    );
    let chars: Vec<char> = clipped.chars().collect();
    let brand = theme.style(theme.brand_tone(panel), Modifier::empty());
    let mut row_tints: Vec<_> = panel
        .art_tints
        .iter()
        .filter(|tint| tint.row == row && tint.len > 0)
        .collect();
    row_tints.sort_by_key(|tint| tint.start);

    let mut spans = Vec::new();
    let mut cursor = 0;
    for tint in row_tints {
        let start = tint.start.max(cursor).min(chars.len());
        let end = tint.start.saturating_add(tint.len).min(chars.len());
        if start >= end {
            continue;
        }
        if cursor < start {
            spans.push(Span::styled(
                chars[cursor..start].iter().collect::<String>(),
                brand,
            ));
        }
        spans.push(Span::styled(
            chars[start..end].iter().collect::<String>(),
            theme.style(
                theme.brand_rgb_tone(tint.color, Some(tint.color_rgb)),
                Modifier::empty(),
            ),
        ));
        cursor = end;
    }
    if cursor < chars.len() {
        spans.push(Span::styled(
            chars[cursor..].iter().collect::<String>(),
            brand,
        ));
    }
    pad_line_to(Line::from(spans), PROVIDER_ART_WIDTH).spans
}

fn provider_stats_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
    layout: ProviderLayout,
) -> Vec<Vec<Span<'static>>> {
    let Some(headline) = panel.spending.as_ref().map(|spending| spending.headline) else {
        fn dash(_: u64) -> String {
            "–".to_owned()
        }

        let mut left = vec![
            Span::styled(
                theme.glyph(GlyphRole::CockpitSessions).to_owned(),
                theme.styled(Component::Sessions, Modifier::empty()),
            ),
            Span::styled(format!(" {}", panel.active_sessions), theme.body()),
            Span::raw("  "),
        ];
        let detail = if layout == ProviderLayout::Narrow {
            TokenDetail::Summary
        } else {
            TokenDetail::Full
        };
        left.extend(token_breakdown_spans(
            theme,
            0,
            0,
            0,
            0,
            dash,
            detail,
            &TokenColumns::default(),
        ));
        let right = vec![Span::styled("$ –".to_owned(), theme.muted())];
        return vec![pin_right(left, right, region).spans];
    };
    let detail = provider_token_detail(theme, &headline, layout, region);
    vec![provider_stats_row(theme, panel, &headline, detail, region).spans]
}

fn provider_stats_row(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    headline: &SpendWindow,
    detail: TokenDetail,
    region: usize,
) -> Line<'static> {
    let left = provider_stats_left_spans(theme, headline, detail);
    let (label, style) = panel
        .day_budget
        .as_ref()
        .filter(|budget| budget.parked)
        .map_or_else(
            || (dollars2(headline.usd), theme.money_style(Modifier::BOLD)),
            |budget| {
                (
                    format!(
                        "{} of {}/day",
                        dollars2(budget.spend_usd),
                        dollars_cap(budget.cap_usd)
                    ),
                    theme.alarm(Modifier::BOLD),
                )
            },
        );
    let right = vec![Span::styled(label, style)];
    pin_right(left, right, region)
}

fn provider_stats_left_spans(
    theme: &Theme,
    headline: &SpendWindow,
    detail: TokenDetail,
) -> Vec<Span<'static>> {
    let mut left = vec![
        Span::styled(
            theme.glyph(GlyphRole::CockpitSessions).to_owned(),
            theme.styled(Component::Sessions, Modifier::empty()),
        ),
        Span::styled(format!(" {}", headline.sessions), theme.body()),
        Span::raw("  "),
    ];
    let token_breakdown = token_breakdown_spans(
        theme,
        headline.tokens,
        headline.input,
        headline.output,
        headline.cache_read,
        tokens_int,
        detail,
        &TokenColumns::default(),
    );
    left.extend(token_breakdown);
    left
}

fn provider_token_detail(
    theme: &Theme,
    headline: &SpendWindow,
    layout: ProviderLayout,
    region: usize,
) -> TokenDetail {
    match layout {
        ProviderLayout::Wide => return TokenDetail::Full,
        ProviderLayout::Narrow => return TokenDetail::Summary,
        ProviderLayout::Normal => {}
    }
    let left = provider_stats_left_spans(theme, headline, TokenDetail::Full);
    let right = vec![Span::styled(
        dollars2(headline.usd),
        theme.money_style(Modifier::BOLD),
    )];
    if spans_width(&left) + spans_width(&right) < region {
        TokenDetail::Full
    } else {
        TokenDetail::Summary
    }
}

/// The provider's budget bars within `region`: a metered account drains one
/// "mana" bar per reported window (`5h`, `7d`, `30d`, …, ordered short→long);
/// a metered account whose windows have not arrived yet shows one unknown-track
/// row per descriptor-declared placeholder, or one anonymous fallback row when
/// the shape is unknown; an unmetered account shows one `api` budget row, full
/// with `∞` when uncapped.
/// Each reset reads a two-unit countdown scaled to its magnitude. Each row
/// aligns front and back within `region`, so they line up across providers too.
fn provider_bar_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
) -> Vec<Vec<Span<'static>>> {
    if !panel.metered {
        return vec![api_credits_bar_row(
            theme,
            panel.extra_credits.as_ref(),
            region,
            zones,
        )];
    }
    if panel.windows.is_empty() {
        if panel.window_placeholders.is_empty() {
            return vec![unknown_bar_row(theme, "", region)];
        }
        return panel
            .window_placeholders
            .iter()
            .map(|label| unknown_bar_row(theme, label, region))
            .collect();
    }
    select_provider_bars(panel)
        .into_iter()
        .map(|bar| match bar {
            ProviderBar::Window(window) => metered_bar_row(
                theme,
                window,
                region,
                longer_window_spent(panel, window),
                zones,
                now,
            ),
            ProviderBar::Extra => {
                extra_credits_bar_row(theme, "ex", panel.extra_credits.as_ref(), region, zones)
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ProviderBar<'a> {
    Window(&'a RateLimitWindow),
    Extra,
}

fn select_provider_bars(panel: &SidebarProviderPanel) -> Vec<ProviderBar<'_>> {
    if panel.windows.iter().any(|window| window.scope.is_some()) {
        return panel.windows.iter().map(ProviderBar::Window).collect();
    }
    let Some(first) = panel.windows.first() else {
        return Vec::new();
    };
    let last = panel.windows.last().unwrap_or(first);
    let first_and_last = || provider_window_pair(first, last);
    let extra_disabled = panel
        .extra_credits
        .as_ref()
        .is_some_and(ExtraCredits::is_disabled);
    let extra_usable = panel
        .extra_credits
        .as_ref()
        .is_none_or(ExtraCredits::is_usable);

    if let Some(longest_spent) = panel
        .windows
        .iter()
        .filter(|window| window.is_spent())
        .max_by_key(|window| window.duration_mins.unwrap_or(0))
    {
        if !std::ptr::eq(first, longest_spent) {
            if extra_disabled {
                return provider_window_pair(first, longest_spent);
            }
            return vec![ProviderBar::Window(longest_spent), ProviderBar::Extra];
        }
        if extra_usable {
            return vec![ProviderBar::Window(first), ProviderBar::Extra];
        }
    }
    first_and_last()
}

fn provider_window_pair<'a>(
    left: &'a RateLimitWindow,
    right: &'a RateLimitWindow,
) -> Vec<ProviderBar<'a>> {
    if std::ptr::eq(left, right) {
        vec![ProviderBar::Window(left)]
    } else {
        vec![ProviderBar::Window(left), ProviderBar::Window(right)]
    }
}

/// Whether a window with a strictly longer duration is spent — a higher-level cap
/// being exhausted gates this shorter window (its budget is unusable until the
/// longer one resets), so the renderer paints the shorter row exhausted too (e.g.
/// a spent 7-day cap gating the 5-hour bar).
fn longer_window_spent(panel: &SidebarProviderPanel, window: &RateLimitWindow) -> bool {
    if window.scope.is_some() {
        return false;
    }
    let Some(mins) = window.duration_mins.filter(|mins| *mins > 0) else {
        return false;
    };
    panel.windows.iter().any(|other| {
        other.scope.is_none()
            && other
                .duration_mins
                .is_some_and(|other_mins| other_mins > mins)
            && other.used_percentage.is_some_and(|used| used >= 100)
    })
}

/// One metered budget bar row: the window's label (`5h`/`7d`/`30d`), the draining
/// mana bar (filled = remaining), and the `↻ <reset>` countdown right-aligned in
/// the value column. The reset marker is toned by burn pace when the window
/// carries enough timing data, cooling toward green only once the early-window
/// gate has elapsed; the countdown text stays in the neutral soft tier. The
/// label mirrors its bar's remaining-budget tone. `force_exhausted`
/// paints the row as fully spent — red, no countdown — regardless of the window's
/// own reading (a longer spent window gates it). A window with no usage
/// percentage paints as an unknown dim track, preserving the label but claiming
/// no remaining budget. A lifted window paints as an unlimited full bar with
/// `∞` in the reset-marker cell until the provider reports that duration again.
///
/// A window that has **not started** drops its countdown — a full bar with no
/// `↻` reads "send a message to start it" rather than a misleading ticking reset.
/// These are sliding windows that begin counting only on the first token, so until
/// then the provider keeps `resets_at` slid a full window-length ahead. Detect that
/// by the reset distance ([`RateLimitWindow::not_started`]), not a 0% reading — a fresh 5h
/// window still reports ~1% used, never 0. Codex reports a placeholder usedPercent
/// (~99) with no `resets_at` before the first token; that variant is caught by the
/// absent-reset + known-duration check in the `remaining` computation below.
fn metered_bar_row(
    theme: &Theme,
    window: &RateLimitWindow,
    region: usize,
    force_exhausted: bool,
    zones: &BudgetBarConfig,
    now: Timestamp,
) -> Vec<Span<'static>> {
    let label = window_label(window);
    if !force_exhausted && window.lifted {
        let bar_width = provider_bar_width(region);
        return bar_row(
            &label,
            mana_style(theme, 100, zones),
            mana_bar_spans(theme, 100, bar_width, zones),
            unlimited_value_spans(theme),
        );
    }
    if !force_exhausted && window.used_percentage.is_none() {
        return unknown_bar_row(theme, &label, region);
    }

    let not_started = !force_exhausted && window.not_started(now);
    let remaining = if force_exhausted {
        0
    } else {
        let raw = 100u8.saturating_sub(window.used_percentage.unwrap_or(100));
        // Codex reports a placeholder usedPercent (≈99) with no resetsAt before the
        // first token and a known duration — normalise to full so the bar matches
        // the empty countdown.
        if not_started || (window.resets_at.is_none() && window.duration_mins.is_some() && raw > 0)
        {
            100
        } else {
            raw
        }
    };
    let reset = if force_exhausted || not_started {
        None
    } else {
        window.resets_at.map(|at| reset_countdown(at, now))
    };
    let bar_width = provider_bar_width(region);
    let reset_marker_style = if reset.is_none() {
        theme.body()
    } else {
        window
            .used_percentage
            .zip(window.duration_mins)
            .zip(window.resets_at)
            .and_then(|((used, mins), at)| {
                pace_reading(
                    used,
                    SignedDuration::from_secs(i64::from(mins) * 60),
                    at.duration_since(now),
                )
            })
            .map(|reading| pace_style(theme, reading, &zones.burn_rate))
            .unwrap_or_else(|| theme.body())
    };
    bar_row(
        &label,
        mana_style(theme, remaining, zones),
        mana_bar_spans(theme, remaining, bar_width, zones),
        reset_value_spans(theme, reset.as_deref(), reset_marker_style),
    )
}

/// Unknown metered budget row: the same bar geometry as a reported window with
/// no usage, with either a reported or descriptor-declared window label, or a
/// blank label when the provider's window shape is unknown.
fn unknown_bar_row(theme: &Theme, label: &str, region: usize) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    bar_row(
        label,
        theme.muted(),
        unknown_mana_bar_spans(theme, bar_width),
        blank_value_spans(),
    )
}

fn bar_row(
    label: &str,
    label_style: Style,
    bar: Vec<Span<'static>>,
    value: Vec<Span<'static>>,
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(format!("{label:<PROVIDER_LABEL_WIDTH$}"), label_style),
        Span::raw(" "),
    ];
    spans.extend(bar);
    spans.push(Span::raw(" "));
    spans.extend(value);
    spans
}

fn blank_value_spans() -> Vec<Span<'static>> {
    vec![Span::raw(" ".repeat(PROVIDER_VALUE_WIDTH))]
}

fn unlimited_value_spans(theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            theme.glyph(GlyphRole::MeterUnlimited).to_owned(),
            theme.body(),
        ),
        Span::raw(" ".repeat(PROVIDER_VALUE_WIDTH.saturating_sub(1))),
    ]
}

/// The fixed-width reset value column. Only the reset marker carries a pace
/// tone; the countdown text stays at the neutral soft tier in a fixed six-cell
/// slot.
fn reset_value_spans(
    theme: &Theme,
    countdown: Option<&str>,
    marker_style: Style,
) -> Vec<Span<'static>> {
    let Some(countdown) = countdown.filter(|value| !value.is_empty()) else {
        return blank_value_spans();
    };
    let countdown = pad_countdown(countdown);
    vec![
        Span::styled(theme.glyph(GlyphRole::MeterReset).to_owned(), marker_style),
        Span::styled(format!(" {countdown}"), theme.body()),
    ]
}

fn pad_countdown(countdown: &str) -> String {
    let chars = countdown.chars().count();
    if chars >= PROVIDER_RESET_COUNTDOWN_WIDTH {
        countdown.to_owned()
    } else {
        format!("{countdown:>PROVIDER_RESET_COUNTDOWN_WIDTH$}")
    }
}

fn extra_credits_bar_row(
    theme: &Theme,
    label: &str,
    credits: Option<&ExtraCredits>,
    region: usize,
    zones: &BudgetBarConfig,
) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    let remaining = credits.and_then(ExtraCredits::remaining_percentage);
    let label_style = remaining
        .map(|remaining| mana_style(theme, remaining, zones))
        .unwrap_or_else(|| theme.muted());
    let bar = if let Some(remaining) = remaining {
        mana_bar_spans(theme, remaining, bar_width, zones)
    } else {
        unknown_mana_bar_spans(theme, bar_width)
    };
    bar_row(label, label_style, bar, extra_value_spans(theme, credits))
}

fn api_credits_bar_row(
    theme: &Theme,
    credits: Option<&ExtraCredits>,
    region: usize,
    zones: &BudgetBarConfig,
) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    if let Some(remaining) = credits.and_then(ExtraCredits::remaining_percentage) {
        let value = credits
            .and_then(ExtraCredits::remaining_usd_left)
            .map(dollars_compact)
            .map_or_else(blank_value_spans, |value| money_value_spans(theme, &value));
        return bar_row(
            "api",
            mana_style(theme, remaining, zones),
            mana_bar_spans(theme, remaining, bar_width, zones),
            value,
        );
    }
    if credits
        .is_some_and(|credits| credits.limit_usd().is_some() || credits.remaining_usd().is_some())
    {
        let value = credits
            .and_then(ExtraCredits::remaining_usd)
            .map(dollars_compact)
            .map_or_else(blank_value_spans, |value| money_value_spans(theme, &value));
        return bar_row(
            "api",
            theme.muted(),
            unknown_mana_bar_spans(theme, bar_width),
            value,
        );
    }
    bar_row(
        "api",
        mana_style(theme, 100, zones),
        mana_bar_spans(theme, 100, bar_width, zones),
        unlimited_value_spans(theme),
    )
}

fn extra_value_spans(theme: &Theme, credits: Option<&ExtraCredits>) -> Vec<Span<'static>> {
    let value = match credits {
        Some(ExtraCredits::Disabled) => "off".to_owned(),
        Some(credits) => extra_value_label(theme, credits),
        None => theme.glyph(GlyphRole::ChromeInfinity).to_owned(),
    };
    if value == theme.glyph(GlyphRole::ChromeInfinity) {
        return vec![
            Span::raw(" ".repeat(PROVIDER_RESET_MARKER_PAD)),
            Span::styled(value, theme.money_style(Modifier::BOLD)),
            Span::raw(
                " ".repeat(PROVIDER_VALUE_WIDTH.saturating_sub(PROVIDER_RESET_MARKER_PAD + 1)),
            ),
        ];
    }
    money_value_spans(theme, &value)
}

fn money_value_spans(theme: &Theme, value: &str) -> Vec<Span<'static>> {
    let value_width = PROVIDER_VALUE_WIDTH.saturating_sub(1);
    let clipped = clip(value, value_width);
    let pad = value_width.saturating_sub(text_width(&clipped));
    vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(clipped, theme.money_style(Modifier::BOLD)),
        Span::raw(" "),
    ]
}

fn extra_value_label(theme: &Theme, credits: &ExtraCredits) -> String {
    let infinity = theme.glyph(GlyphRole::ChromeInfinity);
    match (
        credits.used_usd(),
        credits.remaining_usd(),
        credits.limit_usd(),
    ) {
        (Some(used), _, Some(limit)) => {
            format!("{}/{}", dollars_compact(used), dollars_compact(limit))
        }
        (_, Some(remaining), _) => dollars_compact(remaining),
        (Some(used), _, None) => dollars_compact(used),
        (None, None, Some(limit)) => format!("?/{}", dollars_compact(limit)),
        (None, None, None) => infinity.to_owned(),
    }
}

fn dollars_compact(usd: f64) -> String {
    let usd = usd.max(0.0);
    if usd >= 1_000.0 {
        return format!("${:.0}k", usd / 1_000.0);
    }
    if (usd.fract()).abs() < f64::EPSILON {
        format!("${usd:.0}")
    } else {
        format!("${usd:.2}")
    }
}

/// The bar's cell width inside a provider `region`: the region less the label,
/// the value column, and the two single-cell gaps that frame the bar. At least
/// one cell, so a narrow sidebar still paints a (short) bar.
fn provider_bar_width(region: usize) -> usize {
    region.saturating_sub(PROVIDER_BAR_ROW_FRAME_WIDTH).max(1)
}
