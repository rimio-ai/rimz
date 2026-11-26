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

pub(super) fn run(request: WaitRequest, json: bool, globals: &GlobalFlags) -> Result<()> {
    request.validate()?;
    let kind = request.kind();
    let on = match request.trigger {
        WaitTrigger::Shell { on, .. } => on,
        WaitTrigger::Watch { .. } => CheckOn::Any,
    };
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = wait::arming_agent(&snapshot, caller.as_ref(), kind)?;
    let room = room(&ctx, &snapshot);
    let armed = wait::arm(&room, agent, request, || ctx.published_snapshot())?;
    let pending = wait::pending_rows(&room, Some(agent), kind)?;
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
        return list::write_rows(&mut out, kind, &ctx, Some(agent), &snapshot, pending);
    };
    let check_in = armed
        .check_in
        .map(|check_in| format!("check-in {}", duration_label(check_in)));
    let on = match on {
        CheckOn::Fail => Some("on fail".to_owned()),
        CheckOn::Success => Some("on success".to_owned()),
        CheckOn::Any => None,
    };
    let notes = on.into_iter().chain(check_in).collect::<Vec<_>>();
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join(", "))
    };
    writeln!(out, "armed {name}: {description} → {target}{notes}")?;
    if let Some(expr) = &armed.condition {
        write_condition_now(&mut out, entry, expr)?;
    }
    list::write_rows(&mut out, kind, &ctx, Some(agent), &snapshot, pending)
}

fn write_condition_now(out: &mut impl Write, entry: &TaskEntry, expr: &WhenExpr) -> Result<()> {
    let now = super::super::wake_view::condition_now(entry, expr, None);
    writeln!(out, "  now: {now}")?;
    Ok(())
}
