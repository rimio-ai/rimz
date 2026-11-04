use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agents::AgentStatus;
use crate::config::GlyphRole;
use crate::sidebar_pane::render::labels::status_rest_style;
use crate::sidebar_pane::render::layout::{ellipsize, spans_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::store::snapshot::{SidebarWorkflow, SidebarWorkflowNode, WorkflowNodeKind};
use crate::store::workflow::RunState;

use super::{RowCtx, content_width, pin_right, strip_owner_style};
pub(super) fn workflow_line(
    ctx: &RowCtx<'_>,
    run: &SidebarWorkflow,
    owner: Option<AgentStatus>,
) -> Line<'static> {
    let width = content_width(ctx.width);
    let word = if owner.is_some() { "" } else { &run.word };
    for trim in 0..=6 {
        let mut left = vec![Span::raw("  ")];
        if trim < 4 {
            left.push(Span::styled(
                format!("{} ", run.workflow),
                ctx.theme
                    .styled(Component::WorkflowBadge, Modifier::empty()),
            ));
        }
        let current = current_spans(ctx, run, owner, if trim == 6 { "" } else { word }, trim < 5);
        let position = run.track.iter().position(|node| {
            node.name == run.detour_parent.as_deref().unwrap_or(&run.current.name)
        });
        if trim < 2
            && let Some(position) = position
        {
            for (index, node) in run.track.iter().enumerate() {
                if index > 0 {
                    left.push(Span::raw(" "));
                }
                if index == position {
                    if run.detour_parent.is_some() {
                        left.extend(detour_parent(ctx.theme, node));
                    }
                    left.extend(current.iter().cloned());
                } else {
                    left.push(node_mark(ctx.theme, node, index < position));
                }
            }
        } else {
            if trim < 3
                && run.detour_parent.is_some()
                && let Some(position) = position
            {
                left.extend(detour_parent(ctx.theme, &run.track[position]));
            }
            left.extend(current);
        }
        if trim == 6 && !word.is_empty() {
            let budget = width.saturating_sub(spans_width(&left) + 1);
            if budget > 0 {
                left.push(Span::raw(" "));
                left.push(Span::styled(
                    ellipsize(word, budget),
                    word_style(ctx.theme, run),
                ));
            }
        }
        let right = if trim == 0 {
            vec![Span::styled(
                elapsed(ctx.now, run.created_at),
                ctx.theme.muted(),
            )]
        } else {
            Vec::new()
        };
        if spans_width(&left) + spans_width(&right) + usize::from(!right.is_empty()) <= width {
            return pin_right(left, right, width);
        }
        if trim == 6 {
            return Line::from(left);
        }
    }
    unreachable!("the final trim always returns the irreducible current node")
}

fn current_spans(
    ctx: &RowCtx<'_>,
    run: &SidebarWorkflow,
    owner: Option<AgentStatus>,
    word: &str,
    duration: bool,
) -> Vec<Span<'static>> {
    let status = if run.state == RunState::Parked {
        AgentStatus::Waiting
    } else {
        AgentStatus::Running
    };
    let style = strip_owner_style(ctx, owner, status_rest_style(ctx.theme, status))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(
            ctx.theme.glyph(GlyphRole::PipelineCurrent).to_owned(),
            style,
        ),
        Span::raw(" "),
        Span::styled(run.current.name.clone(), style),
    ];
    if run.visits >= 2 {
        spans.push(Span::styled(
            format!(" ×{}", run.visits),
            ctx.theme.styled(
                if run.capped {
                    Component::WorkflowVisitsCapped
                } else {
                    Component::WorkflowVisits
                },
                Modifier::empty(),
            ),
        ));
    }
    if !word.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(word.to_owned(), word_style(ctx.theme, run)));
    }
    if duration {
        let mut time = elapsed(ctx.now, run.since);
        if let Some(limit) = &run.hold_limit {
            time.push('/');
            time.push_str(limit);
        }
        spans.push(Span::styled(format!(" {time}"), ctx.theme.muted()));
    }
    spans
}

fn elapsed(now: jiff::Timestamp, since: jiff::Timestamp) -> String {
    crate::theme::fmt::run_duration((now.as_second() - since.as_second()).max(0))
}

pub(super) fn word_style(theme: &Theme, run: &SidebarWorkflow) -> Style {
    theme.styled(
        if run.state == RunState::Parked {
            Component::WorkflowParked
        } else {
            Component::WorkflowState
        },
        Modifier::empty(),
    )
}

fn node_mark(theme: &Theme, node: &SidebarWorkflowNode, reached: bool) -> Span<'static> {
    let role = match (node.kind, reached) {
        (WorkflowNodeKind::Step, true) => GlyphRole::PipelinePassed,
        (WorkflowNodeKind::Step, false) => GlyphRole::PipelineFuture,
        (WorkflowNodeKind::Check, true) => GlyphRole::PipelineCheckPassed,
        (WorkflowNodeKind::Check, false) => GlyphRole::PipelineCheckFuture,
    };
    Span::styled(
        theme.glyph(role).to_owned(),
        if reached {
            theme.styled(Component::PipelinePassed, Modifier::empty())
        } else {
            theme.muted()
        },
    )
}

fn detour_parent(theme: &Theme, node: &SidebarWorkflowNode) -> Vec<Span<'static>> {
    vec![
        node_mark(theme, node, true),
        Span::styled(
            format!(" {} ", node.name),
            theme.styled(Component::PipelinePassed, Modifier::empty()),
        ),
        Span::styled(
            format!(
                "{}{} ",
                theme.glyph(GlyphRole::ChromeBoxBottomLeft),
                theme.glyph(GlyphRole::ChromeHairline)
            ),
            theme.styled(Component::WorkflowDetour, Modifier::empty()),
        ),
    ]
}
