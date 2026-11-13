use super::*;
use crate::cli::{render, select};
use rimz::sidebar::marks;
use rimz::store::snapshot::SidebarRow;

#[derive(Debug, Args)]
#[command(
    override_usage = "rimz agents mark [OPTIONS] <TARGET> <MARK> [TEXT]\n       rimz agents mark [OPTIONS] --filter <QUERY> <MARK> [TEXT]"
)]
pub(super) struct MarkArgs {
    /// Agent address, `@all`, `#channel`, or `-` to read one address per line from stdin;
    /// with `--filter`, the mark instead.
    target: Option<String>,
    /// The mark to apply: read, unread, or alert; with `--filter`, the alert text.
    #[arg(
        value_name = "MARK",
        allow_hyphen_values = true,
        add = clap_complete::ArgValueCandidates::new(mark_candidates)
    )]
    mark: Option<String>,
    /// Alert text, required for `alert`.
    #[arg(value_name = "TEXT", allow_hyphen_values = true)]
    text: Option<String>,
    #[arg(long)]
    worktree: Option<String>,
    /// Mark the agents matching `field=value` terms, as `agents list --filter` reads them;
    /// repeatable, `--filter help` lists fields.
    #[arg(
        long = "filter",
        value_name = "QUERY",
        add = clap_complete::ArgValueCompleter::new(crate::cli::complete::filter_terms)
    )]
    filters: Vec<String>,
    /// Mark every agent `--filter` or stdin selects.
    #[arg(long)]
    all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Mark {
    Read,
    Unread,
    Alert,
}

impl Mark {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Unread => "unread",
            Self::Alert => "alert",
        }
    }
}

fn mark_candidates() -> Vec<clap_complete::CompletionCandidate> {
    Mark::value_variants()
        .iter()
        .map(|mark| clap_complete::CompletionCandidate::new(mark.as_str()))
        .collect()
}

/// The parsed positionals: the target (none under `--filter`), the mark, and
/// any alert text.
fn positionals(args: &MarkArgs) -> Result<(Option<&str>, Mark, Option<&str>)> {
    let (target, mark, text) = if args.filters.is_empty() {
        let (Some(target), Some(mark)) = (args.target.as_deref(), args.mark.as_deref()) else {
            return Err(UsageError::new(
                "name the agent and the mark: rimz agents mark <TARGET> <MARK> [TEXT]".to_owned(),
            )
            .into());
        };
        (Some(target), mark, args.text.as_deref())
    } else {
        if args.text.is_some() {
            return Err(UsageError::new(
                "--filter names the agents, so mark takes only <MARK> [TEXT]".to_owned(),
            )
            .into());
        }
        let mark = args.target.as_deref().ok_or_else(|| {
            UsageError::new(
                "name the mark: rimz agents mark --filter <QUERY> <MARK> [TEXT]".to_owned(),
            )
        })?;
        (None, mark, args.mark.as_deref())
    };
    let mark = Mark::from_str(mark, false).map_err(|_| {
        UsageError::new(format!(
            "invalid mark `{mark}`; expected read, unread, or alert"
        ))
    })?;
    if text.is_some_and(|text| text.trim().is_empty()) || (mark != Mark::Alert && text.is_some()) {
        return Err(UsageError::new("only alert takes non-empty TEXT".into()).into());
    }
    if mark == Mark::Alert && text.is_none() {
        return Err(UsageError::new("alert needs TEXT: the alert to raise".into()).into());
    }
    Ok((target, mark, text))
}

pub(super) fn run(args: MarkArgs, globals: &GlobalFlags) -> Result<()> {
    if select::filter_help(&args.filters)? {
        return Ok(());
    }
    let (target, mark, text) = positionals(&args)?;
    let ctx = Ctx::open(globals)?;
    let caller = rimz::launcher::context::resolve_caller(&ctx.store)?;
    if mark == Mark::Read && caller.is_some() {
        bail!("read means a person looked; agents can mark unread or alert instead");
    }
    ctx.runtime().ensure_dirs()?;
    let actor = caller.as_ref().map(|_| {
        rimz::launcher::context::sender_for(caller.as_ref(), ctx.channel(), false).render()
    });
    let marking = Marking {
        ctx: &ctx,
        mark,
        text,
        actor,
        worktree: args.worktree.as_deref(),
        mux: globals.mux,
    };
    let references = Vec::from_iter(target.map(str::to_owned));
    if select::selects(&references, &args.filters) {
        let flags = select::TargetFlags {
            references: &references,
            filters: &args.filters,
            all: args.all,
            room: false,
        };
        let select::Targets::Selected(selected) = select::select_targets(&ctx, "mark", flags)?
        else {
            return Ok(());
        };
        let mut failed = 0;
        let mut out = render::out();
        for target in &selected {
            failed += usize::from(!marking.selected(&mut out, target)?);
        }
        return select::batch_result(failed);
    }
    let target = target.unwrap_or_default();
    let rows = marking.resolve(target)?;
    let mut failed = 0;
    let mut out = render::out();
    for row in &rows {
        failed += usize::from(!marking.row(&mut out, row, None)?);
    }
    select::batch_result(failed)
}

struct Marking<'a> {
    ctx: &'a Ctx,
    mark: Mark,
    text: Option<&'a str>,
    actor: Option<String>,
    worktree: Option<&'a str>,
    mux: Option<rimz::ids::MuxName>,
}

impl Marking<'_> {
    fn resolve(&self, target: &str) -> Result<Vec<SidebarRow>> {
        crate::cli::resolve_hint(
            target,
            marks::resolve(
                &self.ctx.workspace,
                &self.ctx.store,
                target,
                self.worktree,
                &self.ctx.address_context(),
                self.mux,
            )
            .map_err(|error| match error {
                marks::MarkErr::Target(error) => anyhow::Error::from(error),
                error => error.into(),
            }),
        )
    }

    /// Mark one selected agent, its card resolved from its address now;
    /// `false` when it failed.
    fn selected(&self, out: &mut impl Write, target: &select::Selected) -> Result<bool> {
        let rows = match self.resolve(&target.address) {
            Ok(rows) => rows,
            Err(err) => {
                writeln!(out, "error {}: {err:#}", target.address)?;
                return Ok(false);
            }
        };
        let [row] = rows.as_slice() else {
            writeln!(
                out,
                "error {}: names {} agent cards, not one",
                target.address,
                rows.len()
            )?;
            return Ok(false);
        };
        self.row(out, row, Some(target))
    }

    /// Mark the agent behind `row` when a read taken now still finds it live,
    /// registered, and, for a selected agent, the one selected, and print its
    /// receipt line; `false` when it failed.
    fn row(
        &self,
        out: &mut impl Write,
        row: &SidebarRow,
        selected: Option<&select::Selected>,
    ) -> Result<bool> {
        let ctx = self.ctx;
        let snapshot = ctx.cached_snapshot()?;
        let agent = snapshot.agents.iter().find(|agent| {
            agent.agent_id.as_str() == row.id
                && agent.kind.as_str() == row.name
                && !agent.agent_id.is_provisional()
        });
        let live = agent.filter(|agent| agent.ended_at.is_none());
        let label = selected.map_or_else(
            || match live {
                Some(agent) => {
                    rimz::address::paste_address(agent, &snapshot, &ctx.address_context())
                }
                None => row.display_name().to_owned(),
            },
            |selected| selected.address.clone(),
        );
        if let (Some(selected), Some(agent)) = (selected, agent)
            && let Err(err) = selected.check(Some(agent))
        {
            writeln!(out, "error {label}: {err}")?;
            return Ok(false);
        }
        let Some(agent) = live else {
            writeln!(
                out,
                "error {label}: marks require a live registered agent card"
            )?;
            return Ok(false);
        };
        self.apply(row, agent)?;
        writeln!(out, "marked {} {label}", self.mark.as_str())?;
        Ok(true)
    }

    fn apply(&self, row: &SidebarRow, agent: &AgentState) -> Result<()> {
        let ctx = self.ctx;
        let at = jiff::Timestamp::now();
        let diag = rimz::diag::DiagSink::for_workspace(
            ctx.workspace.workspace_id.clone(),
            ctx.workspace.session_name.clone(),
            None,
        );
        let rows = std::slice::from_ref(row);
        match self.mark {
            Mark::Read => {
                marks::read(ctx.runtime(), rows, &diag, at.as_millisecond())?;
                rimz::store::raised::clear_alert(
                    ctx.store.paths(),
                    &agent.kind,
                    &agent.agent_id,
                    at,
                )?;
            }
            Mark::Unread => marks::unread(ctx.runtime(), rows, &diag, at.as_millisecond())?,
            Mark::Alert => rimz::store::raised::set_alert(
                ctx.store.paths(),
                rimz::store::raised::Entry {
                    kind: agent.kind.clone(),
                    agent_id: agent.agent_id.clone(),
                    value: rimz::agents::state::CardAlert {
                        text: self.text.unwrap_or_default().to_owned(),
                        raised_at: at,
                        raised_by: self.actor.clone(),
                    },
                },
            )?,
        }
        let mut params = serde_json::json!({"mark": self.mark.as_str()});
        if let Some(actor) = &self.actor {
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
        Ok(())
    }
}
