use super::*;

#[test]
fn pi_on_a_metered_sub_borrows_the_sibling_kinds_windows() {
    // Pi reads no window surface of its own, but an OAuth subscription is the
    // sibling provider's account — `openai` maps to the codex kind — so the
    // Pi panel borrows codex's stable windows: same account, same bars.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);

    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![codex, pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered, "an OAuth sub is metered");
    assert_eq!(
        pi_panel.windows,
        vec![reading],
        "pi borrows the codex account's windows"
    );
}

#[test]
fn probed_pi_version_reaches_the_provider_panel() {
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(true),
            version: Some("0.78.0".to_owned()),
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.0"));
}

#[test]
fn version_only_pi_probe_enriches_active_panel_without_creating_idle_one() {
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            version: Some("0.78.1".to_owned()),
            ..Default::default()
        },
    );

    let active = room(Vec::new(), vec![agent("pi", "p1", AgentStatus::Idle, 20)])
        .with_provider_aggregates(&probed, &BTreeMap::new(), &BTreeMap::new());
    let pi_panel = active
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("active pi panel present");
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.1"));

    let idle = room(Vec::new(), Vec::new()).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert!(
        idle.providers.is_empty(),
        "a binary version alone is not a logged-in account"
    );
}

#[test]
fn pi_sub_without_borrowable_windows_stays_bar_less() {
    // A metered Pi sub whose sibling has no readings (no codex session ever
    // reported), or whose provider maps to no kind, keeps the bar-less block.
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("GitHub Copilot OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("github-copilot".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.windows.is_empty(), "no sibling kind, no bars");
}

#[test]
fn pi_api_key_sub_never_borrows_windows() {
    // An unmetered (API-key) credential has no budget to meter, so even a
    // borrowable sibling leaves the `∞` bar untouched.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI API Key".to_owned()),
            metered: Some(false),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![codex, pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(!pi_panel.metered);
    assert!(pi_panel.windows.is_empty(), "an API key meters nothing");
}
