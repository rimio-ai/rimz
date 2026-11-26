//! Type-led wait entries share the subagent layout and pin armed age right. Agents the card awaits outside its subagents list come first, one entry each named by profile and named lane, else `agent`: a live one behind its own state lead, a click target and cursor stop for its pane, and a settled run still reporting or an agent with no row behind the static signal lead. Shell waits and background shells carry their command on line 2 when one is known, and signals carry a deadline there when present. Timer, condition, and team waits take one line; a team wait names the team and lane its wait names, then its stage, which never clips. Live watches retain the working animation; timers, signals, and teams use static leads. Waits precede background shells and then scripts shown past their threshold, which draw as shells labelled with the script description (else `script`), with signals last and a pending idle stop after them. Subagent waits draw only through the card's awaited list, so a child with its own entry is not listed twice, and a team wait on a team the card launched draws only under launched.

use jiff::Timestamp;

use crate::agents::{
    BackgroundShell, PendingIdleStop, PendingWait, PendingWaitTrigger, single_line_description,
    usable_description,
};
use crate::ids::{LaneName, TeamInstanceId};
use crate::proc::command::{command_program_basename, program_label};
use crate::sidebar_pane::render::labels::hour_age_glyph;
use crate::store::snapshot::{CardScript, SidebarAwaitedAgent, SidebarLaunched};

use super::*;

/// The waits drawn as wait entries: no `Subagent` wait, which draws through the card's
/// awaited list, and no `Team` wait on a team the card launched, which lists under
/// launched.
pub(super) fn visible_waits<'a>(
    waits: &'a [PendingWait],
    launched: &'a [SidebarLaunched],
) -> impl Iterator<Item = &'a PendingWait> {
    waits.iter().filter(|wait| match wait.trigger {
        PendingWaitTrigger::Subagent { .. } => false,
        PendingWaitTrigger::Team { .. } => !launched.iter().any(|entry| {
            entry
                .team
                .as_ref()
                .is_some_and(|team| wait.name.parse::<TeamInstanceId>().ok().as_ref() == Some(team))
        }),
        _ => true,
    })
}

/// An awaited agent whose run is working or compacting: its entry animates, as a child's does.
pub(super) fn awaited_in_motion(awaited: &SidebarAwaitedAgent) -> bool {
    awaited.settled.is_none()
        && awaited.status.is_some_and(|status| {
            status == AgentStatus::Running || (awaited.compacting && !status.is_attention())
        })
}

/// One entry per awaited agent, in the card's order, each line targeting the
/// awaited pane when it has one.
pub(super) fn awaited_entry_lines(
    ctx: &RowCtx<'_>,
    awaited: &[SidebarAwaitedAgent],
    selected: bool,
) -> Vec<CardLine> {
    let theme = ctx.theme;
    let mut lines = Vec::new();
    for agent in awaited {
        let headline = match &agent.settled {
            Some(status) => Some(format!("{status}, reporting")),
            None => agent.description.clone(),
        };
        let mut entry = entry(
            ctx,
            theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
            "agent",
            headline,
            None,
            agent.since,
        );
        entry.who = agent_who(
            theme,
            Span::styled(
                agent.profile.clone().unwrap_or_else(|| "agent".to_owned()),
                theme.styled(Component::EntryAgentName, Modifier::empty()),
            ),
            agent.lane.as_deref(),
        );
        if let (Some(status), None) = (agent.status, &agent.settled) {
            entry.lead = if agent.compacting && !status.is_attention() {
                Span::styled(
                    role_glyph(theme, AnimationRole::Compacting, ctx.head_step),
                    role_style(theme, AnimationRole::Compacting, ctx.animation_phase),
                )
            } else {
                Span::styled(
                    agent_glyph(theme, status, agent.phase, ctx.head_step),
                    agent_role_style_at(theme, status, agent.phase, ctx.animation_phase),
                )
            };
        }
        entry.focused = selected
            && agent
                .pane
                .as_ref()
                .is_some_and(|pane| ctx.entry_cursor == Some(&EntryCursor::SubAgent(pane.clone())));
        let mut entry_lines = Vec::new();
        push_entry(ctx, &mut entry_lines, entry);
        lines.extend(entry_lines.into_iter().map(|line| CardLine {
            line,
            target: agent.pane.clone().map(HitTarget::SubAgentPane),
        }));
    }
    lines
}

/// A wait its watcher is actively working on: it animates while armed.
pub(super) fn is_live_watch(trigger: &PendingWaitTrigger) -> bool {
    matches!(trigger, PendingWaitTrigger::Command { .. })
}

pub(super) fn wait_entry_lines<'a>(
    ctx: &RowCtx<'_>,
    waits: &[PendingWait],
    launched: &[SidebarLaunched],
    shells: &[BackgroundShell],
    scripts: impl Iterator<Item = &'a CardScript>,
    idle_stop: Option<&PendingIdleStop>,
) -> Vec<Line<'static>> {
    let (signals, others): (Vec<_>, Vec<_>) = visible_waits(waits, launched).partition(|wait| {
        matches!(
            wait.trigger,
            PendingWaitTrigger::Signal { .. } | PendingWaitTrigger::Condition { .. }
        )
    });
    let mut lines = Vec::new();
    for entry in others
        .into_iter()
        .map(|wait| wait_entry(ctx, wait))
        .chain(shells.iter().map(|shell| shell_entry(ctx, shell)))
        .chain(scripts.map(|script| script_entry(ctx, script)))
        .chain(signals.into_iter().map(|wait| wait_entry(ctx, wait)))
        .chain(idle_stop.map(|stop| {
            entry(
                ctx,
                ctx.theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
                "stop",
                Some(stop.label(ctx.now)),
                None,
                Some(stop.stop.requested_at),
            )
        }))
    {
        push_entry(ctx, &mut lines, entry);
    }
    lines
}

fn wait_entry(ctx: &RowCtx<'_>, wait: &PendingWait) -> Entry {
    let theme = ctx.theme;
    let lead = match wait.trigger {
        PendingWaitTrigger::Timer { .. } => theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
        PendingWaitTrigger::Signal { .. }
        | PendingWaitTrigger::Team { .. }
        | PendingWaitTrigger::Workflow { .. }
        | PendingWaitTrigger::Condition { .. } => theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
        _ => role_glyph(theme, AnimationRole::Working, ctx.head_step),
    };
    let mut entry = entry(
        ctx,
        lead,
        wait.trigger.kind_word(),
        Some(wait.trigger.headline(ctx.now)),
        wait.trigger.detail(),
        wait.armed_at,
    );
    if let PendingWaitTrigger::Team { stage } = &wait.trigger {
        let team = wait.name.parse::<TeamInstanceId>().ok();
        let lane = team
            .as_ref()
            .and_then(TeamInstanceId::lane)
            .filter(|lane| **lane != LaneName::trunk());
        entry.who = team_who(
            theme,
            team.as_ref().map(TeamInstanceId::team),
            lane.map(LaneName::as_str),
        );
        entry.headline = match stage {
            Some(stage) => stage_spans(theme, stage, false),
            None => vec![Span::styled("stage unknown", theme.muted())],
        };
        entry.clip_who = true;
    }
    entry
}

fn shell_entry(ctx: &RowCtx<'_>, shell: &BackgroundShell) -> Entry {
    let detail = shell
        .command
        .as_deref()
        .and_then(|command| single_line_description(&command_program_basename(command)));
    let headline = shell
        .description
        .as_deref()
        .filter(|value| usable_description(value))
        .and_then(single_line_description)
        .or_else(|| shell.command.as_deref().map(program_label));
    entry(
        ctx,
        role_glyph(ctx.theme, AnimationRole::Working, ctx.head_step),
        "shell",
        headline,
        detail,
        Some(shell.started_at),
    )
}

/// A script past the shown threshold, drawn as a shell whose headline is the
/// script's label.
fn script_entry(ctx: &RowCtx<'_>, script: &CardScript) -> Entry {
    entry(
        ctx,
        role_glyph(ctx.theme, AnimationRole::Working, ctx.head_step),
        "shell",
        Some(script.label.clone()),
        None,
        Some(script.started_at),
    )
}

/// A wait's words in the shared entry shape: the lead in the wait tone, the
/// armed age pinned right, and the detail muted on line 2.
fn entry(
    ctx: &RowCtx<'_>,
    lead: String,
    kind: &str,
    headline: Option<String>,
    detail: Option<String>,
    since: Option<Timestamp>,
) -> Entry {
    let theme = ctx.theme;
    Entry {
        focused: false,
        lead: Span::styled(lead, theme.styled(Component::WaitHeader, Modifier::empty())),
        who: vec![Span::styled(kind.to_owned(), theme.body())],
        headline: headline
            .map(|headline| Span::styled(headline, theme.body()))
            .into_iter()
            .collect(),
        marker: Vec::new(),
        clip_who: false,
        right: since
            .map(|at| {
                let secs = age_secs(at, ctx.now);
                vec![Span::styled(
                    format!("{} {:>3}", hour_age_glyph(theme, secs), elapsed_label(secs)),
                    theme.muted(),
                )]
            })
            .unwrap_or_default(),
        detail: detail.map(|detail| {
            Line::from(trim_spans_to_width(
                vec![Span::raw("      "), Span::styled(detail, theme.muted())],
                content_width(ctx.width),
            ))
        }),
    }
}
