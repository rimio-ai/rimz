use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::{AgentState, ProviderCapacity};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::row::SidebarRow;

use super::layout::{
    GroupEntry, GroupResolver, GroupRoots, compare_groups, group_branch_label, sort_rows,
    status_counts,
};
use super::score;
use super::{SidebarWorktreeGroup, SidebarWorktreeKind};

mod status;
mod subagents;

#[cfg(test)]
pub(in crate::store::snapshot) use subagents::{attach_sub_agents, sub_agent_from_state};

type AgentKey = (AgentKind, AgentSessionId);

struct AgentProjectionIndex<'a> {
    roots: BTreeMap<AgentKey, &'a AgentState>,
    children: BTreeMap<AgentKey, Vec<&'a AgentState>>,
}

impl<'a> AgentProjectionIndex<'a> {
    fn new(agents: &'a [AgentState]) -> Self {
        let mut roots = BTreeMap::new();
        let mut children: BTreeMap<AgentKey, Vec<&AgentState>> = BTreeMap::new();
        for agent in agents {
            if let Some(parent_id) = &agent.parent_agent_id {
                children
                    .entry((agent.kind.clone(), parent_id.clone()))
                    .or_default()
                    .push(agent);
            } else {
                roots
                    .entry((agent.kind.clone(), agent.agent_id.clone()))
                    .or_insert(agent);
            }
        }
        Self { roots, children }
    }

    fn root(&self, key: &AgentKey) -> Option<&'a AgentState> {
        self.roots.get(key).copied()
    }

    fn children(&self) -> impl Iterator<Item = (&AgentKey, &[&'a AgentState])> {
        self.children
            .iter()
            .map(|(key, children)| (key, children.as_slice()))
    }
}

/// The per-machine attention timing windows the row fold reads, bundled so the
/// fold entry point stays under the argument cap and the two knobs travel
/// together from the config that owns them.
#[derive(Clone, Copy)]
pub(super) struct AttentionWindows {
    /// Silent-`running` → actionable `!` projection window (`stalled_after_secs`).
    pub stalled_after_secs: u32,
    /// No-activity → inactive-sink window (`inactive_after_secs`).
    pub inactive_after_secs: u32,
    /// No-activity → archive-sink window (`archive_after_secs`).
    pub archive_after_secs: u32,
}

impl AttentionWindows {
    pub(super) fn from_config(config: &crate::config::AttentionConfig) -> Self {
        let inactive_after_secs = config.inactive_after_secs.get();
        // `archive_after_secs` is a display knob. A value at or below the
        // inactive window is interpreted as "just after inactive" instead of
        // rejecting the config at startup.
        let archive_after_secs = config
            .archive_after_secs
            .get()
            .max(inactive_after_secs.saturating_add(1));
        Self {
            stalled_after_secs: config.stalled_after_secs.get(),
            inactive_after_secs,
            archive_after_secs,
        }
    }
}

pub(super) struct AgentProjection<'a> {
    pub agents: &'a [AgentState],
    pub provider_capacities: &'a BTreeMap<AgentKind, ProviderCapacity>,
    pub exhausted_resumes: &'a BTreeSet<(AgentKind, AgentSessionId)>,
}

pub(super) fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agent_projection: AgentProjection<'_>,
    roots: GroupRoots<'_>,
    now: Timestamp,
    windows: AttentionWindows,
) -> Vec<SidebarWorktreeGroup> {
    let agent_index = AgentProjectionIndex::new(agent_projection.agents);
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint every live (`rows_from_panes`) card flows through, so
    // nesting behaves identically for process, agent, and attention rows.
    subagents::attach_sub_agents_indexed(&mut rows, &agent_index, now);
    // A delegating parent's work is its children's, so their activity advances
    // the parent row's displayed clock before the stall check reads it.
    subagents::fold_child_activity_onto_parents(&mut rows);
    // Project the displayed status now that each row knows its subagents and
    // the full agent set is in hand.
    status::project_display_status(
        &mut rows,
        &agent_index,
        agent_projection.provider_capacities,
        agent_projection.exhausted_resumes,
        now,
        windows.stalled_after_secs,
    );
    stamp_attention(&mut rows, now, windows);

    let resolver = GroupResolver::new(
        roots,
        rows.iter().map(|row| GroupEntry {
            channel: row.channel.as_deref(),
            path: row.worktree_path.as_deref(),
            branch: row.worktree_branch.as_deref(),
        }),
    );

    let mut by_group: BTreeMap<String, (String, SidebarWorktreeKind, Vec<SidebarRow>)> =
        BTreeMap::new();
    for row in rows {
        let identity = resolver.resolve(GroupEntry {
            channel: row.channel.as_deref(),
            path: row.worktree_path.as_deref(),
            branch: row.worktree_branch.as_deref(),
        });
        by_group
            .entry(identity.key)
            .and_modify(|(_, _, rows)| rows.push(row.clone()))
            .or_insert_with(|| (identity.label, identity.kind, vec![row]));
    }

    let mut groups = by_group
        .into_iter()
        .map(|(key, (label, kind, mut rows))| {
            sort_rows(&mut rows);
            // Prefer a branch label over the path-basename seed for worktree
            // pods. Root pods keep the room name; external keeps its catch-all
            // label even if a stray branch rode the row.
            let label = if kind == SidebarWorktreeKind::Worktree {
                group_branch_label(&rows).unwrap_or(label)
            } else {
                label
            };
            let status_counts = status_counts(&rows);
            SidebarWorktreeGroup {
                key,
                label,
                kind,
                status_counts,
                rows,
                diff_added: None,
                diff_removed: None,
                commits_ahead: None,
                commits_behind: None,
                trunk: None,
                worktree_backed: false,
                finished: false,
                clean: None,
                landed: None,
                trunk_sync: None,
                pr_state: None,
                pr_ci: None,
                pr_number: None,
                pr_url: None,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

/// Stamp attention ranking facts: no-activity age drives the inactive sink, the
/// archive sink, and the fixed-point score every later presentation sort reads.
/// Process rows are exempt from the sinks because their activity clock is
/// foreground-process start, not attention, and row ordering already seats them
/// below every agent card. Both boundaries are strict (`>`), so the configured
/// second is the last member of the previous band.
fn stamp_attention(rows: &mut [SidebarRow], now: Timestamp, windows: AttentionWindows) {
    for row in rows {
        let age_secs = score::age_secs(now, row.last_activity);
        let is_process = row.is_process();
        row.inactive = !is_process && age_secs > windows.inactive_after_secs;
        row.archived = !is_process && age_secs > windows.archive_after_secs;
        row.attention_score = if is_process {
            0
        } else {
            score::attention_score(
                row.status(),
                age_secs,
                windows.inactive_after_secs,
                windows.archive_after_secs,
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::config::AttentionConfig;
    use crate::store::snapshot::row::{AgentCard, ProcessCard, RowCard};
    use std::num::NonZeroU32;

    fn now() -> Timestamp {
        Timestamp::from_second(1_750_000_000).expect("fixed test instant is valid")
    }

    /// An idle agent row whose only activity was `age_secs` before `now`.
    fn row_aged(age_secs: i64) -> SidebarRow {
        SidebarRow {
            id: "r".into(),
            name: "r".into(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: now() - std::time::Duration::from_secs(age_secs as u64),
            card: RowCard::Agent(Box::new(AgentCard {
                status: AgentStatus::Idle,
                ..Default::default()
            })),
        }
    }

    #[test]
    fn inactive_window_is_threshold_driven_and_strict() {
        // The same row sinks or stays live by the configured window alone.
        let mut rows = vec![row_aged(1_800)];
        stamp_attention(
            &mut rows,
            now(),
            AttentionWindows {
                stalled_after_secs: 1,
                inactive_after_secs: 3_600,
                archive_after_secs: 86_400,
            },
        );
        assert!(
            !rows[0].inactive,
            "30m of silence is live under a one-hour window"
        );
        stamp_attention(
            &mut rows,
            now(),
            AttentionWindows {
                stalled_after_secs: 1,
                inactive_after_secs: 1_200,
                archive_after_secs: 86_400,
            },
        );
        assert!(
            rows[0].inactive,
            "the same row sinks under a twenty-minute window"
        );

        // The boundary is strict: the configured window is the last live second.
        let mut exact = vec![row_aged(3_600)];
        stamp_attention(
            &mut exact,
            now(),
            AttentionWindows {
                stalled_after_secs: 1,
                inactive_after_secs: 3_600,
                archive_after_secs: 86_400,
            },
        );
        assert!(!exact[0].inactive, "exactly at the window is still live");
        let mut past = vec![row_aged(3_601)];
        stamp_attention(
            &mut past,
            now(),
            AttentionWindows {
                stalled_after_secs: 1,
                inactive_after_secs: 3_600,
                archive_after_secs: 86_400,
            },
        );
        assert!(past[0].inactive, "one second past the window sinks");
    }

    #[test]
    fn process_rows_never_sink_inactive() {
        let aged = now() - std::time::Duration::from_secs(7_200);
        let mut rows = vec![
            SidebarRow {
                id: "agent".into(),
                name: "agent".into(),
                pane: None,
                worktree_path: None,
                worktree_branch: None,
                channel: None,
                unread: false,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: aged,
                card: RowCard::Agent(Box::new(AgentCard {
                    status: AgentStatus::Idle,
                    ..Default::default()
                })),
            },
            SidebarRow {
                id: "process".into(),
                name: "process".into(),
                pane: None,
                worktree_path: None,
                worktree_branch: None,
                channel: None,
                unread: false,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: aged,
                card: RowCard::Process(ProcessCard::default()),
            },
        ];

        stamp_attention(
            &mut rows,
            now(),
            AttentionWindows {
                stalled_after_secs: 1,
                inactive_after_secs: 3_600,
                archive_after_secs: 86_400,
            },
        );

        assert!(rows[0].inactive, "aged agent rows sink");
        assert!(!rows[0].archived, "aged agent rows below archive stay warm");
        assert!(!rows[1].inactive, "aged process rows stay live");
        assert!(!rows[1].archived, "aged process rows never archive");
    }

    #[test]
    fn archive_window_is_threshold_driven_and_strict() {
        let windows = AttentionWindows {
            stalled_after_secs: 1,
            inactive_after_secs: 3_600,
            archive_after_secs: 86_400,
        };
        let mut exact = vec![row_aged(86_400)];
        stamp_attention(&mut exact, now(), windows);
        assert!(!exact[0].archived, "exactly at archive is still warm");

        let mut past = vec![row_aged(86_401)];
        stamp_attention(&mut past, now(), windows);
        assert!(past[0].archived, "one second past archive parks");
        assert_eq!(
            past[0].attention_score,
            score::status_weight(AgentStatus::Idle),
            "archive band keeps flat status score"
        );
    }

    #[test]
    fn stamp_attention_records_score() {
        let windows = AttentionWindows {
            stalled_after_secs: 1,
            inactive_after_secs: 3_600,
            archive_after_secs: 86_400,
        };
        let mut rows = vec![row_aged(1_800)];
        rows[0].as_agent_mut().unwrap().status = AgentStatus::Waiting;
        stamp_attention(&mut rows, now(), windows);
        assert_eq!(
            rows[0].attention_score,
            score::attention_score(Some(AgentStatus::Waiting), 1_800, 3_600, 86_400)
        );
    }

    #[test]
    fn attention_windows_lift_archive_below_inactive() {
        let config = AttentionConfig {
            inactive_after_secs: NonZeroU32::new(3_600).unwrap(),
            archive_after_secs: NonZeroU32::new(60).unwrap(),
            ..AttentionConfig::default()
        };

        let windows = AttentionWindows::from_config(&config);

        assert_eq!(windows.inactive_after_secs, 3_600);
        assert_eq!(
            windows.archive_after_secs, 3_601,
            "archive values at or below inactive lift to the first warm second"
        );
    }
}
