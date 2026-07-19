use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::remote::link::{LinkAck, LinkProbe, LinkStatsFile};
use rimz::room::session::workspace_record_for_session;

const LINK_SCHEMA_MISMATCH_EXIT: i32 = 2;
const PORTS_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const PORTS_SWEEP_ENV: &str = "RIMZ_PORTS_SWEEP_MS";
const PROC_NET_DIR_ENV: &str = "RIMZ_PROC_NET_DIR";

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
    let (runtime, client) = link_stats_runtime(args)?;
    runtime
        .ensure_dirs()
        .context("preparing runtime directories for link stats")?;
    let path = rimz::remote::link::stats_path(&runtime);
    let result = ingest_probes(&path, &client);
    remove_stats_if_owned(&path, &client);
    result
}

fn ingest_probes(path: &Path, client: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut ports = PortReporter::new();
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
        let file = LinkStatsFile::new(
            rimz::sidebar::timing::unix_now_ms(),
            client.to_owned(),
            probe.stats.clone(),
        );
        rimz::store::atomic::write_temp_then_rename_cache(path, &file)
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
}

impl PortReporter {
    fn new() -> Self {
        let interval = std::env::var(PORTS_SWEEP_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(PORTS_SWEEP_INTERVAL);
        Self {
            interval,
            last_sweep: None,
            ports: None,
        }
    }

    fn report(&mut self) -> Option<Vec<u16>> {
        if self
            .last_sweep
            .is_none_or(|last| last.elapsed() >= self.interval)
        {
            self.ports = read_candidate_ports();
            self.last_sweep = Some(Instant::now());
        }
        self.ports.clone()
    }
}

fn read_candidate_ports() -> Option<Vec<u16>> {
    let root = std::env::var_os(PROC_NET_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/proc/net"));
    let tcp = std::fs::read_to_string(root.join("tcp")).ok()?;
    let tcp6 = std::fs::read_to_string(root.join("tcp6")).ok()?;
    Some(rimz::remote::forward::candidate_ports(
        &tcp,
        &tcp6,
        nix::unistd::getuid().as_raw(),
    ))
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

fn link_stats_runtime(args: LinkStatsIngestArgs) -> Result<(rimz::RuntimePaths, String)> {
    let workspace_id = match (args.session, args.dir) {
        (Some(session), None) => {
            workspace_record_for_session(&session)?
                .with_context(|| format!("no RimZ workspace record for session `{session}`"))?
                .workspace_id
        }
        (None, Some(dir)) => {
            rimz::WorkspaceResolver::resolve(&dir, None)
                .with_context(|| format!("resolving remote room dir {}", dir.display()))?
                .workspace_id
        }
        _ => bail!("give exactly one of --session or --dir"),
    };
    let runtime = rimz::RuntimePaths::for_workspace(workspace_id)?;
    Ok((runtime, link_client_id()))
}

fn link_client_id() -> String {
    std::env::var("SSH_CONNECTION").unwrap_or_else(|_| "ssh".to_owned())
}
