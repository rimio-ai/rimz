//! Static, allocation-free identity and capability data for one agent.
//!
//! One `const` [`AgentDescriptor`] per adapter directory; the registry holds
//! `&'static` references. Everything here is data a `match kind { … }` used to
//! encode somewhere in core — folded into the one place an agent is declared,
//! so adding an agent never edits a shared dispatch site. Anything that parses
//! a payload, touches the filesystem or network, or spawns a helper is an
//! [`AgentAdapter`](super::AgentAdapter) trait method, never descriptor data.

use std::time::Duration;

use serde_json::Value;

/// Static identity, branding, capabilities, and classification tables for one
/// agent. See the module doc for the descriptor-vs-trait split.
#[derive(Debug)]
pub struct AgentDescriptor {
    /// The stable kind string — the `--source` tag, the per-provider bucket
    /// key, the rollup `kind`.
    pub kind: &'static str,
    /// Human display name; the provider dashboard panel title.
    pub display_name: &'static str,
    /// Brand emblem + color for the provider dashboard panel.
    pub brand: Brand,
    /// How a raw plan tier becomes a brand label (`max` → `Claude Max`).
    pub plan_label: PlanLabel,
    /// Subscription provider ids whose account budget this agent meters, as a
    /// multi-provider client's auth file names them (Pi's `auth.json` keys:
    /// `anthropic`, `openai`, …). A provider that exposes no window surface of
    /// its own (Pi) but runs on one of these subscriptions shares that
    /// account's budget, so the dashboard borrows the sibling kind's windows
    /// — resolved through [`kind_for_sub_provider`](super::kind_for_sub_provider).
    pub sub_providers: &'static [&'static str],
    /// Tool-name classification tables for the lifecycle `ToolUsed` bits.
    pub tools: ToolClassification,
    /// What this agent can and cannot do — consumed by the sidebar and doctor
    /// so a missing surface renders as a declared absence, never an
    /// accidental gap.
    pub capabilities: Capabilities,
    /// Maximum time a blocking hook may hold the bridge open before falling
    /// back to the neutral no-op. Set from the upstream's published deadline,
    /// with margin so the bridge times out before the agent kills the hook.
    pub hook_cap: Duration,
    /// Process names this agent's instance can run under — its own `comm`
    /// plus any launcher (`node` for a JS bundle). Drives the PID-attribution
    /// `/proc` walk.
    pub process_names: &'static [&'static str],
    /// Lifecycle events, in this agent's own wire vocabulary, that prove the
    /// agent is actively making progress — a tool completed, a turn started
    /// or ended, a subagent spawned or finished. These refresh the per-agent
    /// activity heartbeat. A blocking pre-tool gate (Claude's `PreToolUse`,
    /// pi's `tool_call`) is deliberately excluded: it can fire in the same
    /// tool call as a blocking ask, so touching on it would race the ask
    /// creation and instantly un-block the row. An idle notification is
    /// excluded too — waiting for input is not progress.
    pub activity_events: &'static [&'static str],
    /// User-facing reason shown by doctor/start when
    /// [`Capabilities::hook_install`] is false.
    pub hook_install_unavailable: Option<&'static str>,
    /// How this agent's transcript files map to billing threads, for the
    /// spending session count.
    pub thread_key: ThreadKey,
}

/// How a provider's transcript files map to billing threads (sessions), so the
/// spending pass counts one thread once however many files it spread across.
#[derive(Clone, Copy, Debug)]
pub enum ThreadKey {
    /// One transcript file per session — the file path is the thread.
    PerFile,
    /// One directory per session holding a main JSONL plus `subagents/*.jsonl`
    /// children — the session directory is the thread, so a subagent file
    /// folds under its parent session (Claude).
    SessionDir,
}

/// Brand styling for the provider dashboard panel.
#[derive(Debug)]
pub struct Brand {
    /// ASCII emblem lines, already split.
    pub emblem: &'static [&'static str],
    /// 256-color index.
    pub color: u8,
}

/// How a raw plan tier string becomes its brand label.
#[derive(Debug)]
pub enum PlanLabel {
    /// `"<prefix> <TitleCase(tier)>"` — Claude → "Claude Max",
    /// Codex → "ChatGPT Pro".
    Prefixed { prefix: &'static str },
    /// Just title-case the tier — for an agent whose sessions span many
    /// provider accounts, where no single brand prefix is honest.
    TitleCaseOnly,
}

/// The agent's tool vocabulary, classified for the lifecycle `ToolUsed` bits.
#[derive(Debug)]
pub struct ToolClassification {
    /// Tools that mutate the workspace — write files or run commands. A
    /// mutating tool is proof of real work, so its `PostToolUse` is the only
    /// tool event recorded on the lifecycle channel.
    pub mutating: &'static [&'static str],
    /// The file-editing subset of `mutating` — the turn's first edit moves it
    /// from reasoning to acting. A shell tool mutates but does not edit, so a
    /// research turn that only runs commands keeps the thinking sparkle.
    pub editing: &'static [&'static str],
}

/// Explicit capability declaration. A provider that *cannot* do something
/// declares it here instead of leaving an inferable absence. Three flags
/// gate behavior today: `rate_limit_windows` (the provider dashboard's
/// budget bars), `registers_lazily` (cwd pane binding and synthesized idle
/// rows), `hook_install` (the install and doctor surfaces), and
/// `native_ask_ui` (whether an unresolved blocking ask becomes a `native_ui`
/// feed item). The rest state the adapter contract up front — pinned by each
/// adapter's tests, consumed as shared sites grow capability-aware.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// Can natively hold a turn open for a permission/plan/question decision
    /// (the blocking-feed channel).
    pub blocking_feed: bool,
    /// Renders its own ask UI in the pane — permission prompts, plan
    /// approvals, questions — so a blocking ask no resolver answers can hand
    /// off to the agent's surface as a `native_ui` feed item. An agent
    /// without one (pi gates tools only through the extension) resolves the
    /// same ask neutrally with no feed item: there is no surface the item
    /// could route the human to, so pushing one would strand it waiting.
    pub native_ask_ui: bool,
    /// Surfaces rate-limit windows / plan budgets the dashboard can meter.
    pub rate_limit_windows: bool,
    /// Routes child tasks through `Subagent{Start,Stop}` lifecycle signals.
    pub subagents: bool,
    /// Has a notion of parking a turn on still-in-flight background work.
    pub background_tasks: bool,
    /// Registers its session lazily and/or routes hooks through a daemon, so
    /// an instance can be present without a stamped session. The sidebar
    /// binds such a session to its pane by cwd and synthesizes an idle row
    /// for a wired-but-unbound pane.
    pub registers_lazily: bool,
    /// Rimz can install a hook configuration the agent actually executes.
    pub hook_install: bool,
}

impl AgentDescriptor {
    /// The kind as a typed identity — the one sanctioned mint of an
    /// [`AgentKind`](crate::ids::AgentKind) for a known adapter.
    pub fn kind_id(&self) -> crate::ids::AgentKind {
        crate::ids::AgentKind::new_unchecked(self.kind)
    }

    /// Whether an event refreshes the per-agent activity heartbeat. See
    /// [`activity_events`](Self::activity_events).
    pub fn records_activity(&self, event_name: &str) -> bool {
        self.activity_events.contains(&event_name)
    }

    /// Whether a tool-use payload names a workspace-mutating tool. The tool
    /// name rides `tool_name` in every provider's payload.
    pub fn tool_mutates(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.mutating)
    }

    /// Whether a tool-use payload names a *file-editing* tool.
    pub fn tool_edits_files(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.editing)
    }

    fn tool_in(&self, payload: &Value, set: &[&str]) -> bool {
        payload
            .get("tool_name")
            .and_then(Value::as_str)
            .is_some_and(|name| set.contains(&name))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::agents::registry::ADAPTERS;

    #[test]
    fn every_descriptor_keeps_editing_a_subset_of_mutating() {
        for adapter in ADAPTERS {
            let descriptor = adapter.descriptor();
            for tool in descriptor.tools.editing {
                assert!(
                    descriptor.tools.mutating.contains(tool),
                    "{}: editing tool {tool} missing from the mutating set",
                    descriptor.kind,
                );
            }
        }
    }

    #[test]
    fn tool_classification_reads_the_tool_name() {
        let claude = crate::agents::registry::descriptor_by_kind("claude").unwrap();
        assert!(claude.tool_mutates(&json!({ "tool_name": "Edit" })));
        assert!(claude.tool_mutates(&json!({ "tool_name": "Bash" })));
        assert!(!claude.tool_mutates(&json!({ "tool_name": "Read" })));
        assert!(!claude.tool_mutates(&json!({})));
        // Command runners mutate but do not edit — the reasoning phase survives.
        assert!(!claude.tool_edits_files(&json!({ "tool_name": "Bash" })));
        assert!(claude.tool_edits_files(&json!({ "tool_name": "Write" })));

        let codex = crate::agents::registry::descriptor_by_kind("codex").unwrap();
        assert!(codex.tool_mutates(&json!({ "tool_name": "apply_patch" })));
        assert!(codex.tool_edits_files(&json!({ "tool_name": "apply_patch" })));
        assert!(!codex.tool_edits_files(&json!({ "tool_name": "shell" })));
    }
}
