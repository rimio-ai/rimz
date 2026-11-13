//! Unit tests for the agent selector: grammar, field table, match, defaults.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use super::*;
use crate::address::ChannelOrigin;
use crate::agents::{AgentContext, AgentTurnError, BudgetPark, BudgetScope, BudgetWindow};
use crate::ids::{AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;
use crate::store::snapshot::PaneAgent;

fn now() -> Timestamp {
    Timestamp::from_second(10_000).unwrap()
}

fn reader() -> AddressContext {
    AddressContext {
        channel: None,
        origin: ChannelOrigin::Stamped,
    }
}

fn agent(id: &str, pane: &str) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent("codex", id, None);
    agent.name = Some(id.to_owned());
    agent.kind_ordinal = None;
    agent.status = AgentStatus::Idle;
    agent.last_activity = now();
    agent.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, pane)));
    agent.worktree_path = Some("/repo/feat".to_owned());
    agent.worktree_branch = Some("feat".to_owned());
    crate::testkit::set_own_lane(&mut agent);
    agent
}

fn child(id: &str, parent: &str, pane: &str) -> AgentState {
    let mut child = agent(id, pane);
    child.parent_agent_id = Some(parent.into());
    child.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    child.launch_depth = Some(1);
    child
}

fn bound(agent: &AgentState) -> PaneAgent {
    PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: agent.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(agent.agent_id.clone()),
        pane_id: agent.pane.as_ref().unwrap().pane_id.clone(),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }
}

fn snapshot(agents: Vec<AgentState>, live: &[&str]) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        agents,
        now(),
    );
    snapshot.agent_panes = snapshot
        .agents
        .iter()
        .filter(|agent| live.contains(&agent.agent_id.as_str()))
        .map(bound)
        .collect::<Vec<_>>()
        .into();
    snapshot
}

fn rate_limited(agent: &mut AgentState) {
    agent.status = AgentStatus::Running;
    agent.last_activity = now() - std::time::Duration::from_secs(1);
    let mut context = AgentContext::new("codex", now());
    context.turn_error = Some(AgentTurnError {
        ended_turn: false,
        class: TurnErrorClass::PausedRateLimit,
        at: now(),
        label: Some("Usage limit reached".to_owned()),
    });
    agent.context = Some(context);
}

fn query(filters: &[&str]) -> Query {
    Query::parse(filters).unwrap_or_else(|err| panic!("{filters:?}: {err}"))
}

fn error(filters: &[&str]) -> String {
    match Query::parse(filters) {
        Ok(query) => panic!("{filters:?} parsed: {query:?}"),
        Err(err) => err.to_string(),
    }
}

fn selected(snapshot: &SidebarSnapshot, filters: &[&str]) -> Vec<String> {
    let query = query(filters);
    live_rows(snapshot, &ParkDemotion::default(), &reader(), now())
        .iter()
        .filter(|row| query.admits(row, &reader()))
        .map(|row| row.agent().agent_id.to_string())
        .collect()
}

#[test]
fn every_production_parses() {
    for filters in [
        &["status=idle"][..],
        &["status!=idle"],
        &["ctx>50%"],
        &["ctx>=50"],
        &["age<30m"],
        &["tokens<=2M"],
        &["cost>$1.50"],
        &["status=idle,running kind=codex"],
        &["  status=idle   kind=co*  "],
        &["name=\"two words\""],
        &["name=\"a,b\",plain"],
        &["status=idle", "kind=codex"],
        &["error=none"],
        &["parent=*,none"],
        &["parent=@coder"],
        &["parent=coder"],
        &["parent=@coder#feat"],
        &["parent=co*"],
        &["lane=#feat"],
        &["lane=feat"],
        &["lane=fe*"],
        &["team=forge"],
        &["team=@forge"],
        &["team=forge#feat"],
        &["team=@forge#feat"],
        &["STATUS=IDLE"],
        &["live=YES"],
    ] {
        query(filters);
    }
    let parsed = query(&["status=idle,running kind=codex", "ctx>50%"]);
    assert_eq!(parsed.terms.len(), 3, "{parsed:?}");
    assert_eq!(
        parsed.terms[0].values,
        [
            Value::Status(AgentStatus::Idle),
            Value::Status(AgentStatus::Running)
        ]
    );
    assert_eq!(parsed.terms[2].op, Op::Gt);
    assert_eq!(parsed.terms[2].values, [Value::Number(50.0)]);
    let quoted = query(&["name=\"a,b\",plain"]);
    assert_eq!(
        quoted.terms[0].values,
        [Value::Text("a,b".into()), Value::Text("plain".into())]
    );
    assert_eq!(query(&["tokens<=2M"]).terms[0].values, [Value::Number(2e6)]);
    assert_eq!(
        query(&["tokens<100k"]).terms[0].values,
        [Value::Number(1e5)]
    );
    assert_eq!(query(&["age<30m"]).terms[0].values, [Value::Number(1800.0)]);
    assert_eq!(query(&["cost>$1.50"]).terms[0].values, [Value::Number(1.5)]);
    assert_eq!(query(&["cost>2"]).terms[0].values, [Value::Number(2.0)]);
}

#[test]
fn strict_errors_name_the_offending_term() {
    for (filters, expected) in [
        (&[""][..], "empty"),
        (&["   "], "empty"),
        (&["status"], "`status` has no operator"),
        (&["name=\"open"], "unterminated quote"),
        (&["status=idle,"], "`status=idle,` has an empty value"),
        (&["status="], "`status=` has an empty value"),
        (&["=idle"], "`=idle` has no field"),
        (
            &["stauts=idle"],
            "unknown field `stauts`; did you mean `status`?",
        ),
        (
            &["status=sleepy"],
            "`sleepy` is not a status value; use one of running, waiting, idle, success, failed, paused, sleeping",
        ),
        (
            &["error=rate_limit"],
            "use one of rate-limit, spend-limit, overloaded, not-entitled, failed, unknown, none",
        ),
        (&["live=maybe"], "use one of yes, no"),
        (&["name>a"], "`name>a`: name is not ordered"),
        (&["lane<feat"], "`lane<feat`: lane is not ordered"),
        (&["status>=idle"], "`status>=idle`: status is not ordered"),
        (&["age>soon"], "`age>soon`: `soon` is not a duration"),
        (&["ctx>lots"], "`ctx>lots`: `lots` is not a percent"),
        (&["ctx>120%"], "`ctx>120%`: `120%` is not a percent"),
        (&["tokens<1q"], "`tokens<1q`: `1q` is not a size"),
        (&["cost>$x"], "`cost>$x`: `$x` is not a dollar amount"),
        (&["ctx>1,2"], "`ctx>1,2`: a comparison takes one value"),
        (&["ctx>none"], "`ctx>none`: a comparison takes one value"),
        (&["status=idle", "kind"], "`kind` has no operator"),
        (&["parent=@"], "`parent=@`: `@` is not an address"),
        (&["parent=@@x"], "`parent=@@x`: `@@x` is not an address"),
        (&["lane=#"], "`lane=#`: `#` is not a lane"),
        (&["team=@"], "`team=@`: `@` is not a team"),
        (&["team=@#feat"], "`team=@#feat`: `@#feat` is not a team"),
        (&["team=forge#"], "`team=forge#`: `forge#` is not a team"),
    ] {
        let message = error(filters);
        assert!(
            message.contains(expected),
            "{filters:?}: {message:?} lacks {expected:?}"
        );
    }
}

#[test]
fn field_table_matches_the_row_keys() {
    let snapshot = snapshot(vec![agent("root", "%1")], &["root"]);
    let rows = live_rows(&snapshot, &ParkDemotion::default(), &reader(), now());
    let serde_json::Value::Object(fields) = serde_json::to_value(&rows[0]).unwrap() else {
        panic!("row serializes as an object");
    };
    let keys = fields.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let names = Field::ALL
        .map(Field::name)
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(keys, names);
    for field in Field::ALL {
        assert_eq!(Field::from_name(field.name()), Some(field));
    }
    assert_eq!(
        Field::Status.values().len(),
        STATUSES.len(),
        "every status is a value"
    );
    assert_eq!(Field::Error.values().last(), Some(&"none"));
}

#[test]
fn none_is_an_ordinary_value_and_comparisons_skip_it() {
    let mut limited = agent("limited", "%1");
    rate_limited(&mut limited);
    let mut filled = agent("filled", "%2");
    filled.usage.context_pct = Some(80);
    let plain = agent("plain", "%3");
    let snapshot = snapshot(
        vec![limited, filled, plain],
        &["limited", "filled", "plain"],
    );
    let ids = |filters: &[&str]| selected(&snapshot, filters);
    assert_eq!(ids(&["error=none"]), ["filled", "plain"]);
    assert_eq!(ids(&["error!=rate-limit"]), ["filled", "plain"]);
    assert_eq!(ids(&["error!=none"]), ["limited"]);
    assert_eq!(ids(&["error=*"]), ["limited"]);
    assert_eq!(ids(&["ctx=none"]).len(), 2);
    assert_eq!(ids(&["ctx=*"]), ["filled"]);
    assert_eq!(ids(&["ctx>50%"]), ["filled"]);
    assert_eq!(ids(&["ctx>=80%"]), ["filled"]);
    assert!(ids(&["ctx<50%"]).is_empty());
    assert!(ids(&["ctx<=79"]).is_empty());
    assert_eq!(ids(&["ctx<=80"]), ["filled"]);
}

#[test]
fn spaces_and_flags_and_while_commas_or_and_exclude() {
    let mut named = agent("alpha", "%1");
    named.profile = Some("reviewer".into());
    let mut running = agent("beta", "%2");
    running.status = AgentStatus::Running;
    let gamma = agent("gamma", "%3");
    let snapshot = snapshot(vec![named, running, gamma], &["alpha", "beta", "gamma"]);
    let ids = |filters: &[&str]| selected(&snapshot, filters);
    assert_eq!(ids(&["name=alpha,beta"]), ["alpha", "beta"]);
    assert_eq!(ids(&["name!=alpha,beta"]), ["gamma"]);
    assert_eq!(ids(&["name=alpha,beta status=idle"]), ["alpha"]);
    assert_eq!(ids(&["name=alpha,beta", "status=idle"]), ["alpha"]);
    assert_eq!(ids(&["name=*a"]), ["alpha", "beta", "gamma"]);
    assert_eq!(ids(&["name=g*"]), ["gamma"]);
    assert_eq!(ids(&["profile=none"]), ["beta", "gamma"]);
    assert_eq!(ids(&["status=RUNNING"]), ["beta"]);
    assert_eq!(ids(&["account=default kind=codex"]).len(), 3);
    assert_eq!(ids(&["handle=@reviewer"]), ["alpha"], "the printed address");
    assert_eq!(ids(&["handle=beta"]), ["beta"]);
    let handles = live_rows(&snapshot, &ParkDemotion::default(), &reader(), now())
        .iter()
        .map(|row| row.handle().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(handles, ["@reviewer", "@beta", "@gamma"]);
    assert_eq!(ids(&["lane=feat"]).len(), 3);
    assert_eq!(ids(&["lane=#feat"]).len(), 3);
    assert!(ids(&["lane=other"]).is_empty());
}

#[test]
fn defaults_step_aside_only_for_the_field_named() {
    let root = agent("root", "%1");
    let live_child = child("kid", "root", "%2");
    let mut ended_child = child("done", "root", "%3");
    ended_child.ended_at = Some(now());
    let snapshot = snapshot(
        vec![root, live_child, ended_child],
        &["root", "kid", "done"],
    );
    let ids = |filters: &[&str]| selected(&snapshot, filters);
    assert_eq!(ids(&[]), ["root"]);
    assert_eq!(ids(&["parent=*"]), ["kid"]);
    assert_eq!(ids(&["parent=*,none"]), ["root", "kid"]);
    assert_eq!(ids(&["parent=@root"]), ["kid"]);
    assert_eq!(ids(&["parent=*", "live=no"]), ["done"]);
    assert_eq!(ids(&["parent=*", "live=*"]), ["kid", "done"]);
    assert!(
        ids(&["live=no"]).is_empty(),
        "a closed child is still a child"
    );
    assert!(query(&["live=no"]).reads_closed());
    assert!(query(&["live=yes,no"]).reads_closed());
    assert!(query(&["live!=yes"]).reads_closed());
    assert!(!query(&["live=yes"]).reads_closed());
    assert!(!query(&["status=idle"]).reads_closed());
    assert!(query(&["lane=*"]).names(Field::Lane));
    assert!(!query(&["status=idle"]).names(Field::Lane));
}

#[test]
fn matches_applies_the_terms_without_the_default_set() {
    let root = agent("root", "%1");
    let live_child = child("kid", "root", "%2");
    let mut ended_child = child("done", "root", "%3");
    ended_child.ended_at = Some(now());
    let snapshot = snapshot(
        vec![root, live_child, ended_child],
        &["root", "kid", "done"],
    );
    let matched = |filters: &[&str]| {
        let query = query(filters);
        live_rows(&snapshot, &ParkDemotion::default(), &reader(), now())
            .iter()
            .filter(|row| query.matches(row, &reader()))
            .map(|row| row.agent().agent_id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(matched(&[]), ["root", "kid", "done"]);
    assert_eq!(matched(&["live=no"]), ["done"]);
    assert_eq!(matched(&["parent=@root"]), ["kid", "done"]);
    assert_eq!(matched(&["parent=none"]), ["root"]);
    assert_eq!(
        selected(&snapshot, &[]),
        ["root"],
        "admits keeps the default set"
    );
}

#[test]
fn a_rate_limited_child_keeps_its_error_after_the_park_is_spent() {
    let root = agent("root", "%1");
    let mut parked = child("parked", "root", "%2");
    rate_limited(&mut parked);
    let snapshot = snapshot(vec![root, parked], &["root", "parked"]);
    let spent = ParkDemotion::new(
        &BTreeMap::new(),
        BTreeSet::from([(
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("parked"),
        )]),
        now(),
    );
    for (demotion, status) in [
        (&ParkDemotion::default(), AgentStatus::Paused),
        (&spent, AgentStatus::Failed),
    ] {
        let rows = live_rows(&snapshot, demotion, &reader(), now());
        let row = rows
            .iter()
            .find(|row| row.agent().agent_id.as_str() == "parked")
            .unwrap();
        assert_eq!(row.status, status);
        assert_eq!(row.error, Some(ErrorClass::RateLimit));
        assert!(query(&["parent=* error=rate-limit"]).admits(row, &reader()));
        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["error"], "rate-limit");
        assert_eq!(json["parent"], "@root");
    }
}

#[test]
fn a_budget_park_is_paused_without_an_error() {
    let mut parked = agent("budget", "%1");
    parked.budget_park = Some(BudgetPark {
        cap_usd: 1.0,
        spend_usd: 1.2,
        window: BudgetWindow::Session,
        at: now(),
        scope: BudgetScope::default(),
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    let snapshot = snapshot(vec![parked], &["budget"]);
    let rows = live_rows(&snapshot, &ParkDemotion::default(), &reader(), now());
    assert_eq!(rows[0].status, AgentStatus::Paused);
    assert_eq!(rows[0].error, None);
    assert_eq!(
        selected(&snapshot, &["status=paused error=none"]),
        ["budget"]
    );
}

#[test]
fn every_printed_address_resolves_back_to_its_row() {
    let live = snapshot(
        vec![
            agent("otter", "%1"),
            child("fox", "otter", "%2"),
            agent("owl", "%3"),
            agent("stray", "%7"),
        ],
        &["otter", "fox", "owl"],
    );
    let mut closed_otter = agent("otter-old", "%9");
    closed_otter.name = Some("otter".into());
    closed_otter.ended_at = Some(now());
    let mut closed_wren = agent("wren", "%8");
    closed_wren.ended_at = Some(now());
    let mut audit_agents = live.agents.to_vec();
    audit_agents.extend([closed_otter, closed_wren]);
    let audit = snapshot(audit_agents, &[]);
    let demotion = ParkDemotion::default();
    let listed = live_rows(&live, &demotion, &reader(), now());
    let closed = closed_rows(&audit, &live, &listed, &demotion, &reader(), now());
    let closed_ids = closed
        .iter()
        .map(|row| row.agent().agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(closed_ids, ["otter-old", "wren"]);
    assert!(closed.iter().all(|row| !row.live));
    for row in &listed {
        let matches = resolve_many(&live, row.handle(), None, &reader()).unwrap();
        assert_eq!(matches.as_slice(), [row.agent()], "{}", row.handle());
    }
    for row in &closed {
        assert!(
            resolve_many(&live, row.handle(), None, &reader()).is_err(),
            "{} also names a live agent",
            row.handle()
        );
        let matches = resolve_many(&audit, row.handle(), None, &reader()).unwrap();
        assert_eq!(matches.len(), 1, "{}", row.handle());
        assert_eq!(
            matches[0].agent_id,
            row.agent().agent_id,
            "{}",
            row.handle()
        );
    }
}

fn nested(id: &str, status: AgentStatus) -> SidebarSubAgent {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "kind": "codex",
        "name": "codex",
        "status": status,
        "last_activity": now(),
    }))
    .unwrap()
}

#[test]
fn a_child_stamped_with_a_launch_id_finds_its_parent_and_nested_card() {
    let mut root = agent("root", "%1");
    root.launch_id = Some("launch-root".into());
    root.prior_launch_ids = vec!["launch-old".into()];
    let mut kid = child("kid", "launch-root", "%2");
    kid.status = AgentStatus::Running;
    let mut old = child("old", "launch-old", "%3");
    old.status = AgentStatus::Running;
    let mut live = snapshot(vec![root, kid, old], &["root", "kid", "old"]);
    let path = std::path::Path::new("/repo/feat");
    let mut card_row = crate::sidebar::test_support::activity_row(true, None, now(), path);
    card_row.id = "root".into();
    let crate::store::snapshot::RowCard::Agent(card) = &mut card_row.card else {
        unreachable!("activity_row(true, ..) builds an agent card");
    };
    std::sync::Arc::make_mut(card).sub_agents = vec![
        nested("kid", AgentStatus::Failed),
        nested("old", AgentStatus::Failed),
    ];
    std::sync::Arc::make_mut(&mut live.worktree_groups).push(
        crate::sidebar::test_support::worktree_group(path, vec![card_row]),
    );
    let rows = live_rows(&live, &ParkDemotion::default(), &reader(), now());
    for id in ["kid", "old"] {
        let row = rows
            .iter()
            .find(|row| row.agent().agent_id.as_str() == id)
            .unwrap();
        assert_eq!(row.parent.as_deref(), Some("@root"), "{id}");
        assert_eq!(
            row.status,
            AgentStatus::Failed,
            "{id}: the nested card's status"
        );
        assert_eq!(
            child_card(&live, row.agent()).map(|card| card.status),
            Some(AgentStatus::Failed),
            "{id}"
        );
    }
    assert_eq!(selected(&live, &["parent=@root"]), ["kid", "old"]);
    assert_eq!(selected(&live, &["parent=*"]), ["kid", "old"]);
}

#[test]
fn a_hyphenated_field_name_is_an_unknown_field() {
    assert_eq!(
        error(&["foo-bar=x"]),
        "filter term `foo-bar=x`: unknown field `foo-bar`; see `--filter help`"
    );
}

#[test]
fn rows_in_scope_reads_the_current_lane_until_the_query_names_one() {
    let feat = agent("feat", "%1");
    let mut docs = agent("docs", "%2");
    docs.worktree_path = Some("/repo/docs".to_owned());
    docs.worktree_branch = Some("docs".to_owned());
    crate::testkit::set_own_lane(&mut docs);
    let live = snapshot(vec![feat, docs], &["feat", "docs"]);
    let mut closed = agent("gone", "%3");
    closed.ended_at = Some(now());
    let mut audit_agents = live.agents.to_vec();
    audit_agents.push(closed);
    let audit = snapshot(audit_agents, &[]);
    let feat_lane = LaneSelector::parse(Some("feat"));
    let ids =
        |audit: Option<&SidebarSnapshot>, scope: LaneScope<&LaneSelector>, filters: &[&str]| {
            rows_in_scope(
                &live,
                audit,
                scope,
                &query(filters),
                &ParkDemotion::default(),
                &reader(),
                now(),
            )
            .iter()
            .map(|row| row.agent().agent_id.to_string())
            .collect::<Vec<_>>()
        };
    assert_eq!(ids(None, LaneScope::Current(&feat_lane), &[]), ["feat"]);
    assert_eq!(ids(None, LaneScope::Room, &[]), ["feat", "docs"]);
    assert_eq!(
        ids(None, LaneScope::Current(&feat_lane), &["lane=docs"]),
        ["docs"],
        "a query naming lane replaces the current lane"
    );
    assert_eq!(
        ids(None, LaneScope::Current(&feat_lane), &["lane=*"]),
        ["feat", "docs"]
    );
    assert!(
        ids(None, LaneScope::Named(&feat_lane), &["lane=docs"]).is_empty(),
        "a named lane holds beside the query's"
    );
    assert_eq!(
        ids(Some(&audit), LaneScope::Current(&feat_lane), &["live=no"]),
        ["gone"]
    );
    assert_eq!(
        ids(Some(&audit), LaneScope::Current(&feat_lane), &[]),
        ["feat"],
        "closed rows join only a query that reads them"
    );
    assert_eq!(LaneScope::Current("feat").lane(&query(&["lane=*"])), None);
    assert_eq!(
        LaneScope::Named("feat").lane(&query(&["lane=*"])),
        Some("feat")
    );
    assert_eq!(LaneScope::Current("feat").lane(&query(&[])), Some("feat"));
    assert_eq!(LaneScope::<&str>::Room.lane(&query(&[])), None);
}

#[test]
fn a_closed_provisional_row_never_prints_a_live_agents_name() {
    let mut holder = agent("s-live", "%1");
    holder.name = Some("otter".into());
    let live = snapshot(vec![holder], &["s-live"]);
    // A card that ended before its session registered: without a pane, or in
    // the pane the live agent now holds, no Audit form beyond the name is left.
    for pane in [None, Some("%1")] {
        let mut closed = agent("launch_abc", "%9");
        closed.pane = pane.map(|pane| PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, pane)));
        closed.name = Some("otter".into());
        closed.name_explicit = true;
        closed.ended_at = Some(now());
        assert!(closed.agent_id.is_provisional());
        let mut audit_agents = live.agents.to_vec();
        audit_agents.push(closed);
        let audit = snapshot(audit_agents, &[]);
        let demotion = ParkDemotion::default();
        let listed = live_rows(&live, &demotion, &reader(), now());
        let closed = closed_rows(&audit, &live, &listed, &demotion, &reader(), now());
        let [row] = closed.as_slice() else {
            panic!("{pane:?}: one closed row: {closed:?}");
        };
        assert_eq!(row.agent().agent_id.as_str(), "launch_abc");
        let live_matches = resolve_many(&live, row.handle(), None, &reader()).unwrap_or_default();
        assert!(
            live_matches.is_empty(),
            "{pane:?}: {} resolves to live {:?}",
            row.handle(),
            live_matches
                .iter()
                .map(|agent| agent.agent_id.as_str())
                .collect::<Vec<_>>()
        );
    }
}

/// Selection pins the agent; an act-time read that finds the address taken by
/// another agent, or the agent ended or gone since, refuses it, and the reason
/// names no address, which the caller prints beside it.
#[test]
fn a_selected_address_holds_only_while_it_names_the_live_agent_selected() {
    let pinned = Selected {
        address: "@otter".to_owned(),
        agent: Some((AgentKind::new_unchecked("codex"), "otter".into())),
    };
    let otter = agent("otter", "%1");
    pinned.check(Some(&otter)).unwrap();
    let mut ended = otter.clone();
    ended.ended_at = Some(now());
    assert_eq!(pinned.check(Some(&ended)), Err(SelectionMoved::Ended));
    let fox = agent("fox", "%2");
    assert_eq!(pinned.check(Some(&fox)), Err(SelectionMoved::Another));
    assert_eq!(pinned.check(None), Err(SelectionMoved::Ended));
    assert_eq!(
        SelectionMoved::Another.to_string(),
        "now names another agent than the one selected"
    );
    assert_eq!(SelectionMoved::Ended.to_string(), "has ended");

    let line = Selected {
        address: "@otter".to_owned(),
        agent: None,
    };
    line.check(Some(&fox)).unwrap();
    line.check(None).unwrap();
    assert_eq!(line.check(Some(&ended)), Err(SelectionMoved::Ended));
}
