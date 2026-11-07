//! The theme-resolved terminal edge of the palette.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rimz::config::{ThemeConfig, ThemeProviderStyle};
use rimz::theme::{Palette, Tone, resolve_provider_brand};

use super::Mode;
use super::model::PaletteState;
use crate::cli::render::tui_color;

pub(super) struct PaletteTheme {
    palette: Palette,
    providers: BTreeMap<String, ThemeProviderStyle>,
    no_color: bool,
}

impl PaletteTheme {
    pub(super) fn resolve(config: &ThemeConfig, truecolor: bool, no_color: bool) -> Self {
        Self {
            palette: Palette::resolve(config, config.effective_theme_mode().depth(truecolor)),
            providers: config.providers.clone(),
            no_color,
        }
    }

    fn style(&self, tone: Tone) -> Style {
        if self.no_color {
            Style::default()
        } else {
            Style::default().fg(tui_color(tone))
        }
    }

    fn selected(&self) -> Style {
        let style = self
            .style(self.palette.selection())
            .add_modifier(Modifier::BOLD);
        if self.no_color {
            style
        } else {
            style.bg(tui_color(self.palette.selection_bg()))
        }
    }
}

pub(super) fn render(frame: &mut Frame<'_>, state: &PaletteState, theme: &PaletteTheme) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let body = theme.style(theme.palette.body());
    let muted = theme.style(theme.palette.muted());
    frame.render_widget(
        Paragraph::new(format!("palette: {}", state.query)).style(body),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let mut top = area.y.saturating_add(1);
    if state.mode() == Mode::Commands && area.height > 2 {
        let target = state.target().map_or("no card", |card| card.label.as_str());
        frame.render_widget(
            Paragraph::new(format!("target: {target}")).style(muted),
            Rect::new(area.x, top, area.width, 1),
        );
        top += 1;
    }
    let footer_y = area.bottom().saturating_sub(1);
    let notice = state.notice_text();
    let list_bottom = footer_y.saturating_sub(u16::from(notice.is_some()));
    let capacity = usize::from(list_bottom.saturating_sub(top) / 2);
    if capacity > 0 {
        let entries = state.entries();
        let start = state.selected.saturating_sub(capacity - 1);
        for (index, entry) in entries.iter().enumerate().skip(start).take(capacity) {
            let selected = index == state.selected;
            let row_style = if selected { theme.selected() } else { body };
            let name_style = entry.kind.as_deref().map_or(row_style, |kind| {
                let brand = resolve_provider_brand(kind, &theme.providers);
                row_style.patch(theme.style(brand.tone(&theme.palette)))
            });
            let line = Line::from(vec![
                Span::styled(if selected { "▶ " } else { "  " }, row_style),
                Span::styled(&entry.label, name_style),
                Span::styled(format!("  {}", entry.meta), row_style),
            ]);
            let y = top + ((index - start) * 2) as u16;
            frame.render_widget(
                Paragraph::new(line).style(row_style),
                Rect::new(area.x, y, area.width, 1),
            );
            frame.render_widget(
                Paragraph::new(format!("  {}", entry.detail)).style(if selected {
                    row_style
                } else {
                    muted
                }),
                Rect::new(area.x, y + 1, area.width, 1),
            );
        }
    }
    if let Some(notice) = notice.filter(|_| list_bottom >= top) {
        frame.render_widget(
            Paragraph::new(notice).style(theme.style(theme.palette.warn())),
            Rect::new(area.x, list_bottom, area.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(state.footer()).style(theme.style(theme.palette.accent())),
        Rect::new(area.x, footer_y, area.width, 1),
    );
}
