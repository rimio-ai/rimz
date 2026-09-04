use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use rimz::config::TaskEntry;
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
    let target = delivery_target(&ctx, caller.as_ref(), args.target.as_deref())?;
    let matches = parse_matches(&args.matches)?;
    self_wake_guard(args.signal.as_deref(), &matches, &target)?;

    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    let petname = rimz::harness::petname::mint(
        catalog
            .visible()
            .keys()
            .filter_map(|name| name.strip_prefix("wake-")),
    );
    let name = format!("wake-{petname}");
    let prompt = args.prompt.or_else(|| {
        args.prompt_file
            .is_none()
            .then(|| "The wake condition you were waiting for completed.".to_owned())
    });
    let mut entry = TaskEntry {
        wake: Some(target.clone()),
        prompt,
        prompt_file: args.prompt_file,
        root: ctx.workspace.project_root.clone(),
        ..TaskEntry::default()
    };
    let trigger = if let Some(delay) = args.in_after {
        entry.at = Some(resolve_in(delay)?);
        format!("in {}", duration_label(delay))
    } else if let Some(signal) = args.signal {
        signal
            .parse::<rimz::harness::schedule::signal::SignalName>()
            .map_err(anyhow::Error::msg)?;
        entry.signal = Some(signal.clone());
        entry.matches = (!matches.is_empty()).then_some(matches);
        entry.once = Some(true);
        rimz::harness::schedule::TaskShape::compile(&name, &entry)
            .trigger()
            .as_ref()
            .map_err(Clone::clone)?
            .describe()
    } else {
        let command = command_string(&args.command)?;
        entry.watch = Some(command.clone());
        entry.on = Some(parse_on(args.on.as_deref()));
        entry.timeout = args.timeout.map(duration_label);
        format!("watch: {command}")
    };
    rimz::harness::schedule::TaskShape::compile(&name, &entry)
        .trigger()
        .as_ref()
        .map_err(Clone::clone)?;

    let created = Timestamp::now();
    catalog.replace_machine(&name, &entry)?;
    if entry.watch.is_some() {
        spawn_watcher(&ctx, &name)?;
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
        let record = wait::for_record(&ctx, &name, created, timeout)?;
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
    if (args.on.is_some() || args.timeout.is_some()) && args.command.is_empty() {
        bail!("--on and --timeout require a command after --");
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
    Ok(())
}

fn resolve_in(duration: Duration) -> Result<String> {
    let mut target = Timestamp::now()
        .to_zoned(rimz::config::MachineConfig::load_lenient().time_zone())
        .checked_add(duration)
        .context("resolving --in against the configured clock")?;
    if target.second() != 0 || target.subsec_nanosecond() != 0 {
        target = target
            .checked_add(Duration::from_secs((60 - target.second()) as u64))
            .context("rounding --in to the next scheduler minute")?;
    }
    Ok(format!("{:02}:{:02}", target.hour(), target.minute()))
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

fn spawn_watcher(ctx: &Ctx, name: &str) -> Result<()> {
    let mut command = Command::new(rimz::proc::rimz_exe());
    command
        .args(["--root"])
        .arg(&ctx.workspace.project_root)
        .args(["wake", "watch", name])
        .current_dir(&ctx.runtime().shared_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    rimz::child_process::spawn_detached_reaped(&mut command, "wake-watch")
        .context("starting wake watcher")?;
    Ok(())
}
