# Sandbox rooms

A sandbox room is a disposable RimZ room built from your checkout. `cargo xtask sandbox room` starts it on a private tmux or Zellij server under a scratch root with its own `HOME`, prints a card of ready-made commands, and holds it until a timer runs out. Nothing it does reaches your real rooms, multiplexer servers, or provider logins. Use one for any live check of a change: the [sidebar live check](./sidebar-live-check.md) is the most common, and hooks, loops, workflows, and hibernation are checked the same way. [probes.md](./probes.md) covers checks you build by hand without a room.

## Hold a room

The room verb is Linux-only. From an agent's tool-sandboxed shell, run every command on this page unsandboxed (escalated): inside the tool sandbox the build cannot reach the target directory, and tmux socket connects fail with `Operation not permitted`.

1. Build the testkit binary from the checkout root:

   ```sh
   cargo build -p rimz --bin rimz --features testkit
   ```

   Without it, `sandbox room` refuses with `development rimz missing`.

2. Hold the room:

   ```sh
   env -u NO_COLOR cargo xtask sandbox room --mux tmux
   ```

   `env -u NO_COLOR` keeps colours and the sidebar's pixel graphics when your shell exports `NO_COLOR`. Use `--mux zellij` for the other backend.

The command prints the room card and holds the room for 30 minutes. Pass `--for 45m` for another length, or `--for off` to hold it until you stop it. Keep the command running while you work in the room. `RIMZ_BIN` names another `rimz` binary to run in place of `target/debug/rimz`.

Every held room has the same contents:

- a Git repository at `<root>/home/room`, the room's project
- the team `forge` launched on the worktree `<root>/home/room-worktrees/probe`, with `@coder#probe` and `@reviewer#probe`
- a board on the `Build` stage, owned by `coder`
- three tabs, each with a sidebar: `rimzd` (the daemon tab), `rimz` (a shell), and `#probe` (the team)
- a stub `claude` (`xtask/assets/sandbox-room/claude`) in place of the real provider: it answers `--version` and never takes a turn

A room that fails to start keeps its sandbox root at the path the error prints, with the startup error in `<root>/kept`. Its servers and processes are already stopped. Remove the root with the printed `rm -rf` command when you are done reading it.

## Read the room card

The card has one block per sidebar and one per agent. A tmux card reads like this (paths shortened):

```text
Sandbox root: /tmp/rimz-sandbox-AcUCxg
Mux: tmux  Session: room-6e59
Worktree: /tmp/rimz-sandbox-AcUCxg/home/room-worktrees/probe
Sidebar: tmux:%3  Tab: rimzd
  Look: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' -- '…/target/debug/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'
  Capture: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' -- '…/target/debug/rimz' --tmux pane capture 'tmux:%3'
  Click: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' -- '…/target/debug/rimz' --tmux sidebar click 'tmux:%3' 2 "${ROOM_ROW:?set ROOM_ROW to the 0-based pipeline row from a fresh capture}"
…
Role: @coder#probe  tmux:%6  pid 3452131
  As: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' --as '@coder#probe' -- '…/target/debug/rimz' --tmux
…
Stage: Build  Owner: coder
Flip: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' -- '…/target/debug/rimz' --tmux teams flip Review 'live check' --team forge
Focus: target/debug/xtask sandbox in '/tmp/rimz-sandbox-AcUCxg' -- tmux -S '/tmp/rimz-sandbox-AcUCxg/runtime/rimz/tmux/server' display -p '#{pane_id}'
Run a sidebar's Look first, then capture or click it: an unwatched sidebar can hold a stale frame.
```

| Line | What it gives you |
| --- | --- |
| `Sandbox root` | The scratch root. Every other path in the room sits under it. |
| `Worktree` | The team's checkout, and the directory `sandbox in` runs in by default. |
| `Sidebar` | One sidebar pane and its tab. A tab can carry more than one sidebar, so address a sidebar by its pane id. |
| `Look` | Brings the sidebar's tab into the client's view: `pane focus` on tmux, `zellij action go-to-tab` on Zellij. |
| `Capture` | Prints the sidebar's current frame. Add `--ansi` to keep colours. |
| `Click` | Sends a click at column 2 of the row in `ROOM_ROW` (testkit-only). |
| `Role`, `As` | An agent's pane and process id, and a prefix that runs a RimZ command as that agent. |
| `Stage`, `Flip` | The board's stage and owner, and a command that moves it to `Review`. |
| `Focus` | Reads which pane has focus from the multiplexer. On Zellij it lists every pane, with `is_focused` per tab. |

A Zellij card prints pane ids as `zellij:terminal_<n>` and `--zellij` in place of `--tmux`. Its Look runs `zellij --session <session> action go-to-tab <n>`, and its Focus runs `zellij --session <session> action list-panes -a -j`.

Paste commands from your own card, since the root, pane ids, and binary path differ per room. The card spells the verb `target/debug/xtask` because the held `cargo xtask` owns Cargo's lock for as long as it runs: a second `cargo xtask` waits for the room to end.

## Run a command in the room

```text
target/debug/xtask sandbox in <root> [--cwd <dir>] [--as <@handle> | --as-ancestor <@handle>] [--] <command>
```

`sandbox in` runs one command with the room's `HOME`, `RIMZ_HOME`, and XDG roots. It starts in the card's Worktree. Pass `--cwd "$ROOT/home/room"` to run in the room's project, which you also need once a check removes the worktree. The command's `PATH` starts with the stub directory, then the directory of the room's `rimz`. A relative program path that contains `/` resolves from your checkout. A path inside `env` or `sh -c` resolves in that command instead, so pass the RimZ binary there by absolute path.

The environment drops your Zellij and tmux session keys and every ambient `RIMZ_*` variable, so a switch set in your shell never reaches the room. Write room settings into the room's home instead ([Set up the room](#set-up-the-room-for-a-check)).

A command that exits nonzero makes `sandbox in` exit 1 and print `sandboxed command ... exited with ...`. To check an exit code, record it inside the sandbox: `-- sh -c '<command>; echo $? > <file>'`.

Two shell functions save typing for the rest of a check. Keep them as functions: zsh does not split an unquoted variable into words, so a prefix held in a variable fails as one unknown command.

```sh
ROOT=/tmp/rimz-sandbox-AcUCxg   # the card's Sandbox root
BIN=$(jq -r .binary "$ROOT/room.json")
MUX=$(jq -r .mux "$ROOT/room.json")
rz() { target/debug/xtask sandbox in "$ROOT" -- "$BIN" "--$MUX" "$@"; }
a() { target/debug/xtask sandbox in "$ROOT" --as '@coder#probe' -- "$BIN" "--$MUX" "$@"; }
```

`rz` runs RimZ as a user shell in the room, and `a` runs it as `@coder#probe`.

## Act as an agent

A command that needs to know its calling agent (`rimz wait`, `rimz subagents`) refuses from plain `sandbox in`:

```console
$ rz wait --check true
error: arming a wait is only available to an agent RimZ can identify; run this command from an agent pane
```

`--as <@handle>` copies that agent's session keys into the command's environment and sets `RIMZ_AGENT_PID` to the stub, so hooks it feeds are attributed to the agent. Use a qualified handle (`@coder#probe`) when the short form is ambiguous. `--as-ancestor <@handle>` is meant to run the command as a child of the stub with no agent keys, to exercise caller detection by process ancestry. Today it refuses with `agent pid <pid> has no serving seat`, because the stub creates its seat in its own temporary directory and `sandbox in` looks for it under `<root>/tmp`.

The stub never starts a provider session, so the agent stays provisional until you feed one:

```console
$ a wait --check true
error: the calling agent has not registered a real session yet
```

Feed a session with [`scripts/live-check/feed-claude-turn.sh`](../../scripts/live-check/feed-claude-turn.sh). With no steps after the handle, it sends one finished turn: `SessionStart`, `UserPromptSubmit`, a statusline with a context reading, and `Stop`. The room applies a hook a moment after the feed returns, so retry the first command after a few seconds if it still refuses:

```console
$ scripts/live-check/feed-claude-turn.sh "$ROOT" '@coder#probe'
session b9d9a012-6a4f-42ea-96a6-fc76ed0c5e7b
$ a wait --check true
armed wait-exact-square: check: true → @coder#probe
@coder#probe · 1 wait
  wait-exact-square  starting  true
```

The script's header lists its other steps: `start` alone registers a session without a turn, `window` gives the meter a window size without a reading, and `compact=N` feeds a finished compaction that leaves `N` tokens. Set `SESSION` to feed more steps into the same session later. A SessionStart alone leaves the card fresh, with no subagents or waits line. Feed a whole turn before you check either.

Other agent states take a hand-fed hook through `a hooks feed --source claude`:

- To raise an ask, feed a `PermissionRequest` event whose `tool_name` is an ordinary tool such as `Bash`. A `Notification` event changes nothing on the card.
- To drive a workflow step agent, feed it as its room handle from `rz agents`, not the pet name `workflow show` prints. After a rest check pushes back, the next `UserPromptSubmit` must carry the pushed message's exact text as `prompt` (read it with `rz message show <id>`).
- To show another provider's card, copy the stub to `$ROOT/home/bin/<kind>` and make its `--version` print a version that provider's adapter accepts. Then run `rz hooks install <kind>`, launch it with `rz agents launch <kind> --isolation host -w probe`, and feed it through the handle `rz agents` lists.
- Session-death recovery needs a SessionStart for every agent. A room of provisional sessions publishes an empty live roster, so the next `rimz start` has nothing to recover.

The room restarts a stub whose process is killed, so an agent leaves its tab only when its pane closes. Closing a stub's pane ends its row, and its handle then matches no agent.

## Set up the room for a check

`rz config set <key> <value>` writes the room's own settings: `theme.*` keys go to `$ROOT/home/.rimz/theme.toml` and the rest to `$ROOT/home/.rimz/config.toml`. `rz doctor` confirms both parse. A running room picks most settings up within a few seconds. A setting that must hold from the room's birth needs `RIMZ_BIN` to name a wrapper script that runs `config set` before it execs the real binary on `--tmux start` or `--zellij start`.

Each kind of launch has its own preconditions:

- The room installs hooks for `claude` only. Run `rz hooks install <kind>` before launching any other kind, and before a loop task's supervised launch fires.
- The room's project has no folder-trust decision, so a loop launch or `a subagents claude ...` refuses with `claude has no trust decision`. Run `rz trust grant --agents claude` first. A bare `rimz trust grant` from your own shell writes your real provider config.
- The team runs with `--isolation host`, and `rimz agents restart` keeps a recorded isolation. For a scoped agent, launch a new one: `rz agents launch claude --isolation scoped -w probe`.
- `rimz agents restart` of a stub prints `restarted fresh` and resumes nothing, since the stub writes no conversation. A check of the resume path needs a [host room](#check-against-a-real-provider).

On Zellij, a command run without `--zellij` (such as a `Resume:` line a receipt prints) picks tmux, since `sandbox in` drops the session keys. To run a printed command as a user would, add `default = "zellij"` under `[mux]` in `$ROOT/home/.rimz/config.toml`.

A check that needs a second room starts it in the same sandbox:

```sh
mkdir "$ROOT/home/probe2"
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- git init -q .
target/debug/xtask sandbox in "$ROOT" --cwd "$ROOT/home/probe2" -- "$BIN" "--$MUX" start "$ROOT/home/probe2" --no-attach
```

Commands run with `--cwd "$ROOT/home/probe2"` then resolve that room. A Zellij room started with `--no-attach` has no client, and Zellij builds a new tab's panes only for an attached client, so tab launches into it fail with `layout panes did not materialize`. Attach a client first, for example from a detached tmux session running `"$BIN" --zellij attach <session>`.

To keep a room on a fixed build while the checkout rebuilds, copy `target/debug/rimz` and `target/debug/xtask` into a directory `P` as `P/debug/rimz` and `P/xtask`. Then run every sandbox verb as `CARGO_TARGET_DIR=P P/xtask sandbox ...`. Keep the copy's file name `rimz`: RimZ recognises an agent pane's `rimz agents exec` wrapper only under that name.

A resume check that seeds a Claude transcript writes it at `$ROOT/home/.claude/projects/<dir>/<id>.jsonl`. `<dir>` is the session's working directory with every character other than an ASCII letter or digit replaced by `-`. `rimz agents resume` looks only in that folder and refuses a transcript placed anywhere else with `no saved conversation`.

[hibernate.md](../internals/harness/hibernate.md#live-check) gives the two room changes a hibernation check needs.

## Check against a real provider

A held room replaces `HOME`, so no provider login is reachable inside it and no real Claude or Codex turn can run there. Such a check runs in a host room started from the branch binary, and only the user can start one: `rimz start` refuses inside an agent's scoped view. A plan that rests on a real turn raises it with the user first. From a host shell:

```sh
cargo build -p rimz --bin rimz
mkdir -p /var/tmp/rimz-livecheck/bin /var/tmp/rimz-livecheck/ws
cp target/debug/rimz /var/tmp/rimz-livecheck/bin/
export RIMZ_BIN=/var/tmp/rimz-livecheck/bin/rimz PATH=/var/tmp/rimz-livecheck/bin:$PATH
cd /var/tmp/rimz-livecheck/ws
rimz trust grant --agents claude,codex
rimz start --tmux
```

Every `rimz` that room spawns must be the branch build: the shell that sends messages, the provider hooks (which resolve `rimz` from the agent's `PATH`), and the sidebar host (which reads `RIMZ_BIN`). Before you read a result, run `command -v rimz` and `rimz --version` in the sending shell. A shell rc that puts `~/.cargo/bin` first runs the installed release instead, and the check then passes or fails on the wrong binary.

An OpenCode turn can run in a held room once the user imports a login into it. OpenCode 2.x keeps its login in a SQLite database under `XDG_DATA_HOME`, so copying `auth.json` brings none. `opencode auth export` prints secrets, so the user runs the import from a host shell, with `R` the room root as the host sees it (`~/.rimz/ws/<workspace>/tmp/<agent temp unit>/rimz-sandbox-XXXXXX` for a room an agent holds):

```sh
opencode auth export | env HOME="$R/home" XDG_DATA_HOME="$R/data" XDG_CONFIG_HOME="$R/config" XDG_STATE_HOME="$R/state" XDG_CACHE_HOME="$R/cache" opencode auth import --standalone
```

Then write a nonempty kind base at `$ROOT/home/.rimz/agents/opencode.md` and run `rz hooks install opencode` before the first `a subagents opencode`. The room root now holds the login: delete it after the check, and confirm the host path is gone too.

## Stop the room

The room ends when `--for` runs out, or when you stop the `xtask sandbox room` process. Ctrl-C works, and so does a signal to its PID or its process group, since the cleanup reaper runs outside that group. Killing a wrapper shell leaves the room running. Find the PID with `pgrep -x xtask`, never with `pkill -f` or `pgrep -f`: the pattern also matches the shell that runs it, and the kill takes that shell down. Cleanup stops both multiplexer servers and every process, then removes the root.

## Traps

- A live check of anything the host, a hook, or a loop fire spawns must run in a sandbox room. In your real room those children are the installed `rimz`, so the check exercises the released binary and passes either way.
- Wrapping `sandbox room` in your own `bwrap` needs `--dev-bind /dev /dev`. Without it the room exits before it starts, with `starting sandbox cleanup reaper` and `Permission denied (os error 13)`.
- `ssh` reads `~/.ssh` from your passwd home, not `$HOME`, so an SSH command run in the room uses your real keys and config.
- To check a signal emitter from an agent's shell tool, keep the call alive until the run is recorded: `a events emit <signal>; sleep 2; a loop show <task> --json` in one call. An emit whose call ends at once can record no run, for a reason not established. Judge delivery by the run record or `rz message show <id>`, never by the emitter's `fired` count.
- `rimz stats --refresh --hold` needs a terminal and exits with `No such device or address (os error 6)` under `sandbox in`. Give it one with `script -qfc '<bin> stats --refresh --hold' <file>` inside the sandbox.
