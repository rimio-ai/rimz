# RimZ core

Local contract for `crates/rimz/` — the CLI binary, hook entrypoints, and the runtime/domain library. Extends the root [AGENTS.md](../../AGENTS.md). The module map lives in the root [code map](../../AGENTS.md#code-map), its structural rationale in [ARCHITECTURE.md](../../ARCHITECTURE.md); deeper subtree contracts live in [src/cli/](./src/cli/AGENTS.md), [src/agents/](./src/agents/AGENTS.md), [src/room/](./src/room/AGENTS.md), [src/harness/](./src/harness/AGENTS.md), [src/message/](./src/message/AGENTS.md), [src/store/](./src/store/AGENTS.md), [src/mux/](./src/mux/AGENTS.md), [src/sidebar/](./src/sidebar/AGENTS.md), [src/sidebar_pane/](./src/sidebar_pane/AGENTS.md), [src/remote/](./src/remote/AGENTS.md), [src/diag/](./src/diag/AGENTS.md), and [tests/integration/](./tests/integration/AGENTS.md).

## Crate-wide seams

- Command handlers (`src/cli/`) parse, call domain modules, and present; domain logic lives in the domain module, never in a handler.
- Domain modules stay free of Zellij, tmux, and agent-specific dependencies — backend knowledge enters through `mux/`, agent knowledge through `agents/`.
- Run-wake matching uses `(workspace_id, run_id)` — never PID alone.
- Normalized pane IDs (`zellij:terminal_3`, `tmux:%3`) travel everywhere outside `mux/`; raw IDs stay inside the backend adapters.
