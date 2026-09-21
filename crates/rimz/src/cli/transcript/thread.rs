use std::collections::{BTreeMap, HashMap, HashSet};

use super::chat::{base_handle, within_window};
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DisplayLane {
    Margin,
    Thread {
        component: usize,
        root_at: Option<jiff::Timestamp>,
    },
}

impl DisplayLane {
    pub(super) fn is_margin(&self) -> bool {
        matches!(self, Self::Margin)
    }

    pub(super) fn group_key(&self) -> Option<usize> {
        match self {
            Self::Margin => None,
            Self::Thread { component, .. } => Some(*component),
        }
    }

    pub(super) fn root_at(&self) -> Option<jiff::Timestamp> {
        match self {
            Self::Margin => None,
            Self::Thread { root_at, .. } => *root_at,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct DisplayEntry {
    pub(super) entry: RenderEntry,
    pub(super) lane: DisplayLane,
    pub(super) block: usize,
    pub(super) archived: bool,
    pub(super) source_index: usize,
}

pub(super) fn entries_for_view(view: &RenderedChat) -> Vec<DisplayEntry> {
    let mut entries = assemble_threads(&view.entries, view.archive_prefix, view.flat);
    keep_last_blocks(&mut entries, view.last);
    entries
}

pub(super) fn selected_chat_lines(view: &RenderedChat) -> Vec<ChatLine> {
    let mut entries = entries_for_view(view);
    entries.sort_by_key(|entry| entry.source_index);
    entries.into_iter().map(|entry| entry.entry.chat).collect()
}

pub(super) fn flat_entries(entries: &[RenderEntry], archive_prefix: usize) -> Vec<DisplayEntry> {
    entries
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, entry)| DisplayEntry {
            entry,
            lane: DisplayLane::Margin,
            block: index,
            archived: index < archive_prefix,
            source_index: index,
        })
        .collect()
}

/// Lays the view out in threads without moving the conversation in time.
/// Prompts, messages and flips keep their order: one continues the thread of
/// the entry it answers only while that thread is still the latest
/// conversation, and opens a new thread at the margin once anything else has
/// intervened. Turn output and answers move instead, under the entry that
/// opened them ([`turn_openers`]) however late they ran, and leave the latest
/// conversation as they found it.
pub(super) fn assemble_threads(
    entries: &[RenderEntry],
    archive_prefix: usize,
    flat: bool,
) -> Vec<DisplayEntry> {
    if flat || entries.len() < 2 {
        return flat_entries(entries, archive_prefix);
    }

    let by_message_id = message_index(entries);
    let openers = turn_openers(entries);
    let mut heads = Vec::<usize>::with_capacity(entries.len());
    let mut lanes = Vec::<DisplayLane>::with_capacity(entries.len());
    let mut current = None;
    let mut latest_by_sender = HashMap::<&str, usize>::new();
    for (index, entry) in entries.iter().enumerate() {
        let sender = base_handle(&entry.chat.from);
        let ordered = !is_turn_output(entry) && entry.kind() != Some(TranscriptKind::Answer);
        let target = if entry.is_stage() {
            latest_by_sender
                .get(sender)
                .copied()
                .filter(|&latest| within_window(entries[latest].chat.at, entry.chat.at))
        } else if is_turn_output(entry) {
            openers[index]
                .iter()
                .copied()
                .filter(|&opener| opener < index)
                .max()
        } else {
            entry
                .chat
                .reply_to
                .iter()
                .filter_map(|parent| by_message_id.get(parent.as_str()).copied())
                .filter(|&parent| parent < index && thread_edge(&entries[parent], entry))
                .max()
        };
        let target = target.filter(|&target| !ordered || Some(heads[target]) == current);
        let head = target.map_or(index, |target| heads[target]);
        let lane = match target {
            Some(target) if entry.is_stage() => lanes[target].clone(),
            Some(_) => DisplayLane::Thread {
                component: head,
                root_at: entries[head].chat.at,
            },
            None => DisplayLane::Margin,
        };
        heads.push(head);
        lanes.push(lane);
        if ordered {
            current = Some(head);
        }
        if !entry.is_stage() {
            latest_by_sender.insert(sender, index);
        }
    }

    let mut members = BTreeMap::<usize, Vec<usize>>::new();
    for (index, head) in heads.into_iter().enumerate() {
        members.entry(head).or_default().push(index);
    }

    let mut display = Vec::with_capacity(entries.len());
    for (block, members) in members {
        for source_index in members {
            display.push(DisplayEntry {
                entry: entries[source_index].clone(),
                lane: lanes[source_index].clone(),
                block,
                archived: source_index < archive_prefix,
                source_index,
            });
        }
    }
    display
}

/// Drops RimZ automation from the human view as whole turns: harness openers,
/// and the `Assistant`/`Error` output of turns opened only by harness messages.
/// Asks stay because a blocking question still needs the user, and so does the
/// output of a turn that asked one. `asked_ids` names the messages whose turns
/// asked in the full log, since superseded asks leave the view before this runs.
pub(super) fn hide_harness_turns(
    entries: Vec<RenderEntry>,
    asked_ids: &HashSet<String>,
) -> Vec<RenderEntry> {
    let openers = turn_openers(&entries);
    // A turn that asked the user ends in a reply to their answer.
    let asked_turns = entries
        .iter()
        .zip(&openers)
        .filter(|(entry, _)| entry.kind() == Some(TranscriptKind::Ask))
        .flat_map(|(_, openers)| openers.iter().copied())
        .collect::<HashSet<_>>();
    let hidden = entries
        .iter()
        .zip(&openers)
        .map(|(entry, openers)| {
            entry.is_harness()
                || (matches!(
                    entry.kind(),
                    Some(TranscriptKind::Assistant | TranscriptKind::Error)
                ) && !openers.is_empty()
                    && openers.iter().all(|&opener| {
                        let opener_entry = &entries[opener];
                        opener_entry.is_harness()
                            && !asked_turns.contains(&opener)
                            && opener_entry
                                .chat
                                .message_id
                                .as_ref()
                                .is_none_or(|id| !asked_ids.contains(id))
                    }))
        })
        .collect::<Vec<_>>();
    let opener_hidden = entries
        .iter()
        .zip(&openers)
        .map(|(entry, openers)| {
            entry.chat.reply_to.is_empty() && openers.iter().any(|&opener| hidden[opener])
        })
        .collect::<Vec<_>>();
    entries
        .into_iter()
        .zip(hidden)
        .zip(opener_hidden)
        .filter_map(|((mut entry, hidden), fallback_hidden)| {
            if let LineSource::Log { opener_hidden, .. } = &mut entry.source {
                *opener_hidden = fallback_hidden;
            }
            (!hidden).then_some(entry)
        })
        .collect()
}

/// The entries that opened each turn-output entry's turn: its resolved
/// `reply_to` parents, or, when nothing was recorded (typed prompts), the
/// latest opener for the same agent session, unless that opener was hidden.
/// Other entries open no turn.
pub(super) fn turn_openers(entries: &[RenderEntry]) -> Vec<Vec<usize>> {
    let by_message_id = message_index(entries);
    let mut agent_openers = HashMap::<&AgentKey, Vec<usize>>::new();
    let mut openers = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let LineSource::Log {
            kind,
            agent,
            opener_hidden,
            ..
        } = &entry.source
        else {
            openers.push(Vec::new());
            continue;
        };
        openers.push(match kind {
            TranscriptKind::Prompt
            | TranscriptKind::Message
            | TranscriptKind::SubagentReport
            | TranscriptKind::Wait => {
                agent_openers.entry(agent).or_default().push(index);
                Vec::new()
            }
            TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error
                if *opener_hidden =>
            {
                Vec::new()
            }
            TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error
                if entry.chat.reply_to.is_empty() =>
            {
                agent_openers
                    .get(agent)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&candidate| {
                        entries[candidate]
                            .chat
                            .arrived_at()
                            .zip(entry.chat.at)
                            .is_none_or(|(arrival, output)| arrival <= output)
                    })
                    .max_by(|&left, &right| {
                        scope::compare_optional_timestamps(
                            entries[left].chat.arrived_at(),
                            entries[right].chat.arrived_at(),
                        )
                        .then_with(|| left.cmp(&right))
                    })
                    .into_iter()
                    .collect()
            }
            TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error => entry
                .chat
                .reply_to
                .iter()
                .filter_map(|parent| by_message_id.get(parent.as_str()).copied())
                .collect(),
            TranscriptKind::Answer => Vec::new(),
        });
    }
    openers
}

fn is_turn_output(entry: &RenderEntry) -> bool {
    matches!(
        entry.kind(),
        Some(TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error)
    )
}

fn message_index(entries: &[RenderEntry]) -> HashMap<&str, usize> {
    let mut by_message_id = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(message_id) = entry.chat.message_id.as_deref() {
            by_message_id.entry(message_id).or_insert(index);
        }
    }
    by_message_id
}

pub(super) fn keep_last_blocks(entries: &mut Vec<DisplayEntry>, last: Option<usize>) {
    let Some(last) = last else {
        return;
    };
    let len = entries.len();
    let mut drop = len.saturating_sub(last);
    while drop > 0 && drop < len && !entries[drop].lane.is_margin() {
        drop -= 1;
    }
    if drop > 0 {
        entries.drain(..drop);
    }
}

/// A causal `reply_to` edge joins a thread only when it continues the
/// conversation: a turn's output pairs with the message that opened the turn
/// (see [`turn_openers`]), and a sent message continues the thread only as a
/// reply back to its parent's sender. A hand-off to a third party roots a new
/// exchange.
fn thread_edge(parent: &RenderEntry, child: &RenderEntry) -> bool {
    let Some(kind) = child.kind() else {
        return false;
    };
    match kind {
        TranscriptKind::Assistant
        | TranscriptKind::Ask
        | TranscriptKind::Error
        | TranscriptKind::Answer => true,
        TranscriptKind::Prompt
        | TranscriptKind::Message
        | TranscriptKind::SubagentReport
        | TranscriptKind::Wait => {
            parent.chat.from != "user"
                && child
                    .chat
                    .to
                    .as_deref()
                    .is_some_and(|to| base_handle(to) == base_handle(&parent.chat.from))
        }
    }
}
