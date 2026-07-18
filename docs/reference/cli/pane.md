# Pane CLI

`rimz pane` exposes the public pane primitives that humans and scripts share: see the room as panes, read what is on screen, type into one, and move focus. `pane send` is the same explicit input path as `message --steer` — literal keystrokes into a real pane, nothing more — and `pane capture` reads back the visible buffer, so a script observes and drives a pane exactly as a person at the keyboard would. It targets panes by id or by the [agent-address grammar](./agents.md#addressing-agents). The operator-facing safety rules for treating captured text as untrusted are in [security.md](../../guide/security.md).

```sh
rimz pane list
rimz pane capture @codex#auth-refresh --lines 80                             # read an agent's visible buffer
rimz pane capture zellij:terminal_4 --lines 80                                # read a precise pane id
rimz pane send @codex#auth-refresh --key ctrl-u --enter -- "cargo xtask test" # clear line, type, run
rimz pane focus tmux:%3
rimz pane split
rimz pane detach
```

`list` is the room seen as panes: every pane grouped under its native tab, each row labelled with the agent that lives in it (`@kind#worktree`) or `process` for a plain pane, with status and working directory. RimZ's own sidebar pane is omitted. The topology carries no per-tab active mark; attached selection lives in the optional session register consumed by the sidebar. On Zellij, listing a named session requires a known RimZ workspace record because the pane roster comes from RimZ's presence-plugin topology cache.

```text
#auth-refresh
 @claude#auth-refresh   running   ~/code/qe-wt/auth-refresh   zellij:terminal_3
 @codex#auth-refresh    idle      ~/code/qe-wt/auth-refresh   zellij:terminal_4
 process                -         ~/code/qe-wt/auth-refresh   zellij:terminal_5
```

The agent labels are a best-effort overlay folded from the workspace snapshot, so a pane the multiplexer has handed back to a shell reads `process`; the tab grouping always works, even with no snapshot reachable. `--json` emits the tab tree with a per-pane `kind`, `command`, `cwd`, and `pid`, and an `agent` object for agent panes.

`capture` prints visible pane text and changes nothing. `send` types literal text and named keys in order — the write your keyboard would make. `focus` moves attention. These three target either a pane id or an agent address, and pane ids choose their own backend (`tmux:%3` uses tmux, `zellij:terminal_4` uses Zellij) instead of the ambient session. Named keys are `enter`, `escape`, `tab`, `shift-tab`, `backspace`, the four arrows, `ctrl-c`, `ctrl-d`, and `ctrl-u`, with aliases like `return`, `esc`, `backtab`, and `bs`.

`split` opens a shell beside the current pane along its longer visual edge, matching the room's native new-pane behavior. `detach` detaches the attached client; the session keeps running in the background and comes back on the next attach.

Because `pane capture` returns untrusted terminal text, a script should match bounded patterns before sending anything back. The operator-facing safety rules are in [security.md](../../guide/security.md).
