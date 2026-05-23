# Configuration

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz has two config tiers — one committed with the project, one private per machine. Both are optional. The smallest useful project config is a few lines.

## Minimal example

```toml
# .rimz/config.toml
[workspace]
mux = "auto"               # auto | zellij | tmux

[layout]
sidebar = true
sidebar_width = 30

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"
```

That's enough for `rimz` in this repo to open the right multiplexer, attach to the project's session, install the sidebar, and drop you in a shell.

## What to commit, what to keep local

**Commit.** `.rimz/config.toml` — the team should see the same workspace shape on every checkout.

**Don't commit.** Anything under `~/.config/rimz/` — your sound profile, agent paths, multiplexer preference, and resolver allowlist are per-machine and personal.

## Project config — `<project_root>/.rimz/config.toml`

May define:

- workspace display defaults,
- multiplexer preference (`[workspace] mux`),
- layout IR,
- agent launch commands,
- environment overrides,
- notification defaults,
- privacy defaults.

Project config is inert until the workspace is trusted. See [security.md](../guide/security.md).

## Per-machine config — `~/.config/rimz/projects/<sha256(project_root)>/`

```text
config.toml         local overrides
resolvers.toml      resolver allowlist and chain order
```

May define:

- local workspace display name,
- sound profile,
- hook install state,
- local agent binary paths,
- per-machine mux preference (overrides project `auto`),
- resolver allowlist and chain order.

## Merge order

Later layers win:

1. built-in defaults,
2. project-local config (`.rimz/config.toml`),
3. per-machine project config (`~/.config/rimz/projects/<hash>/config.toml`),
4. CLI flags and `RIMZ_*` environment variables.

The project layer sets the shared defaults every contributor sees. The per-machine layer mutes notifications, swaps sound profiles, overrides agent paths, or disables specific hooks without leaking those choices back into the repo. CLI flags win for one-off use.

## Layout IR

`[layout]` is multiplexer-neutral. Backend adapters compile it to Zellij KDL or tmux command sequences at session start. v0 supports the intersection: session, views (Zellij tabs / tmux windows), panes, split direction, pane size, cwd, command, env, pane/view naming.

```toml
[layout]
sidebar = true
sidebar_width = 30
default_view = "main"

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"

# Backend-only extras, ignored when the other backend is selected.
[layout.zellij]
# Floating/pinned panes, KDL fragments, plugin panes other than the sidebar.

[layout.tmux]
# Popup bindings, status-line snippets.
```

Backend-only entries that execute shell commands enter the trust hash.

## Privacy

```toml
[privacy]
retention_days    = 14
payload_mode      = "redacted"   # metadata | redacted | full
max_payload_bytes = 8192
```

- `metadata` — strips tool inputs, prompts, command arguments, and error text.
- `redacted` — keeps bounded payloads with built-in redaction. Default.
- `full` — keeps hook payloads as delivered. `rimz doctor` warns.

`rimz state export --json` honours the active payload mode.
