//! Pure room folding; only the playback source owns runtime unread reconciliation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiff::Timestamp;

use super::{AgentSpec, Preset, Verb, presets};
use crate::ids::AgentKind;
use crate::store::snapshot::{SidebarRow, SidebarSnapshot, SidebarWorktreeKind};

#[derive(Clone)]
pub(super) struct Room {
    pub(super) snapshot: SidebarSnapshot,
    pub(super) agents: BTreeMap<String, ModelAgent>,
}

#[derive(Clone)]
pub(super) struct ModelAgent {
    pub(super) kind: AgentKind,
    pub(super) parent: Option<String>,
    group: String,
    row: SidebarRow,
}

impl Room {
    pub(super) fn new(preset: Preset) -> Self {
        let mut snapshot = presets::snapshot(preset);
        let mut agents = BTreeMap::new();
        for group in Arc::make_mut(&mut snapshot.worktree_groups) {
            for row in Arc::make_mut(&mut group.rows)
                .iter_mut()
                .filter(|row| row.is_agent())
            {
                let name = row
                    .as_agent()
                    .and_then(|card| card.handle.clone())
                    .unwrap_or_else(|| row.name.clone());
                let mut handle = name.clone();
                let mut ordinal = 2;
                while agents.contains_key(&handle) {
                    handle = format!("{name}-{ordinal}");
                    ordinal += 1;
                }
                if let Some(card) = row.as_agent_mut() {
                    card.handle = Some(handle.clone());
                }
                agents.insert(
                    handle,
                    ModelAgent {
                        kind: AgentKind::new_unchecked(row.name.clone()),
                        parent: None,
                        group: group.key.clone(),
                        row: row.clone(),
                    },
                );
            }
        }
        Self { snapshot, agents }
    }

    pub(super) fn declare(&mut self, spec: &AgentSpec, parent: Option<String>, now: Timestamp) {
        let ordinal = self
            .snapshot
            .rows()
            .chain(self.agents.values().map(|agent| &agent.row))
            .flat_map(SidebarRow::pane_ids)
            .filter_map(crate::ids::PaneId::creation_ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let row = presets::scenario_agent(spec, ordinal, now);
        let group = self
            .snapshot
            .worktree_groups
            .iter()
            .find(|group| {
                (group.kind == SidebarWorktreeKind::Channel) == spec.group.starts_with('#')
                    && (group.key == spec.group
                        || group.label == spec.group.trim_start_matches('#'))
            })
            .map_or_else(|| spec.group.clone(), |group| group.key.clone());
        if !self
            .snapshot
            .wired_kinds
            .iter()
            .any(|kind| kind == spec.kind.as_str())
        {
            self.snapshot.wired_kinds.push(spec.kind.to_string());
        }
        self.agents.insert(
            spec.handle.clone(),
            ModelAgent {
                kind: spec.kind.clone(),
                parent,
                group,
                row,
            },
        );
    }

    pub(super) fn apply(&mut self, handle: &str, verb: &Verb, now: Timestamp) {
        if let Verb::Spawns {
            handle: child,
            kind,
            task,
        } = verb
        {
            // Parsing admits only live handles; every spawn adds a fresh child.
            let group = self.agents[handle].group.clone();
            self.declare(
                &AgentSpec {
                    handle: child.clone(),
                    kind: kind.clone(),
                    group,
                },
                Some(handle.to_owned()),
                now,
            );
            presets::apply_verb(
                &mut self.agents.get_mut(child).expect("just declared child").row,
                &Verb::Runs(task.clone().unwrap_or_default()),
                now,
            );
            return;
        }
        if matches!(verb, Verb::Leaves) {
            let gone = descendants(&self.agents, handle);
            self.agents.retain(|handle, _| !gone.contains(handle));
            return;
        }
        // Parsing checks each target against declarations, spawns, and departures.
        let agent = self.agents.get_mut(handle).expect("parsed target is live");
        presets::apply_verb(&mut agent.row, verb, now);
    }

    pub(super) fn snapshot(mut self, now: Timestamp) -> SidebarSnapshot {
        self.snapshot.now = now;
        self.snapshot.generated_at = now;
        self.snapshot.panes_produced_at_ms = Some(now.as_millisecond() as u64);
        self.snapshot.panes_observed_at_ms = Some(now.as_millisecond() as u64);
        for group in Arc::make_mut(&mut self.snapshot.worktree_groups) {
            Arc::make_mut(&mut group.rows).retain(SidebarRow::is_process);
        }
        let mut children: BTreeMap<String, Vec<_>> = BTreeMap::new();
        for agent in self.agents.values().filter(|agent| agent.parent.is_some()) {
            let mut parent = agent.parent.as_deref();
            let mut root = None;
            while let Some(handle) = parent {
                root = Some(handle.to_owned());
                parent = self.agents[handle].parent.as_deref();
            }
            if let Some(root) = root {
                children
                    .entry(root)
                    .or_default()
                    .push(presets::scenario_child(&agent.row, now));
            }
        }
        for child_rows in children.values_mut() {
            child_rows
                .sort_by_key(|child| child.pane.as_ref().and_then(|pane| pane.creation_ordinal()));
        }
        for (handle, mut agent) in self
            .agents
            .into_iter()
            .filter(|(_, agent)| agent.parent.is_none())
        {
            if let Some(children) = children.remove(&handle)
                && let Some(card) = agent.row.as_agent_mut()
            {
                card.sub_agents.extend(children);
                card.sub_agent_count = card.sub_agents.len() as u32;
                card.sub_agent_cost_usd = card.sub_agents.iter().fold(None, |cost, child| {
                    crate::agents::spending::sum_optional_cost(cost, child.cost_usd)
                });
            }
            let groups = Arc::make_mut(&mut self.snapshot.worktree_groups);
            if !groups.iter().any(|group| group.key == agent.group) {
                groups.push(presets::scenario_group(&agent.group));
            }
            // The group is either present in the preset or was just inserted.
            let group = groups
                .iter_mut()
                .find(|group| group.key == agent.group)
                .expect("agent group exists");
            Arc::make_mut(&mut group.rows).push(agent.row);
        }
        Arc::make_mut(&mut self.snapshot.worktree_groups)
            .retain(|group| !group.rows.is_empty() || group.pipeline.is_some());
        self.snapshot.score_scenario_rows();
        for group in Arc::make_mut(&mut self.snapshot.worktree_groups) {
            group.status_counts = presets::status_counts_from_rows(&group.rows);
        }
        self.snapshot.sort_groups_for_presentation();
        self.snapshot
    }
}

fn descendants(agents: &BTreeMap<String, ModelAgent>, handle: &str) -> BTreeSet<String> {
    let mut gone = BTreeSet::from([handle.to_owned()]);
    loop {
        let before = gone.len();
        for (handle, agent) in agents {
            if agent
                .parent
                .as_ref()
                .is_some_and(|parent| gone.contains(parent))
            {
                gone.insert(handle.clone());
            }
        }
        if gone.len() == before {
            return gone;
        }
    }
}
