# Model upgrades

What to change when a provider ships a new model. The work splits by who resolves the alias a definition names: Claude Code resolves its own, and RimZ resolves Codex's. Pricing follows on its own for both, with two narrow exceptions.

## Claude: fable, opus, sonnet, haiku

Nothing is required for a new version of an existing family. The Claude adapter's `DEFINITIONS.models` in [`adapters/claude/mod.rs`](../../crates/rimz/src/agents/adapters/claude/mod.rs) maps each alias to itself (`opus` → `opus`), so Claude Code picks the current model at launch. Display names are derived from the model id (`agents/model_display.rs`), and the 1M context window is read from the `[1m]` marker on the id, never from a model list.

A new family name, one that is not already an alias, needs an entry in that table. Without one, a definition naming the bare alias cannot infer its kind and has to set `agent:` explicitly. Give the entry an `effort` only when the family needs a default other than the kind's `xhigh`, as `fable` does with `high`.

## Codex: astra, sol, luna, terra

RimZ owns these aliases: `DEFINITIONS.models` in [`adapters/codex/mod.rs`](../../crates/rimz/src/agents/adapters/codex/mod.rs) expands each one to a pinned model id. A release means one commit:

1. Point the alias at the new id in `DEFINITIONS.models`, or add an entry for a new family name. A family without a successor keeps its old id.
2. Confirm the new model accepts `xhigh`, the Codex kind's default effort (`DEFINITIONS.effort`). If it does not, set `effort` on the entry. A seat whose effort the model rejects fails at every launch, so this is the step that breaks users when missed.
3. Update the alias rows in `model_catalog_and_defaults` in [`agents/tools.rs`](../../crates/rimz/src/agents/tools.rs).
4. Update the alias sentence under [Chains and defaults](../reference/definitions.md#chains-and-defaults), the one home for the alias list.

Run `cargo xtask test 'agents::tools'`, `cargo xtask test 'harness::spec'`, and `cargo xtask test 'config::definitions'`, then `cargo xtask docs-links`.

No migration is needed. `config/definitions/agent.rs` expands the alias on every definition load, and resume, restart, and fork re-resolve the stored profile name through `resume::resolve_posture`, so every seat naming the alias moves to the new id at its next launch, restart, or resume. That includes a live agent's next restart, which switches its model mid-thread. A definition that names a full model id is a pin and stays where it is.

## Pricing, for either provider

Usually nothing. An unpriced model resolves to no price, never to its predecessor's: `PriceBook::price` rejects a purely numeric version bump ([Resolving a model](../internals/agents/spending.md#resolving-a-model)). Once the spend walk records the model, the unknown-model chase refetches LiteLLM and models.dev on a 30-minute gate, and `cargo xtask dist` refreshes the embedded snapshot before every release build ([The refresh](../internals/agents/spending.md#the-refresh)). Run `cargo xtask pricing-refresh` by hand only to ship a price before the next release.

Two cases need an edit, both in `crates/rimz/src/agents/pricing/`:

- **Fast mode.** When the model has a fast or priority tier and neither source publishes its multiplier, add the ratio to `fast-multiplier-overrides.json`: `exact` for one id, `normalized_prefix` for a family with dated or namespaced variants. An entry stops applying once upstream publishes the value ([Rates the sources leave unpublished](../internals/agents/spending.md#rates-the-sources-leave-unpublished)).
- **Long-context tier.** When the model bills a higher rate past a request-size threshold, add it to `LONG_CONTEXT_CANARIES` in `source.rs`, so `pricing-refresh --check` fails if a later projection drops the tier. Adding a model without such a tier makes that check fail for a false reason.

## What stays as it is

Test fixtures and insta snapshots that name older models (`claude-opus-4-8`, `gpt-5.6-sol`) exercise the mechanism, not the current lineup. Leave them.
