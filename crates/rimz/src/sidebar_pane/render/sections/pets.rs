use crate::sidebar_pane::pets::{PetCellGrid, PetView};
use crate::sidebar_pane::render::theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Blank columns between the sprite and a side caption.
const CAPTION_GAP: usize = 2;
/// Below this much room to the sprite's right, the caption stacks underneath
/// instead of riding alongside.
const MIN_SIDE_CAPTION: usize = 6;

pub(super) fn pet_panel_lines(
    pet: Option<&PetView>,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let caption = pet
        .and_then(|view| view.caption.as_deref())
        .or_else(|| pet.and_then(|view| view.loading.then_some("fetching pet...")));
    let grid = theme
        .pet_body_enabled()
        .then(|| pet.and_then(|view| view.grid.as_ref()))
        .flatten();

    if let Some(grid) = grid {
        return body_lines(grid, caption, theme, width);
    }

    let fallback = caption.unwrap_or_else(|| {
        if pet.is_some_and(|view| view.loading) {
            "fetching pet..."
        } else {
            "resting"
        }
    });
    vec![centered_caption(fallback, theme, width)]
}

/// The sprite pinned to the right edge — lined up under the right-aligned Pets
/// tab — with its caption set to the left, vertically centered against the body
/// so the panel spends rows on art, not on a separate caption line. Narrow panels
/// (or a bodyless frame) fall back to a caption stacked underneath.
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
        .map(|line| line.chars().count())
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
                let trailing = block_w.saturating_sub(line.chars().count()) + CAPTION_GAP;
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
    let clip = |word: &str| word.chars().take(width).collect::<String>();
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = clip(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = clip(word);
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
/// narrow) frame still sits the pet under the right-aligned Pets tab.
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
    let clipped = if caption.chars().count() > width {
        caption.chars().take(width).collect::<String>()
    } else {
        caption.to_owned()
    };
    let left = width.saturating_sub(clipped.chars().count()) / 2;
    let mut spans = Vec::new();
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    spans.push(Span::styled(clipped, theme.muted()));
    Line::from(spans)
}
