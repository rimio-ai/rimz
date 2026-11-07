use super::*;
use rimz::sidebar::marks;

#[derive(Debug, Args)]
pub(super) struct MarkArgs {
    /// Agent address, `@all`, or `#channel`.
    target: String,
    /// The mark to apply.
    #[arg(value_enum)]
    mark: Mark,
    /// Alert text, required for `alert`.
    #[arg(
        value_name = "TEXT",
        allow_hyphen_values = true,
        required_if_eq("mark", "alert")
    )]
    text: Option<String>,
    #[arg(long)]
    worktree: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Mark {
    Read,
    Unread,
    Alert,
}

pub(super) fn run(args: MarkArgs, globals: &GlobalFlags) -> Result<()> {
    if args
        .text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
        || (args.mark != Mark::Alert && args.text.is_some())
    {
        return Err(UsageError::new("only alert takes non-empty TEXT".into()).into());
    }
    let ctx = Ctx::open(globals)?;
    let caller = rimz::launcher::context::resolve_caller(&ctx.store)?;
    if args.mark == Mark::Read && caller.is_some() {
        bail!("read means a person looked; agents can mark unread or alert instead");
    }
    ctx.runtime().ensure_dirs()?;
    let rows = crate::cli::resolve_hint(
        &args.target,
        marks::resolve(
            &ctx.workspace,
            &ctx.store,
            &args.target,
            args.worktree.as_deref(),
            &ctx.address_context(),
            globals.mux,
        )
        .map_err(|error| match error {
            marks::MarkErr::Target(error) => anyhow::Error::from(error),
            error => error.into(),
        }),
    )?;
    let snapshot = ctx.cached_snapshot()?;
    let targets = rows
        .iter()
        .map(|row| {
            snapshot
                .agents
                .iter()
                .find(|agent| {
                    agent.agent_id.as_str() == row.id
                        && agent.kind.as_str() == row.name
                        && agent.ended_at.is_none()
                        && !agent.agent_id.is_provisional()
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "marks require a live registered agent card: {}",
                        row.display_name()
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let at = jiff::Timestamp::now();
    let diag = rimz::diag::DiagSink::for_workspace(
        ctx.workspace.workspace_id.clone(),
        ctx.workspace.session_name.clone(),
        None,
    );
    match args.mark {
        Mark::Read => marks::read(ctx.runtime(), &rows, &diag, at.as_millisecond())?,
        Mark::Unread => marks::unread(ctx.runtime(), &rows, &diag, at.as_millisecond())?,
        Mark::Alert => {}
    }
    let actor = caller.as_ref().map(|_| {
        rimz::launcher::context::sender_for(caller.as_ref(), ctx.channel(), false).render()
    });
    let mark = match args.mark {
        Mark::Read => "read",
        Mark::Unread => "unread",
        Mark::Alert => "alert",
    };
    for agent in &targets {
        match args.mark {
            Mark::Read => rimz::store::raised::clear_alert(
                ctx.store.paths(),
                &agent.kind,
                &agent.agent_id,
                at,
            )?,
            Mark::Unread => {}
            Mark::Alert => rimz::store::raised::set_alert(
                ctx.store.paths(),
                rimz::store::raised::Entry {
                    kind: agent.kind.clone(),
                    agent_id: agent.agent_id.clone(),
                    value: rimz::agents::state::CardAlert {
                        text: args.text.clone().unwrap_or_default(),
                        raised_at: at,
                        raised_by: actor.clone(),
                    },
                },
            )?,
        }
        let mut params = serde_json::json!({"mark": mark});
        if let Some(actor) = &actor {
            params["actor"] = serde_json::json!(actor);
        }
        rimz::store::raised::audit(
            &ctx.store,
            &ctx.workspace.session_name,
            "card.marked",
            &agent.kind,
            &agent.agent_id,
            params,
        )?;
    }
    let mut kv = crate::cli::render::KeyVals::new();
    kv.push(
        format!("Marked {mark}"),
        crate::cli::render::cell(targets.len().to_string())
            .fg(crate::cli::render::palette::accent()),
    );
    kv.render(&mut crate::cli::render::out())?;
    Ok(())
}
