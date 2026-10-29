use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{GlobalFlags, ctx::Ctx, render};
use rimz::disk::{atomic::write_temp_then_rename_cache, lock::WorkspaceLock, paths};
use rimz::remote::forward::{ForwardEntry, ForwardRecord, ForwardState, PortSync};
use rimz::remote::link::{LINK_STATS_EXPIRE, LinkStatsFile, stats_path};
use rimz::utils::time::unix_now_ms;

pub(super) struct ForwardPublisher {
    sync: PortSync,
    record: ForwardRecord,
    file: Option<ForwardFile>,
    publication_warned: bool,
}

impl ForwardPublisher {
    pub(super) fn new(target: &str, host: &str, auto_forward: bool) -> Self {
        let now_ms = unix_now_ms();
        let record = ForwardRecord {
            target: target.to_owned(),
            host: host.to_owned(),
            pid: std::process::id(),
            started_at_ms: now_ms,
            at_ms: now_ms,
            auto_forward,
            forwards: Vec::new(),
        };
        let file = match ForwardFile::new(&record) {
            Ok(file) => Some(file),
            Err(error) => {
                tracing::warn!(%error, "publishing remote forward record failed");
                None
            }
        };
        let publication_warned = file.is_none();
        Self {
            sync: PortSync::default(),
            record,
            file,
            publication_warned,
        }
    }

    pub(super) fn retry_publish(&mut self) {
        if self.file.is_none() {
            self.record.at_ms = unix_now_ms();
            self.file = ForwardFile::new(&self.record).ok();
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.record.auto_forward
    }

    pub(super) fn local_port(&self, remote: u16) -> Option<u16> {
        self.sync.local_port(remote)
    }

    pub(super) fn reopen_active(&self) -> Vec<rimz::remote::forward::PortAction> {
        self.sync.reopen_active()
    }

    pub(super) fn update<T>(&mut self, change: impl FnOnce(&mut PortSync, u64) -> T) -> T {
        let result = change(&mut self.sync, unix_now_ms());
        let forwards = self.sync.snapshot();
        if forwards != self.record.forwards {
            self.record.forwards = forwards;
            self.record.at_ms = unix_now_ms();
            if let Some(file) = &self.file
                && let Err(error) = write_temp_then_rename_cache(&file.path, &self.record)
            {
                if !self.publication_warned {
                    tracing::warn!(%error, "publishing remote forward record failed");
                    self.publication_warned = true;
                }
                self.file = None;
            }
        }
        result
    }

    pub(super) fn record(&self, now_ms: u64) -> ForwardRecord {
        ForwardRecord {
            at_ms: now_ms,
            ..self.record.clone()
        }
    }
}

struct ForwardFile {
    path: PathBuf,
    _lock: WorkspaceLock,
}

impl ForwardFile {
    fn new(record: &ForwardRecord) -> Result<Self> {
        let dir = paths::forward_records_dir();
        for parent in dir
            .ancestors()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            paths::ensure_private_runtime_dir(parent)?;
        }
        let path = dir.join(format!("{}.json", record.pid));
        let lock = WorkspaceLock::try_acquire(&path.with_extension("lock"))?
            .context("this supervisor's forward record is already locked")?;
        let file = Self { path, _lock: lock };
        write_temp_then_rename_cache(&file.path, record)?;
        Ok(file)
    }
}

impl Drop for ForwardFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("lock"));
    }
}

fn local_records() -> Result<Vec<ForwardRecord>> {
    let dir = paths::forward_records_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error).context("reading local forward records"),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json")
            || path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u32>().ok())
                .is_none()
        {
            continue;
        }
        let lock_path = path.with_extension("lock");
        if let Some(_lock) = WorkspaceLock::try_acquire(&lock_path)? {
            for dead in [&path, &lock_path] {
                match std::fs::remove_file(dead) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("removing {}", dead.display()));
                    }
                }
            }
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        records.push(
            serde_json::from_slice::<ForwardRecord>(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
        );
    }
    records.sort_by_key(|record| (record.started_at_ms, record.pid));
    Ok(records)
}

fn connection_stats(globals: &GlobalFlags) -> Option<LinkStatsFile> {
    let ctx = Ctx::open(globals).ok()?;
    let bytes = std::fs::read(stats_path(ctx.store.runtime_paths())).ok()?;
    let file: LinkStatsFile = serde_json::from_slice(&bytes).ok()?;
    (file.version_ok()
        && unix_now_ms().saturating_sub(file.received_at_ms)
            <= LINK_STATS_EXPIRE.as_millis() as u64)
        .then_some(file)
}

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let connection = connection_stats(globals);
    let records = match &connection {
        Some(file) => file.forwards.iter().cloned().collect(),
        None => local_records()?,
    };
    let received_at_ms = connection.as_ref().map(|file| file.received_at_ms);
    let now_ms = unix_now_ms();
    if json {
        let records = records
            .iter()
            .map(|record| record_json(record, received_at_ms, now_ms))
            .collect::<Vec<_>>();
        return render::json_pretty(&json!({
            "scope": if connection.is_some() { "connection" } else { "machine" },
            "connections": records,
        }));
    }
    write_human(
        &records,
        connection.is_some(),
        received_at_ms,
        now_ms,
        &mut render::out(),
    )?;
    Ok(())
}

fn age_secs(
    record: &ForwardRecord,
    entry: &ForwardEntry,
    received_at_ms: Option<u64>,
    now_ms: u64,
) -> u64 {
    let age_ms = match received_at_ms {
        Some(received) => record
            .at_ms
            .saturating_sub(entry.since_ms)
            .saturating_add(now_ms.saturating_sub(received)),
        None => now_ms.saturating_sub(entry.since_ms),
    };
    age_ms / 1_000
}

#[derive(Serialize)]
struct ForwardAge<'a> {
    remote: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<u16>,
    #[serde(flatten)]
    state: &'a ForwardState,
    age_secs: u64,
}

fn record_json(record: &ForwardRecord, received_at_ms: Option<u64>, now_ms: u64) -> Value {
    let forwards = record
        .forwards
        .iter()
        .map(|entry| ForwardAge {
            remote: entry.remote,
            local: entry.local,
            state: &entry.state,
            age_secs: age_secs(record, entry, received_at_ms, now_ms),
        })
        .collect::<Vec<_>>();
    let mut value = json!(record);
    value["forwards"] = json!(forwards);
    value
}

fn write_human(
    records: &[ForwardRecord],
    connection: bool,
    received_at_ms: Option<u64>,
    now_ms: u64,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if !connection {
        let count = records.len();
        let plural = if count == 1 { "" } else { "s" };
        writeln!(out, "{count} connection{plural} on this machine")?;
    }
    if records.is_empty() {
        if connection {
            writeln!(out, "connection to this room")?;
            return writeln!(out, "no forward data from this connection");
        }
        return writeln!(out, "no rimz remote connect is running on this machine");
    }
    for record in records {
        if connection {
            writeln!(out, "connection to {}", record.host)?;
        } else {
            writeln!(
                out,
                "\nconnection to {} ({}; pid {})",
                record.host, record.target, record.pid
            )?;
        }
        if !record.auto_forward {
            writeln!(out, "auto-forward is off for this connection")?;
            continue;
        }
        if record.forwards.is_empty() {
            writeln!(out, "no listeners forwarded yet")?;
            continue;
        }
        let mut table = render::Table::new(["REMOTE", "LOCAL", "STATE", "SINCE", "REASON"]);
        for entry in &record.forwards {
            let (state, reason) = match &entry.state {
                ForwardState::Forwarded
                    if entry.local.is_some_and(|local| local != entry.remote) =>
                {
                    ("remapped", format!("{} is busy here", entry.remote))
                }
                ForwardState::Forwarded => ("forwarded", String::new()),
                ForwardState::Reopening => ("reopening", "link reconnecting".into()),
                ForwardState::Limit => ("not forwarded", "over the 16-forward limit".into()),
                ForwardState::Refused { error } => {
                    ("not forwarded", format!("ssh refused: {error}"))
                }
                ForwardState::NoLocalPort => ("not forwarded", "no free local port".into()),
                ForwardState::Unknown => ("unknown", "state not understood by this RimZ".into()),
            };
            table.row([
                render::cell(entry.remote.to_string()),
                render::cell(
                    entry
                        .local
                        .map_or_else(|| "-".into(), |port| port.to_string()),
                ),
                render::cell(state).fg(render::status::forward(&entry.state)),
                render::cell(rimz::theme::fmt::age_label(age_secs(
                    record,
                    entry,
                    received_at_ms,
                    now_ms,
                )))
                .fg(render::palette::muted()),
                render::cell(reason).fg(render::palette::muted()),
            ]);
        }
        table.render(out)?;
    }
    Ok(())
}
