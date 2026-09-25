//! Memory accounting and serialized launch admission.

use super::{LspErr, Result, history, memory, registry};
use crate::config::LspPolicy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Admit,
    RefusedOptional,
    WaitForRequired,
}

pub fn decide(
    available: u64,
    committed: u64,
    estimate: u64,
    reserve: u64,
    policy: LspPolicy,
) -> Decision {
    if committed
        .checked_add(estimate)
        .and_then(|used| used.checked_add(reserve))
        .is_some_and(|needed| needed <= available)
    {
        return Decision::Admit;
    }
    match policy {
        LspPolicy::Optional => Decision::RefusedOptional,
        LspPolicy::Required => Decision::WaitForRequired,
    }
}

pub fn reserve_bytes(total: u64, percent: u8, minimum: u64) -> u64 {
    ((u128::from(total) * u128::from(percent) / 100).min(u128::from(u64::MAX)) as u64).max(minimum)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServeRequest {
    pub root: PathBuf,
    pub server: String,
    pub config: crate::config::LspServerConfig,
    pub policy: crate::config::LspConfig,
    pub estimate_bytes: u64,
    pub settings_hash: String,
}

pub struct AdmissionRequest<'a> {
    pub root: &'a Path,
    pub servers: &'a BTreeMap<String, crate::config::LspServerConfig>,
    pub untrusted_servers: &'a [String],
    pub policy: &'a crate::config::LspConfig,
    pub runtime: &'a crate::disk::paths::RuntimePaths,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shortfall {
    pub root: PathBuf,
    pub server: String,
    pub estimate_bytes: u64,
    pub available_bytes: u64,
    pub committed_bytes: u64,
    pub reserve_bytes: u64,
    pub holders: Vec<Holder>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Holder {
    pub root: PathBuf,
    pub server: String,
    pub rss_bytes: u64,
}

pub struct Wait {
    pub shortfall: Shortfall,
    pub position: usize,
    pub remaining: Duration,
}

#[derive(Default)]
pub struct Admitted {
    pub admitted: Vec<registry::Entry>,
    pub refused_optional: Vec<Shortfall>,
    pub wait_for_required: Vec<Wait>,
}

/// Keep this guard across five-second polls; dropping a launch removes its queue file.
#[derive(Default)]
pub struct WaitQueue {
    ticket: Option<Ticket>,
    refused_optional: BTreeSet<String>,
}

struct Ticket {
    path: PathBuf,
    started: Instant,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Serialize, Deserialize)]
struct QueueRecord {
    need_bytes: u64,
    pid: u32,
    start_token: String,
}

fn queue_paths() -> Result<Vec<PathBuf>> {
    let directory = crate::disk::paths::lsp_runtime_dir().join("queue");
    queue_paths_at(&directory)
}

fn queue_paths_at(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some((time, nonce)) = name.to_str().and_then(|name| name.split_once('-')) else {
            continue;
        };
        if time.len() != 20
            || time.parse::<u64>().is_err()
            || uuid::Uuid::parse_str(nonce).is_err()
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let path = entry.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let record: QueueRecord = serde_json::from_slice(&bytes)?;
        if crate::proc::process_is_live(record.pid, Some(&record.start_token)) {
            paths.push(path);
        } else {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    paths.sort();
    Ok(paths)
}

#[derive(Debug, PartialEq, Eq)]
enum RequiredDecision {
    Admit,
    Wait,
    Timeout,
}

fn required_decision(
    memory: Decision,
    position: usize,
    elapsed: Duration,
    timeout: Duration,
) -> RequiredDecision {
    if elapsed >= timeout {
        return RequiredDecision::Timeout;
    }
    if memory == Decision::Admit && position == 1 {
        RequiredDecision::Admit
    } else {
        RequiredDecision::Wait
    }
}

fn queue_timeout(wait_timeout: &str, shortfall: &Shortfall) -> LspErr {
    LspErr::QueueTimeout(Box::new(QueueTimeout {
        wait_timeout: wait_timeout.to_owned(),
        shortfall: shortfall.clone(),
    }))
}

#[derive(Debug)]
pub struct QueueTimeout {
    pub wait_timeout: String,
    pub shortfall: Shortfall,
}

impl std::error::Error for QueueTimeout {}

impl std::fmt::Display for QueueTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shortfall = &self.shortfall;
        let holders = shortfall
            .holders
            .iter()
            .map(|holder| {
                format!(
                    "{} {} ({})",
                    holder.root.display(),
                    holder.server,
                    decimal_bytes(holder.rss_bytes)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let held_by = if holders.is_empty() {
            String::new()
        } else {
            format!(", held by {holders}")
        };
        let free = shortfall
            .available_bytes
            .saturating_sub(shortfall.committed_bytes)
            .saturating_sub(shortfall.reserve_bytes);
        write!(
            formatter,
            "language server {} is required but memory stayed short for {}: needs {}, {} free{held_by}; stop one with rimz lsp stop, or lower [lsp] reserve-percent",
            shortfall.server,
            self.wait_timeout,
            decimal_bytes(shortfall.estimate_bytes),
            decimal_bytes(free)
        )
    }
}

fn decimal_bytes(bytes: u64) -> String {
    for (factor, unit) in [(1_000_000_000_u64, "GB"), (1_000_000, "MB"), (1_000, "KB")] {
        if bytes >= factor {
            let amount = format!("{:.1}", bytes as f64 / factor as f64);
            return format!("{} {unit}", amount.strip_suffix(".0").unwrap_or(&amount));
        }
    }
    format!("{bytes} B")
}

#[cfg(feature = "testkit")]
pub mod testkit {
    use super::*;

    pub fn queue_order(directory: &Path) -> Result<Vec<PathBuf>> {
        queue_paths_at(directory)
    }

    pub fn enqueue_servers(directory: &Path, estimates: &[u64]) -> Result<WaitQueue> {
        let mut queue = WaitQueue::default();
        let need_bytes = estimates.iter().copied().sum();
        for _ in estimates {
            enqueue_at(directory, need_bytes, &mut queue)?;
        }
        Ok(queue)
    }
}

fn enqueue(need_bytes: u64, queue: &mut WaitQueue) -> Result<&Ticket> {
    enqueue_at(
        &crate::disk::paths::lsp_runtime_dir().join("queue"),
        need_bytes,
        queue,
    )
}

fn enqueue_at<'a>(
    directory: &Path,
    need_bytes: u64,
    queue: &'a mut WaitQueue,
) -> Result<&'a Ticket> {
    let pid = std::process::id();
    let start_token = crate::proc::process_start_token(pid)
        .ok_or_else(|| LspErr::Protocol("cannot identify queue owner".into()))?;
    let ticket = queue.ticket.get_or_insert_with(|| Ticket {
        path: directory.join(format!(
            "{:020}-{}",
            crate::utils::time::unix_now_ms(),
            uuid::Uuid::now_v7()
        )),
        started: Instant::now(),
    });
    crate::disk::atomic::write_temp_then_rename_cache(
        &ticket.path,
        &QueueRecord {
            need_bytes,
            pid,
            start_token,
        },
    )?;
    Ok(ticket)
}

fn timeout(config: &crate::config::LspServerConfig) -> Result<Duration> {
    use crate::utils::time::{DurationUnit, parse_duration_units};
    parse_duration_units(
        &config.wait_timeout,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
        ],
    )
    .map_err(|error| LspErr::Configuration(error.to_string()))
}

fn record_shortfall(event: &str, shortfall: &Shortfall) -> Result<()> {
    crate::diag::lsp::append(&crate::diag::lsp::Record {
        at: jiff::Timestamp::now(),
        root: shortfall.root.clone(),
        server: shortfall.server.clone(),
        event: event.to_owned(),
        details: serde_json::to_value(shortfall)?,
    });
    Ok(())
}

pub fn committed_bytes(entries: &[registry::Entry]) -> (u64, Vec<Holder>) {
    let mut committed = 0_u64;
    let mut holders = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| !matches!(entry.state, registry::State::Stopped { .. }))
    {
        let rss_bytes = entry
            .server_pid
            .and_then(crate::proc::tree_totals)
            .map_or(0, |totals| totals.rss_kb.saturating_mul(1024));
        committed = committed.saturating_add(committed_growth(entry.estimate_bytes, rss_bytes));
        holders.push(Holder {
            root: entry.root.clone(),
            server: entry.server.clone(),
            rss_bytes,
        });
    }
    (committed, holders)
}

fn committed_growth(estimate: u64, rss: u64) -> u64 {
    estimate.saturating_sub(rss)
}

/// One bounded admission pass. The CLI prints outcomes and polls required waits every five seconds.
pub fn admit_launch(request: &AdmissionRequest<'_>, queue: &mut WaitQueue) -> Result<Admitted> {
    if let Some(server) = request.untrusted_servers.first() {
        return Err(LspErr::Configuration(format!(
            "project config declares language server {server} but the project is not trusted; run rimz trust"
        )));
    }
    if request.policy.reserve_percent > 100 || request.policy.kill_floor_percent > 100 {
        return Err(LspErr::Configuration(
            "lsp percentages must be between 0 and 100".into(),
        ));
    }
    let root = std::fs::canonicalize(request.root)?;
    let parse_size =
        |raw: &str| crate::utils::size::parse_byte_size(raw).map_err(LspErr::Configuration);
    let minimum = parse_size(&request.policy.reserve_min)?;
    let mut matched = Vec::new();
    for (server, config) in request.servers {
        registry::directory(&root, server)?;
        if config.root_markers.is_empty()
            || config.extensions.is_empty()
            || config.command.first().is_none_or(String::is_empty)
        {
            return Err(LspErr::Configuration(format!(
                "language server {server} needs command, extensions, and root-markers"
            )));
        }
        for marker in &config.root_markers {
            if Path::new(marker)
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(LspErr::Configuration(format!(
                    "language server {server}: root-markers must be relative to the checkout"
                )));
            }
        }
        if !config
            .root_markers
            .iter()
            .any(|marker| root.join(marker).exists())
        {
            continue;
        }
        if config
            .init_options
            .as_ref()
            .is_some_and(|options| !options.is_object())
        {
            return Err(LspErr::Configuration(format!(
                "language server {server}: init-options must be a table"
            )));
        }
        if which::which_in(&config.command[0], std::env::var_os("PATH"), &root).is_err() {
            return Err(LspErr::Configuration(format!(
                "language server {server}: {} not found on PATH; install it or remove [lsp.servers.{server}]",
                config.command[0]
            )));
        }
        let hash = history::settings_hash(config);
        let estimate =
            history::estimate(&root, server, &hash, parse_size(&config.memory_estimate)?);
        matched.push((server, config, hash, estimate, timeout(config)?));
    }
    if matched.is_empty() {
        queue.ticket = None;
        return Ok(Admitted::default());
    }
    let _lock = registry::lock()?;
    let mut entries = registry::sweep_locked()?;
    let required_need = matched
        .iter()
        .filter(|(server, config, ..)| {
            config.policy == LspPolicy::Required
                && !entries
                    .iter()
                    .any(|entry| entry.root == root && entry.server == **server)
        })
        .fold(0_u64, |need, (_, _, _, estimate, _)| {
            need.saturating_add(*estimate)
        });
    let mut result = Admitted::default();
    for (server, config, settings_hash, estimate_bytes, wait_timeout) in matched {
        if queue.refused_optional.contains(server) {
            continue;
        }
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.root == root && entry.server == *server)
        {
            result.admitted.push(entry.clone());
            continue;
        }
        let memory = memory::sample()?;
        let (committed_bytes, holders) = committed_bytes(&entries);
        let reserve_bytes =
            reserve_bytes(memory.total_bytes, request.policy.reserve_percent, minimum);
        let shortfall = Shortfall {
            root: root.clone(),
            server: server.clone(),
            estimate_bytes,
            available_bytes: memory.available_bytes,
            committed_bytes,
            reserve_bytes,
            holders,
        };
        let decision = decide(
            memory.available_bytes,
            committed_bytes,
            estimate_bytes,
            reserve_bytes,
            config.policy,
        );
        if config.policy == LspPolicy::Required
            && (decision != Decision::Admit || !queue_paths()?.is_empty())
        {
            let ticket = enqueue(required_need, queue)?;
            let position = queue_paths()?
                .iter()
                .position(|path| path == &ticket.path)
                .map_or(1, |position| position + 1);
            let required =
                required_decision(decision, position, ticket.started.elapsed(), wait_timeout);
            if required == RequiredDecision::Timeout {
                record_shortfall("queue_timeout", &shortfall)?;
                return Err(queue_timeout(&config.wait_timeout, &shortfall));
            }
            if required == RequiredDecision::Wait {
                result.wait_for_required.push(Wait {
                    shortfall,
                    position,
                    remaining: wait_timeout.saturating_sub(ticket.started.elapsed()),
                });
                continue;
            }
        }
        if decision == Decision::RefusedOptional {
            record_shortfall("refused", &shortfall)?;
            queue.refused_optional.insert(server.clone());
            result.refused_optional.push(shortfall);
            continue;
        }
        let serve = ServeRequest {
            root: root.clone(),
            server: server.clone(),
            config: config.clone(),
            policy: request.policy.clone(),
            estimate_bytes,
            settings_hash,
        };
        let payload = serde_json::to_string(&serve)?;
        crate::disk::paths::ensure_private_runtime_dir(&request.runtime.shared_root)?;
        crate::child_process::spawn_detached_rimz(
            request.runtime,
            ["lsp", "serve", "--request", &payload],
            "lsp-broker",
        )?;
        let started = Instant::now();
        let path = registry::directory(&root, server)?.join("entry.json");
        let entry = loop {
            match std::fs::read(&path) {
                Ok(bytes) => break serde_json::from_slice::<registry::Entry>(&bytes)?,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && started.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(error) => return Err(error.into()),
            }
        };
        entries.push(entry.clone());
        result.admitted.push(entry);
    }
    if result.wait_for_required.is_empty() {
        queue.ticket = None;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_queue_admits_only_the_oldest_when_memory_frees_and_times_out_with_holders() {
        let limit = Duration::from_secs(20);
        for position in [1, 2] {
            assert_eq!(
                required_decision(Decision::WaitForRequired, position, Duration::ZERO, limit),
                RequiredDecision::Wait
            );
        }
        assert_eq!(
            required_decision(Decision::Admit, 1, Duration::from_secs(5), limit),
            RequiredDecision::Admit
        );
        assert_eq!(
            required_decision(Decision::Admit, 2, Duration::from_secs(5), limit),
            RequiredDecision::Wait
        );
        assert_eq!(
            required_decision(Decision::Admit, 1, limit, limit),
            RequiredDecision::Timeout
        );
        let shortfall = Shortfall {
            root: "/new".into(),
            server: "rust".into(),
            estimate_bytes: 8000,
            available_bytes: 5000,
            committed_bytes: 2000,
            reserve_bytes: 1000,
            holders: vec![Holder {
                root: "/held".into(),
                server: "rust".into(),
                rss_bytes: 6000,
            }],
        };
        assert!(
            queue_timeout("20s", &shortfall)
                .to_string()
                .contains("held by /held rust (6 KB)")
        );
        let record = serde_json::to_value(&shortfall).unwrap();
        assert_eq!(record["available_bytes"], 5000);
        assert_eq!(record["reserve_bytes"], 1000);
    }

    #[test]
    fn memory_admission_preserves_reserve_and_distinguishes_policies() {
        assert_eq!(committed_growth(8, 6), 2);
        assert_eq!(committed_growth(8, 10), 0);
        assert_eq!(reserve_bytes(100, 10, 8), 10);
        assert_eq!(reserve_bytes(100, 10, 20), 20);
        for (available, committed, estimate, reserve, expected) in [
            (50, 10, 20, 20, Decision::Admit),
            (49, 10, 20, 20, Decision::RefusedOptional),
            (1, 10, 20, 0, Decision::RefusedOptional),
        ] {
            assert_eq!(
                decide(available, committed, estimate, reserve, LspPolicy::Optional),
                expected
            );
        }
        assert_eq!(
            decide(1, 0, 2, 0, LspPolicy::Required),
            Decision::WaitForRequired
        );
    }
}
