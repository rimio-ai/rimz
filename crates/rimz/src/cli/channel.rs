//! `rimz channel` — durable named cooperation lanes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{GlobalFlags, machine_config, open_store};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd, TabOptions};
use rimz::room::{RoomContext, RoomSizing};
use rimz::workspace::{RootClass, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct ChannelArgs {
    #[command(subcommand)]
    command: ChannelSubcmd,
}

#[derive(Debug, Subcommand)]
enum ChannelSubcmd {
    /// Create a durable named channel.
    New {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// List named and live channels.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a named channel record.
    #[command(alias = "remove")]
    Rm {
        #[arg(
            value_name = "NAME",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
        )]
        name: String,
    },
}

pub fn run(args: ChannelArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace =
        WorkspaceResolver::resolve(".", globals.root.clone()).context("resolving current room")?;
    let store = open_store(&workspace)?;
    match args.command {
        ChannelSubcmd::New { name } => {
            ensure_named_channel_available(&workspace, &name)?;
            let record = rimz::channel::register(store.paths(), &name)?;
            open_channel_tab(&workspace, globals, &record.name);
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("created {}", record.name);
            }
            Ok(())
        }
        ChannelSubcmd::List { json } => list_channels(&workspace, &store, json),
        ChannelSubcmd::Rm { name } => {
            let removed = rimz::channel::remove(store.paths(), &name)?;
            if removed.is_none() && worktree_channel_exists(&workspace, &name) {
                bail!(
                    "channel `{name}` is backed by a worktree; use `rimz worktree remove {name}`"
                );
            }
            let Some(record) = removed else {
                bail!("no named channel `{name}`");
            };
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("removed {}", record.name);
            }
            Ok(())
        }
    }
}

fn list_channels(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    json: bool,
) -> Result<()> {
    let mut entries = BTreeMap::<String, ChannelListEntry>::new();
    for record in rimz::channel::list(&store.paths().channels_record)? {
        entries.insert(
            record.name.clone(),
            ChannelListEntry {
                channel: record.name,
                backing: "named".to_owned(),
                agents: Vec::new(),
            },
        );
    }
    for worktree in worktree_channels(workspace)? {
        entries.entry(worktree.clone()).or_insert(ChannelListEntry {
            channel: worktree,
            backing: "worktree".to_owned(),
            agents: Vec::new(),
        });
    }

    let snapshot = store.snapshot_cached().ok();
    let agents: Vec<&AgentState> = snapshot
        .as_ref()
        .map(rimz::harness::target::addressable_agents)
        .unwrap_or_default();
    let mut live_by_channel: BTreeMap<String, LiveChannelAgents<'_>> = BTreeMap::new();
    for agent in agents {
        if let Some(channel) = rimz::harness::target::agent_channel(agent) {
            let entry = live_by_channel.entry(channel.clone()).or_default();
            entry.explicit_named |= agent.channel.as_deref() == Some(channel.as_str());
            entry.agents.push(agent);
        }
    }
    for (channel, here) in live_by_channel {
        let backing = entries
            .get(&channel)
            .map(|entry| entry.backing.clone())
            .unwrap_or_else(|| live_backing(&channel, here.explicit_named));
        let agents = here
            .agents
            .iter()
            .map(|agent| rimz::harness::target::agent_handle(agent, &here.agents, false))
            .collect();
        entries.insert(
            channel.clone(),
            ChannelListEntry {
                channel,
                backing,
                agents,
            },
        );
    }

    let entries = entries.into_values().collect::<Vec<_>>();
    if json {
        return render::json_pretty(&entries);
    }

    let mut table = render::Table::new(["CHANNEL", "BACKING", "AGENTS"]);
    for entry in entries {
        let agents = if entry.agents.is_empty() {
            "-".to_owned()
        } else {
            entry.agents.join(" ")
        };
        table.row([
            render::cell(format!("#{}", entry.channel)).fg(render::palette::accent()),
            render::cell(entry.backing).dash(),
            render::cell(agents).fg(render::palette::accent()).dash(),
        ]);
    }
    table.render(&mut render::out())?;
    Ok(())
}

#[derive(Serialize)]
struct ChannelListEntry {
    channel: String,
    backing: String,
    agents: Vec<String>,
}

#[derive(Default)]
struct LiveChannelAgents<'a> {
    explicit_named: bool,
    agents: Vec<&'a AgentState>,
}

fn live_backing(_channel: &str, explicit_named: bool) -> String {
    if explicit_named {
        return "named".to_owned();
    }
    "directory".to_owned()
}

fn worktree_channels(workspace: &rimz::ResolvedWorkspace) -> Result<BTreeSet<String>> {
    if workspace.root_class != RootClass::Repo {
        return Ok(BTreeSet::new());
    }
    let entries = rimz::worktree::discover_owned(&workspace.project_root)?;
    Ok(entries
        .into_iter()
        .map(|entry| entry.branch.unwrap_or(entry.marker.name))
        .collect())
}

fn worktree_channel_exists(workspace: &rimz::ResolvedWorkspace, name: &str) -> bool {
    worktree_channels(workspace).is_ok_and(|channels| channels.contains(name))
}

pub(crate) fn ensure_named_channel_available(
    workspace: &rimz::ResolvedWorkspace,
    name: &str,
) -> Result<()> {
    if worktree_channel_exists(workspace, name) {
        bail!("channel `{name}` is backed by a worktree; use `--worktree {name}`");
    }
    Ok(())
}

pub(crate) fn named_channel_registered(store: &rimz::Store, name: &str) -> bool {
    rimz::channel::list(&store.paths().channels_record)
        .is_ok_and(|records| records.iter().any(|record| record.name == name))
}

fn open_channel_tab(workspace: &rimz::ResolvedWorkspace, globals: &GlobalFlags, channel: &str) {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return;
    };
    let machine_config = machine_config();
    let Ok(room) =
        RoomContext::from_resolved(workspace, machine_config, mux, RoomSizing::OrdinaryTab)
    else {
        return;
    };
    let backend = room.backend();
    let Ok(sessions) = backend.list_sessions() else {
        return;
    };
    if !sessions
        .iter()
        .any(|session| session == &workspace.session_name)
    {
        return;
    }
    let sidebar = room.sidebar_options(&workspace.worktree_root, Vec::new(), None);
    let _ = backend.open_tab(&TabOptions {
        title: format!("#{channel}"),
        panes: LayoutPanes {
            columns: vec![LayoutColumn {
                panes: vec![PaneCmd {
                    argv: rimz::harness::launch::channel_shell_argv(
                        &workspace.workspace_id,
                        &workspace.project_root,
                        &workspace.worktree_root,
                        channel,
                    ),
                }],
                stacked: false,
            }],
        },
        focus: true,
        dock_sidebar: true,
        sidebar,
    });
}
