use proptest::prelude::*;

use crate::agents::AgentStatus;
use crate::{SidebarSubAgent, SidebarWorktreeKind};

use super::*;

#[derive(Clone, Debug)]
struct RowSpec {
    text: String,
    branch: String,
    status: AgentStatus,
    sub_agents: usize,
}

#[derive(Clone, Debug)]
struct ProviderSpec {
    product: String,
    art: Vec<String>,
    plan: String,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn adversarial_snapshot_data_does_not_panic(
        width in 0u16..=200,
        height in 0u16..=200,
        selected_index in 0usize..64,
        scroll_offset in 0usize..400,
        display_name in weird_text(),
        rows in prop::collection::vec(row_spec(), 0..16),
        provider in prop::option::of(provider_spec()),
    ) {
        let snapshot = build_adversarial_snapshot(display_name, rows, provider);
        let ui = UiState {
            selected_index,
            scroll_offset,
            ..Default::default()
        };

        let theme = Theme::for_sidebar(&snapshot.theme);
        let composed = compose_lines(&snapshot, None, &ui, &theme, width, height);
        prop_assert_eq!(composed.interactions.line_count(), composed.lines.len());

        if width > 0 && height > 0 {
            let mut out = Vec::new();
            render_fixed(&mut out, &snapshot, None, width, height)
                .expect("fixed render succeeds");
        }

    }
}

fn build_adversarial_snapshot(
    display_name: String,
    rows: Vec<RowSpec>,
    provider: Option<ProviderSpec>,
) -> crate::SidebarSnapshot {
    let agents = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut state = agent(
                &format!("agent-{idx}"),
                "claude",
                row.status,
                Some("/repo/main"),
                Some(&row.branch),
                Some(&row.text),
            );
            state.prompt = Some(row.text.clone());
            state.description = Some(row.text.clone());
            state.model = Some(row.text.clone());
            state.effort = Some(row.text.clone());
            state.usage.context_pct = Some((idx % 101) as u8);
            state.usage.context_window = Some(200_000);
            state.usage.total_tokens = Some((idx as u64).saturating_mul(1000));
            state
        })
        .collect::<Vec<_>>();
    let mut snapshot = snapshot_with(agents);
    snapshot.display_name = display_name;
    for (idx, group) in snapshot.worktree_groups.iter_mut().enumerate() {
        if let Some(row) = rows.get(idx % rows.len().max(1)) {
            group.key = row.text.clone();
            group.label = row.text.clone();
            group.kind = SidebarWorktreeKind::Worktree;
        }
        for (row_idx, rendered) in group.rows.iter_mut().enumerate() {
            let Some(spec) = rows.get(row_idx % rows.len().max(1)) else {
                continue;
            };
            rendered.name = spec.text.clone();
            rendered.worktree_branch = Some(spec.branch.clone());
            if let Some(card) = rendered.as_agent_mut() {
                card.task = Some(spec.text.clone());
                card.prompt = Some(spec.text.clone());
                card.description = Some(spec.text.clone());
                card.model = Some(spec.text.clone());
                card.effort = Some(spec.text.clone());
                card.sub_agents = sub_agents(spec.sub_agents, &spec.text);
                card.turn_error_label = Some(spec.text.clone());
            }
        }
    }
    if let Some(provider) = provider {
        let mut panel = provider_panel("claude", "Claude", 5, true, false, Some((50, 75)));
        panel.product_name = provider.product;
        panel.art = provider.art;
        panel.plan = Some(provider.plan);
        snapshot.providers = vec![panel];
    }
    snapshot
}

fn sub_agents(count: usize, text: &str) -> Vec<SidebarSubAgent> {
    (0..count)
        .map(|idx| SidebarSubAgent {
            id: format!("sub-{idx}"),
            name: text.to_owned(),
            status: AgentStatus::Running,
            phase: crate::agents::TurnPhase::Acting,
            task: Some(text.to_owned()),
            model: Some(text.to_owned()),
            effort: Some(text.to_owned()),
            description: Some(text.to_owned()),
            total_tokens: Some((idx as u64).saturating_mul(1_000_000)),
            cost_usd: Some(999_999.99),
            elapsed_secs: Some(idx as i64),
            started_at: Some(fixed_now()),
            last_activity: fixed_now(),
            registered_at: Some(fixed_now()),
        })
        .collect()
}

fn row_spec() -> impl Strategy<Value = RowSpec> {
    (weird_text(), weird_text(), 0_u8..6, 0_usize..24).prop_map(
        |(text, branch, status, sub_agents)| RowSpec {
            text,
            branch,
            status: status_from(status),
            sub_agents,
        },
    )
}

fn provider_spec() -> impl Strategy<Value = ProviderSpec> {
    (
        weird_text(),
        prop::collection::vec(weird_text(), 0..8),
        weird_text(),
    )
        .prop_map(|(product, art, plan)| ProviderSpec { product, art, plan })
}

fn weird_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("a"),
            Just(" "),
            Just("界"),
            Just("🙂"),
            Just("\u{0301}"),
            Just("\u{200b}"),
            Just("\u{200d}"),
            Just("─"),
        ],
        0..160,
    )
    .prop_map(|parts| parts.concat())
}

fn status_from(value: u8) -> AgentStatus {
    match value {
        0 => AgentStatus::Running,
        1 => AgentStatus::Waiting,
        2 => AgentStatus::Idle,
        3 => AgentStatus::Success,
        4 => AgentStatus::Failed,
        _ => AgentStatus::Paused,
    }
}
