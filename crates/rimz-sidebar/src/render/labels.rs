//! Semantic sidebar vocabulary: the canonical status glyphs and mode pills.

use ratatui::style::{Color, Modifier, Style};
use rimz::feed::{AgentMode, AgentStatus};

pub(super) fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Waiting => "◆",
        AgentStatus::Failed => "✗",
        AgentStatus::Running => "▸",
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
    }
}

pub(super) fn status_style(status: AgentStatus) -> Style {
    match status {
        AgentStatus::Waiting => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        AgentStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        AgentStatus::Running => Style::default().fg(Color::Green),
        AgentStatus::Idle => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
        AgentStatus::Success => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::DIM),
    }
}

pub(super) fn mode_pill(mode: AgentMode) -> Option<&'static str> {
    match mode {
        AgentMode::Interactive | AgentMode::Unknown => None,
        AgentMode::Plan => Some("plan"),
        AgentMode::Auto => Some("auto"),
        AgentMode::Bypass => Some("bypass"),
    }
}

pub(super) fn mode_style(mode: AgentMode) -> Style {
    match mode {
        AgentMode::Bypass => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}
