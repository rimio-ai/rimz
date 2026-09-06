use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use rimz::config::{TaskEntry, WakeArmer, WakeMeta};
use rimz::harness::schedule::catalog::TaskCatalog;

use super::*;

#[derive(Serialize)]
struct WakeReceipt<'a> {
    name: &'a str,
    trigger: &'a str,
    target: &'a str,
}

pub(super) fn run(args: WakeArgs, globals: &GlobalFlags) -> Result<()> {
    validate_shape(&args)?;
    let waits_inline = args.wait.is_some();
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let caller_agent = caller_agent(&snapshot, caller.as_ref())?;
    let target = delivery_target(&ctx, &snapshot, caller_agent, args.target.as_deref())?;
    let scope = caller_agent
        .or_else(|| {
            snapshot.agents.iter().find(|agent| {
                agent.kind.as_str() == target.kind && agent.agent_id.as_str() == target.session
            })
        })
        .context("wake target disappeared from its resolution snapshot")?;
    let mut matches = parse_matches(&args.matches)?;
    default_signal_matches(
        &ctx.workspace,
        &snapshot.agents,
        scope,
        args.signal.as_deref(),
        &mut matches,
    )?;
    self_wake_guard(args.signal.as_deref(), &matches, &target)?;

    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    let mut name = if args.signal.is_some() {
        "wake".to_owned()
    } else {
        let petname = rimz::agents::petname::mint(
            catalog
                .visible()
                .keys()
                .filter_map(|name| name.strip_prefix("wake-")),
        );
        format!("wake-{petname}")
    };
    let created = Timestamp::now();
    let armed_by = caller_agent.map_or(WakeArmer::Human, |agent| WakeArmer::Agent {
        handle: rimz::harness::target::agent_handle(
            agent,
            &rimz::harness::target::addressable_agents(&snapshot),
            true,
        ),
    });
    let mut entry = TaskEntry {
        wake: Some(target.clone()),
        wake_meta: Some(WakeMeta {
            armed_by,
            armed_at: created,
            delay: args.in_after.map(duration_label),
        }),
        prompt: args.prompt,
        prompt_file: args.prompt_file,
        root: ctx.workspace.project_root.clone(),
        ..TaskEntry::default()
    };
    let trigger = if let Some(delay) = args.in_after {
        entry.at = Some(rimz::harness::schedule::delayed_at(delay)?);
        format!("in {}", duration_label(delay))
    } else if let Some(signal) = args.signal {
        signal
            .parse::<rimz::harness::schedule::signal::SignalSelector>()
            .map_err(anyhow::Error::msg)?;
        entry.signal = Some(signal.clone());
        entry.matches = (!matches.is_empty()).then_some(matches);
        let timeout = args.timeout.unwrap_or(Duration::from_secs(59 * 60));
        entry.timeout = Some(duration_label(timeout));
        entry.deadline = Some(
            created
                .checked_add(timeout)
                .context("resolving signal wake deadline")?,
        );
        rimz::harness::schedule::TaskShape::compile(&name, &entry)
            .trigger()
            .as_ref()
            .map_err(Clone::clone)?
            .describe()
    } else {
        let command = command_string(&args.command)?;
        entry.watch = Some(command.clone());
        entry.on = Some(parse_on(args.on.as_deref()));
        entry.timeout = Some(duration_label(
            args.timeout.unwrap_or(Duration::from_secs(59 * 60)),
        ));
        format!("watch: {command}")
    };
    rimz::harness::schedule::TaskShape::compile(&name, &entry)
        .trigger()
        .as_ref()
        .map_err(Clone::clone)?;

    let output_file = if entry.watch.is_some() {
        let path = rimz::harness::schedule::signal::wake_log_path(ctx.store.paths(), &name);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("creating wake output {}", path.display()))?;
        file.set_len(0).context("clearing wake output")?;
        Some(file)
    } else {
        None
    };
    if entry.signal.is_some() {
        let (armed_name, _, _) = catalog.arm_signal_wake(&entry, created)?;
        name = armed_name;
    } else {
        catalog.replace_machine(&name, &entry)?;
    }
    if let Some(file) = output_file {
        spawn_watcher(&ctx, &name, file)?;
    }

    if args.json && !waits_inline {
        super::super::render::json(&WakeReceipt {
            name: &name,
            trigger: &trigger,
            target: &target.handle,
        })?;
    } else if !args.json {
        writeln!(
            super::super::render::out(),
            "armed {name}: {trigger} → {}",
            target.handle
        )?;
    }

    if let Some(timeout) = args.wait {
        let record = wait::for_record(&name, created, timeout)?;
        wait::print_and_settle(&ctx, &record, args.json)?;
    }
    Ok(())
}

fn validate_shape(args: &WakeArgs) -> Result<()> {
    let shapes = usize::from(args.in_after.is_some())
        + usize::from(args.signal.is_some())
        + usize::from(!args.command.is_empty());
    if shapes != 1 {
        bail!("choose exactly one wake trigger: --in, --signal, or a command after --");
    }
    if args.wait.is_some() && args.command.is_empty() {
        bail!("--wait requires a command after --");
    }
    if args.on.is_some() && args.command.is_empty() {
        bail!("--on requires a command after --");
    }
    if args.timeout.is_some() && args.command.is_empty() && args.signal.is_none() {
        bail!("--timeout requires --signal or a command after --");
    }
    if args.in_after.is_some_and(|duration| duration.is_zero()) {
        bail!("--in must be greater than zero");
    }
    if args
        .in_after
        .is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60))
    {
        bail!("--in must be less than 24h");
    }
    if args.timeout.is_some_and(|duration| duration.is_zero()) {
        bail!("--timeout must be greater than zero");
    }
    if args
        .timeout
        .is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60))
    {
        bail!("--timeout must be less than 24h");
    }
    Ok(())
}

fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    for (unit_seconds, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        if seconds >= unit_seconds && seconds.is_multiple_of(unit_seconds) {
            return format!("{}{suffix}", seconds / unit_seconds);
        }
    }
    format!("{seconds}s")
}

fn spawn_watcher(ctx: &Ctx, name: &str, output: File) -> Result<()> {
    let mut command = Command::new(rimz::proc::rimz_exe());
    command
        .args(["--root"])
        .arg(&ctx.workspace.project_root)
        .args(["wake", "watch", name])
        .current_dir(&ctx.runtime().shared_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(output))
        .process_group(0);
    rimz::child_process::spawn_detached_reaped(&mut command, "wake-watch")
        .context("starting wake watcher")?;
    Ok(())
}
