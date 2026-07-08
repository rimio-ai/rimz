# Pane CLI

`rimz pane` exposes the public pane primitives that humans and scripts share: see the room as panes, read what is on screen, type into one, and move focus. It targets panes by id or by the [agent-address grammar](./agents.md#addressing-agents).

```sh
rimz pane list
rimz pane capture @codex#auth-refresh --lines 80                             # read an agent's visible buffer
rimz pane capture zellij:terminal_4 --lines 80                                # read a precise pane id
rimz pane send @codex#auth-refresh --key ctrl-u --enter -- "cargo xtask test" # clear line, type, run
rimz pane focus tmux:%3
rimz pane split
rimz pane detach
```

`list` is the room seen as panes: every pane grouped under its native tab, each row labelled with the agent that lives in it (`@kind#worktree`) or `process` for a plain pane, with status and working directory. Rimz's own sidebar pane is omitted, and a `●` marks the active pane in each tab. On Zellij, listing a named session requires a known Rimz workspace record because the pane roster comes from Rimz's presence-plugin topology cache.

```text
#auth-refresh
 ●  @claude#auth-refresh   running   ~/code/qe-wt/auth-refresh   zellij:terminal_3
    @codex#auth-refresh    idle      ~/code/qe-wt/auth-refresh   zellij:terminal_4
    process                -         ~/code/qe-wt/auth-refresh   zellij:terminal_5
```

The agent labels are a best-effort overlay folded from the workspace snapshot, so a pane the multiplexer has handed back to a shell reads `process`; the tab grouping always works, even with no snapshot reachable. `--json` emits the tab tree with a per-pane `kind`, `command`, `cwd`, and `pid`, and an `agent` object for agent panes.

`capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention. These three target either a pane id or an agent address, and pane ids choose their own backend (`tmux:%3` uses tmux, `zellij:terminal_4` uses Zellij) instead of the ambient session. Named keys are `enter`, `escape`, `tab`, `backspace`, the four arrows, `ctrl-c`, `ctrl-d`, and `ctrl-u`, with aliases like `return`, `esc`, and `bs`.

`split` opens a shell beside the current pane along its longer visual edge, matching the room's native new-pane behavior. `detach` detaches the attached client; the session keeps running in the background and comes back on the next attach.

Pane capture is untrusted terminal text: a script matches bounded patterns before sending anything back, and `pane send` is the same explicit input path as `message --steer`. The operator-facing safety rules are in [security.md](../../guide/security.md).
