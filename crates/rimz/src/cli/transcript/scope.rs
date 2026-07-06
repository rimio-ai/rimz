use super::chat::render_handle;
use super::*;
use anyhow::bail;

#[derive(Clone, Debug)]
pub(super) struct LiveRootAgent {
    pub(super) key: AgentKey,
    pub(super) channel: Option<String>,
    pub(super) registered_at: Option<jiff::Timestamp>,
}

pub(super) fn live_root_agents(workspace: &rimz::ResolvedWorkspace) -> Vec<LiveRootAgent> {
    crate::cli::open_ledger(workspace)
        .ok()
        .and_then(|ledger| ledger.snapshot_cached().ok())
        .map(|snapshot| {
            snapshot
                .agents
                .into_iter()
                .filter(|agent| agent.parent_agent_id.is_none())
                .map(|agent| {
                    let channel = rimz::harness::target::agent_channel(&agent);
                    LiveRootAgent {
                        key: (agent.kind, agent.agent_id),
                        channel,
                        registered_at: agent.registered_at,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn live_boundary(scope: &Scope, live: &[LiveRootAgent]) -> Option<jiff::Timestamp> {
    live.iter()
        .filter(|agent| live_agent_in_scope(agent, scope))
        .filter_map(|agent| agent.registered_at)
        .min()
}

fn live_agent_in_scope(agent: &LiveRootAgent, scope: &Scope) -> bool {
    match &scope.focus_keys {
        Some(keys) => keys.contains(&agent.key),
        None => channel_matches(agent.channel.as_deref(), scope.channel_filter.as_deref()),
    }
}

pub(super) fn entry_in_scope(entry: &ChatEntry, scope: &Scope) -> bool {
    scope
        .focus_keys
        .as_ref()
        .is_some_and(|focus| focus.contains(&entry_key(entry)))
        || channel_matches(entry.channel.as_deref(), scope.channel_filter.as_deref())
}

pub(super) fn compare_optional_timestamps(
    left: Option<jiff::Timestamp>,
    right: Option<jiff::Timestamp>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

pub(super) fn entry_matches_focus(
    entry: &ChatEntry,
    chat: &ChatLine,
    scope: &Scope,
    identities: &HashMap<AgentKey, Identity>,
) -> bool {
    scope.focus_keys.as_ref().is_none_or(|focus| {
        focus.contains(&entry_key(entry))
            || sender_matches_focus(
                &chat.from,
                focus,
                identities,
                scope.channel_filter.as_deref(),
            )
    })
}

pub(super) fn sender_matches_focus(
    sender: &str,
    focus: &BTreeSet<AgentKey>,
    identities: &HashMap<AgentKey, Identity>,
    channel_filter: Option<&str>,
) -> bool {
    let matches = matching_handle_keys(sender, channel_filter, identities);
    matches.len() == 1 && focus.contains(matches[0])
}

pub(super) fn matching_handle_keys<'a>(
    handle: &str,
    channel_filter: Option<&str>,
    identities: &'a HashMap<AgentKey, Identity>,
) -> Vec<&'a AgentKey> {
    let (base, channel) = split_rendered_handle(handle);
    let mut matches: Vec<_> = identities
        .iter()
        .filter_map(|(key, identity)| {
            (identity.base_handle == base
                && channel_matches(identity.channel.as_deref(), channel.or(channel_filter)))
            .then_some(key)
        })
        .collect();
    matches.sort();
    matches
}

pub(super) fn split_rendered_handle(handle: &str) -> (&str, Option<&str>) {
    handle
        .split_once('#')
        .map_or((handle, None), |(base, channel)| (base, Some(channel)))
}

pub(super) fn dedup_asks(entries: Vec<ChatEntry>) -> Vec<ChatEntry> {
    let mut latest_asks: HashMap<RequestId, (usize, jiff::Timestamp)> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.entry == ChatKind::Ask
            && let Some(request_id) = entry.request_id.as_ref()
        {
            latest_asks
                .entry(request_id.clone())
                .and_modify(|prior| {
                    if entry.at >= prior.1 {
                        *prior = (index, entry.at);
                    }
                })
                .or_insert((index, entry.at));
        }
    }
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if entry.entry == ChatKind::Ask
                && let Some(request_id) = entry.request_id.as_ref()
            {
                return (latest_asks.get(request_id).map(|(latest, _)| *latest) == Some(index))
                    .then_some(entry);
            }
            Some(entry)
        })
        .collect()
}

pub(super) fn build_identities(entries: &[ChatEntry]) -> HashMap<AgentKey, Identity> {
    let mut identities = HashMap::new();
    for entry in entries {
        let candidate = Identity {
            base_handle: rimz::message::identity_handle(
                &entry.kind,
                entry.profile.as_deref(),
                entry.role.as_deref(),
            ),
            channel: entry.channel.clone(),
            name: entry.name.clone(),
            profile: entry.profile.clone(),
            role: entry.role.clone(),
            last_at: entry.at,
            rich: entry.role.is_some() || entry.name.is_some() || entry.profile.is_some(),
        };
        identities
            .entry(entry_key(entry))
            .and_modify(|existing: &mut Identity| {
                existing.last_at = existing.last_at.max(candidate.last_at);
                if existing.channel.is_none() {
                    existing.channel = candidate.channel.clone();
                }
                if candidate.rich && !existing.rich {
                    existing.base_handle = candidate.base_handle.clone();
                    existing.name = candidate.name.clone();
                    existing.profile = candidate.profile.clone();
                    existing.role = candidate.role.clone();
                    existing.rich = true;
                }
            })
            .or_insert(candidate);
    }
    identities
}

pub(super) fn resolve_scope(
    target: Option<&str>,
    worktree: Option<&str>,
    current: Option<&str>,
    identities: &HashMap<AgentKey, Identity>,
    live_root_keys: &BTreeSet<AgentKey>,
) -> Result<Scope> {
    match target {
        None => {
            let channel = worktree.or(current).map(ToOwned::to_owned);
            let include_channel = channel.is_none();
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus: None,
                focus_keys: None,
                include_channel,
            })
        }
        Some(raw) if raw.starts_with('#') => {
            let channel = raw.trim_start_matches('#');
            if channel.is_empty() {
                bail!("channel target must be `#<name>`");
            }
            reconcile_transcript_channel(raw, Some(channel), worktree, None)?;
            Ok(single_channel_scope(channel.to_owned()))
        }
        Some(raw) if raw == "@all" || raw.starts_with("@all#") => {
            let (_, inline) = parse_transcript_target(raw)?;
            let channel = reconcile_transcript_channel(raw, inline.as_deref(), worktree, None)?;
            let include_channel = channel.is_none();
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus: None,
                focus_keys: None,
                include_channel,
            })
        }
        Some(raw) => {
            let (selector, inline) = parse_transcript_target(raw)?;
            let exact_session = exact_session_selector(&selector, identities);
            let requested_channel =
                reconcile_transcript_channel(raw, inline.as_deref(), worktree, current)?;
            let resolution_channel = (!exact_session)
                .then_some(requested_channel.as_deref())
                .flatten();
            let matches = matching_identities(&selector, resolution_channel, identities);
            let Some((key, identity)) = select_identity_match(&matches, live_root_keys) else {
                bail!("no agent matches target `{raw}` in the transcript log");
            };
            let channel = if exact_session {
                identity.channel.clone()
            } else {
                requested_channel.or_else(|| identity.channel.clone())
            };
            let include_channel = channel.is_none();
            let focus = Some(render_handle(
                &identity.base_handle,
                identity.channel.as_deref(),
                include_channel,
            ));
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus,
                focus_keys: Some(BTreeSet::from([(*key).clone()])),
                include_channel,
            })
        }
    }
}

pub(super) fn single_channel_scope(channel: String) -> Scope {
    Scope {
        channel: Some(channel.clone()),
        channel_filter: Some(channel),
        focus: None,
        focus_keys: None,
        include_channel: false,
    }
}

pub(super) fn parse_transcript_target(raw: &str) -> Result<(String, Option<String>)> {
    if matches!(raw.split_once('#'), Some((_, ""))) {
        bail!("channel suffix in target `{raw}` must name a channel");
    }
    match rimz::harness::target::parse_selector(raw) {
        Ok(parsed) => Ok(parsed),
        Err(rimz::TargetErr::NoMatch { .. } | rimz::TargetErr::InvalidPaneId(_)) => {
            Ok(split_transcript_target(raw))
        }
        Err(err) => Err(err.into()),
    }
}

pub(super) fn split_transcript_target(raw: &str) -> (String, Option<String>) {
    raw.split_once('#').map_or_else(
        || (raw.to_owned(), None),
        |(selector, channel)| (selector.to_owned(), Some(channel.to_owned())),
    )
}

pub(super) fn reconcile_transcript_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    fallback: Option<&str>,
) -> Result<Option<String>> {
    match rimz::harness::target::reconcile_channel(raw, inline, flag, fallback) {
        Ok(channel) => Ok(channel),
        Err(rimz::TargetErr::ChannelMismatch {
            target,
            channel,
            flag,
        }) => bail!("target `{target}` names channel `#{channel}` but --worktree names `{flag}`"),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn matching_identities<'a>(
    selector: &str,
    channel: Option<&str>,
    identities: &'a HashMap<AgentKey, Identity>,
) -> Vec<(&'a AgentKey, &'a Identity)> {
    let selector = selector.strip_prefix('@').unwrap_or(selector);
    let mut exact: Vec<_> = identities
        .iter()
        .filter(|(key, _)| key.1.as_str() == selector)
        .collect();
    if !exact.is_empty() {
        exact.sort_by(|left, right| {
            candidate_label(left.0, left.1).cmp(&candidate_label(right.0, right.1))
        });
        return exact;
    }
    let wanted_handle = format!("@{selector}");
    let mut matches: Vec<_> = identities
        .iter()
        .filter(|(key, identity)| {
            (identity.base_handle == wanted_handle
                || key.0.as_str() == selector
                || identity.name.as_deref() == Some(selector)
                || identity.profile.as_deref() == Some(selector)
                || identity.role.as_deref() == Some(selector)
                || key.1.as_str() == selector
                || key.1.as_str().starts_with(selector))
                && channel_matches(identity.channel.as_deref(), channel)
        })
        .collect();
    matches.sort_by(|left, right| {
        candidate_label(left.0, left.1).cmp(&candidate_label(right.0, right.1))
    });
    matches
}

pub(super) fn exact_session_selector(
    selector: &str,
    identities: &HashMap<AgentKey, Identity>,
) -> bool {
    let selector = selector.strip_prefix('@').unwrap_or(selector);
    identities.keys().any(|key| key.1.as_str() == selector)
}

pub(super) fn select_identity_match<'a>(
    matches: &[(&'a AgentKey, &'a Identity)],
    live_root_keys: &BTreeSet<AgentKey>,
) -> Option<(&'a AgentKey, &'a Identity)> {
    let pool: Vec<_> = if matches.iter().any(|(key, _)| live_root_keys.contains(*key)) {
        matches
            .iter()
            .copied()
            .filter(|(key, _)| live_root_keys.contains(*key))
            .collect()
    } else {
        matches.to_vec()
    };
    pool.into_iter().max_by(|left, right| {
        left.1
            .last_at
            .cmp(&right.1.last_at)
            .then_with(|| left.0.1.as_str().cmp(right.0.1.as_str()))
    })
}

pub(super) fn candidate_label(key: &AgentKey, identity: &Identity) -> String {
    let handle = render_handle(&identity.base_handle, identity.channel.as_deref(), true);
    format!("{handle} ({})", key.1.as_str())
}

pub(super) fn channel_matches(entry_channel: Option<&str>, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| entry_channel == Some(filter))
}

pub(super) fn entry_key(entry: &ChatEntry) -> AgentKey {
    (entry.kind.clone(), entry.agent_id.clone())
}

pub(super) fn keep_last<T>(items: &mut Vec<T>, last: Option<usize>) {
    let Some(last) = last else {
        return;
    };
    let drop = items.len().saturating_sub(last);
    if drop > 0 {
        items.drain(..drop);
    }
}
