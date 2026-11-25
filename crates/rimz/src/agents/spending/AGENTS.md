# Spending

Local contract for `crates/rimz/src/agents/spending/`. Extends [agents/AGENTS.md](../AGENTS.md).

Before a change of one of these kinds, read the section named beside it and do what it asks. No gate catches a skipped one.

- A spending session key split: [Spend and pricing](../../../../../docs/contributing/agent-adapters.md#spend-and-pricing).
- The room spend slice (`aggregate_scoped_rollups` and what it calls) or the walker's memo build (`SpendingWalker::ensure_memo`): [Timing comparisons](../../../../../docs/contributing/running-gates.md#timing-comparisons).
- A new write to a spending publication, each of which has one writer: [What reads the totals](../../../../../docs/internals/agents/spending.md#what-reads-the-totals).
