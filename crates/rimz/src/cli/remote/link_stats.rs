use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::ids::WorkspaceId;
use rimz::remote::link::{LinkAck, LinkProbe, LinkStatsFile};
use rimz::room::session::workspace_record_for_session;

const LINK_SCHEMA_MISMATCH_EXIT: i32 = 2;
const PORTS_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const PORTS_SWEEP_ENV: &str = "RIMZ_PORTS_SWEEP_MS";
const PROC_ROOT_ENV: &str = "RIMZ_PROC_ROOT";

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(super) struct LinkStatsIngestArgs {
    /// Existing room session name.
    #[arg(long)]
    session: Option<String>,
    /// Room directory, resolved like `rimz start <dir>`.
    #[arg(long)]
    dir: Option<PathBuf>,
}

pub(super) fn ingest(args: LinkStatsIngestArgs) -> Result<()> {
    let (runtime, workspace_id) = link_stats_runtime(args)?;
    let client = link_client_id();
    runtime
        .ensure_dirs()
        .context("preparing runtime directories for link stats")?;
    let path = rimz::remote::link::stats_path(&runtime);
    let result = ingest_probes(&path, &client, workspace_id);
    remove_stats_if_owned(&path, &client);
    result
}

fn ingest_probes(path: &Path, client: &str, workspace_id: WorkspaceId) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut ports = PortReporter::new(workspace_id);
    for line in stdin.lock().lines() {
        let line = line.context("reading link probe")?;
        if line.trim().is_empty() {
            continue;
        }
        let probe: LinkProbe = serde_json::from_str(&line).context("parsing link probe")?;
        if !probe.version_ok() {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "unsupported link probe schema `{}`", probe.v);
            remove_stats_if_owned(path, client);
            std::process::exit(LINK_SCHEMA_MISMATCH_EXIT);
        }
        let mut file = LinkStatsFile::new(
            rimz::utils::time::unix_now_ms(),
            client.to_owned(),
            probe.stats.clone(),
        );
        file.forwards = probe.forwards;
        rimz::disk::atomic::write_temp_then_rename_cache(path, &file)
            .with_context(|| format!("writing {}", path.display()))?;
        let ack = match ports.report() {
            Some(ports) => LinkAck::with_ports(probe.seq, ports),
            None => LinkAck::new(probe.seq),
        };
        serde_json::to_writer(&mut stdout, &ack).context("writing link ack")?;
        writeln!(stdout).context("writing link ack newline")?;
        stdout.flush().context("flushing link ack")?;
    }
    Ok(())
}

struct PortReporter {
    interval: Duration,
    last_sweep: Option<Instant>,
    ports: Option<Vec<u16>>,
    proc_root: PathBuf,
    workspace_id: WorkspaceId,
    // Positive and negative verdicts live only while their candidate inode stays listed.
    verdicts: BTreeMap<u64, bool>,
}

impl PortReporter {
    fn new(workspace_id: WorkspaceId) -> Self {
        let interval = std::env::var(PORTS_SWEEP_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(PORTS_SWEEP_INTERVAL);
        Self {
            interval,
            last_sweep: None,
            ports: None,
            proc_root: std::env::var_os(PROC_ROOT_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/proc")),
            workspace_id,
            verdicts: BTreeMap::new(),
        }
    }

    fn report(&mut self) -> Option<Vec<u16>> {
        if self
            .last_sweep
            .is_none_or(|last| last.elapsed() >= self.interval)
        {
            self.ports =
                read_candidate_ports(&self.proc_root, &self.workspace_id, &mut self.verdicts);
            self.last_sweep = Some(Instant::now());
        }
        self.ports.clone()
    }
}

#[cfg(target_os = "linux")]
fn read_candidate_ports(
    root: &Path,
    workspace_id: &WorkspaceId,
    verdicts: &mut BTreeMap<u64, bool>,
) -> Option<Vec<u16>> {
    let tcp = std::fs::read_to_string(root.join("net/tcp")).ok()?;
    let tcp6 = std::fs::read_to_string(root.join("net/tcp6")).ok()?;
    let listeners =
        rimz::remote::forward::candidate_listeners(&tcp, &tcp6, nix::unistd::getuid().as_raw());
    let inodes = listeners
        .iter()
        .map(|listener| listener.inode)
        .collect::<BTreeSet<_>>();
    verdicts.retain(|inode, _| inodes.contains(inode));
    let unknown = inodes
        .iter()
        .filter(|inode| !verdicts.contains_key(*inode))
        .copied()
        .collect();
    let room_inodes = rimz::proc::socket_inodes_for_env(
        root,
        nix::unistd::getuid().as_raw(),
        rimz::workspace::ENV_WORKSPACE_ID,
        workspace_id.as_str(),
        &unknown,
    );
    for inode in unknown {
        verdicts.insert(inode, room_inodes.contains(&inode));
    }
    let room_inodes = verdicts
        .iter()
        .filter_map(|(inode, in_room)| in_room.then_some(*inode))
        .collect();
    Some(rimz::remote::forward::candidate_ports(
        &listeners,
        &room_inodes,
    ))
}

#[cfg(not(target_os = "linux"))]
fn read_candidate_ports(
    _root: &Path,
    _workspace_id: &WorkspaceId,
    _verdicts: &mut BTreeMap<u64, bool>,
) -> Option<Vec<u16>> {
    None
}

fn remove_stats_if_owned(path: &Path, client: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(file) = serde_json::from_slice::<LinkStatsFile>(&bytes) else {
        return;
    };
    if file.client == client {
        let _ = std::fs::remove_file(path);
    }
}

fn link_stats_runtime(args: LinkStatsIngestArgs) -> Result<(rimz::RuntimePaths, WorkspaceId)> {
    let (project_root, workspace_id) = match (args.session, args.dir) {
        (Some(session), None) => {
            let room = workspace_record_for_session(&session)?
                .with_context(|| format!("no RimZ workspace record for session `{session}`"))?;
            (room.project_root, room.workspace_id)
        }
        (None, Some(dir)) => {
            let room = rimz::WorkspaceResolver::resolve(&dir, None)
                .with_context(|| format!("resolving remote room dir {}", dir.display()))?;
            (room.project_root, room.workspace_id)
        }
        _ => bail!("give exactly one of --session or --dir"),
    };
    let state = rimz::StatePaths::for_project_root(&project_root)?;
    let store = crate::cli::open_existing_store_at(state)?
        .with_context(|| crate::cli::no_room(format_args!("at {}", project_root.display())))?;
    Ok((store.runtime_paths().clone(), workspace_id))
}

fn link_client_id() -> String {
    std::env::var("SSH_CONNECTION").unwrap_or_else(|_| "ssh".to_owned())
}
