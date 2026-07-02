//! Cell-width text layout for sidebar lines.

use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn text_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(super) fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

pub(super) fn trim_spans_to_width(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut remaining = width;
    let mut trimmed = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let span_width = span.width();
        if span_width <= remaining {
            remaining -= span_width;
            trimmed.push(span);
            continue;
        }
        let content = take_cells(span.content.as_ref(), remaining);
        if !content.is_empty() {
            trimmed.push(Span::styled(content, span.style));
        }
        break;
    }
    trimmed
}

pub(super) fn pad_line_to(mut line: Line<'static>, width: usize) -> Line<'static> {
    line.spans = trim_spans_to_width(line.spans, width);
    let pad = width.saturating_sub(line.width());
    if pad > 0 {
        line.spans.push(Span::raw(" ".repeat(pad)));
    }
    line
}

pub(super) fn pin_right(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    if right.is_empty() {
        return Line::from(trim_spans_to_width(left, width));
    }
    let right_width = spans_width(&right);
    let mut spans = trim_spans_to_width(left, width.saturating_sub(right_width + 1));
    let padding = width
        .saturating_sub(spans_width(&spans) + right_width)
        .max(1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    Line::from(spans)
}

pub(super) fn truncate_left(text: &str, budget: usize) -> String {
    if text_width(text) <= budget {
        return text.to_owned();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_owned();
    }

    let tail_budget = budget - 1;
    let mut used = 0;
    let mut tail = Vec::new();
    for ch in text.chars().rev() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > tail_budget {
            break;
        }
        used += width;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}

pub(super) fn clip(text: &str, max_cells: usize) -> String {
    take_cells(text, max_cells)
}

pub(super) fn ellipsize(text: &str, max_cells: usize) -> String {
    if text_width(text) <= max_cells {
        return text.to_owned();
    }
    if max_cells == 0 {
        return String::new();
    }
    if max_cells <= 3 {
        return ".".repeat(max_cells);
    }
    format!("{}...", take_cells(text, max_cells - 3))
}

fn take_cells(content: &str, width: usize) -> String {
    let mut taken = String::new();
    let mut used = 0;
    for ch in content.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width > width {
            break;
        }
        used += char_width;
        taken.push(ch);
    }
    taken
}
