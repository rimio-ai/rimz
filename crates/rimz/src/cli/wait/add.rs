use std::io::Write;

use serde::Serialize;

use rimz::config::TaskEntry;
use rimz::harness::schedule::arm::duration_label;
use rimz::harness::schedule::wait::WaitArmed;
use rimz::harness::when::WhenExpr;

use super::*;

#[derive(Serialize)]
struct WakeReceipt<'a> {
    #[serde(flatten)]
    armed: &'a WaitArmed,
    pending: Vec<list::WakeRow>,
}

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    if args.timeout.is_some() {
        bail!(
            "`--timeout` is now `--check-in`: it sends one still-running notice and never stops the command"
        );
    }
    let json = args.json;
    let request = args.into_request();
    request.validate()?;
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = wait::arming_agent(&snapshot, caller.as_ref())?;
    let dir = match &request.file {
        Some(_) => {
            std::env::current_dir().context("resolving --file against the current directory")?
        }
        None => std::path::PathBuf::new(),
    };
    let room = room(&ctx, &snapshot);
    let armed = wait::arm(&room, agent, request, &dir, || ctx.published_snapshot())?;
    let pending = wait::pending_rows(&room, Some(agent))?;
    if json {
        return super::super::render::json(&WakeReceipt {
            armed: &armed,
            pending,
        });
    }
    let WaitArmed {
        name,
        description,
        target,
        ..
    } = &armed;
    let mut out = super::super::render::out();
    let Some(entry) = &armed.entry else {
        writeln!(out, "already waiting {name}: {description} → {target}")?;
        return list::write_rows(&mut out, &ctx, Some(agent), &snapshot, pending);
    };
    let check_in = armed
        .check_in
        .map(|check_in| format!(" (check-in {})", duration_label(check_in)))
        .unwrap_or_default();
    writeln!(out, "armed {name}: {description} → {target}{check_in}")?;
    if let Some(expr) = &armed.condition {
        write_condition_now(&mut out, entry, expr)?;
    }
    list::write_rows(&mut out, &ctx, Some(agent), &snapshot, pending)
}

fn write_condition_now(out: &mut impl Write, entry: &TaskEntry, expr: &WhenExpr) -> Result<()> {
    let now = super::super::wake_view::condition_now(entry, expr, None);
    writeln!(out, "  now: {now}")?;
    Ok(())
}
