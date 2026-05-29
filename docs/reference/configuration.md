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
sidebar_width = 30              # percent of terminal width

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"
```

That's enough for `rimz` in this repo to open the right multiplexer, create or find the project's session, launch the native sidebar pane, and drop you in a shell when the caller is interactive.

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

## Per-machine config — `~/.config/rimz/`

Personal, never committed. Two scopes:

```text
config.toml                         machine-wide preferences (see below)
resolvers.toml                      resolver allowlist and chain order
projects/<sha256(project_root)>/    per-project, per-machine state
  trust.toml                          executable-surface grant for this project
```

`config.toml` may define:

- remote-control auto-launch, per agent (`[remote_control] claude` / `codex`),
- local workspace display name,
- sound profile,
- hook install state,
- local agent binary paths,
- per-machine mux preference (overrides project `auto`).

### Remote control auto-launch

```toml
[remote_control]
claude = true          # off when unset
codex  = true          # off when unset
```

Each toggle is independent: when it is set *and* that agent is on PATH, `rimz start` launches its remote-control host into one shared background view — a `rimz-rc` tab on Zellij, a `rimz-rc` window on tmux — out of your working pane and idempotent on that name (a second `rimz start` is a no-op). Both hosts live in that one view, side by side in separate panes.

- **Claude** runs `claude remote-control --spawn worktree`: a long-lived foreground host. It runs from the project root (the main checkout), so on-demand remote sessions get isolated worktrees off the canonical repo rather than the current worktree.
- **Codex** runs `codex remote-control start`, which brings up the Codex app-server daemon with remote control enabled, then returns — so its pane is kept open on its start receipt (or, if your Codex install can't manage the daemon, the error telling you how to fix it). That daemon is the one Codex enrichment re-uses: `rimz codex refresh-context` prefers the running daemon's control socket (`codex app-server proxy`) over cold-spawning a throwaway `codex app-server`, and always falls back to a cold-spawn so enrichment never depends on the daemon being up. Set `RIMZ_CODEX_APP_SERVER_SOCK=` (empty) to force the cold-spawn path.

This tier is per-machine on purpose: remote control links *your* agent accounts and accepts remote spawn commands, so a clone never inherits it and it never enters the project trust hash. Neither host pane is a coding agent — the sidebar shows each as a pinned, specially-coloured row (`remote control` for Claude, `codex remote` for Codex), never as an idle agent.

## Merge order

Later layers win:

1. built-in defaults,
2. project-local config (`.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/config.toml`),
4. CLI flags and `RIMZ_*` environment variables.

The project layer sets the shared defaults every contributor sees. The per-machine layer mutes notifications, swaps sound profiles, overrides agent paths, or disables specific hooks without leaking those choices back into the repo. CLI flags win for one-off use.

## Layout IR

`[layout]` is multiplexer-neutral. Backend adapters compile it to Zellij or tmux command sequences at session start. v0 supports the intersection: session, views (Zellij tabs / tmux windows), panes, split direction, pane size, cwd, command, env, pane/view naming. By default the sidebar is a native pane — no plugin install — launched at session start on both backends. Zellij users can opt in to a docked plugin rail instead (`[layout.zellij]` below); the native pane stays the fallback.

```toml
[layout]
sidebar = true
sidebar_width = 30              # percent of terminal width
default_view = "main"

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"

# Backend-only extras, ignored when the other backend is selected.
[layout.zellij]
sidebar_plugin = false   # opt in to the docked plugin rail instead of a native pane
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
