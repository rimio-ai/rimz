//! Per-section composition: feed groups, agent rollup, activity stream.
//! The four sidebar groups documented in DESIGN.md each map to one entry
//! point here; `mod.rs` orchestrates the order.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::feed::{AgentState, ResolverStepState};
use rimz::{EventEnvelope, FeedItem, SidebarActivity, Surface};

use super::fmt::{clip, time_ago, time_remaining, worktree_from_path};
use super::labels::{agent_mode, agent_status, kind_label, resolution_method, status_label};

pub(super) const MAX_ROWS_PER_GROUP: usize = 8;

#[derive(Clone, Copy)]
pub(super) enum SectionMode {
    NeedsAttention,
    ResolverWorking,
    RecentlyAnswered,
    RecentActivity,
}

pub(super) fn section_title(lines: &mut Vec<Line<'static>>, title: &'static str) {
    lines.push(Line::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
}

pub(super) fn feed_section(
    lines: &mut Vec<Line<'static>>,
    title: &'static str,
    items: &[FeedItem],
    mode: SectionMode,
    group_by_worktree: bool,
) {
    section_title(lines, title);
    if items.is_empty() {
        lines.push(Line::from("  -"));
        lines.push(Line::from(""));
        return;
    }

    let mut current_group = String::new();
    for item in items.iter().take(MAX_ROWS_PER_GROUP) {
        if group_by_worktree {
            let group = worktree_from_path(item.worktree_path.as_deref());
            if group != current_group {
                current_group = group.clone();
                lines.push(Line::styled(
                    format!("  Worktree: {group}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        lines.push(feed_item_line(item, mode));
    }
    if items.len() > MAX_ROWS_PER_GROUP {
        lines.push(overflow_line(items.len() - MAX_ROWS_PER_GROUP));
    }
    lines.push(Line::from(""));
}

pub(super) fn activity_section(
    lines: &mut Vec<Line<'static>>,
    title: &'static str,
    items: &[SidebarActivity],
) {
    section_title(lines, title);
    if items.is_empty() {
        lines.push(Line::from("  -"));
        lines.push(Line::from(""));
        return;
    }

    for item in items.iter().take(MAX_ROWS_PER_GROUP) {
        lines.push(activity_line(item));
    }
    if items.len() > MAX_ROWS_PER_GROUP {
        lines.push(overflow_line(items.len() - MAX_ROWS_PER_GROUP));
    }
    lines.push(Line::from(""));
}

pub(super) fn agent_line(agent: &AgentState) -> Line<'static> {
    let worktree = agent
        .worktree_path
        .as_deref()
        .map(|path| worktree_from_path(Some(path)))
        .or_else(|| {
            agent
                .worktree_branch
                .as_ref()
                .filter(|branch| !branch.is_empty())
                .cloned()
        })
        .unwrap_or_else(|| "Workspace".to_owned());
    Line::from(format!(
        "  {:<10} {:<8} {:<11} {}",
        clip(&agent.kind, 10),
        agent_status(agent.status),
        agent_mode(agent.mode),
        worktree
    ))
}

fn overflow_line(count: usize) -> Line<'static> {
    Line::styled(
        format!("  +{count} more"),
        Style::default().fg(Color::DarkGray),
    )
}

fn feed_item_line(item: &FeedItem, mode: SectionMode) -> Line<'static> {
    let left = format!(
        "  {:<10} {:<9} ",
        clip(&item.source, 10),
        status_label(item.status, item.surface)
    );
    let detail = match mode {
        SectionMode::NeedsAttention => needs_attention_detail(item),
        SectionMode::ResolverWorking => resolver_detail(item),
        SectionMode::RecentlyAnswered => answered_detail(item),
        SectionMode::RecentActivity => format!("{}: {}", kind_label(item.kind), item.title),
    };
    Line::from(vec![
        Span::styled(left, Style::default().fg(Color::DarkGray)),
        Span::raw(detail),
    ])
}

fn activity_line(activity: &SidebarActivity) -> Line<'static> {
    match activity {
        SidebarActivity::Feed { item } => feed_item_line(item, SectionMode::RecentActivity),
        SidebarActivity::Event { event } => event_line(event),
    }
}

fn event_line(event: &EventEnvelope) -> Line<'static> {
    let left = format!("  {:<10} {:<9} ", clip(&event.source, 10), "event");
    let kind = event
        .params
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&event.method);
    let title = event
        .params
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("activity");
    Line::from(vec![
        Span::styled(left, Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{kind}: {title} ({})", time_ago(event.timestamp))),
    ])
}

fn needs_attention_detail(item: &FeedItem) -> String {
    let mut detail = format!("{}: {}", kind_label(item.kind), item.title);
    match item.surface {
        Surface::NativeUi => detail.push_str(" [focus] [dismiss]"),
        Surface::Script if !item.options.is_empty() => {
            detail.push(' ');
            detail.push_str(
                &item
                    .options
                    .iter()
                    .take(4)
                    .map(|option| format!("[{}]", clip(option, 12)))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        Surface::Bridge => detail.push_str(" [override]"),
        Surface::Script => {}
    }
    detail
}

fn resolver_detail(item: &FeedItem) -> String {
    let active = item
        .chain_active_resolver
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            item.chain
                .iter()
                .find(|step| matches!(step.state, ResolverStepState::Active))
                .map(|step| step.resolver_id.to_string())
        })
        .unwrap_or_else(|| "resolver".to_owned());
    let remaining = item
        .chain_active_until
        .map(time_remaining)
        .unwrap_or_else(|| "budget unknown".to_owned());
    format!("{active} active - {remaining} - {}", item.title)
}

fn answered_detail(item: &FeedItem) -> String {
    let method = item
        .resolution
        .as_ref()
        .map(|resolution| resolution_method(resolution.method))
        .unwrap_or("unknown");
    format!(
        "{}: {} ({method}, {})",
        kind_label(item.kind),
        item.title,
        time_ago(item.updated_at)
    )
}
