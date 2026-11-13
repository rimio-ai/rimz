//! Target selection shared by the agent action commands: one source of targets
//! per invocation (positional references, `--filter`, or a positional `-` that
//! reads addresses from stdin), the selector's rows over a pane-carrying
//! snapshot, the consent rule for more than one selected agent, and
//! `--filter help`.

use std::io::{BufRead, Write as _};

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::address::LaneSelector;
use rimz::address::selector::{self, LaneScope, Query};
use rimz::agents::{AgentState, ParkDemotion};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::snapshot::SidebarSnapshot;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

use super::{Ctx, UsageError, render};

/// The positional reference that reads target addresses from stdin.
pub(crate) const STDIN: &str = "-";

/// An action command's target arguments, as clap parsed them.
pub(crate) struct TargetFlags<'a> {
    /// The positional references, `-` included.
    pub(crate) references: &'a [String],
    pub(crate) filters: &'a [String],
    /// Consent to act on every agent `--filter` or stdin selects.
    pub(crate) all: bool,
}

/// What an action command acts on.
pub(crate) enum Targets {
    /// The positional references, none included, for the command's own resolver.
    References(Vec<String>),
    /// The agents `--filter` or stdin selected, in order, with any consent the
    /// count needs given. Never empty.
    Selected(Vec<Selected>),
    /// Nothing to act on, and the command exits 0: `--filter help` printed its
    /// table, or nothing was selected and a stderr line said so.
    Done,
}

/// One selected agent: the address to resolve at act time and, for a filter
/// row, the agent that address named at selection, so a re-read can tell
/// that agent apart from one that has since taken the address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) address: String,
    pub(crate) agent: Option<(AgentKind, AgentSessionId)>,
}

#[derive(Debug, PartialEq, Eq)]
enum Source {
    References,
    Filter,
    Stdin,
}

/// Resolve `flags` to the command's targets. `verb` names the command in the
/// consent refusal.
pub(crate) fn select_targets(ctx: &Ctx, verb: &str, flags: TargetFlags<'_>) -> Result<Targets> {
    if is_filter_help(flags.filters) {
        render::filter_help(&mut render::out(), FILTER_DEFAULTS)?;
        return Ok(Targets::Done);
    }
    let source = source(&flags)?;
    let (selected, miss) = match source {
        Source::References => return Ok(Targets::References(flags.references.to_vec())),
        Source::Filter => {
            let query =
                Query::parse(flags.filters).map_err(|err| UsageError::new(err.to_string()))?;
            let scope = ctx
                .channel()
                .map_or(LaneScope::Room, |lane| LaneScope::Current(lane.as_str()));
            let rows = filter_rows(ctx, &query, scope)?;
            (rows, no_match_line(scope.lane(&query)))
        }
        Source::Stdin => {
            let lines = read_addresses(std::io::stdin().lock())?;
            let addresses = lines
                .into_iter()
                .map(|address| Selected {
                    address,
                    agent: None,
                })
                .collect();
            (addresses, "no agent address on stdin".to_owned())
        }
    };
    if selected.is_empty() {
        writeln!(render::err(), "{miss}")?;
        return Ok(Targets::Done);
    }
    let argv = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    require_consent(&selected, flags.all, verb, &argv)?;
    Ok(Targets::Selected(selected))
}

/// Whether the filters ask for the field table instead of a selection.
pub(crate) fn is_filter_help(filters: &[String]) -> bool {
    filters.iter().any(|filter| filter.trim() == "help")
}

/// The one line a filter that matches nothing prints, by the lane it read.
pub(crate) fn no_match_line(lane: Option<&str>) -> String {
    match lane {
        Some(lane) => format!("no agent in '#{lane}' matches the filter"),
        None => "no agent in this room matches the filter".to_owned(),
    }
}

/// The parked-agent demotion the selector's status reads.
pub(crate) fn park_demotion(
    store: &Store,
    state: &StatePaths,
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> ParkDemotion {
    rimz::harness::park_demotion(
        Some(store),
        state,
        runtime,
        &super::machine_config(),
        &snapshot.agents,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
        now,
    )
}

/// The Audit projection's agents as a snapshot the selector's closed rows read.
pub(crate) fn closed_snapshot(
    workspace: &ResolvedWorkspace,
    runtime: &RuntimePaths,
    audit: Vec<AgentState>,
    now: Timestamp,
) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(workspace.workspace_id.clone(), audit, now)
        .with_agent_context(rimz::store::agent_context::read_all(runtime))
}

/// The rows `query` admits in `scope`, read as `agents list --filter` reads
/// them: over the published snapshot, which carries the pane bindings a row's
/// liveness reads.
fn filter_rows(ctx: &Ctx, query: &Query, scope: LaneScope<&str>) -> Result<Vec<Selected>> {
    let snapshot = ctx.published_snapshot()?;
    let now = Timestamp::now();
    let demotion = park_demotion(&ctx.store, ctx.store.paths(), ctx.runtime(), &snapshot, now);
    let audit = if query.reads_closed() {
        let agents = ctx
            .store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit agent rollup")?
            .agents;
        Some(closed_snapshot(&ctx.workspace, ctx.runtime(), agents, now))
    } else {
        None
    };
    let lane = scope.map(|lane| LaneSelector::parse(Some(lane)));
    let reader = ctx.address_context();
    let rows = selector::rows_in_scope(
        &snapshot,
        audit.as_ref(),
        lane.as_ref(),
        query,
        &demotion,
        &reader,
        now,
    );
    Ok(rows
        .iter()
        .map(|row| Selected {
            address: row.handle().to_owned(),
            agent: Some((row.agent().kind.clone(), row.agent().agent_id.clone())),
        })
        .collect())
}

fn source(flags: &TargetFlags<'_>) -> Result<Source, UsageError> {
    let stdin = flags.references.iter().any(|reference| reference == STDIN);
    match (flags.references, flags.filters.is_empty(), stdin) {
        ([], false, _) => Ok(Source::Filter),
        (_, true, false) => Ok(Source::References),
        ([_], true, true) => Ok(Source::Stdin),
        _ => Err(UsageError::new(format!(
            "give targets one way: references, `--filter`, or a lone `{STDIN}` for addresses \
             on stdin"
        ))),
    }
}

/// The non-blank lines of `input`, trimmed, in order.
fn read_addresses(input: impl BufRead) -> Result<Vec<String>> {
    let mut addresses = Vec::new();
    for line in input.lines() {
        let line = line.context("reading agent addresses from stdin")?;
        let address = line.trim();
        if !address.is_empty() {
            addresses.push(address.to_owned());
        }
    }
    Ok(addresses)
}

/// Refuse more than one selected agent without `--all`, listing them and
/// the invocation with `--all` added.
fn require_consent(
    selected: &[Selected],
    all: bool,
    verb: &str,
    argv: &[String],
) -> Result<(), UsageError> {
    if all || selected.len() < 2 {
        return Ok(());
    }
    let mut retry = argv.to_vec();
    let at = retry
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(retry.len());
    retry.insert(at, "--all".to_owned());
    let retry =
        shlex::try_join(retry.iter().map(String::as_str)).unwrap_or_else(|_| retry.join(" "));
    let mut message = format!(
        "{} agents selected; {verb} acts on more than one only with --all:",
        selected.len()
    );
    for agent in selected {
        message.push('\n');
        message.push_str(&agent.address);
    }
    message.push_str(&format!("\nor {verb} every match: {retry}"));
    Err(UsageError::new(message))
}

/// What `agents list` and an action command's `--filter` select until a term
/// names the field.
pub(crate) const FILTER_DEFAULTS: &str =
    "naming lane, live, or parent replaces its default: the current lane, live=yes, parent=none";

#[cfg(test)]
mod tests {
    use super::*;

    fn flags<'a>(references: &'a [String], filters: &'a [String]) -> TargetFlags<'a> {
        TargetFlags {
            references,
            filters,
            all: false,
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn selected(addresses: &[&str]) -> Vec<Selected> {
        addresses
            .iter()
            .map(|address| Selected {
                address: (*address).to_owned(),
                agent: None,
            })
            .collect()
    }

    #[test]
    fn one_source_per_invocation() {
        let none = strings(&[]);
        let refs = strings(&["@a", "@b"]);
        let stdin = strings(&["-"]);
        let filter = strings(&["status=idle"]);
        assert_eq!(source(&flags(&none, &none)).unwrap(), Source::References);
        assert_eq!(source(&flags(&refs, &none)).unwrap(), Source::References);
        assert_eq!(source(&flags(&none, &filter)).unwrap(), Source::Filter);
        assert_eq!(source(&flags(&stdin, &none)).unwrap(), Source::Stdin);
        for (references, filters) in [
            (strings(&["@a"]), filter.clone()),
            (stdin.clone(), filter.clone()),
            (strings(&["-", "@a"]), none.clone()),
            (strings(&["-", "-"]), none.clone()),
        ] {
            let err = source(&flags(&references, &filters)).unwrap_err();
            assert!(
                err.to_string().contains("--filter"),
                "{references:?} {filters:?}: {err}"
            );
        }
    }

    #[test]
    fn stdin_addresses_skip_blank_lines_and_keep_their_order() {
        let input = "@b\n\n  @a#feat  \n\t\n@c";
        assert_eq!(
            read_addresses(input.as_bytes()).unwrap(),
            ["@b", "@a#feat", "@c"]
        );
        assert!(read_addresses("\n \n".as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn more_than_one_selected_agent_needs_all() {
        let argv = strings(&[
            "rimz",
            "agents",
            "restart",
            "--filter",
            "parent=* error=rate-limit",
        ]);
        let two = selected(&["@otter", "@fox#feat"]);
        let err = require_consent(&two, false, "restart", &argv).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("2 agents selected"), "{message}");
        let lines = message.lines().collect::<Vec<_>>();
        assert!(lines.contains(&"@otter"), "{message}");
        assert!(lines.contains(&"@fox#feat"), "{message}");
        assert!(
            message.contains(
                "or restart every match: rimz agents restart --filter 'parent=* error=rate-limit' --all"
            ),
            "{message}"
        );
        require_consent(&two, true, "restart", &argv).unwrap();
        require_consent(&two[..1], false, "restart", &argv).unwrap();
        let argv = strings(&["rimz", "agents", "stop", "-", "--", "x"]);
        let message = require_consent(&two, false, "stop", &argv)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("or stop every match: rimz agents stop - --all -- x"),
            "{message}"
        );
    }
}
