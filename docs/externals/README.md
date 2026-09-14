# RimZ externals

These pages mirror the upstream surfaces RimZ binds to: each coding agent's hooks, transcripts, and launch modes, and each multiplexer's control API. They record what upstream ships, pinned to source URLs and a refresh baseline, so a contributor can implement against it and check it for drift when upstream moves. They make no claim about what RimZ supports.

The RimZ side of each surface is in the [internals](../internals/README.md): an external page says what upstream does, and its internals page says how RimZ maps it onto its own types. Per-agent support status is in [agent-support.md](../reference/agent-support.md). To use RimZ rather than change it, start at the [user documentation](../README.md).

## Keeping a mirror current

Each page names its upstream sources and the version or commit it was last refreshed against, and the tables below copy that version into a **Refreshed against** column. Before implementation work on a surface, compare the column to upstream's current release: the gap between the two is the release diff and changelog range to read, and re-fetching the page's sources closes it. A refresh updates the page's baseline and this column in the same change. Where upstream's docs and its source or installed binary disagree, the page follows the wire and flags the disagreement. A new built-in adapter starts with its reference page here; [agent-adapters.md](../contributing/agent-adapters.md) is the playbook.

## Agent adapters

`agent-adapter/` holds one page per coding agent: its hook events and decision channel, session identity and resume, transcripts, permissions, headless modes, authentication, and usage.

| Page | Upstream | Refreshed against | RimZ mapping |
| --- | --- | --- | --- |
| [claude-reference.md](./agent-adapter/claude-reference.md) | Claude Code | 2.1.270 (tag `v2.1.270`, 2026-09-12) | [adapter_claude.md](../internals/agents/adapter_claude.md) |
| [codex-reference.md](./agent-adapter/codex-reference.md) | Codex | 0.154.0 | [adapter_codex.md](../internals/agents/adapter_codex.md) |
| [amp-reference.md](./agent-adapter/amp-reference.md) | Amp CLI | `@ampcode/cli` 0.0.1789344113-g6e4515 (2026-09-14) | [adapter_amp.md](../internals/agents/adapter_amp.md) |
| [antigravity-reference.md](./agent-adapter/antigravity-reference.md) | Antigravity CLI | 1.1.2 (source tag 1.1.1) | [adapter_antigravity.md](../internals/agents/adapter_antigravity.md) |
| [copilot-reference.md](./agent-adapter/copilot-reference.md) | GitHub Copilot CLI | 1.0.83 | [adapter_copilot.md](../internals/agents/adapter_copilot.md) |
| [cursor-reference.md](./agent-adapter/cursor-reference.md) | Cursor CLI | 2026.07.09-a3815c0 | [adapter_cursor.md](../internals/agents/adapter_cursor.md) |
| [droid-reference.md](./agent-adapter/droid-reference.md) | Factory Droid CLI | 0.171.0 (exec sections: SDK 0.6.0) | [adapter_droid.md](../internals/agents/adapter_droid.md) |
| [grok-reference.md](./agent-adapter/grok-reference.md) | Grok Build | 0.1.220-alpha.4 | [adapter_grok.md](../internals/agents/adapter_grok.md) |
| [kimi-reference.md](./agent-adapter/kimi-reference.md) | Kimi Code | 0.23.6 | [adapter_kimi.md](../internals/agents/adapter_kimi.md) |
| [kiro-reference.md](./agent-adapter/kiro-reference.md) | Kiro CLI | 2.12.1 | [adapter_kiro.md](../internals/agents/adapter_kiro.md) |
| [opencode-reference.md](./agent-adapter/opencode-reference.md) | OpenCode | 1.18.30 | [adapter_opencode.md](../internals/agents/adapter_opencode.md) |
| [pi-reference.md](./agent-adapter/pi-reference.md) | Pi | 0.85.1 | [adapter_pi.md](../internals/agents/adapter_pi.md) |
| [qwen-reference.md](./agent-adapter/qwen-reference.md) | Qwen Code | 0.19.10 | [adapter_qwen.md](../internals/agents/adapter_qwen.md) |

The provider-neutral contracts every adapter page feeds are [model.md](../internals/agents/model.md) for lifecycle and status, [adapter.md](../internals/agents/adapter.md) for the adapter layer, and [providers.md](../internals/agents/providers.md) for accounts and spend.

## Multiplexer adapters

`mux-adapter/` holds one page per multiplexer backend. Both backends are first-class, so RimZ's core behaviour may rely only on what the two pages have in common.

| Page | Upstream | Refreshed against | RimZ mapping |
| --- | --- | --- | --- |
| [zellij-reference.md](./mux-adapter/zellij-reference.md) | Zellij: the wasm plugin API, CLI control surface, configuration, layout KDL, terminal graphics, and session serialization | 0.45.0 | [multiplexers.md](../internals/multiplexers.md), and [web.md](../internals/web.md) for browser access |
| [tmux-reference.md](./mux-adapter/tmux-reference.md) | tmux: the client/server and socket model, command verbs, format language, hooks, options, session environment, and control mode | 3.7b | [multiplexers.md](../internals/multiplexers.md) |
