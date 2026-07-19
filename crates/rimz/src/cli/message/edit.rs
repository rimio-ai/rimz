use super::*;

pub(super) fn edit_from_flags(flags: EditFlags) -> Result<MessageEdit> {
    let EditFlags {
        text,
        file,
        on,
        schedule,
        no_schedule,
        force,
        no_force,
        enter,
        no_enter,
        smart_compact,
        no_smart_compact,
    } = flags;
    let text = match (text, file) {
        (Some(text), None) => Some(resolve_message(&[text], None, None)?),
        (None, Some(path)) => Some(resolve_message(&[], Some(path.as_path()), None)?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap enforces --text/--file conflicts"),
    };
    let machine_config = crate::cli::machine_config();
    let now = Timestamp::now().to_zoned(machine_config.time_zone());
    let not_before = if no_schedule {
        Some(None)
    } else {
        schedule
            .as_deref()
            .map(|raw| parse_schedule_at(raw, &now).map_err(anyhow::Error::msg))
            .transpose()?
            .map(Some)
    };
    let force = force.then_some(true).or_else(|| no_force.then_some(false));
    let enter = enter.then_some(true).or_else(|| no_enter.then_some(false));
    let auto_compact = smart_compact
        .map(Some)
        .or_else(|| no_smart_compact.then_some(None));
    Ok(MessageEdit {
        text,
        gate: on,
        not_before,
        force,
        enter,
        auto_compact,
    })
}

pub(super) fn edit_message(
    message_id: MessageId,
    flags: EditFlags,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let edit = edit_from_flags(flags)?;
    if edit.is_empty() {
        bail!("nothing to edit; pass --text, --file, --on, --schedule, or another edit flag");
    }
    let fields = edit.changed_fields();
    match store.edit_message(&message_id, edit, &workspace.session_name)? {
        EditOutcome::Edited(_) => {
            deliver::register_message_wake(workspace, store)?;
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("edited {message_id} ({})", fields.join(", "));
            }
            Ok(())
        }
        EditOutcome::NotOpen(MessageStatus::Claimed) => {
            bail!("{message_id} delivery in progress; retry in a moment")
        }
        EditOutcome::NotOpen(status) if status.is_terminal() => {
            bail!("{message_id} is {status}; use `rimz message requeue {message_id}`")
        }
        EditOutcome::NotOpen(status) => {
            bail!("{message_id} is {status}; only queued messages can be edited")
        }
        EditOutcome::NotFound => bail!("message {message_id} not found"),
    }
}

pub(super) fn steer_queued_message(
    message_id: MessageId,
    force: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let messages = store.list_messages()?;
    let Some(record) = messages
        .iter()
        .find(|record| record.message_id == message_id)
    else {
        if let Some(history) = store
            .list_message_history()?
            .into_iter()
            .find(|record| record.message_id == message_id)
        {
            bail!(
                "{message_id} is {}; use `rimz message requeue {message_id}`",
                history.status
            );
        }
        bail!("message {message_id} not found");
    };
    match record.status {
        MessageStatus::Queued => {}
        MessageStatus::Claimed => bail!("{message_id} delivery in progress; retry in a moment"),
        status if status.is_terminal() => {
            bail!("{message_id} is {status}; use `rimz message requeue {message_id}`")
        }
        status => bail!("{message_id} is {status}; only queued messages can be steered"),
    }
    let mut snapshot = ctx.resolution_snapshot(globals)?;
    if let Ok(runtime) = rimz::RuntimePaths::for_workspace(record.workspace_id.clone()) {
        snapshot = snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
    }
    let label = message_target_for_record(record, &snapshot);
    let delivered = deliver::deliver_one(
        workspace,
        store,
        &message_id,
        Duration::ZERO,
        globals.mux,
        deliver::DeliveryPolicy::Steer { force },
    )?;
    if delivered {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("sent to {label} ({message_id})");
        }
        return Ok(());
    }
    let messages = store.list_messages()?;
    let Some(record) = messages
        .iter()
        .find(|record| record.message_id == message_id)
    else {
        bail!("message {message_id} is no longer queued");
    };
    let check = deliver::explain(record, &messages, &snapshot, Timestamp::now());
    bail!("{}", steer_failure(&check, &label, &message_id))
}

pub(super) fn requeue_message(
    message_id: MessageId,
    flags: EditFlags,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let record = if let Some(record) = store
        .list_message_history()?
        .into_iter()
        .find(|record| record.message_id == message_id)
    {
        record
    } else if let Some(record) = store
        .list_messages()?
        .into_iter()
        .find(|record| record.message_id == message_id)
    {
        record
    } else if projected_messages(store)?
        .into_iter()
        .any(|row| row.message_id == message_id)
    {
        bail!("message {message_id} content is not retained; send a new message instead");
    } else {
        bail!("message {message_id} not found");
    };
    if !record.status.is_terminal() {
        if matches!(
            record.status,
            MessageStatus::Queued | MessageStatus::Claimed
        ) {
            bail!("{message_id} is still queued; use `rimz message edit` or `rimz message steer`");
        }
        bail!(
            "{message_id} is {}; wait for it to finish before requeueing",
            record.status
        );
    }
    if record.text.is_empty() {
        bail!("message {message_id} content is not retained; send a new message instead");
    }
    let edit = edit_from_flags(flags)?;
    let mut copy = MessageRecord::requeue_from(&record);
    edit.apply(&mut copy);
    let new_id = copy.message_id.clone();
    store.queue_message(&copy, &workspace.session_name)?;
    deliver::register_message_wake(workspace, store)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let label = message_target_for_record(&copy, &snapshot);
    #[expect(clippy::print_stdout, reason = "command result")]
    {
        println!("queued for {label} ({new_id})  (from {message_id})");
    }
    Ok(())
}

pub(super) fn message_target_for_record(
    record: &MessageRecord,
    snapshot: &SidebarSnapshot,
) -> String {
    let row = MessageListRow::from_record(record.clone());
    let agents: Vec<&AgentState> = snapshot.root_agents().collect();
    scoped_handle(message_target(&row, &agents), row.channel.as_deref())
}

pub(super) fn cancel_messages(message_ids: Vec<MessageId>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let mut failed = false;
    for message_id in message_ids {
        if store.cancel_message(&message_id, &workspace.session_name, "cancel")? {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("canceled {message_id}");
            }
        } else {
            failed = true;
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("{message_id} cannot be canceled");
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

pub(super) fn clear_messages(
    target: Option<String>,
    worktree: Option<String>,
    channel_flag: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let snapshot = ctx.cached_snapshot()?;
    let channel = ctx.channel().map(ToOwned::to_owned);
    if let Some(target) = target {
        rimz::harness::target::require_mention(&target)?;
        let agent = crate::cli::resolve_agent_one(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        )?;
        let canceled = store.clear_messages_for(
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
            &workspace.session_name,
        )?;
        print_canceled_summary(&format!("for {target}"), &canceled);
        return Ok(());
    }
    let lane = worktree
        .as_deref()
        .or(channel_flag.as_deref())
        .or(channel.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "message clear needs an @agent target or scoped channel; pass --channel NAME or run from a RimZ channel"
            )
        })?;
    let canceled = store.clear_channel_messages(lane, &workspace.session_name)?;
    print_canceled_summary(&format!("in #{lane}"), &canceled);
    Ok(())
}

pub(super) fn print_canceled_summary(scope: &str, canceled: &[MessageRecord]) {
    let ids: Vec<String> = canceled
        .iter()
        .map(|message| message.message_id.to_string())
        .collect();
    #[expect(clippy::print_stdout, reason = "final user-facing message")]
    {
        if ids.is_empty() {
            println!("canceled 0 message(s) {scope}");
        } else {
            println!(
                "canceled {} message(s) {scope}: {}",
                ids.len(),
                ids.join(", ")
            );
        }
    }
}

pub(super) fn deliver_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    deliver::deliver_one(
        workspace,
        store,
        &message_id,
        rimz::message::settle_duration_from_env(),
        globals.mux,
        deliver::DeliveryPolicy::Boundary,
    )?;
    Ok(())
}

pub(super) fn sweep_messages(globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    deliver::sweep(workspace, store, globals.mux)?;
    Ok(())
}
