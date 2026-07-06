use super::feed_item::{attach_resolver_chain, build_item, hook_cap_for, payload_agent_id};
use super::*;

pub(super) fn handle_blocking_feed(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    feed_kind: FeedKind,
    payload: Value,
) -> Result<()> {
    // A fresh ask supersedes any earlier native_ui ask this session left
    // pending — the agent only ever shows one at a time in its own UI. The
    // push below expires the priors inside its own critical section, so the
    // sidebar never stacks two rows for one session and the hook pays one
    // lock cycle. Bridge asks resolve via their socket and are left alone.
    let superseded_session: Option<String> = payload_agent_id(&payload).map(ToOwned::to_owned);
    let supersede = superseded_session
        .as_deref()
        .map(|agent_id| (agent.descriptor().kind, agent_id));

    let allowlist = Allowlist::load().context("loading resolver allowlist")?;
    let fresh = fresh_enrolled(ledger.runtime_paths(), &allowlist)
        .context("checking resolver heartbeat freshness")?;

    if fresh.is_empty() {
        // No fresh enrolled resolver: native_ui path. The hook writes the feed
        // item, wakes sidebars, returns the neutral no-op, and exits — the
        // agent's own UI is the answer surface. An agent with no native ask
        // UI (pi) skips the item: neutral already lets the tool run, and a
        // `native_ui` row would strand waiting on a surface that doesn't
        // exist.
        if agent.descriptor().capabilities.native_ask_ui {
            let item = build_item(workspace, Surface::NativeUi, feed_kind, agent, payload);
            push_feed_item_recording_ask(
                ledger,
                agent,
                event_name,
                &item,
                supersede,
                &workspace.session_name,
            )?;
        }
        emit_neutral(agent, event_name)?;
        return Ok(());
    }

    // Bridge path. Bind before push so a fast resolver can't miss the socket.
    let mut item = build_item(workspace, Surface::Bridge, feed_kind, agent, payload);
    let cap = hook_cap_for(agent);
    item.hook_wait_timeout_seconds = cap.as_secs();
    attach_resolver_chain(&mut item, &fresh);
    let active_resolver = item.chain_active_resolver.clone().ok_or_else(|| {
        anyhow::anyhow!("fresh resolver list did not produce an active chain step")
    })?;
    let (sock, sock_path) = bridge_api::bind(ledger.runtime_paths(), &item.request_id)
        .context("binding bridge socket")?;
    let guard = SocketGuard::new(sock_path);

    // TOCTOU guard — the resolver may have died between the freshness
    // walk and now. If so, downgrade.
    if let Err(err) = restat(
        ledger.runtime_paths(),
        &allowlist,
        &active_resolver,
        &item.request_id,
    ) {
        debug!(
            resolver = active_resolver.as_str(),
            error = %err,
            "bridge: downgrading to native_ui — resolver heartbeat stale"
        );
        drop(guard);
        // Same native-ask gate as the no-resolver branch: with no surface to
        // hand off to, the downgrade is the neutral answer alone.
        if agent.descriptor().capabilities.native_ask_ui {
            let mut downgraded = item;
            downgraded.surface = Surface::NativeUi;
            downgraded.hook_wait_timeout_seconds = 0;
            downgraded.chain.clear();
            downgraded.chain_active_resolver = None;
            downgraded.chain_active_until = None;
            push_feed_item_recording_ask(
                ledger,
                agent,
                event_name,
                &downgraded,
                supersede,
                &workspace.session_name,
            )?;
        }
        emit_neutral(agent, event_name)?;
        return Ok(());
    }

    let request_id = item.request_id.clone();
    let expected = ExpectedFrame {
        workspace_id: item.workspace_id.clone(),
        request_id: request_id.clone(),
        nonce: item.nonce.clone(),
    };

    push_feed_item_recording_ask(
        ledger,
        agent,
        event_name,
        &item,
        supersede,
        &workspace.session_name,
    )?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building bridge runtime")?;

    let result: Result<()> = runtime.block_on(async {
        let sock = bridge_api::adopt(sock).context("adopting bridge socket")?;
        run_bridge_poll(
            agent,
            ledger,
            &allowlist,
            workspace,
            event_name,
            &request_id,
            &expected,
            &sock,
            cap,
        )
        .await
    });
    drop(guard);
    result
}

fn push_feed_item_recording_ask(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    item: &FeedItem,
    supersede: Option<(&str, &str)>,
    session_name: &str,
) -> Result<()> {
    let chat = chat_ask_entry(agent, event_name, item);
    ledger.push_feed_item_superseding(item, supersede, session_name)?;
    if let Some(entry) = chat.as_ref()
        && let Err(err) = rimz::chat::append(ledger.paths(), entry)
    {
        warn!(
            agent = agent.descriptor().kind,
            request_id = %item.request_id,
            error = %err,
            "bridge: failed to record transcript ask",
        );
    }
    Ok(())
}

fn chat_ask_entry(
    agent: &dyn AgentAdapter,
    event_name: &str,
    item: &FeedItem,
) -> Option<rimz::chat::ChatEntry> {
    if item.source_kind != "agent-hook" || !item.kind.is_ask() {
        return None;
    }
    let agent_id = item
        .agent_session_id()
        .map(rimz::ids::AgentSessionId::from)?;
    let questions = agent.ask_question_detail(event_name, &item.payload)?;
    if questions.is_empty() {
        return None;
    }
    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::TurnStarted);
    observation.worktree_path = item.worktree_path.clone();
    observation.worktree_branch = item.worktree_branch.clone();
    observation.transcript_path = item
        .payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned);

    let last = agent
        .last_assistant_message(event_name, &item.payload, &observation)
        .map(|message| message.trim().to_owned())
        .filter(|message| !message.is_empty());
    let text = last.unwrap_or_default();
    // lane: basename fallback; item carries no stamped channel.
    let channel = rimz::chat::entry_channel(None, item.worktree_path.as_deref());
    let mut entry = rimz::chat::ChatEntry::new(
        item.created_at,
        rimz::ids::AgentKind::new_unchecked(item.source.clone()),
        agent_id,
        rimz::chat::ChatKind::Ask,
        text,
    );
    entry.channel = channel;
    entry.request_id = Some(item.request_id.clone());
    entry.questions = questions;
    Some(entry)
}

/// Poll loop driving the bridge from a resolver answer, a per-step budget,
/// or a stale heartbeat to a chain advance or a clean fallback. The socket
/// is reused across iterations so a late wakeup frame from a previously
/// active resolver still lands; each branch reloads the feed item from disk
/// because the ledger is the source of truth and another writer (sidebar
/// dismiss, sibling pane abstain) may have moved the state.
#[expect(
    clippy::too_many_arguments,
    reason = "private helper; the bridge loop borrows every state slice exactly once"
)]
async fn run_bridge_poll(
    agent: &dyn AgentAdapter,
    ledger: &Ledger,
    allowlist: &Allowlist,
    workspace: &ResolvedWorkspace,
    event_name: &str,
    request_id: &rimz::ids::RequestId,
    expected: &ExpectedFrame,
    sock: &tokio::net::UnixDatagram,
    cap: Duration,
) -> Result<()> {
    const POLL_TICK: Duration = Duration::from_secs(1);
    let session_name = workspace.session_name.as_str();
    let started = std::time::Instant::now();

    loop {
        let live = match ledger.load_feed_item(request_id) {
            Ok(live) => live,
            Err(err) if is_feed_item_not_found(&err, request_id) => {
                emit_neutral(agent, event_name)?;
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        if let Some(done) = handle_terminal_status(&live, agent, event_name)? {
            return done;
        }

        let elapsed = started.elapsed();
        if elapsed >= cap {
            return finish_on_cap(ledger, agent, event_name, request_id, session_name);
        }
        let hook_remaining = cap - elapsed;

        let Some(active) = live.chain_active_resolver.clone() else {
            match ledger.mark_feed_item_timed_out(
                request_id,
                session_name,
                AbandonReason::ChainExhausted,
            ) {
                Ok(_) => {}
                Err(err) if is_feed_item_not_found(&err, request_id) => {}
                Err(err) => return Err(err.into()),
            }
            emit_neutral(agent, event_name)?;
            return Ok(());
        };

        let inner_cap = hook_remaining
            .min(step_budget_remaining(&live).unwrap_or(hook_remaining))
            .min(POLL_TICK);

        match bridge_api::wait_for_resolution(sock, expected, Some(inner_cap)).await? {
            BridgeOutcome::Resolved => continue,
            BridgeOutcome::Terminal => {
                emit_neutral(agent, event_name)?;
                return Ok(());
            }
            BridgeOutcome::Neutral => {
                if started.elapsed() < cap {
                    advance_chain_if_step_lapsed(
                        ledger,
                        allowlist,
                        request_id,
                        &active,
                        session_name,
                    )?;
                }
            }
        }
    }
}

/// If the reloaded item is in a terminal state, deliver the agent-side
/// answer and tell the loop to exit. `Ok(None)` means "still pending, keep
/// polling".
fn handle_terminal_status(
    live: &FeedItem,
    agent: &dyn AgentAdapter,
    event_name: &str,
) -> Result<Option<Result<()>>> {
    match live.status {
        FeedStatus::Resolved => {
            let resolution = live.resolution.as_ref().ok_or_else(|| {
                anyhow::anyhow!("bridge reload: status=resolved but no resolution on disk")
            })?;
            print_decision(agent, live, resolution)?;
            Ok(Some(Ok(())))
        }
        FeedStatus::TimedOut | FeedStatus::Abandoned => {
            emit_neutral(agent, event_name)?;
            Ok(Some(Ok(())))
        }
        FeedStatus::Pending => Ok(None),
    }
}

/// Hook cap fired. Mark the item timed-out with `BridgeCapElapsed`; if a
/// concurrent resolver landed a decision in the same tick the ledger will
/// have observed it first (CAS) — render that decision instead of neutral.
fn finish_on_cap(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    request_id: &rimz::ids::RequestId,
    session_name: &str,
) -> Result<()> {
    let timeout = match ledger.mark_feed_item_timed_out(
        request_id,
        session_name,
        AbandonReason::BridgeCapElapsed,
    ) {
        Ok(timeout) => timeout,
        Err(err) if is_feed_item_not_found(&err, request_id) => {
            emit_neutral(agent, event_name)?;
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    if timeout.status == FeedStatus::Resolved {
        let resolved = match ledger.load_feed_item(request_id) {
            Ok(resolved) => resolved,
            Err(err) if is_feed_item_not_found(&err, request_id) => {
                emit_neutral(agent, event_name)?;
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        if let Some(resolution) = resolved.resolution.as_ref() {
            return print_decision(agent, &resolved, resolution);
        }
        warn!(
            request_id = %request_id,
            "bridge: timeout observed Resolved but no resolution on disk"
        );
    }
    emit_neutral(agent, event_name)
}

/// After a Neutral frame inside the cap, decide whether the chain advances.
/// The reload-then-check pattern is required because another writer may have
/// already moved the state; CAS in `elapse_chain_step` will reject if so and
/// the next iteration picks up the new state.
fn advance_chain_if_step_lapsed(
    ledger: &Ledger,
    allowlist: &Allowlist,
    request_id: &rimz::ids::RequestId,
    active: &rimz::ids::ResolverId,
    session_name: &str,
) -> Result<()> {
    let post = match ledger.load_feed_item(request_id) {
        Ok(post) => post,
        Err(err) if is_feed_item_not_found(&err, request_id) => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if !post.status.allows_resolution() || post.chain_active_resolver.as_ref() != Some(active) {
        return Ok(());
    }

    let advance_reason = if post
        .chain_active_until
        .is_some_and(|d| d <= jiff::Timestamp::now())
    {
        Some(AbandonReason::BudgetElapsed)
    } else if !is_resolver_fresh(ledger.runtime_paths(), allowlist, active) {
        Some(AbandonReason::HeartbeatStale)
    } else {
        None
    };

    if let Some(reason) = advance_reason
        && let Err(err) = ledger.elapse_chain_step(request_id, active, reason, session_name)
    {
        debug!(error = %err, ?reason, "elapse_chain_step failed; reloading");
    }
    Ok(())
}

fn is_feed_item_not_found(
    err: &rimz::ledger::LedgerErr,
    request_id: &rimz::ids::RequestId,
) -> bool {
    matches!(
        err,
        rimz::ledger::LedgerErr::FeedStore(rimz::ledger::FeedStoreErr::NotFound(missing))
            if missing.as_str() == request_id.as_str()
    )
}

fn print_decision(
    agent: &dyn AgentAdapter,
    item: &FeedItem,
    resolution: &rimz::feed::Resolution,
) -> Result<()> {
    let decision = agent.render_decision(item, resolution)?;
    let rendered = serde_json::to_string(&decision)?;
    #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
    {
        println!("{rendered}");
    }
    Ok(())
}

/// Remaining time on the active resolver's per-step budget, or `None` when
/// the feed item carries no deadline (chain not attached, or already moved).
fn step_budget_remaining(item: &FeedItem) -> Option<Duration> {
    let deadline = item.chain_active_until?;
    let span = deadline.duration_since(jiff::Timestamp::now());
    if span.is_negative() {
        return Some(Duration::ZERO);
    }
    // Past the negative guard the span is non-negative, so both components are too.
    Some(Duration::new(
        span.as_secs() as u64,
        span.subsec_nanos() as u32,
    ))
}

fn emit_neutral(agent: &dyn AgentAdapter, event_name: &str) -> Result<()> {
    if let Some(payload) = agent.render_neutral(event_name)? {
        let rendered = serde_json::to_string(&payload)?;
        // Hook stdout is the decision channel: legal stdout site.
        #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
        {
            println!("{rendered}");
        }
    }
    Ok(())
}
