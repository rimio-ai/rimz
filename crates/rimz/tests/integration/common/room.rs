//! Durable writes normally made by the shim room's renderer and child hooks.

use rimz::agents::LaunchParams;
use rimz::ids::{MuxName, PaneId};
use std::time::Duration;

/// The trace shim lists the room as live but runs no presence plugin, so a
/// launch that finds no fresh topology or sidebar heartbeat spends the whole
/// Zellij health-probe budget inspecting it. Seed both, as a live room has.
pub fn seed_live_zellij_room(
    runtime: &rimz::RuntimePaths,
    session_name: &str,
    panes: Vec<rimz::mux::zellij::pane_topology::PaneTopologyPane>,
) {
    runtime.ensure_dirs().expect("runtime dirs");
    rimz::mux::zellij::pane_topology::write_pane_topology_cache(
        runtime,
        &rimz::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: session_name.to_owned(),
            produced_at_ms: rimz::utils::time::unix_now_ms(),
            writer: None,
            focused_pane: None,
            clients: None,
            panes,
        },
    )
    .expect("write pane topology");
    let heartbeat = rimz::wakeup::heartbeat::SidebarHeartbeat::new(
        runtime.workspace_id.clone(),
        rimz::ids::SidebarInstanceId::new(),
        MuxName::Zellij,
        session_name,
        runtime.sock_dir.join("sidebar.sock"),
        None,
    );
    std::fs::write(
        runtime.heartbeat_dir.join("sidebar.seeded.json"),
        serde_json::to_vec(&heartbeat).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
}

/// Publish the renderer's first heartbeat after birth (not before the rebirth
/// purge), and answer topology requests as the presence plugin would.
pub struct ShimRoom {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl ShimRoom {
    pub fn watch(env: &super::Env, trace: &std::path::Path, panes: &str) -> Self {
        let runtime = env.runtime_paths();
        let workspace =
            rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve shim room");
        let mut topology = rimz::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: workspace.session_name.clone(),
            produced_at_ms: 0,
            writer: None,
            focused_pane: None,
            clients: None,
            panes: serde_json::from_str(panes).expect("shim room panes"),
        };
        let trace = trace.to_owned();
        let mut consumed = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .count();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = stopped.clone();
        let writer = std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let log = std::fs::read_to_string(&trace).unwrap_or_default();
                let log = &log[..log.rfind('\n').map_or(0, |end| end + 1)];
                for line in log.lines().skip(consumed) {
                    if line.contains("\tattach\t--create-background\t") {
                        seed_live_zellij_room(
                            &runtime,
                            &workspace.session_name,
                            topology.panes.clone(),
                        );
                        continue;
                    } else if !line.contains("\trimz:dump_topology\t") {
                        continue;
                    }
                    topology.produced_at_ms = rimz::utils::time::unix_now_ms();
                    rimz::mux::zellij::pane_topology::write_pane_topology_cache(
                        &runtime, &topology,
                    )
                    .expect("publish shim room topology");
                }
                consumed = log.lines().count();
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stopped,
            writer: Some(writer),
        }
    }
}

impl Drop for ShimRoom {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(writer) = self.writer.take() {
            let result = writer.join();
            if !std::thread::panicking() {
                result.expect("shim room writer");
            }
        }
    }
}

/// The trace shim opens child panes that run nothing, so no child hook binds a
/// launch and every child would sit out the whole subagent pane-bind wait.
/// Stand in for each child's session-start hook: bind the launch once the trace
/// shows its pane opening, the order a live pane produces.
pub fn bind_child_panes(
    store: &rimz::Store,
    trace_path: &std::path::Path,
    session_name: &str,
    launched: &std::sync::atomic::AtomicBool,
) {
    let mut bound = Vec::new();
    while !launched.load(std::sync::atomic::Ordering::Relaxed) {
        let trace = std::fs::read_to_string(trace_path).unwrap_or_default();
        let opened = trace
            .lines()
            .filter(|line| line.contains("\tnew-pane\t"))
            .filter_map(|line| {
                let args = line.split('\t').collect::<Vec<_>>();
                let name = args.windows(2).find(|args| args[0] == "--name")?[1];
                Some(name.to_owned())
            })
            .filter(|name| !bound.contains(name))
            .collect::<Vec<_>>();
        for name in opened {
            let records = rimz::harness::run::list(store.paths()).expect("list child runs");
            let Some(record) = records
                .iter()
                .find(|record| record.agent_name.as_deref() == Some(name.as_str()))
            else {
                continue;
            };
            let agents = store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .expect("child launch history")
                .agents;
            let child = agents
                .iter()
                .find(|agent| agent.name.as_deref() == Some(name.as_str()))
                .expect("opened pane has a launch");
            store
                .bind_agent_launch(
                    &rimz::store::writer::AgentLaunchIdentity {
                        kind: child.kind.clone(),
                        agent_id: child.agent_id.clone(),
                        name: name.clone(),
                        name_explicit: false,
                        launch: LaunchParams::default(),
                        run_id: Some(record.run_id.clone()),
                        prompt: None,
                    },
                    session_name,
                    &record.worktree_path,
                    &PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", bound.len() + 3)),
                )
                .expect("bind child pane");
            bound.push(name);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
