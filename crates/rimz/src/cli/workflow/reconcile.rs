//! The workflow pass: `rimz --root <root> workflow reconcile`, which the scheduler's tick spawns
//! while the room's live directory holds a run that has ended, or that this RimZ cannot continue.
//! Each home is settled under its own workspace-lock holds, and one failing home never stops the
//! rest. A stranded run is ended as failed in its record, its agents are released as ordinary
//! sessions, its loop report is made from the record's header, and its home moves to the
//! finished directory with a header-only history; a record holding no JSON object is set aside
//! unmodified. `workflow advance` and `workflow cancel` on a stranded id settle that one run.

use super::*;
use rimz::disk::lock::WorkspaceLock;

/// Why one run was ended or set aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Cause {
    Stranded(records::StrandedCause),
    NotObject,
}

impl Cause {
    /// The phrase the notice groups ended runs by, after their count.
    fn group(&self) -> String {
        match self {
            Self::Stranded(records::StrandedCause::Older { version }) => {
                format!("written by an older RimZ (record version {version})")
            }
            Self::Stranded(records::StrandedCause::MissingVersion) => {
                "whose record names no version".into()
            }
            Self::Stranded(records::StrandedCause::InvalidVersion) => {
                "whose record version is not a number".into()
            }
            Self::Stranded(records::StrandedCause::Undecodable { .. }) => {
                format!("whose version {} record does not decode", records::VERSION)
            }
            Self::NotObject => "whose record holds no JSON object".into(),
        }
    }
}

/// A run ended stranded or set aside.
#[derive(Clone)]
pub(super) struct Ended {
    pub(super) id: WorkflowRunId,
    pub(super) cause: Cause,
    pub(super) task: Option<String>,
    /// The agents this call released from the run, by name: a release that succeeds is followed
    /// by the move in the same call, so the call that announces the run names them.
    pub(super) released: Vec<String>,
}

/// What settling one home did.
#[derive(Default)]
pub(super) struct Settled {
    /// The run, when its record was ended stranded or holds no JSON object; none for a home that
    /// needed only its report or its move.
    pub(super) ended: Option<Ended>,
    /// This call moved the home to the finished directory: an ended run is announced by the
    /// call that moves it, so once.
    pub(super) moved: bool,
    /// The run still owes its loop task a report that did not land.
    pub(super) unreported: bool,
    /// Why the run stays live: its report did not land, or its agents could not be released.
    pub(super) warning: Option<String>,
}

impl Settled {
    /// The run to announce: ended or set aside, and moved by this call.
    fn announced(&self) -> Option<&Ended> {
        self.ended.as_ref().filter(|_| self.moved)
    }
}

/// The room a pass settles homes in.
pub(super) struct Room<'a> {
    pub(super) state: &'a rimz::StatePaths,
    pub(super) store: &'a rimz::Store,
    pub(super) session_name: &'a str,
}

/// Settle one live home: end a stranded run, report a finished run that owes its loop task,
/// release the agents of a run ended stranded or set aside, and move the run to the finished
/// directory. The report is always tried, and frees the loop; the release follows a report that
/// landed, and the move follows a release that succeeded, so a home that stops short stays live
/// for the next pass and a successful release is announced by the pass that moves the home.
/// A run this RimZ continues, a newer RimZ's run and a home without a record are left alone.
pub(super) fn settle(room: &Room<'_>, id: &WorkflowRunId) -> Result<Settled> {
    let paths = room.state.workflow(id);
    match records::classify(&paths)? {
        records::RunClass::Stranded { .. } => {
            let guard = WorkspaceLock::acquire(&room.state.workspace_lock)?;
            let _ = records::end_stranded(&paths, &guard)?;
        }
        // An owed report of a run this RimZ reads is retried by its paced advance.
        records::RunClass::Current(record)
            if !record.state.is_terminal() || record.owes_loop_report() =>
        {
            return Ok(Settled::default());
        }
        records::RunClass::Newer { .. } | records::RunClass::Recordless => {
            return Ok(Settled::default());
        }
        _ => {}
    }
    // The cause is read from the record, so a pass after one that stopped short of the move
    // still knows why the run ended.
    let class = records::classify(&paths)?;
    let cause = match &class {
        records::RunClass::NotObject => Some(Cause::NotObject),
        class => class
            .header()
            .and_then(|header| header.stranded.clone())
            .map(Cause::Stranded),
    };
    let task = class.header().and_then(|header| header.task.clone());
    if let Err(error) = run_log::try_collect_run(room.state, id) {
        return Ok(Settled {
            ended: cause.map(|cause| Ended {
                id: id.clone(),
                cause,
                task,
                released: Vec::new(),
            }),
            moved: false,
            unreported: true,
            warning: Some(format!(
                "run `{id}` still owes its loop report, so it stays live for the next pass: \
                 {error:#}"
            )),
        });
    }
    let released = match cause {
        Some(_) => rimz::harness::run::release_workflow(room.store, id, room.session_name),
        None => Ok(Vec::new()),
    };
    let (released, moved, warning) = match released {
        Ok(agents) => {
            let names = agents
                .into_iter()
                .map(|agent| agent.name.unwrap_or_else(|| agent.agent_id.to_string()))
                .collect();
            (names, finish_any(room.state, id)?, None)
        }
        Err(error) => (
            Vec::new(),
            false,
            Some(format!(
                "could not release the agents of workflow run `{id}`, so it stays live for \
                 the next pass: {error:#}"
            )),
        ),
    };
    Ok(Settled {
        ended: cause.map(|cause| Ended {
            id: id.clone(),
            cause,
            task,
            released,
        }),
        moved,
        unreported: false,
        warning,
    })
}

/// Move a finished live home this RimZ may move: a run it reads through [`finish_run`], one it
/// reads only by header with a header-only history, and a record holding no JSON object as it is.
/// Returns whether this call moved the home.
fn finish_any(state: &rimz::StatePaths, id: &WorkflowRunId) -> Result<bool> {
    let found = match records::find(state, id)? {
        Some(found) if !found.finished => found,
        _ => return Ok(false),
    };
    Ok(match found.class {
        records::RunClass::Current(_) => finish_run(state, id)?.moved,
        records::RunClass::Ended(header) if !header.owes_loop_report() => {
            let why = match header.version {
                Some(version) if version != u64::from(records::VERSION) => format!(
                    "record version {version} was written by another RimZ, so its steps are not \
                     shown"
                ),
                _ => "its record does not decode, so its steps are not shown".into(),
            };
            let history = workflow_view::history::header_only(id, Some(&header), &why);
            move_home(state, id, header.notes.as_deref(), Some(&history))?
        }
        records::RunClass::NotObject => move_home(state, id, None, None)?,
        _ => false,
    })
}

/// `rimz workflow reconcile`: settle every home of the room's live directory, then tell the user
/// once, through the notification handlers and a sidebar toast, about the runs this pass ended.
/// A pass already running in the room is left to finish alone.
pub(super) fn run(globals: &super::super::GlobalFlags) -> Result<()> {
    let ctx = super::super::ctx::Ctx::open(globals)?;
    let state = ctx.store.paths();
    let Some(_flight) = WorkspaceLock::try_acquire(
        &ctx.store
            .runtime_paths()
            .lock_path("workflow-reconcile.lock"),
    )?
    else {
        return Ok(());
    };
    let room = Room {
        state,
        store: &ctx.store,
        session_name: &ctx.workspace.session_name,
    };
    let mut ended = Vec::new();
    let mut unreported = 0;
    for live in records::live(state)? {
        let Some(id) = live
            .paths
            .home
            .file_name()
            .and_then(|name| name.to_str()?.parse::<WorkflowRunId>().ok())
        else {
            continue;
        };
        match settle(&room, &id) {
            Ok(settled) => {
                if let Some(warning) = &settled.warning {
                    writeln!(render::err(), "warning: {warning}")?;
                }
                unreported += u64::from(settled.unreported);
                ended.extend(settled.announced().cloned());
            }
            Err(error) => writeln!(
                render::err(),
                "warning: workflow run `{id}` not settled: {error:#}"
            )?,
        }
    }
    report(&ctx, &ended, unreported, true);
    Ok(())
}

/// Settle one stranded run a command named by id, in the room that holds it, and record it.
pub(super) fn end_named(
    state: &rimz::StatePaths,
    id: &WorkflowRunId,
    globals: &super::super::GlobalFlags,
    notify: bool,
) -> Result<Settled> {
    let ctx = super::super::ctx::Ctx::for_workspace(state.workspace_id.clone(), globals.mux)?;
    let room = Room {
        state,
        store: &ctx.store,
        session_name: &ctx.workspace.session_name,
    };
    let settled = settle(&room, id)?;
    if let Some(warning) = &settled.warning {
        writeln!(render::err(), "warning: {warning}")?;
    }
    report(
        &ctx,
        settled.announced().cloned().as_slice(),
        u64::from(settled.unreported),
        notify,
    );
    Ok(settled)
}

/// Record the runs a command ended in one diagnostic record and, with `notify`, tell the user
/// once through one notice naming no agent. Nothing is sent when no run was ended.
pub(super) fn report(ctx: &super::super::ctx::Ctx, ended: &[Ended], unreported: u64, notify: bool) {
    if ended.is_empty() {
        return;
    }
    let root = &ctx.workspace.project_root;
    let mut groups: Vec<(String, usize)> = Vec::new();
    for run in ended {
        let group = run.cause.group();
        match groups.iter_mut().find(|(seen, _)| *seen == group) {
            Some((_, count)) => *count += 1,
            None => groups.push((group, 1)),
        }
    }
    let count = ended.len();
    let runs = if count == 1 { "run" } else { "runs" };
    let command = match ended.iter().find(|run| run.task.is_none()) {
        None => format!("{} loop list", resolve::rimz_prefix(Some(root))),
        Some(run) => resolve::command("show", &[run.id.as_str()], Some(root), None, &[], &[]),
    };
    let mut body = format!(
        "{count} workflow {runs} this RimZ cannot continue {}: {}.{} Inspect with `{command}`.",
        if count == 1 {
            "was ended as failed"
        } else {
            "were ended as failed"
        },
        groups
            .iter()
            .map(|(group, count)| format!("{count} {group}"))
            .collect::<Vec<_>>()
            .join(", "),
        if ended.iter().any(|run| !run.released.is_empty()) {
            " Their agents were left running."
        } else {
            ""
        }
    );
    if unreported > 0 {
        body.push_str(&format!(
            " {unreported} loop {} did not land yet and will be retried.",
            if unreported == 1 { "report" } else { "reports" }
        ));
    }
    if notify {
        super::super::room_notice::send(
            root,
            rimz::sidebar::notify::NotificationKind::WorkflowEnded,
            format!("RimZ: {count} workflow {runs} ended"),
            body,
        );
    }
    rimz::diag::DiagSink::for_workspace(
        ctx.workspace.workspace_id.clone(),
        ctx.workspace.session_name.clone(),
        None,
    )
    .emit(rimz::diag::record::DiagEvent::WorkflowRunsEnded {
        runs: ended.iter().map(|run| run.id.clone()).collect(),
        causes: groups
            .iter()
            .map(|(group, count)| format!("{count} {group}"))
            .collect(),
        released: ended
            .iter()
            .flat_map(|run| run.released.iter().cloned())
            .collect(),
        unreported,
    });
}
