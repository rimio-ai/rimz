//! Shared type-led delegation entry, with a pinned right cluster and optional detail.

use super::*;

pub(super) struct Entry {
    pub(super) lead: Span<'static>,
    pub(super) kind: String,
    pub(super) headline: Option<String>,
    pub(super) right: Vec<Span<'static>>,
    pub(super) detail: Option<Line<'static>>,
}

pub(super) fn push_entry(ctx: &RowCtx<'_>, lines: &mut Vec<Line<'static>>, entry: Entry) {
    let theme = ctx.theme;
    let mut left = vec![
        Span::raw("    "),
        entry.lead,
        Span::raw(" "),
        Span::styled(entry.kind, theme.body()),
    ];
    if let Some(headline) = entry.headline.filter(|headline| !headline.is_empty()) {
        left.push(Span::styled(value_seam(theme), theme.muted()));
        left.push(Span::styled(headline, theme.body()));
    }
    lines.push(pin_right(left, entry.right, content_width(ctx.width)));
    if let Some(detail) = entry.detail {
        lines.push(detail);
    }
}
