# Hand-built probes and live checks

What a probe or live check you build by hand needs so that it measures the thing you mean and leaves the user's machine alone: a private multiplexer, a fixture repository, a provider CLI, an isolated home, or a branch binary run against real state. The sandbox room procedures are in [sandbox-rooms.md](./sandbox-rooms.md), and the integration suite's own fixtures are in its [suite contract](../../crates/rimz/tests/integration/AGENTS.md) and [integration-tests.md](./integration-tests.md).

## Sandbox rooms

- `cargo xtask sandbox -- <command>` drives a real multiplexer from disposable roots.
- To run a command as a room agent, hold a room with `sandbox room` and use `sandbox in <root> --as @handle` ([recipe](./sandbox-rooms.md#act-as-an-agent)).
- From an agent pane, run a live room check through `cargo xtask sandbox room`. A bare `rimz start` refuses inside an agent pane's own sandbox.
- A hand-built check of a command that resolves a room (`rimz worktree new`, `rimz workflow run`) runs the same way, through `sandbox in` on a held room. In a bare repository it refuses with `no room at <root>`.
- A check that needs a real Claude or Codex turn cannot run in a sandbox room, which replaces `HOME` and holds no provider login. It needs a host room that only the user can start ([real provider](./sandbox-rooms.md#check-against-a-real-provider)). A plan or hand-off that rests on such a check raises it with the user first.
- `cargo xtask sandbox -- rimz <args>` runs an interactive smoke check of a built binary. The sandbox puts this checkout's build directory (`$CARGO_TARGET_DIR/debug`, or `target/debug`) first on the command's `PATH`, so `rimz`, as the program or inside a nested shell, is this checkout's build and not the installed release. With no build there, a bare `rimz` refuses and names `cargo build -p rimz --bin rimz --features testkit`. It builds nothing, so rebuild with that command after every edit, commit, or rebase. `rimz --version` in the sandbox shows `+g` and the commit the binary was built from, which must match `git rev-parse --short=12 HEAD`.
- `sandbox in` puts the held room's stub directory first, then the directory of the room's binary. A program named by path runs as written.
- The sandbox replaces HOME, every persistent XDG root, `TMUX_TMPDIR`, the Zellij runtime and config roots, and `TMPDIR`. It tears down both private mux servers when the command exits. It drives both multiplexers from inside a scoped agent pane.
- A bare `rimz start` outside `cargo xtask sandbox` is a real product invocation, and it addresses the user's normal multiplexer server.

## Multiplexers

- A hand-built Zellij probe runs under `env -u ZELLIJ_SOCKET_DIR -u ZELLIJ -u ZELLIJ_SESSION_NAME` with its own `XDG_RUNTIME_DIR`. The inherited socket dir otherwise addresses the user's live sessions.
- Zellij tries `$HOME/.config/zellij` before `$XDG_CONFIG_HOME/zellij`. A probe that seeds a user config writes it under the isolated `HOME`, or points `ZELLIJ_CONFIG_FILE` at it.
- On 0.45.1, `zellij --layout <file> attach --create <name>` births the session with Zellij's default layout (tab-bar, status-bar, one pane), and ignores the file. Attach first, build the probe's tabs with `zellij --session <name> action new-tab --layout <file>`, close the bootstrap tab, and count tabs and panes before you time anything.
- A hand-built tmux probe runs as a bash script under `tmux -L <name> -f /dev/null` with `TMUX_TMPDIR=/tmp/<8 chars>`. The session `TMPDIR` makes the socket path exceed the 108-byte limit, and zsh does not word-split an unquoted variable that holds several arguments.
- A hand-built tmux probe unsets `TMUX` and `TMUX_PANE`, and stops its server by socket after checking its pid with `display -p '#{pid}'`: `env -u TMUX -u TMUX_PANE tmux -S <socket> kill-server`. A `tmux` call with no `-L` or `-S` follows an inherited `$TMUX` before `TMUX_TMPDIR`, so a bare `kill-server` in an agent pane stops the user's live server.
- `new-window -t` names the session with a trailing colon (`-t s:`). A bare session name resolves to its current window and fails with `index 0 in use`.
- A probe room of hundreds of panes runs in its own user and mount namespace with a private devpts instance. The host's shared devpts caps PTYs at its `max=` mount option (1,026 on the shared development host), counting every other agent's panes, and past it `new-window` fails with `fork failed: No space left on device`. `grep devpts /proc/mounts` shows the cap. This opens a shell with room for 2,048 PTYs:

  ```sh
  unshare --user --map-root-user --mount sh -c '
    mount -t devpts -o newinstance,ptmxmode=0666,max=2048 devpts /dev/pts
    mount --bind /dev/pts/ptmx /dev/ptmx
    exec "$SHELL"
  '
  ```

- A probe that reads the drawn status row or sends mouse clicks attaches its client through a Python `pty` driven by `uv run --with pyte`.
- A probe of key tables writes real bytes through a Python `pty` client. `send-keys -K` left `client_key_table` at `root` and sent the keys to the pane.
- A pty that does not answer tmux's terminal queries holds a bare `Esc` for 500 ms whatever `escape-time` says, so wait longer than that before you read the result.
- A probe of a command that RimZ sends through `TmuxBackend::run_script` feeds the same single-quoted words to `source-file -`. Shell argv parses `;` and `\;` in a `bind-key` body differently: argv `M-\;` binds `M-`, while source-file `'M-\;'` binds `M-;` (tmux 3.8).
- A probe of a key RimZ presses through tmux sets the server options a room sets, which `tmux_server_options` and `tmux_terminal_features_commands` in [`mux/tmux/options.rs`](../../crates/rimz/src/mux/tmux/options.rs) resolve (`extended-keys`, `extended-keys-format`, `escape-time`, `terminal-features`). With extended keys on, tmux encodes a modified key in the mode the application requested: a room sends `M-m` to agy as `ESC[109;3u`, a `-f /dev/null` server as ESC `m`.
- Where Zellij places a pane depends on the session options RimZ fixes at birth, which `zellij_session_options` in [`mux/zellij.rs`](../../crates/rimz/src/mux/zellij.rs) resolves: `auto_layout false`, `stacked_resize true`, and `stacked_pane_list false`. A hand-built placement probe sets them, as top-level lines in its layout or `config.kdl` the way the birth layout carries them, and an upstream source read about placement states them. Without them, either describes a session RimZ never runs.

## Git and Cargo

- A hand-built probe changes directory only in a subshell (`( cd "$D" && git commit ... )`) or names the directory with `git -C "$D"`. Some agent shells carry a bare `cd` into every later call, so the next command runs in the fixture and a later `git` writes there or misses the checkout.
- It expands that directory as `"${D:?}"` and runs under `set -eu`. `git -C ""` runs in the current directory, so an empty lookup commits into the checkout.
- A hand-built git index probe makes entries stat-stale with past mtimes (`touch -d '-1 hour'`). A future mtime leaves an entry racily clean, so every status rehashes it and the timings mislead.
- Under `GIT_TRACE2_EVENT=1`, status reports how many entries it rehashed as the `refresh/sum_scan` data event on stderr.
- A rule about which git commands take `index.lock` is tested in a superproject with an initialized submodule, since git passes `GIT_OPTIONAL_LOCKS` down to the submodule's own commands.
- A rule that treats some of a tool's output codes differently from others (porcelain status codes) gets one case per code, run against the real tool. A hand-written output string tests the parser and not the tool.
- A rule that takes a git file's mtime as a signal for HEAD or the refs is probed in each ref backend (`git init --ref-format=files|reftable`) and each HEAD state (loose branch, packed branch, detached, unborn). The file is also polled in the live checkout over a few idle minutes (`stat -c %Y`).
- Under reftable the `HEAD` file is a fixed `ref: refs/heads/.invalid` stub.
- `git gc --auto` from any worktree runs `reflog expire --all`, which rewrites every worktree's `logs/HEAD` and both reftable stacks with unchanged content. It also runs `pack-refs --auto`, which packs every worktree's branch once loose refs pass about `5 * log2(packed-refs bytes / 100)` (at least 16 on git 2.48). A checkout over its loose-object threshold runs it about once a minute.
- To find why Cargo reran a build script, compare each `rerun-if-changed` path in `target/debug/build/<crate>-*/output` (`stat -c %.9Y`) with the `invoked.timestamp` file beside it: a newer path is the trigger. `CARGO_LOG` fingerprint logging shows nothing through `cargo xtask`, which keeps only its summary line.

## Processes and SSH

- A probe that checks a killed child is gone gives the child a unique argv token (`sleep 347`, never `sleep 30`) and searches for that token. The host runs other agents' identical commands, and their `/proc/<pid>/environ` is unreadable across sandboxes.
- That unreadability says nothing about the process itself. To tell whether a process's own children can read its environment, check the file's owner with `stat -c '%U %a' /proc/<pid>/environ`. proc(5) makes it root for a non-dumpable process and the process's user otherwise.
- A probe that reads `/proc/<pid>/fd` or `/proc/<pid>/cwd` of a process outside your agent's sandbox runs on the host, through the room's tmux server: `tmux -S <runtime root>/tmux/server run-shell "sh $RIMZ_SHARED/<task>/probe.sh > $RIMZ_SHARED/<task>/probe.out"` (the socket is in the [paths table](../reference/cli/paths.md#the-table)). The sandbox's own user namespace is denied those reads (`Permission denied`, while `cmdline` still reads), and its `/tmp` is private, so the host shell finds no script there. Keep the script and its output under `$RIMZ_SHARED`, which names the same host path inside and outside the sandbox. A file you pass by flag to a branch binary goes under `$RIMZ_SHARED` too.
- A probe that signals a process takes the pid from its own child (the pid its spawn returned, or a pid that child printed), never from a match on `ps` or `pgrep` text: other worktrees run the same binaries.
- Stop a process you started by its PID (`$!`), never with `pkill -f PATTERN`, which matches the shell that runs `pkill` and kills it.
- Find a running build by process name (`pgrep -ax cargo`), never with `pgrep -af PATTERN`. An agent's command line quotes its whole brief, so `-af` matches every agent whose brief names the tool.

A hand-built `rimz remote connect` probe against real OpenSSH runs its own sshd. This block writes everything under `$TMPDIR/sshd-probe`, starts the server on `127.0.0.1`, and points `RIMZ_SSH_BIN` at a wrapper that reaches it as the host `probe`. Set `port` to any free high port.

```sh
set -eu
P="$TMPDIR/sshd-probe"
port=42222
mkdir -p "$P/remote-home"
ssh-keygen -q -t ed25519 -N '' -f "$P/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$P/client_key"
cp "$P/client_key.pub" "$P/authorized_keys"
cat > "$P/force.sh" <<EOF
#!/bin/sh
exec env -i HOME="$P/remote-home" PATH="\$PATH" sh -c "\${SSH_ORIGINAL_COMMAND:-exec sh -l}"
EOF
chmod +x "$P/force.sh"
cat > "$P/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $P/host_key
PidFile $P/sshd.pid
AuthorizedKeysFile $P/authorized_keys
StrictModes no
UsePAM no
ForceCommand $P/force.sh
EOF
grep -v '^tty:' /etc/group > "$P/group"
bwrap --dev-bind / / --ro-bind "$P/group" /etc/group -- /usr/sbin/sshd -D -e -f "$P/sshd_config" 2> "$P/sshd.log" &
cat > "$P/ssh_config" <<EOF
Host probe
  HostName 127.0.0.1
  Port $port
  User $(id -un)
  IdentityFile $P/client_key
  IdentitiesOnly yes
  UserKnownHostsFile $P/known_hosts
  StrictHostKeyChecking accept-new
EOF
cat > "$P/ssh" <<EOF
#!/bin/sh
exec ssh -F "$P/ssh_config" "\$@"
EOF
chmod +x "$P/ssh"
export RIMZ_SSH_BIN="$P/ssh"
```

`"$RIMZ_SSH_BIN" -tt probe tty` prints a `/dev/pts/N` path when the server works. The `ForceCommand` script runs each session under `env -i` with `$TMPDIR/sshd-probe/remote-home` as its home. Stop the server by the PID in its `PidFile`:

```sh
kill "$(cat "$TMPDIR/sshd-probe/sshd.pid")"
```

- The group copy is needed because the agent sandbox leaves gid 5 unmapped. A plain sshd aborts every PTY session with `chown(/dev/pts/N, <uid>, 5) failed: Invalid argument`.
- An agent on Codex cannot run this probe. Its sandbox refuses TCP sockets (`socket(AF_INET, SOCK_STREAM)` fails with `Operation not permitted`), so sshd exits with `Cannot bind any address.`. Run the probe from a Claude agent's pane. Unix sockets work under Codex, so a Codex agent still runs the tmux and Zellij integration tests.

## Provider CLIs

- A check of Claude alone (a mod, a flag, a hook payload) needs no room. Run it from a Claude agent's pane, on the login its shell already exports: start `claude` under `env -i` with `PATH`, `HOME`, `TERM` and that `CLAUDE_CONFIG_DIR`, from a scratch directory under `$TMPDIR`, with `--model haiku` and one-line prompts. Put the prompt before a variadic flag such as `--tools`, which otherwise takes the prompt as a value.
- Pass `--setting-sources project,local`, so the account's own hooks, `env`, and permission rules (which may deny the native `Agent` tool) stay out.
- A dialog check drives the same command in a private tmux server (`tmux -L <name> -f /dev/null`, short `TMUX_TMPDIR`).
- A check that needs `ExitPlanMode` drives an interactive session started with `--permission-mode plan` in the private tmux server: print mode (`-p`) does not offer the tool. A scratch directory whose `.claude/settings.json` declares hooks asks for folder trust first. Approving the plan writes a plan file under the account's `plans/` directory: remove it afterwards.
- Run it yourself: a subagent on another provider has no Claude login. It spends the user's real usage, so keep it to the questions the change rests on.
- Print mode (`-p`) offers no `REPL` or `Poll` tool, and a hand-started interactive session runs `REPL` blocking, with no dispatch receipt. To learn a dispatch-mode tool response, read `toolUseResult` in a room Claude agent's own transcript: it is the object PostToolUse carries as `tool_response` (Claude 2.1.330).
- To read a closed-source CLI's bundled JavaScript (`~/.local/share/claude/versions/<v>`), find literals with Python `mmap.find` and print a window around each hit: `grep -a -o` with a context regex takes minutes on the binary.
- To find the first Claude Code version carrying a literal, bisect the per-platform npm packages, which ship the native build from 2.1.110 on: list them with `npm view @anthropic-ai/claude-code-linux-x64 versions --json`, and test one with `curl -sfL https://registry.npmjs.org/@anthropic-ai/claude-code-linux-x64/-/claude-code-linux-x64-<V>.tgz | tar -xzOf - | rg -a -c -F '<literal>'`. Read the code around the first hit before you call it the behavior's first version.
- To hold a Claude turn on a tool call, a probe asks Claude to run a script file that loops `sleep 1` and prints a line per step: Claude 2.1.332 refuses a standalone `sleep` and a sleep routed through another interpreter.
- A check of agy (Antigravity) alone needs no room and no model turn. Run `agy` under `env -i` with `PATH`, a scratch `HOME`, its own `RIMZ_HOME`, and a short `XDG_RUNTIME_DIR`, from a scratch directory under `$TMPDIR`. A logged-in check copies `~/.gemini` into the scratch home, removes that home in a cleanup that runs on every exit, and lists the path afterwards to confirm it is gone. On a host whose agy logins are RimZ named accounts, `~/.gemini` can read signed out, and the login is then the named account's token that the quota bullet below names.
- A scratch agy home opens a data-use consent screen with its box ticked. Copy `~/.gemini/antigravity-cli/cache/onboarding.json` into the scratch home's `cache/` so the probe answers no consent for the user (1.5.17). Run agy in Auto mode with `permissions.allow` rules for the commands the probe needs, not `--dangerously-skip-permissions`, which a permission check on the launching agent may refuse.
- Print-mode slash commands (`agy -p /usage --output-format json`), `agy plugin validate`, and `agy plugin list` spend no quota.
- A check that spends a model turn first runs `agy -p /usage --output-format json` in the scratch home and reads, without printing the body, whether the model it needs has quota left. A login whose quota is spent fails the turn with `RESOURCE_EXHAUSTED (code 429)`, which says nothing about the model. Copy a [named account](../guide/accounts.md)'s `~/.rimz/accounts/antigravity/<name>/antigravity-oauth-token` over the scratch home's `.gemini/antigravity-cli/antigravity-oauth-token` instead.
- A capture's notes name the login kind it ran on (default or named account) and each model it tried, and a model that failed on quota is recorded as untested, never as a negative result.
- A logged-out `agy -p` waits 60 s and opens a browser unless its environment carries an SSH session variable (`SSH_CONNECTION`, `SSH_TTY`, or `SSH_CLIENT`) and its stdin delivers a newline.
- Never print the token, a `/usage` response body, or the account email.
- A provider's raw hook and statusline frames carry that email too (agy's carry an `email` key). Look at a frame only through a filter that drops the `email` key and any value holding `@`, before the first print.
- agy logs each session under `~/.gemini/antigravity-cli/log` (grep `agent executor error` for a stopped turn), and RimZ archives drained hook events under the workspace's `log/archive/events.*.jsonl`: search both before planning a capture of a provider stop.
- agy's TUI prints the signed-in account on its start screen, so read a pane capture of it only through a filter that drops every line holding `@`. Apply that filter to agy's output alone: RimZ prints an account as `<kind>@<name>`, so the same filter over `rimz` output drops its account rows and receipts.
- A check that drives one of agy's full-screen panels (`/tasks`) runs agy in a private tmux session of at least 120 by 30. A subagent's own pane can be one row high, where the panel draws nothing and the keys go in blind.
- A probe of a bind onto a path that may be missing records the mode of what bubblewrap leaves there, beside what the provider reads from it. Bubblewrap creates a missing file destination as an empty `0444` file (0.11.1), which a later in-place write cannot open.
- A check of how agy behaves for a RimZ launch runs in the TUI with the first prompt passed by `--prompt-interactive`, and reads the first turn and the second: print mode (`-p`) can differ. On 1.5.13 an inline `mcpServers` server of a custom agent answers a call on the first turn in print mode and only from the second turn in the TUI, and the TUI half held on 1.5.14, while a plugin `mcp_config.json` server answers on the first TUI turn (1.5.14).
- A probe of agy's outside-workspace prompt edits an existing line of a file that sits outside the temp tree and outside any repository, and passes `"allowNonWorkspaceAccess": false` in `--settings`, which overrides a saved `true`. Run the case with no allow rule first: it must prompt. In Auto mode agy edits a file under `/tmp` with no prompt and no rule (1.5.16), so a target under `$TMPDIR` passes for the wrong reason.
- A check of Auto mode runs in the TUI and reads the mode off the footer: `agy -p --mode auto` warned that Auto is unavailable and ran in Default (1.5.17). A sandbox check keeps its fixture outside `/tmp`, which agy opens; on a Rust checkout, `read_file` rules for the real `~/.cargo` and `~/.rustup` with the toolchain's `PATH` were verified only with a prebuilt `xtask` binary, not with `cargo` itself.
- A probe of a key or command RimZ will send to a provider TUI runs the sequence RimZ sends around it (for an interrupt: the interrupt keys, the wait for the stop, the paste, then the send-now keys), and records what became of the text as well as of the turn. A key pressed alone can stop a turn and still return the queued text to the composer unsent.
- A probe of a new way to submit text into an agent also runs once in a disposable room ([sandbox-rooms.md](./sandbox-rooms.md)) and reads the message record to its terminal status. A submit the provider reports through no turn-start hook leaves the record `Sent`, and reconciliation pastes the text again.
- A mid-turn agy probe first answers the folder-trust prompt of a fresh scratch folder (Enter), and keeps the turn running by asking for `run_command` with `WaitMsBeforeAsync` set past the command's length. Otherwise agy backgrounds the command, the turn yields, and text typed afterwards opens a new turn instead of queueing (1.5.14).
- A check that Codex accepts a `-c` config key needs no login. `codex -c <key>=<value> features list` prints a feature's resolved value. `codex --strict-config -c <key>=<value>`, started in a private tmux (`tmux -L <name> -f /dev/null`, short `TMUX_TMPDIR`), either refuses the key or reaches the sign-in screen. `features` rejects `--strict-config`, so the two checks stay separate.
- A logged-in check against an account home under `~/.rimz/accounts/codex/` is the user's to allow.
- A live Codex turn needs a Codex login. A Claude agent's shell exports no `CODEX_HOME`, and `~/.codex` holds a login only while the user's default Codex account is signed in (a bare `codex exec` there answers `401 Unauthorized` otherwise). A plan that asks for a live Codex turn names a Codex agent to run it on its own login, or raises it with the user first.
- A hand-built Claude transcript for a spend probe is written as compact JSON (no space after `:` or `,`) at a new path: an existing path is a rewrite merge that keeps no rows older than eight days, and spaced JSON fails the parser's byte pre-filter.
- Before tearing down a probe's sandbox or scratch home, copy every provider transcript, raw hook payload, store event log, and the prompt of each case into the shared task directory, beside the log that cites them.

## Isolated homes

A hand-built isolated home sets or unsets each of these. An agent shell exports its own account's values, and a probe that isolates only `HOME` still reaches the user's real state through the rest.

| Variable | Do | Otherwise |
| --- | --- | --- |
| `XDG_RUNTIME_DIR` | Use a fresh, short one (`/tmp/<8 chars>`) | RimZ refuses a runtime root whose socket paths exceed 108 bytes |
| `RIMZ_HOME` | Set it | The real one wins over `HOME`. The probe resolves the real account homes, and a `rimz trust grant` writes the user's real provider config |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | Unset them, or point them inside the isolated home | The `default` login reads the agent's own account directory, and the probe writes that account's `.claude.json` |
| `XDG_CONFIG_HOME` | Unset it, or point it inside the isolated home | Cursor and Muse hooks resolve their config there, not under `HOME` |
| `XDG_DATA_HOME` | Unset it, or point it inside the isolated home | OpenCode's database and OMP resolve their data there, so a spend probe reads the real account's history |
| Every other `RIMZ_*` variable | Unset it, as `common::ScrubSessionEnvExt` does, or run under `env -i` | The room identity pin (`RIMZ_PROJECT_ROOT`, `RIMZ_WORKSPACE_ID`, `RIMZ_CHANNEL`, `RIMZ_WORKTREE_PATH`) makes the probe resolve the agent's own project, and `rimz loop list --all` lists that project's tasks |
| `RUSTUP_HOME`, `CARGO_HOME` | Pass them through, pointing at the real toolchain, for a probe that runs a Rust toolchain or rust-analyzer | An isolated `HOME` has no rustup default, so the server crashes with `rustup could not choose a version` |

- Run the probe's commands from a fresh directory under `$TMPDIR`, and keep the files it writes under `$RIMZ_SHARED`. From a directory under the real `HOME` that is outside every git checkout, `$RIMZ_SHARED` among them, the project walk (`crates/rimz/src/workspace.rs::resolve_marker`) climbs to the real `HOME`, whose `.rimz/config.toml` counts as a project marker because only the active `RIMZ_HOME` is excluded, so a branch binary reads the real `~/.rimz/config.toml` as project config.
- On a shared host a before and after listing of the real provider home proves nothing, since other agents write there while the probe runs. Prove isolation from the probe's side: start every provider process under `env -i` with the scratch `HOME`, and afterwards grep the real provider home for the scratch path and for each conversation id the probe created.
- Build each probe home in a directory that does not exist yet, and remove it at the end by its literal path. A Claude agent's shell refuses `rm -rf` on a variable inside a script, so a pre-clean step fails the whole probe.
- A live check of a branch binary runs under an isolated `RIMZ_HOME` seeded from a copy of the state it reads (`cp -r ~/.rimz/cache/providers "$RIMZ_HOME/cache/"`), never the real home: `~/.rimz/cache/` is shared by every running RimZ, and a branch that bumps a publication version rewrites it for all of them. A documented read-only command (`rimz doctor` without `--clear` or `--output`) may be timed against the real home when the branch changes no published cache format.
- An isolated `rimz accounts` check first runs `rimz accounts add <provider> <name>` (or `rimz hooks install <provider>` for `default`) inside the isolated home. A missing-hooks problem takes the `PLAN` column and hides a lapse or the normal plan label.
- The check also needs its own `XDG_RUNTIME_DIR`. The accounts lane elects its prober through `accounts.lock` under the runtime directory, not under `RIMZ_HOME`, and a caller that loses that election to a live room serves its cache as it stands without probing.
- An isolated `rimz accounts list` check for Antigravity runs outside an agent's command sandbox. `agy` binds a loopback socket for its account reading, and a sandbox that refuses the bind makes the check fail. Rerun the same command outside the sandbox.
- A text diff of `rimz accounts list` between a release binary and a `testkit` build expects the Muse row to read `hooks missing` on the `testkit` side. `HOOK_ENV_NAMES` in `crates/rimz/src/agents/hook_types.rs` carries a `testkit`-only entry, and Muse's expected hook environment derives from it, so a `testkit` build never matches a hook that a release build installed.
- A walk or path comparison rooted at the RimZ home (a size walk, a prefix check, a relative link) is tested with a home that is itself a symlink, and a live check of one uses such a home too. The [accounts guide](../guide/accounts.md) supports a `~/.rimz` linked into a dotfiles checkout, and `fs::symlink_metadata` on that root reads the link, not the home behind it. `disk::usage` once walked nothing under such a home, so `rimz doctor --audit` reported the room as not created. `disk::usage::tests::measure_follows_a_symlinked_home_root_only` holds that walk.
- A probe that seeds an isolated home with a user's Markdown definitions also copies the skill library they name (`~/.agents/skills` into the home's `.rimz/skills`). A definition whose skill is missing fails resolution and is dropped, so the probe measures a home without it.
- It also copies the user's kind bases `agents/<kind>.md`. A definition whose kind has no base there runs on RimZ's built-in one, so the probe measures another prompt and grant, and a kind base on a kind that takes no system prompt fails to load.
- Kind bases live only under `agents/`. Under `subagents/`, a file named after a kind is refused as taking a kind's name.
- A Claude or Codex definition that the probe writes itself lists `tools`, which those kinds require. Every definition the probe writes carries a non-empty `description:`.
- A kind base takes every profile key but `tier:`, and a key it sets fills every definition on its kind that leaves the key unset. A probe base that sets `tools` or `model` changes every definition the probe loads that leaves the key unset.
- A live check of a change to definition or skill loading runs `rimz agents explain` on a healthy profile beside each broken item, and on each agent kind a broken item is named after, bare and as `<kind>-yolo` and `<kind>-fast`. `rimz agents validate` and `rimz agents profiles` never show which launches refuse.
- A hand-built `rimz packs` probe of a Git source runs under `env -i` with its own `HOME`, `RIMZ_HOME`, and short `XDG_RUNTIME_DIR`. It points `GIT_CONFIG_GLOBAL` at a file holding `[url "file://<fixture dir>/"]` with `insteadOf = https://github.com/rimio-ai/`. Write the fixture directory as an absolute path: a relative one fails the fetch.
- Each fixture repository (`rimz-crew`, `rimz-skills`) needs only a commit on its default branch, which an official source with no ref follows (`crates/rimz/src/packs/git.rs::Client::resolve`). A fixture manifest whose `rimz` range excludes the probe's binary holds the install. The fixture in `crates/rimz/tests/integration/packs/git.rs` is the shape.

## Console blocks

A `console` block in a doc, rustdoc, or help text is captured from a copy of the built binary (the tmpfs hides `target/debug/`) under a scratch home: `bwrap --dev-bind / / --tmpfs /home --bind "$TMPDIR/<dir>" /home/me --clearenv --setenv PATH /usr/bin:/bin --setenv TMPDIR /tmp --setenv HOME /home/me --setenv RIMZ_HOME /home/me/.rimz --chdir /tmp -- "$TMPDIR/rimz" <args>`. `--clearenv` drops the variables the [isolated-home table](#isolated-homes) names, which a room pane exports and which point the copy back at real state. Output that needs a room or agents is captured in a held [sandbox room](./sandbox-rooms.md). Where it cannot be captured, it is recomputed from the renderer's doc comments (`render::Table`, `render::KeyVals`, `render::roster::Roster`), or, for an error, derived from the format string plus the `error: ` prefix `render::write_report` adds, and the change says it was derived. A change to one cell regrounds the whole block.

## Branch binaries against real state

- A live check whose command rereads transcripts (`rimz stats --refresh`) finds none under an isolated home, because the transcripts live in the real account and provider homes. Run each binary inside `bwrap --dev-bind / / --ro-bind ~/.rimz ~/.rimz --bind "$TMPDIR/cache-copy" ~/.rimz/cache --ro-bind ~/.claude ~/.claude --ro-bind ~/.codex ~/.codex --ro-bind ~/.config ~/.config -- <binary> ...`, where `cache-copy` is a copy of `~/.rimz/cache`. The binary reads the real transcripts and writes only the copy.
- `--refresh` refuses `--json`. For refolded JSON, remove `providers/provider-spending.json` from the copy and run `rimz stats --json`, which then finds no publication from today and refolds from the transcripts.
- To compare two binaries, give each its own cache copy with `providers/spending.json` removed as well as `providers/provider-spending.json`. Bind a frozen `cp -a` copy of the provider's session tree read-only over the real one (`--ro-bind "$TMPDIR/codex-sessions" ~/.codex/sessions`), so both walk identical files from scratch. Otherwise one binary can reuse its cursor while the other reparses rollouts that live agents are still writing.
- Add `--setenv XDG_RUNTIME_DIR /tmp/<8 chars>`, a fresh directory, when the measured process must own the spending walker. With the real runtime directory it joins the live room's spending service and measures nothing of its own. Confirm a `rimz-spending-w` thread (the walker's name, cut to 15 characters) in `/proc/<pid>/task/*/comm`.
- Build the head for such a comparison with `cargo build --release -p rimz --bin rimz --features testkit`. A debug build's cold fold took 1,933 s where the installed release base took 305 s.
- Compare the two `--json` outputs with `jq` after rounding every dollar field, since two cold folds can differ in the last digits of a float sum.
- A probe run from an agent pane that must not act as that agent needs `--unshare-pid --proc /proc` as well as `--clearenv`: with the variables gone, `crates/rimz/src/launcher/context.rs::resolve_caller_from` finds the calling agent by process ancestry, so `rimz agents attribution` reports that agent's lane whatever `--chdir` names. After `--clearenv` it also needs `--ro-bind "$XDG_RUNTIME_DIR/rimz" "$XDG_RUNTIME_DIR/rimz" --setenv XDG_RUNTIME_DIR "$XDG_RUNTIME_DIR"`, or active times drop out of the output with no error.
- Some commands of a branch binary write the room's own records: a `rimz lsp` query appends to the room's query usage log, and `rimz loop list`, `loop show`, and `loop logs` open `<room>/locks/lease-gate.lock` to probe any run lock there. Run a live check of such a command under `bwrap --dev-bind / / --bind "$TMPDIR/records" <room>/records --bind "$TMPDIR/locks" <room>/locks -- <binary> ...`. `<room>` is the room's state directory under `~/.rimz/ws/`, and each bound directory is a copy of the room's, so the check reads the real room and never writes it.
- A live check of a branch binary's `rimz sidebar snapshot` without `--no-produce` also writes outside the room's state directory. So does its `rimz sidebar frame`, which produces when the session has no published sidebar frame. Each republishes `live/agent_context/` and `lanes/` under `$XDG_RUNTIME_DIR/rimz/ws/<room>/`.
- When free space is under the `gc` floor, its heavy-lane refresh also spawns `rimz gc --tmp-floor`, which evicts ended agents' temp units in every room and appends assist records to `~/.rimz/logs/assists.log.jsonl`. Read `rimz doctor`'s `TEMP UNITS` row first: below the floor, the helper's evictions are real.
- Bind copies over those two runtime directories and `~/.rimz/logs`, as well as `~/.rimz/cache` and the room's `records`, `locks`, `cache`, `log` and `audit`. Where `log` or `audit` is too large to copy, bind it read-only (`--ro-bind`).
- A live check of a user-shell view (`rimz wait list`, `rimz loop list --all`) from an agent pane runs the binary from a script started with `setsid -f`. The script writes its output to files under `$TMPDIR` and touches a marker file last. The orphaned script has no agent among its ancestors.
- Unsetting `RIMZ_AGENT_*` and `TMUX_PANE` is not enough, because caller resolution falls back to process ancestry (`CallerIdentity::from_process_ancestry`). bwrap `--unshare-pid --proc /proc` cannot mount proc under scoped isolation.
- An Antigravity session whose brain was set aside lives under `<account>/.rimz-aside/<stamp>/brain`. Read it under `bwrap --ro-bind / / --tmpfs <account> --ro-bind <account>/.rimz-aside <account>/.rimz-aside --ro-bind <account>/antigravity-oauth-token <account>/antigravity-oauth-token --ro-bind <account>/installation_id <account>/installation_id --ro-bind <account>/.rimz-aside/<stamp>/brain <account>/brain -- <binary> agents history <session> --json`.
