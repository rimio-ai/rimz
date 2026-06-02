# Token pricing

> See [transcript.md → Cost history](./transcript.md#cost-history) for the read-path that consumes this table, and [account.md](./account.md) for how the resulting per-provider spend reaches the dashboard panels.

Rimz totals an agent's spend from its transcript history. Claude and Pi log `costUSD` directly; Codex logs only token counts, so converting Codex events to dollars needs a per-model price table. This doc owns that table: where prices come from, how a model resolves to a price, and how the table stays fresh.

Pricing is **enrichment, never correctness** — the no-transcript-correctness rule. A failed fetch, a missing snapshot, an unknown model: each degrades to stale-but-usable prices or an omitted entry, never a hard failure.

The table lives in [`agents/pricing/`](../../crates/rimz/src/agents/pricing/mod.rs): per-token [`Pricing`](../../crates/rimz/src/agents/pricing/mod.rs) keyed by model in a [`PriceBook`](../../crates/rimz/src/agents/pricing/mod.rs). Lookups are pure and network-free; the only network is the gated refresh in `load_for_spending`.

## Three layers

The book is assembled from three sources, the later ones winning, so a stale or missing remote entry never overrides a price the team maintains:

| Layer | When | Source |
| --- | --- | --- |
| 1. Embedded snapshot | always, at process start | the checked-in LiteLLM snapshot, compacted into the binary by [`build.rs`](../../crates/rimz/build.rs) and `include_str!`-ed ([`embedded.rs`](../../crates/rimz/src/agents/pricing/embedded.rs)) |
| 2. Remote refresh | once per TTL, on disk | a fresh LiteLLM pull, plus models.dev filling models the snapshot lacks ([`remote.rs`](../../crates/rimz/src/agents/pricing/remote.rs)) |
| 3. Builtins | always, applied last | hardcoded prices for the OpenAI/Codex family ([`builtins.rs`](../../crates/rimz/src/agents/pricing/builtins.rs)) |

`gpt-5` is mandatory in the builtins: it is the Codex parser's fallback model, so a Codex event with no resolvable model still prices.

## The refresh

`rimz sidebar snapshot` is a one-shot process, so the refresh is disk-cached at `{runtime_root}/pricing-cache.json` rather than held in memory: the producer reads the embedded snapshot plus the cache instantly, and re-fetches only when the cache is older than a day. A failed fetch records its attempt time and backs off an hour, so a persistent outage never re-fetches on every snapshot. `RIMZ_PRICING_OFFLINE` skips the fetch entirely.

`build.rs` never touches the network: it embeds the checked-in vendored snapshot at [`crates/rimz/pricing/litellm-pricing.json`](../../crates/rimz/pricing/litellm-pricing.json) (or a `RIMZ_PRICING_JSON_PATH` override), so every build is reproducible and hermetic. `cargo xtask pricing-refresh` is the deliberate update path — it fetches upstream and rewrites that snapshot as a reviewable, committed diff; its compaction mirrors `build.rs`.

## Resolving a model

`PriceBook::price` resolves a model id to a price by exact match, then a boundary-aware fuzzy scan: the longest stored key that is a word-boundary prefix of the (normalized: trimmed, lowercased, `.`/`@`→`-`) lookup wins. So `claude-sonnet-4-20250514-via-bedrock` resolves to its base model. A purely numeric, non-date version bump is rejected — a new `gpt-5`-family version is never silently priced as the old one; it falls through to its own entry or to no price.

## Computing Codex cost

`spending::compute_spending` multiplies each [`CodexTokenEvent`](../../crates/rimz/src/agents/transcript/codex.rs): uncached input at the input rate, the cached slice at the cache-read rate, and output (which already includes reasoning tokens) at the output rate. An event whose model has no known price contributes nothing rather than guessing.
