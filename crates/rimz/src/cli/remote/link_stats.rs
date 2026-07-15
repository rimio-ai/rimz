use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::remote::link::{LinkAck, LinkProbe, LinkStatsFile};
use rimz::room::session::workspace_record_for_session;

const LINK_SCHEMA_MISMATCH_EXIT: i32 = 2;

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
        serde_json::to_writer(&mut stdout, &LinkAck::new(probe.seq)).context("writing link ack")?;
        writeln!(stdout).context("writing link ack newline")?;
        stdout.flush().context("flushing link ack")?;
    }
    Ok(())
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
