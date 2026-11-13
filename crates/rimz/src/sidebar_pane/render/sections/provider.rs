//! The pinned provider dashboard — per-provider header, brand emblem, stats and
//! budget bars — and the W/M fleet store rows.

use crate::agents::{BudgetWindow, RateLimitWindow};
use crate::config::{BudgetBarConfig, GlyphRole};
use crate::sidebar_pane::pets::PetView;
use crate::sidebar_pane::pixel::logo::{LogoBox, LogoReserve};
use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::pixel::{image_id_color, placeholder_cluster};
use crate::sidebar_pane::render::labels::value_seam;
use crate::store::snapshot::{
    CreditDisplay, ProviderBar, ProviderBudget, RedeemForecast, RemoteControlBadge,
    SidebarProviderPanel, WindowReading,
};
use crate::theme::fmt::dollars_left;
use crate::{SpendTally, SpendWindow};
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::{
    compact_count, dollars_cap, dollars2, expiry_label, reset_countdown, tokens_short, window_label,
};
use crate::sidebar_pane::render::labels::{
    ManaTick, TokenColumns, TokenDetail, budget_bar_spans, mana_glyphs_overridden, mana_style,
    pace_style, token_breakdown_spans,
};
use crate::sidebar_pane::render::layout::{clip, pad_line_to, spans_width, text_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::sidebar_pane::render::{HitTarget, RenderedBlock};

use super::{pin_right, trim_spans_to_width};

/// The provider dashboard's fixed art column width: the brand emblem is padded
/// to this many cells so the bar column to its right starts at one shared cell
/// for every provider block — the bars align across providers by construction.
/// Dropped (bars run full-width) below [`PROVIDER_ART_MIN_WIDTH`].
const PROVIDER_ART_WIDTH: usize = 9;

/// Maximum emblem rows the body art column holds. A crest rides the chrome line
/// directly above the body — the header when it reserves the art column, the
/// wide spacer otherwise. Shorter right columns can raise three-row art too.
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

/// The provider bar's label slot (`5h` / `7d` / `mo` / `ex` / `api`) and value
/// column, shared by every provider bar so they align front and back. The label
/// fits three cells (`api`); the value holds `↻ ` plus a six-cell reset countdown
/// slot (`↻  4h50m` / `↻ 20h20m`), an unknown slot, or a compact paid-usage
/// value.
const PROVIDER_LABEL_WIDTH: usize = 3;
const PROVIDER_VALUE_WIDTH: usize = 8;
const PROVIDER_BAR_ROW_FRAME_WIDTH: usize = 13;
const PROVIDER_RESET_COUNTDOWN_WIDTH: usize = 6;
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
/// tabbed mode the top hairline becomes a tab rail — each
/// account set into the rule, the active one a brand-filled bold chip — over
/// the active provider's block alone, so the budgets read one account at a time;
/// the header then drops the name the rail carries and shares the emblem column
/// when a crest is raised. A metered account drains "mana" bars for its selected windows and
/// may swap in an `ex` paid-usage row when an included cap is spent; an
/// unmetered API-key account shows one `api` budget row. The bars share one start
/// and one end column across every block, so the dashboard reads as one aligned
/// grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DashboardMode {
    Stacked,
    Tabbed,
    Pet,
}

impl DashboardMode {
    pub(crate) fn owns_store(self, providers_present: bool) -> bool {
        self == Self::Pet && providers_present
    }

    fn tabbed(self) -> bool {
        self != Self::Stacked
    }

    fn allow_wide(self) -> bool {
        self != Self::Pet
    }
}

pub(in crate::sidebar_pane::render) struct DashboardContext<'a> {
    pub(in crate::sidebar_pane::render) theme: &'a Theme,
    pub(in crate::sidebar_pane::render) providers: &'a [SidebarProviderPanel],
    pub(in crate::sidebar_pane::render) active_provider: Option<&'a crate::ids::LoginKey>,
    pub(in crate::sidebar_pane::render) selected_sub_provider:
        Option<(crate::ids::AgentKind, &'a str)>,
    pub(in crate::sidebar_pane::render) last_sub_providers:
        &'a std::collections::BTreeMap<crate::ids::AgentKind, String>,
    pub(in crate::sidebar_pane::render) mode: DashboardMode,
    pub(in crate::sidebar_pane::render) fleet_tally: Option<&'a SpendTally>,
    pub(in crate::sidebar_pane::render) pet: Option<&'a PetView>,
    pub(in crate::sidebar_pane::render) folded_footer: Option<super::super::chrome::FooterParts>,
    pub(in crate::sidebar_pane::render) width: usize,
    pub(in crate::sidebar_pane::render) zones: &'a BudgetBarConfig,
    pub(in crate::sidebar_pane::render) now: Timestamp,
    /// The frame's meter images: lent, every budget bar draws in pixels.
    pub(in crate::sidebar_pane::render) pixels: Option<&'a mut MeterPixels>,
    /// A frame without image ids on a pane that draws logos once it holds a
    /// slot: a block whose logo has landed keeps its rows and draws its text
    /// emblem in them.
    pub(in crate::sidebar_pane::render) logo_reserve: Option<LogoReserve>,
}

pub(in crate::sidebar_pane::render) fn dashboard_block(
    mut context: DashboardContext<'_>,
) -> RenderedBlock {
    let mut output = RenderedBlock::default();
    let Some(first) = context.providers.first() else {
        if context.mode == DashboardMode::Pet {
            output.extend_inert(super::pets::pet_panel_lines(
                context.pet,
                context.theme,
                context.width,
            ));
        }
        return output;
    };

    if context.mode == DashboardMode::Stacked {
        let providers = context.providers;
        let mut lines = Vec::new();
        let mut logos = Vec::new();
        for (index, panel) in providers.iter().enumerate() {
            lines.push(if index == 0 {
                super::super::hairline_rule(context.theme, context.width)
            } else {
                Line::from("")
            });
            let width = context.width;
            let block = provider_block_lines(&mut context, panel, width, None);
            logos.extend(block.logo.map(|logo| logo.below(lines.len())));
            lines.extend(block.lines);
        }
        place_logos(&mut lines, logos);
        output.extend_inert(lines);
        return output;
    }

    let first_login = first.login_key();
    let active_kind = context
        .active_provider
        .filter(|key| {
            context
                .providers
                .iter()
                .any(|panel| panel.login_key() == **key)
        })
        .unwrap_or(&first_login);
    let active = context
        .providers
        .iter()
        .find(|panel| panel.login_key() == *active_kind)
        .unwrap_or(first);
    output.append(provider_tab_rail(
        context.theme,
        context.providers,
        active_kind,
        context.width,
    ));

    if context.mode == DashboardMode::Pet
        && let Some(pet) =
            super::pets::dashboard_pet_column(context.pet, context.theme, context.width)
        && pet.width + PET_COLUMN_GAP < context.width
    {
        let pet_w = pet.width;
        let block_w = context.width.saturating_sub(pet_w + PET_COLUMN_GAP);
        output.push_inert(pet.caption);
        let block = provider_block_lines(&mut context, active, block_w, None).placed();
        let footer_rows = usize::from(context.folded_footer.is_some());
        let block_rows = block.len() + footer_rows;
        let pet_rows = pet.body.len();
        let rows = block_rows.max(pet_rows);
        let footer = context
            .folded_footer
            .clone()
            .map(|footer| folded_footer_line(footer, block_w));
        output.extend_inert(zip_provider_pet_lines(
            block,
            pet.body,
            footer,
            ProviderPetZipLayout {
                pet_top: rows.saturating_sub(pet_rows),
                rows,
                block_w,
                pet_w,
                width: context.width,
            },
        ));
        return output;
    }

    // A blank line below the rail sets tabs apart from the active account.
    let mut lines = vec![Line::from("")];
    let (width, folded_footer) = (context.width, context.folded_footer.clone());
    let block = provider_block_lines(&mut context, active, width, folded_footer);
    let logo = block.logo.map(|logo| logo.below(lines.len()));
    lines.extend(block.lines);
    place_logos(&mut lines, logo);
    output.extend_inert(lines);
    output
}

/// One provider block, and where its logo goes when it draws one.
struct ProviderBlock {
    lines: Vec<Line<'static>>,
    logo: Option<BlockLogo>,
}

impl ProviderBlock {
    /// The block with its logo placed inside its own lines.
    fn placed(self) -> Vec<Line<'static>> {
        let mut lines = self.lines;
        place_logos(&mut lines, self.logo);
        lines
    }
}

/// A logo beside `detail_rows` rows starting at line `detail_top`.
#[derive(Clone)]
struct BlockLogo {
    detail_top: usize,
    detail_rows: usize,
    mark: LogoMark,
    logo_box: LogoBox,
}

/// What fills a logo's box: the image's placeholder cells, or, in a frame
/// without image ids, the text emblem's rows centred in the same box.
#[derive(Clone)]
enum LogoMark {
    Image(u32),
    Text(Vec<Vec<Span<'static>>>),
}

impl BlockLogo {
    fn below(self, lines_above: usize) -> Self {
        Self {
            detail_top: self.detail_top + lines_above,
            ..self
        }
    }
}

/// Draw each logo over the rows its box covers, top to bottom. A box row past
/// the detail rows takes a blank line there that no earlier logo holds, or the
/// block gains one, so the mark keeps one size beside any number of rows.
fn place_logos(lines: &mut Vec<Line<'static>>, logos: impl IntoIterator<Item = BlockLogo>) {
    let mut gained = 0;
    for logo in logos {
        gained += place_logo(lines, logo.below(gained));
    }
}

fn place_logo(lines: &mut Vec<Line<'static>>, logo: BlockLogo) -> usize {
    let BlockLogo {
        detail_top,
        detail_rows,
        mark,
        logo_box,
    } = logo;
    let blank = |line: &Line<'_>| line.spans.iter().all(|span| span.content.trim().is_empty());
    let above = logo_box.top.unsigned_abs() * usize::from(logo_box.top < 0);
    let free_above = (1..=above)
        .take_while(|row| detail_top >= *row && blank(&lines[detail_top - row]))
        .count();
    let at = detail_top - free_above;
    let gained_above = above - free_above;
    lines.splice(at..at, std::iter::repeat_n(Line::default(), gained_above));
    let top = detail_top + gained_above;
    let box_end = logo_box.top + logo_box.rows as isize;
    let below = usize::try_from(box_end).map_or(0, |end| end.saturating_sub(detail_rows));
    let end = top + detail_rows;
    let free_below = (0..below)
        .take_while(|row| lines.get(end + row).is_some_and(blank))
        .count();
    let at = end + free_below;
    let gained_below = below - free_below;
    lines.splice(at..at, std::iter::repeat_n(Line::default(), gained_below));
    let first = top.saturating_add_signed(logo_box.top);
    match mark {
        LogoMark::Image(image_id) => {
            let style = Style::default().fg(image_id_color(image_id));
            for row in 0..logo_box.rows {
                let cells = (0..PROVIDER_ART_WIDTH as u16)
                    .map(|col| Span::styled(placeholder_cluster(row, col), style))
                    .collect();
                art_column_row(&mut lines[first + usize::from(row)], cells);
            }
        }
        LogoMark::Text(rows) => {
            let offset = usize::from(logo_box.rows).saturating_sub(rows.len()) / 2;
            for (row, art) in rows.into_iter().enumerate() {
                if let Some(line) = lines.get_mut(first + offset + row) {
                    art_column_row(line, art);
                }
            }
        }
    }
    gained_above + gained_below
}

/// Put `cells` in place of the art column's leading blank cells.
fn art_column_row(line: &mut Line<'static>, cells: Vec<Span<'static>>) {
    let mut skip = PROVIDER_ART_WIDTH;
    let rest = std::mem::take(&mut line.spans)
        .into_iter()
        .filter_map(|span| {
            let cells = span.content.chars().count();
            let kept = span.content.chars().skip(skip).collect::<String>();
            skip = skip.saturating_sub(cells);
            (!kept.is_empty()).then(|| Span::styled(kept, span.style))
        })
        .collect::<Vec<_>>();
    let pad = PROVIDER_ART_WIDTH.saturating_sub(spans_width(&cells));
    line.spans = cells
        .into_iter()
        .chain((pad > 0).then(|| Span::raw(" ".repeat(pad))))
        .chain(rest)
        .collect();
}

/// The block's logo, where the pane paints pixels, its budget bars are not all
/// kept as text by a user's mana glyph, its brand resolves to a truecolor tone,
/// a logo tier is ready, and a logo id is free. A frame without image ids on a
/// pane that draws logos reserves the box for a block whose logo has landed and
/// fills it with the text emblem, so its rows match the frame on arrival.
fn block_logo(
    context: &mut DashboardContext<'_>,
    panel: &SidebarProviderPanel,
    detail_rows: usize,
) -> Option<(LogoMark, LogoBox)> {
    if (context.pixels.is_none() && context.logo_reserve.is_none())
        || mana_glyphs_overridden(context.theme)
    {
        return None;
    }
    let rgb = context.theme.pixel_rgb(context.theme.brand_tone(panel))?;
    let logo_box = LogoBox::centred_on(detail_rows);
    let cols = PROVIDER_ART_WIDTH as u16;
    let mut sources = context.theme.provider_logos(&panel.kind).into_iter();
    let mark = match (context.pixels.as_deref_mut(), context.logo_reserve) {
        (Some(pixels), _) => LogoMark::Image(
            sources.find_map(|source| pixels.logos.intern(source, rgb, cols, logo_box))?,
        ),
        (None, Some(reserve)) => {
            if !sources.any(|source| reserve.landed(source, rgb, cols)) {
                return None;
            }
            LogoMark::Text(
                (0..panel.art.len())
                    .map(|row| art_row_spans(context.theme, panel, row))
                    .collect(),
            )
        }
        (None, None) => return None,
    };
    Some((mark, logo_box))
}

fn provider_block_lines(
    context: &mut DashboardContext<'_>,
    panel: &SidebarProviderPanel,
    width: usize,
    folded_footer: Option<super::super::chrome::FooterParts>,
) -> ProviderBlock {
    let panel = context
        .selected_sub_provider
        .as_ref()
        .filter(|(kind, _)| kind.as_str() == panel.kind)
        .and_then(|(_, provider)| panel.for_sub_provider(provider))
        .or_else(|| {
            context
                .last_sub_providers
                .get(&crate::ids::AgentKind::new_unchecked(&panel.kind))
                .and_then(|provider| panel.for_sub_provider(provider))
        })
        .unwrap_or(panel);
    let layout = ProviderLayout::for_width(width, context.mode.allow_wide());
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_column = if show_art { PROVIDER_ART_WIDTH + 1 } else { 0 };
    let bar_region = width.saturating_sub(art_column);
    let mut rights = provider_stats_rows(context.theme, panel, bar_region, layout);
    rights.extend(provider_bar_rows(
        context.theme,
        panel,
        bar_region,
        context.zones,
        context.now,
        context.pixels.as_deref_mut(),
    ));
    let header_holds_art = context.mode.tabbed() || layout.inline_art();
    let detail_rows = rights.len() + usize::from(header_holds_art);
    let logo = show_art
        .then(|| block_logo(context, panel, detail_rows))
        .flatten();
    let crest = art_crest_row(panel, rights.len()).filter(|_| show_art && logo.is_none());
    let mut lines = vec![provider_header_line(
        context.theme,
        panel,
        width,
        context.mode.tabbed(),
        layout.inline_art(),
        crest,
        context.now,
    )];
    if context.mode == DashboardMode::Stacked && layout == ProviderLayout::Wide {
        // Wide stacked blocks keep the historical identity/body breathing room.
        lines.push(
            crest
                .map(|row| pad_line_to(Line::from(art_row_spans(context.theme, panel, row)), width))
                .unwrap_or_else(|| Line::from("")),
        );
    }
    let detail_top = if header_holds_art { 0 } else { lines.len() };
    let body = if logo.is_some() {
        BodyArt::Logo
    } else {
        BodyArt::Text(crest)
    };
    lines.extend(provider_body_lines(
        context.theme,
        panel,
        width,
        rights,
        body,
    ));
    if context.mode == DashboardMode::Pet {
        lines.extend(total_spend_lines(
            context.theme,
            context.fleet_tally,
            width,
            layout,
        ));
    }
    if let Some(footer) = folded_footer {
        lines.push(folded_footer_line(footer, width));
    }
    ProviderBlock {
        lines,
        logo: logo.map(|(mark, logo_box)| BlockLogo {
            detail_top,
            detail_rows,
            mark,
            logo_box,
        }),
    }
}

fn folded_footer_line(parts: super::super::chrome::FooterParts, width: usize) -> Line<'static> {
    super::super::chrome::footer_line(parts, width)
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
/// as the pick's shape instead. Labels are resolved product names — the rail
/// carries the product-name role the tabbed header drops. Holds to one screen
/// row: a tab that would overflow `width` is
/// dropped whole (label and hit together), except that the active tab reserves
/// its footprint first so the selected block always has a visible chip. The
/// hit map stays in lockstep with the frame however many kinds register or
/// however narrow the pane.
/// Returns one block containing the rail and its typed tab regions, with every
/// click target spanning the full edge-to-edge footprint.
fn provider_tab_rail(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    active_kind: &crate::ids::LoginKey,
    width: usize,
) -> RenderedBlock {
    let rail = theme.body();
    let hairline = theme.glyph(GlyphRole::ChromeHairline).to_owned();
    let fill = |cells: usize| Span::styled(hairline.repeat(cells), rail);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut hits = Vec::new();
    let mut col: usize = 0;
    let stub = RAIL_STUB.min(width);
    spans.push(fill(stub));
    col += stub;

    let selected = selected_provider_tabs(providers, active_kind, width, stub);

    let mut rendered = 0;
    for (index, panel) in providers.iter().enumerate() {
        if !selected[index] {
            continue;
        }
        let gap = if rendered > 0 { RAIL_STUB } else { 0 };
        let active = panel.login_key() == *active_kind;
        let label = panel.product_name.as_str();
        let cells = provider_tab_cells(panel);
        if gap > 0 {
            spans.push(fill(gap));
            col += gap;
        }
        append_provider_tab_spans(&mut spans, theme, panel, label, active);
        hits.push((
            col as u16..(col + cells).min(width) as u16,
            HitTarget::ProviderTab(panel.login_key()),
        ));
        col += cells;
        rendered += 1;
    }
    if col < width {
        spans.push(fill(width - col));
    }
    let mut block = RenderedBlock::default();
    block.push_with_regions(Line::from(trim_spans_to_width(spans, width)), None, hits);
    block
}

fn provider_tab_cells(panel: &SidebarProviderPanel) -> usize {
    text_width(&panel.product_name) + 4
}

fn selected_provider_tabs(
    providers: &[SidebarProviderPanel],
    active_kind: &crate::ids::LoginKey,
    width: usize,
    stub: usize,
) -> Vec<bool> {
    let active_index = providers
        .iter()
        .position(|panel| panel.login_key() == *active_kind);
    let mut selected = vec![false; providers.len()];
    let mut used = stub;
    if let Some(index) = active_index
        && used < width
    {
        selected[index] = true;
        used = (used + provider_tab_cells(&providers[index])).min(width);
    }
    for (index, panel) in providers.iter().enumerate() {
        if Some(index) == active_index {
            continue;
        }
        let gap = RAIL_STUB * usize::from(selected.iter().any(|selected| *selected));
        let cells = provider_tab_cells(panel);
        if used + gap + cells <= width {
            selected[index] = true;
            used += gap + cells;
        }
    }
    selected
}

fn append_provider_tab_spans(
    spans: &mut Vec<Span<'static>>,
    theme: &Theme,
    panel: &SidebarProviderPanel,
    label: &str,
    active: bool,
) {
    let rail = theme.body();
    let fill = || Span::styled(theme.glyph(GlyphRole::ChromeHairline).to_owned(), rail);
    if !active {
        spans.push(fill());
        spans.push(Span::styled(
            format!(" {label} "),
            theme.style(theme.brand_tone(panel), Modifier::empty()),
        ));
        spans.push(fill());
        return;
    }
    // Brand fill and bold are the pick. Under no-color, caps replace fill.
    let chip = theme.chip(theme.brand_tone(panel), Modifier::BOLD);
    let (left, right) = if chip.bg.is_none() {
        (
            Span::styled(theme.glyph(GlyphRole::ChromeTabCapLeft).to_owned(), rail),
            Span::styled(theme.glyph(GlyphRole::ChromeTabCapRight).to_owned(), rail),
        )
    } else {
        (fill(), fill())
    };
    spans.push(left);
    spans.push(Span::styled(format!(" {label} "), chip));
    spans.push(right);
}

/// Width of the tab rail's leading stub and inter-tab gaps.
const RAIL_STUB: usize = 2;

/// The block's header line, with the health-colored `⇅ rc` flag pinned to the
/// top-right corner when remote control is on for the provider. Untabbed it
/// reads `Claude v2.1.158 · Claude Max` — the product name in the brand color,
/// version and plan dim. Tabbed, the rail already names the account, so the
/// name drops and the line reads `Claude Max · v2.1.158` — plan first, the
/// version demoted to trailing trivia. A raised emblem row shares the header
/// when it reserves the art column; wide stacked blocks use their spacer.
/// Plan drops out when unknown; version renders as `v?` until an out-of-band
/// probe or live context fills it.
fn provider_header_line(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    tabbed: bool,
    inline_art: bool,
    crest: Option<usize>,
    now: Timestamp,
) -> Line<'static> {
    let mut right = reset_header_spans(theme, panel, now);
    let remote_control = match panel.remote_control {
        RemoteControlBadge::Hidden => None,
        RemoteControlBadge::Healthy => Some(Component::RemoteControl),
        RemoteControlBadge::Down => Some(Component::RemoteControlDown),
        RemoteControlBadge::OffForRun => Some(Component::RemoteControlOff),
    };
    if let Some(component) = remote_control {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        let off = if panel.remote_control == RemoteControlBadge::OffForRun {
            " off"
        } else {
            ""
        };
        right.push(Span::styled(
            format!("{} rc{off}", theme.glyph(GlyphRole::ChromeRemoteControl)),
            theme.styled(component, Modifier::BOLD),
        ));
    }
    let full = provider_header_left(theme, panel, width, tabbed, inline_art, crest, true);
    let left = if spans_width(&full) + spans_width(&right) <= width {
        full
    } else {
        provider_header_left(theme, panel, width, tabbed, inline_art, crest, false)
    };
    pin_right(left, right, width)
}

/// Heat amount (0.0 green … 1.0 red) for the reset marker at `hours` until the
/// soonest credit expires, or `None` at/after 7d, where the expiry text is hidden.
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

/// The banked reset-credit marker: only Codex adds an auto-redeem forecast,
/// and the soonest expiry rides along within a week, heating only while the
/// user has to act.
fn reset_header_spans(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    now: Timestamp,
) -> Vec<Span<'static>> {
    let Some(reset_credits) = panel.reset_credits.as_ref() else {
        return Vec::new();
    };
    if reset_credits.count == 0 {
        return Vec::new();
    }
    let (glyph, glyph_style) = match panel.redeem_forecast {
        Some(RedeemForecast::Armed) => (GlyphRole::MeterRedeemArmed, theme.good(Modifier::empty())),
        Some(RedeemForecast::Holding) => (GlyphRole::MeterRedeemHold, theme.muted()),
        Some(RedeemForecast::Manual) | None => (GlyphRole::MeterReset, theme.body()),
    };
    let mut spans = vec![
        Span::styled(theme.glyph(glyph).to_owned(), glyph_style),
        Span::styled(format!(" {}", reset_credits.count), theme.body()),
    ];
    let Some(remaining) = reset_credits
        .soonest_expiry
        .map(|at| at.duration_since(now))
    else {
        return spans;
    };
    let Some(amount) = reset_expiry_heat_amount(remaining.as_secs() as f64 / 3600.0) else {
        return spans;
    };
    let tone = match panel.redeem_forecast {
        Some(RedeemForecast::Armed | RedeemForecast::Holding) => theme.calm_tone(amount),
        Some(RedeemForecast::Manual) | None => theme.heat_tone(amount),
    };
    spans.push(Span::styled(value_seam(theme), theme.faint()));
    spans.push(Span::styled(
        expiry_label(remaining),
        theme.style(tone, Modifier::empty()),
    ));
    spans
}

fn provider_header_left(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
    tabbed: bool,
    inline_art: bool,
    crest: Option<usize>,
    include_version: bool,
) -> Vec<Span<'static>> {
    let mut left = Vec::new();
    let art_fits = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    if (tabbed || inline_art) && art_fits {
        if let Some(row) = crest {
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
        if let Some(plan) = panel.plan_label() {
            left.push(Span::styled(plan, theme.muted()));
            if include_version {
                left.push(Span::styled(value_seam(theme), theme.faint()));
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
        if let Some(plan) = panel.plan_label() {
            left.push(Span::styled(value_seam(theme), theme.faint()));
            left.push(Span::styled(plan, theme.muted()));
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
    rights: Vec<Vec<Span<'static>>>,
    art: BodyArt,
) -> Vec<Line<'static>> {
    let BodyArt::Text(crest) = art else {
        let column = " ".repeat(PROVIDER_ART_WIDTH + 1);
        return rights
            .into_iter()
            .map(|right| {
                let spans = std::iter::once(Span::raw(column.clone())).chain(right);
                Line::from(trim_spans_to_width(spans.collect(), width))
            })
            .collect();
    };
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_start = crest.map_or(0, |row| row + 1);

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

/// What the body's art column holds: text art below an optional crest row, or
/// the blank cells a logo is drawn over.
enum BodyArt {
    Text(Option<usize>),
    Logo,
}

fn art_crest_row(panel: &SidebarProviderPanel, right_rows: usize) -> Option<usize> {
    (panel.art.len() > right_rows.min(PROVIDER_ART_BODY_ROWS))
        .then_some(panel.art.len().saturating_sub(PROVIDER_ART_BODY_ROWS + 1))
}

fn art_row_spans(theme: &Theme, panel: &SidebarProviderPanel, row: usize) -> Vec<Span<'static>> {
    let clipped = clip(
        panel.art.get(row).map(String::as_str).unwrap_or(""),
        PROVIDER_ART_WIDTH,
    );
    let emblem_width = panel
        .art
        .iter()
        .map(|row| text_width(&clip(row, PROVIDER_ART_WIDTH)))
        .max()
        .unwrap_or(0);
    let leading_pad = PROVIDER_ART_WIDTH.saturating_sub(emblem_width) / 2;
    let chars: Vec<char> = clipped.chars().collect();
    let brand = theme.style(theme.brand_tone(panel), Modifier::empty());
    let mut row_tints: Vec<_> = panel
        .art_tints
        .iter()
        .filter(|tint| tint.row == row && tint.len > 0)
        .collect();
    row_tints.sort_by_key(|tint| tint.start);

    let mut spans = Vec::new();
    if leading_pad > 0 {
        spans.push(Span::raw(" ".repeat(leading_pad)));
    }
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
    let Some(headline) = panel
        .spending
        .as_ref()
        .map(|spending| spending.headline.clone())
    else {
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
        .map(|budget| (budget, BudgetWindow::Day))
        .or_else(|| {
            panel
                .month_budget
                .map(|budget| (budget, BudgetWindow::Month))
        })
        .filter(|(budget, _)| budget.parked)
        .map_or_else(
            || (dollars2(headline.usd), theme.money_style(Modifier::BOLD)),
            |(budget, window)| {
                (
                    format!(
                        "{} of {}/{}",
                        dollars2(budget.spend_usd),
                        dollars_cap(budget.cap_usd),
                        window.label()
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
        compact_count,
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

/// The provider's budget rows within `region`, drawn from the panel's
/// [`SidebarProviderPanel::budget`] reading: the lapsed line, one `api` row,
/// placeholder unknown tracks (one anonymous row when the shape is unknown),
/// or the metered bars in the reading's order. Each reset reads a two-unit
/// countdown scaled to its magnitude. Each row aligns front and back within
/// `region`, so they line up across providers too. With `pixels` every bar
/// draws in pixels where it can, in the text bar's cells.
fn provider_bar_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
    mut pixels: Option<&mut MeterPixels>,
) -> Vec<Vec<Span<'static>>> {
    match panel.budget(now) {
        ProviderBudget::Undecided => Vec::new(),
        ProviderBudget::Lapsed { since } => {
            let time = since
                .map(crate::utils::time::format_local_timestamp)
                .unwrap_or_else(|| "unknown".to_owned());
            let seam = value_seam(theme);
            vec![trim_spans_to_width(
                vec![Span::styled(
                    format!(
                        "plan lapsed{seam}no {} access{seam}since {time}",
                        theme.provider_product_name(&panel.kind)
                    ),
                    theme.alarm(Modifier::BOLD),
                )],
                region,
            )]
        }
        ProviderBudget::Api(credits) => {
            vec![extra_credits_bar_row(
                theme, "api", credits, region, zones, pixels,
            )]
        }
        ProviderBudget::Pending([]) => vec![unknown_bar_row(theme, "", region, pixels)],
        ProviderBudget::Pending(labels) => labels
            .iter()
            .map(|label| unknown_bar_row(theme, label, region, pixels.as_deref_mut()))
            .collect(),
        ProviderBudget::Bars(bars) => bars
            .into_iter()
            .map(|bar| match bar {
                ProviderBar::Window { window, reading } => metered_bar_row(
                    theme,
                    window,
                    reading,
                    region,
                    zones,
                    now,
                    pixels.as_deref_mut(),
                ),
                ProviderBar::Extra(credits) => extra_credits_bar_row(
                    theme,
                    "ex",
                    credits,
                    region,
                    zones,
                    pixels.as_deref_mut(),
                ),
            })
            .collect(),
    }
}

/// One metered budget bar row: the window's label (`5h`/`7d`/`mo`), the draining
/// mana bar (filled = remaining), and the `↻ <reset>` countdown right-aligned in
/// the value column. The reset marker is toned by burn pace when the reading
/// carries one; the countdown text stays in the neutral soft tier. The label
/// mirrors its bar's remaining-budget tone. A lifted reading paints an
/// unlimited full bar with `∞` in the reset-marker cell, and an unknown one a
/// dim track that keeps the label but claims no remaining budget. Sub-cap
/// ticks draw before the ceiling tick.
fn metered_bar_row(
    theme: &Theme,
    window: &RateLimitWindow,
    reading: WindowReading,
    region: usize,
    zones: &BudgetBarConfig,
    now: Timestamp,
    pixels: Option<&mut MeterPixels>,
) -> Vec<Span<'static>> {
    let label = window_label(window);
    let bar_width = provider_bar_width(region);
    let WindowReading::Draining {
        remaining,
        resets_at,
        pace,
        sub_caps,
        ceiling,
    } = reading
    else {
        if reading == WindowReading::Unknown {
            return unknown_bar_row(theme, &label, region, pixels);
        }
        return bar_row(
            &label,
            mana_style(theme, 100, zones),
            budget_bar_spans(theme, Some(100), bar_width, zones, &[], pixels),
            unlimited_value_spans(theme),
        );
    };
    let reset = resets_at.map(|at| reset_countdown(at, now));
    let ticks: Vec<_> = sub_caps
        .into_iter()
        .map(|remaining_pct| ManaTick {
            remaining_pct,
            role: GlyphRole::MeterManaTick,
        })
        .chain(ceiling.map(|remaining_pct| ManaTick {
            remaining_pct,
            role: GlyphRole::MeterManaCeiling,
        }))
        .collect();
    let reset_marker_style = pace
        .map(|reading| pace_style(theme, reading, &zones.burn_rate))
        .unwrap_or_else(|| theme.body());
    bar_row(
        &label,
        mana_style(theme, remaining, zones),
        budget_bar_spans(theme, Some(remaining), bar_width, zones, &ticks, pixels),
        reset_value_spans(theme, reset.as_deref(), reset_marker_style),
    )
}

/// Unknown metered budget row: the same bar geometry as a reported window with
/// no usage, with either a reported or definition-declared window label, or a
/// blank label when the provider's window shape is unknown.
fn unknown_bar_row(
    theme: &Theme,
    label: &str,
    region: usize,
    pixels: Option<&mut MeterPixels>,
) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    bar_row(
        label,
        theme.muted(),
        budget_bar_spans(
            theme,
            None,
            bar_width,
            &BudgetBarConfig::default(),
            &[],
            pixels,
        ),
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

/// Paid budget: dollars left over a draining bar with a limit, or a full bar
/// for a positive uncapped balance. Unlimited usage shares the window `∞`.
fn extra_credits_bar_row(
    theme: &Theme,
    label: &str,
    credits: CreditDisplay,
    region: usize,
    zones: &BudgetBarConfig,
    pixels: Option<&mut MeterPixels>,
) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    let (remaining, value) = match credits {
        CreditDisplay::Unknown => return unknown_bar_row(theme, label, region, pixels),
        CreditDisplay::Missing | CreditDisplay::Unlimited => (100, unlimited_value_spans(theme)),
        CreditDisplay::Off => (0, money_value_spans(theme, "$0")),
        CreditDisplay::Balance(left) => (
            if left > 0.0 { 100 } else { 0 },
            money_value_spans(theme, &dollars_left(left)),
        ),
        CreditDisplay::Drain { left, percentage } => {
            (percentage, money_value_spans(theme, &dollars_left(left)))
        }
    };
    bar_row(
        label,
        mana_style(theme, remaining, zones),
        budget_bar_spans(theme, Some(remaining), bar_width, zones, &[], pixels),
        value,
    )
}

fn money_value_spans(theme: &Theme, value: &str) -> Vec<Span<'static>> {
    let clipped = clip(value, PROVIDER_VALUE_WIDTH);
    let pad = PROVIDER_VALUE_WIDTH.saturating_sub(text_width(&clipped));
    vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(clipped, theme.money_style(Modifier::BOLD)),
    ]
}

/// The bar's cell width inside a provider `region`: the region less the label,
/// the value column, and the two single-cell gaps that frame the bar. At least
/// one cell, so a narrow sidebar still paints a (short) bar.
fn provider_bar_width(region: usize) -> usize {
    region.saturating_sub(PROVIDER_BAR_ROW_FRAME_WIDTH).max(1)
}
