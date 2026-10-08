# Model upgrades

What to change when a provider ships a new model. The work splits by who resolves the alias a definition names: Claude Code resolves its own, and RimZ resolves Codex's. Pricing follows on its own for both, with two narrow exceptions.

## Claude: fable, opus, sonnet, haiku

Nothing is required for a new version of an existing family. The Claude adapter's `DEFINITIONS.models` in [`adapters/claude/mod.rs`](../../crates/rimz/src/agents/adapters/claude/mod.rs) maps each alias to itself (`opus` → `opus`), so Claude Code picks the current model at launch. Display names are derived from the model id (`agents/model_display.rs`), and the 1M context window is read from the `[1m]` marker on the id, never from a model list.

A new family name, one that is not already an alias, needs an entry in that table. Without one, a definition naming the bare alias cannot infer its kind and has to set `agent:` explicitly. Give the entry an `effort` only when the family needs a default other than the kind's `xhigh`, as `fable` does with `high`.

## Codex: astra, sol, luna, terra

An existing family's new release needs no RimZ change. The exec wrapper resolves aliases from the selected account's Codex catalog at fresh launch, with a one-hour cache. The baked `DEFINITIONS.models` table in [`adapters/codex/mod.rs`](../../crates/rimz/src/agents/adapters/codex/mod.rs) is the offline fallback. See [catalog resolution](../internals/agents/adapter_codex.md#model-catalog-resolution) for version ordering and effort checks.

1. Add an entry in `DEFINITIONS.models` for a new family name, or update an existing family's offline fallback deliberately.
2. Confirm the new model accepts `xhigh`, the Codex kind's default effort (`DEFINITIONS.effort`). If it does not, set `effort` on the entry. A seat whose effort the model rejects fails at every launch, so this is the step that breaks users when missed. The levels a Codex model accepts are its `supported_reasoning_levels` in the per-account `$CODEX_HOME/models_cache.json`, or `supportedReasoningEfforts` in the app-server [`model/list`](../externals/agent-adapter/codex-reference.md) response; the upstream `ReasoningEffort` enum is wider than any single model, so it cannot confirm a level.
3. Update the alias rows in `model_catalog_and_defaults` in [`agents/tools.rs`](../../crates/rimz/src/agents/tools.rs).
4. Update the alias sentence under [Chains and defaults](../reference/definitions.md#chains-and-defaults), the one home for the alias list.

Run `cargo xtask test 'agents::tools'`, `cargo xtask test 'harness::spec'`, and `cargo xtask test 'config::definitions'`, then `cargo xtask docs-links`.

Definitions and tier cells retain the requested alias. With a durable launch record, resume, restart, and fork resolve that requested model through the same pins, catalog, and passthrough path as a fresh launch, ignoring observed `/model` switches. Only legacy sessions without a launch record replay the last observed model for an alias posture, and not when the resume names `--model`, `--tier`, or `--agent`; without an observed model they warn and resolve afresh. Full model ids pass through unchanged. Machine [model alias pins](../guide/configuration.md#model-aliases) override catalog resolution.

## Model tier bindings

For definitions naming `tier:` or a model listed in a tier, edit the ordered model list in machine config; see [model tiers](../guide/configuration.md#model-tiers). Each configured entry belongs to only one tier; aliases remain distinct from full model IDs until launch. Effort belongs to the definition or the model's default, not the list; relative effort is not supported. To change RimZ's shipped lists, update the defaults in `config/tiers.rs`, the commented template, and that guide together.

## Pricing, for either provider

Usually nothing. An unpriced model resolves to no price, never to its predecessor's: `PriceBook::price` rejects a purely numeric version bump ([Resolving a model](../internals/agents/spending.md#resolving-a-model)). Once the spend walk records the model, the unknown-model chase refetches LiteLLM and models.dev on a 30-minute gate, and `cargo xtask dist` refreshes the embedded snapshot before every release build ([The refresh](../internals/agents/spending.md#the-refresh)). Run `cargo xtask pricing-refresh` by hand only to ship a price before the next release.

Two cases need an edit, both in `crates/rimz/src/agents/pricing/`:

- **Fast mode.** When the model has a fast or priority tier and neither source publishes its multiplier, add the ratio to `fast-multiplier-overrides.json`: `exact` for one id, `normalized_prefix` for a family with dated or namespaced variants. An entry stops applying once upstream publishes the value ([Rates the sources leave unpublished](../internals/agents/spending.md#rates-the-sources-leave-unpublished)).
- **Long-context tier.** When the model bills a higher rate past a request-size threshold, add it to `LONG_CONTEXT_CANARIES` in `source.rs`, so `pricing-refresh --check` fails if a later projection drops the tier. Adding a model without such a tier makes that check fail for a false reason.

## What stays as it is

Test fixtures and insta snapshots that name older models (`claude-opus-4-8`, `gpt-5.6-sol`) exercise the mechanism, not the current lineup. Leave them.
