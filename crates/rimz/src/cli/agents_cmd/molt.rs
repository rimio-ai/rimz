//! CLI input and receipt for the calling agent's fresh-session swap.

use super::*;
use rimz::harness::ancestry::resolve_caller;
use rimz::launcher::molt::{MoltErr, MoltSource};

#[derive(Debug, Args)]
pub(super) struct MoltArgs {
    /// Context and instructions for the fresh session.
    #[arg(value_name = "NOTE", allow_hyphen_values = true)]
    note: Option<String>,
    /// Read the note from stdin; an inline instruction goes first.
    #[arg(long)]
    stdin: bool,
    /// Re-base the same job onto this profile or provider.
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
    /// Keep this session open until it has rested this long after its turn ends (`3m`);
    /// by default it closes when the turn ends.
    #[arg(long, value_name = "REST", value_parser = rimz::utils::time::parse_compact_duration)]
    when_idle: Option<std::time::Duration>,
}

pub(super) fn run_molt(args: MoltArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = match Ctx::open(globals) {
        Ok(ctx) => ctx,
        Err(_) if resolve_caller(&[]).is_none() => return Err(molt_error(MoltErr::OutsideAgent)),
        Err(error) => return Err(error),
    };
    let source = MoltSource::resolve(&ctx.store).map_err(molt_error)?;
    let piped = if args.stdin {
        crate::cli::send::read_stdin_prompt()?
    } else {
        None
    };
    let note = crate::cli::send::combine_text_prompt(args.note.as_deref(), piped.as_deref())
        .ok_or_else(|| molt_error(MoltErr::EmptyNote))?;
    let molted = source
        .launch(
            &ctx.workspace,
            ctx.mux(),
            &crate::cli::machine_config(),
            note,
            args.agent,
            |notice| writeln!(crate::cli::render::err(), "{notice}"),
        )
        .map_err(|failure| molt_error(failure.error))?;
    let mut out = crate::cli::render::out();
    let handle = &molted.handle;
    // The replacement has bound, so a receipt that fails to print still lets
    // the replaced session go.
    let rest = args.when_idle.unwrap_or_default();
    let closes = if rest.is_zero() {
        "when this turn ends".to_owned()
    } else {
        format!(
            "after {} idle once this turn ends",
            rimz::harness::schedule::arm::duration_label(rest)
        )
    };
    let receipt = writeln!(
        out,
        "molted {handle} into a fresh session beside this one; this session closes {closes}\nnext: rimz agents show {handle}"
    )
    .and_then(|()| out.flush());
    molted.close_when_rested(rest, |notice| {
        writeln!(crate::cli::render::err(), "{notice}")
    });
    Ok(receipt?)
}

/// The hidden helper a replaced session's turn-end hook spawns to close it once rested.
pub(super) fn run_molt_close(request: rimz::launcher::molt::MoltCloseRequest) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), None)?;
    rimz::launcher::molt::close_rested(&ctx.workspace, &ctx.store, &request)
}

fn molt_error(error: MoltErr) -> anyhow::Error {
    match error {
        MoltErr::OutsideAgent | MoltErr::EmptyNote => UsageError::new(error.to_string()).into(),
        MoltErr::Other(error) => error,
        error => error.into(),
    }
}
