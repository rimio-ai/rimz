# Workflow host

Local contract for `crates/rimz/src/cli/workflow/`. Extends [cli/AGENTS.md](../AGENTS.md).

- The production host (`launcher::workflow_host`) reads facts for the engine and carries out its answers through the library launcher and the tree stop; `host.rs` builds it with the terminal interaction. `harness::workflow` decides: what an observation means, which agents a stop or detach reaches, admission, transitions, and conditions. A new decision goes in the harness, not the host.
