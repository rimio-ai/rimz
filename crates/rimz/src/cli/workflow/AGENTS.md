# Workflow commands

Local contract for `crates/rimz/src/cli/workflow/`. Extends [cli/AGENTS.md](../AGENTS.md).

- `harness::workflow` decides what an observation means, which agents a stop or detach reaches, admission, transitions, and conditions. The host (`launcher::workflow_host`, which `host.rs` builds with the terminal interaction) only reads facts for the engine and carries out its answers, so a new decision goes in the harness, not the host.
