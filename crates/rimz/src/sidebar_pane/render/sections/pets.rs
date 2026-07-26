use crate::sidebar_pane::pets::{PetBody, PetCellGrid, PetView};
use crate::sidebar_pane::pixel::{image_id_color, placeholder_cluster};
use crate::sidebar_pane::render::layout::{clip, text_width};
use crate::sidebar_pane::render::theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Blank columns between the sprite and a side caption.
const CAPTION_GAP: usize = 2;
/// Below this much room to the sprite's right, the caption stacks underneath
/// instead of riding alongside.
const MIN_SIDE_CAPTION: usize = 6;
/// Dashboard pet captions leave three trailing cells so their right edge lines
/// up with the sprite body's inner gap.
const DASHBOARD_CAPTION_RIGHT_PAD: usize = 3;

pub(super) struct DashboardPetColumn {
    pub(super) width: usize,
    pub(super) caption: Line<'static>,
    pub(super) body: Vec<Line<'static>>,
}

pub(super) fn pet_panel_lines(
    pet: Option<&PetView>,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let caption = pet_caption(pet);
    let grid = theme
        .pet_body_enabled()
        .then(|| pet.and_then(|view| pet_cell_grid(view)))
        .flatten();

    if let Some(grid) = grid {
        return body_lines(grid, caption, theme, width);
    }

    let fallback = caption.unwrap_or("resting");
    vec![centered_caption(fallback, theme, width)]
}

pub(super) fn dashboard_pet_column(
    pet: Option<&PetView>,
    theme: &Theme,
    dashboard_width: usize,
) -> Option<DashboardPetColumn> {
    if !theme.pet_body_enabled() {
        return None;
    }
    let pet = pet?;
    let (width, mut body) = match pet.body.as_ref()? {
        PetBody::Pixel(pixel) => {
            let width = usize::from(pixel.size.cols);
            let pixel_style = Style::default().fg(image_id_color(pixel.image_id));
            let lines = (0..pixel.size.rows)
                .map(|row| {
                    Line::from(
                        (0..width)
                            .map(|col| {
                                Span::styled(placeholder_cluster(row, col as u16), pixel_style)
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            (width, lines)
        }
        PetBody::Cell(grid) => {
            let width = grid.iter().map(Vec::len).max().unwrap_or(0);
            (width, grid_lines(grid, width))
        }
    };
    if width == 0 {
        return None;
    }
    body.push(Line::from(" ".repeat(width)));
    Some(DashboardPetColumn {
        width,
        caption: dashboard_caption_line(pet.caption.as_deref(), theme, dashboard_width),
        body,
    })
}

fn pet_caption(pet: Option<&PetView>) -> Option<&str> {
    pet.and_then(|view| view.caption.as_deref())
}

fn pet_cell_grid(view: &PetView) -> Option<&PetCellGrid> {
    match view.body.as_ref()? {
        PetBody::Cell(grid) => Some(grid),
        PetBody::Pixel(_) => None,
    }
}

fn dashboard_caption_line(caption: Option<&str>, theme: &Theme, width: usize) -> Line<'static> {
    let Some(caption) = caption else {
        return Line::from("");
    };
    let caption_w = width.saturating_sub(DASHBOARD_CAPTION_RIGHT_PAD);
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

/// The sprite pinned to the right edge, with its caption set to the left,
/// vertically centered against the body so the panel spends rows on art, not on
/// a separate caption line. Narrow panels (or a bodyless frame) fall back to a
/// caption stacked underneath.
fn body_lines(
    grid: &PetCellGrid,
    caption: Option<&str>,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let grid_w = grid
        .iter()
        .map(|row| row.len())
        .max()
        .unwrap_or(0)
        .min(width);
    let left_edge = width.saturating_sub(grid_w);
    let side_room = left_edge.saturating_sub(CAPTION_GAP);

    let Some(caption) = caption.filter(|_| side_room >= MIN_SIDE_CAPTION) else {
        // Too tight for a side caption (or none) — sprite right-aligned, caption below.
        let mut lines = grid_lines(grid, width);
        if let Some(caption) = caption {
            lines.push(Line::from(""));
            lines.push(centered_caption(caption, theme, width));
        }
        return lines;
    };

    let wrapped = wrap_words(caption, side_room);
    // Right-align the caption block so its words hug the sprite's left edge.
    let block_w = wrapped
        .iter()
        .map(|line| text_width(line))
        .max()
        .unwrap_or(0);
    let lead = left_edge.saturating_sub(CAPTION_GAP + block_w);
    let height = grid.len().max(wrapped.len());
    let top_pad = grid.len().saturating_sub(wrapped.len()) / 2;
    let mut lines = Vec::with_capacity(height);
    for row_index in 0..height {
        let mut spans = Vec::new();
        match row_index.checked_sub(top_pad).and_then(|i| wrapped.get(i)) {
            Some(line) => {
                let trailing = block_w.saturating_sub(text_width(line)) + CAPTION_GAP;
                if lead > 0 {
                    spans.push(Span::raw(" ".repeat(lead)));
                }
                spans.push(Span::styled(line.clone(), theme.muted()));
                spans.push(Span::raw(" ".repeat(trailing)));
            }
            None => spans.push(Span::raw(" ".repeat(left_edge))),
        }
        match grid.get(row_index) {
            Some(row) => spans.extend(row.iter().take(grid_w).map(|cell| {
                Span::styled(
                    cell.ch.to_string(),
                    Style::default().fg(cell.fg).bg(cell.bg),
                )
            })),
            None => spans.push(Span::raw(" ".repeat(grid_w))),
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Greedy word wrap to `width` columns. A word longer than `width` is hard-cut
/// so a caption never overflows the panel.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = clip(word, width);
        } else if text_width(&current) + 1 + text_width(word) <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = clip(word, width);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The sprite rows right-aligned to the panel edge, so a caption-less (or too
/// narrow) frame still hugs the dashboard edge.
fn grid_lines(grid: &PetCellGrid, width: usize) -> Vec<Line<'static>> {
    grid.iter()
        .map(|row| {
            let visible = row.iter().take(width).collect::<Vec<_>>();
            let left = width.saturating_sub(visible.len());
            let mut spans = Vec::with_capacity(visible.len() + usize::from(left > 0));
            if left > 0 {
                spans.push(Span::raw(" ".repeat(left)));
            }
            spans.extend(visible.into_iter().map(|cell| {
                Span::styled(
                    cell.ch.to_string(),
                    Style::default().fg(cell.fg).bg(cell.bg),
                )
            }));
            Line::from(spans)
        })
        .collect()
}

fn centered_caption(caption: &str, theme: &Theme, width: usize) -> Line<'static> {
    let clipped = clip(caption, width);
    let left = width.saturating_sub(text_width(&clipped)) / 2;
    let mut spans = Vec::new();
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    spans.push(Span::styled(clipped, theme.muted()));
    Line::from(spans)
}
