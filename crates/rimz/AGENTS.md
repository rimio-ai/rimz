# Rimz core

Local contract for `crates/rimz/` — the CLI binary, hook entrypoints, and the runtime/domain library. Extends the root [AGENTS.md](../../AGENTS.md); it never restates parent rules. The module map lives in [ARCHITECTURE.md](../../ARCHITECTURE.md); deeper subtree contracts live in [src/agents/](./src/agents/AGENTS.md), [src/ledger/](./src/ledger/AGENTS.md), [src/mux/](./src/mux/AGENTS.md), and [tests/integration/](./tests/integration/AGENTS.md).

## Crate-wide seams

- Command handlers (`src/cli/`) parse, call domain modules, and present; domain logic lives in the domain module, never in a handler.
- Domain modules stay free of Zellij, tmux, and agent-specific dependencies — backend knowledge enters through `mux/`, agent knowledge through `agents/`.
- Resolution matching uses `(workspace_id, request_id, nonce)` — never PID alone.
- Normalized pane IDs (`zellij:terminal_3`, `tmux:%3`) travel everywhere outside `mux/`; raw IDs stay inside the backend adapters.
