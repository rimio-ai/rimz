use std::collections::{BTreeMap, HashMap};

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
    let mut components = Components::new(entries.len());
    let mut attached = HashMap::<usize, usize>::new();
    let mut latest_by_sender = HashMap::<&str, usize>::new();
    for (index, entry) in entries.iter().enumerate() {
        let sender = base_handle(&entry.chat.from);
        if entry.is_stage() {
            // A flip hangs under its flipper's latest line while the grouping
            // window holds; otherwise it stands as its own block.
            if let Some(&latest) = latest_by_sender.get(sender)
                && within_window(entries[latest].chat.at, entry.chat.at)
            {
                attached.insert(index, latest);
                components.union(index, latest);
            }
            continue;
        }
        latest_by_sender.insert(sender, index);
        if is_turn_output(entry) {
            for &opener in &openers[index] {
                components.union(index, opener);
            }
            continue;
        }
        for parent in &entry.chat.reply_to {
            if let Some(parent_index) = by_message_id.get(parent.as_str()).copied()
                && thread_edge(&entries[parent_index], entry)
            {
                components.union(index, parent_index);
            }
        }
    }

    let mut members = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..entries.len() {
        members
            .entry(components.root(index))
            .or_default()
            .push(index);
    }
    let mut blocks = members.into_values().collect::<Vec<_>>();
    blocks.sort_by_key(|members| members[0]);

    let mut display = Vec::with_capacity(entries.len());
    let mut lanes = HashMap::<usize, DisplayLane>::new();
    for members in blocks {
        let block = members[0];
        let root_at = entries[block].chat.at;
        let threaded = members.len() > 1;
        for (position, source_index) in members.into_iter().enumerate() {
            let lane = match attached.get(&source_index) {
                // The target has a lower index in this block, so it was emitted first.
                Some(target) => lanes[target].clone(),
                None if threaded && position > 0 => DisplayLane::Thread {
                    component: block,
                    root_at,
                },
                None => DisplayLane::Margin,
            };
            lanes.insert(source_index, lane.clone());
            display.push(DisplayEntry {
                entry: entries[source_index].clone(),
                lane,
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
/// Asks stay because a blocking question still needs the user.
pub(super) fn hide_harness_turns(entries: Vec<RenderEntry>) -> Vec<RenderEntry> {
    let openers = turn_openers(&entries);
    let hidden = entries
        .iter()
        .zip(&openers)
        .map(|(entry, openers)| {
            entry.is_harness()
                || (matches!(
                    entry.kind(),
                    Some(TranscriptKind::Assistant | TranscriptKind::Error)
                ) && !openers.is_empty()
                    && openers.iter().all(|&opener| entries[opener].is_harness()))
        })
        .collect::<Vec<_>>();
    entries
        .into_iter()
        .zip(hidden)
        .filter_map(|(entry, hidden)| (!hidden).then_some(entry))
        .collect()
}

/// The entries that opened each turn-output entry's turn: its resolved
/// `reply_to` parents, or, when nothing was recorded (typed prompts), the
/// latest opener for the same agent session. Other entries open no turn.
pub(super) fn turn_openers(entries: &[RenderEntry]) -> Vec<Vec<usize>> {
    let by_message_id = message_index(entries);
    let mut latest_opener = HashMap::<&AgentKey, usize>::new();
    let mut openers = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let (Some(kind), Some(agent)) = (entry.kind(), entry.agent()) else {
            openers.push(Vec::new());
            continue;
        };
        openers.push(match kind {
            TranscriptKind::Prompt
            | TranscriptKind::Message
            | TranscriptKind::SubagentReport
            | TranscriptKind::Wait => {
                latest_opener.insert(agent, index);
                Vec::new()
            }
            TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error
                if entry.chat.reply_to.is_empty() =>
            {
                latest_opener.get(agent).copied().into_iter().collect()
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

struct Components {
    parent: Vec<usize>,
}

impl Components {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn root(&mut self, index: usize) -> usize {
        let parent = self.parent[index];
        if parent == index {
            return index;
        }
        let root = self.root(parent);
        self.parent[index] = root;
        root
    }

    fn union(&mut self, left: usize, right: usize) {
        let left = self.root(left);
        let right = self.root(right);
        if left == right {
            return;
        }
        let root = left.min(right);
        self.parent[left] = root;
        self.parent[right] = root;
    }
}
