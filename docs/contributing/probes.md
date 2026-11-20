# Hand-built probes and live checks

What a probe or live check you build by hand needs so that it measures the thing you mean and leaves the user's machine alone: a private multiplexer, a fixture repository, a provider CLI, an isolated home, or a branch binary run against real state. The sandbox room procedures are in [sandbox-rooms.md](./sandbox-rooms.md), and the integration suite's own fixtures are in its [suite contract](../../crates/rimz/tests/integration/AGENTS.md) and [integration-tests.md](./integration-tests.md).

## Sandbox rooms

- `cargo xtask sandbox -- <command>` drives a real multiplexer from disposable roots.
- To run a command as a room agent, hold a room with `sandbox room` and use `sandbox in <root> --as @handle` ([recipe](./sandbox-rooms.md#act-as-an-agent)).
- From an agent pane, run a live room check through `cargo xtask sandbox room`. A bare `rimz start` refuses inside an agent pane's own sandbox.
- A hand-built check of a command that resolves a room (`rimz worktree new`, `rimz workflow run`) runs the same way, through `sandbox in` on a held room. In a bare repository it refuses with `no room at <root>`.
- A check that needs a real Claude or Codex turn cannot run in a sandbox room, which replaces `HOME` and holds no provider login. It needs a host room that only the user can start ([real provider](./sandbox-rooms.md#check-against-a-real-provider)). A plan or hand-off that rests on such a check raises it with the user first.
- `cargo xtask sandbox -- rimz <args>` runs an interactive smoke check of a built binary. The sandbox puts this checkout's build directory (`$CARGO_TARGET_DIR/debug`, or `target/debug`) first on the command's `PATH`, so `rimz`, as the program or inside a nested shell, is this checkout's build and not the installed release. With no build there, a bare `rimz` refuses and names `cargo build -p rimz --bin rimz --features testkit`.
- `sandbox in` puts the held room's stub directory first, then the directory of the room's binary. A program named by path runs as written.
- The sandbox replaces HOME, every persistent XDG root, `TMUX_TMPDIR`, the Zellij runtime and config roots, and `TMPDIR`. It tears down both private mux servers when the command exits. It drives both multiplexers from inside a scoped agent pane.
- A bare `rimz start` outside `cargo xtask sandbox` is a real product invocation, and it addresses the user's normal multiplexer server.

## Multiplexers

- A hand-built Zellij probe runs under `env -u ZELLIJ_SOCKET_DIR -u ZELLIJ -u ZELLIJ_SESSION_NAME` with its own `XDG_RUNTIME_DIR`. The inherited socket dir otherwise addresses the user's live sessions.
- Zellij tries `$HOME/.config/zellij` before `$XDG_CONFIG_HOME/zellij`. A probe that seeds a user config writes it under the isolated `HOME`, or points `ZELLIJ_CONFIG_FILE` at it.
- On 0.45.1, `zellij --layout <file> attach --create <name>` births the session with Zellij's default layout (tab-bar, status-bar, one pane), and ignores the file. Attach first, build the probe's tabs with `zellij --session <name> action new-tab --layout <file>`, close the bootstrap tab, and count tabs and panes before you time anything.
- A hand-built tmux probe runs as a bash script under `tmux -L <name> -f /dev/null` with `TMUX_TMPDIR=/tmp/<8 chars>`. The session `TMPDIR` makes the socket path exceed the 108-byte limit, and zsh does not word-split an unquoted variable that holds several arguments.
- `new-window -t` names the session with a trailing colon (`-t s:`). A bare session name resolves to its current window and fails with `index 0 in use`.
- A probe room of hundreds of panes runs in its own user and mount namespace with a private devpts instance. The host's shared PTY pool otherwise fails `new-window` with `fork failed: No space left on device`. This opens a shell with room for 2,048 PTYs:

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
- Stop a process you started by its PID (`$!`, `pgrep -x`), never with `pkill -f PATTERN`, which matches the shell that runs `pkill` and kills it.
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

- A check of Claude alone (a mod, a flag, a hook payload) needs no room. Run it from a Claude agent's pane, on the login its shell already exports: start `claude` under `env -i` with `PATH`, `HOME`, `TERM` and that `CLAUDE_CONFIG_DIR`, from a scratch directory under `$TMPDIR`, with `--model haiku` and one-line prompts.
- Pass `--setting-sources project,local`, so the account's own hooks, `env`, and permission rules (which may deny the native `Agent` tool) stay out.
- A dialog check drives the same command in a private tmux server (`tmux -L <name> -f /dev/null`, short `TMUX_TMPDIR`).
- Run it yourself: a subagent on another provider has no Claude login. It spends the user's real usage, so keep it to the questions the change rests on.
- A check of agy (Antigravity) alone needs no room and no model turn. Run `agy` under `env -i` with `PATH`, a scratch `HOME`, its own `RIMZ_HOME`, and a short `XDG_RUNTIME_DIR`, from a scratch directory under `$TMPDIR`. A logged-in check copies `~/.gemini` into the scratch home.
- Print-mode slash commands (`agy -p /usage --output-format json`), `agy plugin validate`, and `agy plugin list` spend no quota.
- A logged-out `agy -p` waits 60 s and opens a browser unless its environment carries an SSH session variable (`SSH_CONNECTION`, `SSH_TTY`, or `SSH_CLIENT`) and its stdin delivers a newline.
- Never print the token, a `/usage` response body, or the account email.
- A check that Codex accepts a `-c` config key needs no login. `codex -c <key>=<value> features list` prints a feature's resolved value. `codex --strict-config -c <key>=<value>`, started in a private tmux (`tmux -L <name> -f /dev/null`, short `TMUX_TMPDIR`), either refuses the key or reaches the sign-in screen. `features` rejects `--strict-config`, so the two checks stay separate.
- A logged-in check against an account home under `~/.rimz/accounts/codex/` is the user's to allow.
- A live Codex turn needs a Codex login. A Claude agent's shell exports no `CODEX_HOME`, and `~/.codex` holds a login only while the user's default Codex account is signed in (a bare `codex exec` there answers `401 Unauthorized` otherwise). A plan that asks for a live Codex turn names a Codex agent to run it on its own login, or raises it with the user first.
- A hand-built Claude transcript for a spend probe is written as compact JSON (no space after `:` or `,`) at a new path: an existing path is a rewrite merge that keeps no rows older than eight days, and spaced JSON fails the parser's byte pre-filter.

## Isolated homes

A hand-built isolated home sets or unsets each of these. An agent shell exports its own account's values, and a probe that isolates only `HOME` still reaches the user's real state through the rest.

| Variable | Do | Otherwise |
| --- | --- | --- |
| `XDG_RUNTIME_DIR` | Keep it short (`/tmp/<8 chars>`) | RimZ refuses a runtime root whose socket paths exceed 108 bytes |
| `RIMZ_HOME` | Set it | The real one wins over `HOME`. The probe resolves the real account homes, and a `rimz trust grant` writes the user's real provider config |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | Unset them, or point them inside the isolated home | The `default` login reads the agent's own account directory, and the probe writes that account's `.claude.json` |
| `XDG_CONFIG_HOME` | Unset it, or point it inside the isolated home | Cursor and Muse hooks resolve their config there, not under `HOME` |
| `XDG_DATA_HOME` | Unset it, or point it inside the isolated home | OpenCode's database and OMP resolve their data there, so a spend probe reads the real account's history |
| Every other `RIMZ_*` variable | Unset it, as `common::ScrubSessionEnvExt` does, or run under `env -i` | The room identity pin (`RIMZ_PROJECT_ROOT`, `RIMZ_WORKSPACE_ID`, `RIMZ_CHANNEL`, `RIMZ_WORKTREE_PATH`) makes the probe resolve the agent's own project, and `rimz loop list --all` lists that project's tasks |
| `RUSTUP_HOME`, `CARGO_HOME` | Pass them through, pointing at the real toolchain, for a probe that runs a Rust toolchain or rust-analyzer | An isolated `HOME` has no rustup default, so the server crashes with `rustup could not choose a version` |
- A live check of a branch binary runs under an isolated `RIMZ_HOME` seeded from a copy of the state it reads (`cp -r ~/.rimz/cache/providers "$RIMZ_HOME/cache/"`), never the real home: `~/.rimz/cache/` is shared by every running RimZ, and a branch that bumps a publication version rewrites it for all of them. A documented read-only command (`rimz doctor` without `--clear` or `--output`) may be timed against the real home when the branch changes no published cache format.
- An isolated `rimz accounts` check first runs `rimz accounts add <provider> <name>` (or `rimz hooks install <provider>` for `default`) inside the isolated home. A missing-hooks problem takes the `PLAN` column and hides a lapse or the normal plan label.
- It also sets a fresh, short `XDG_RUNTIME_DIR` (`/tmp/<8 chars>`). The accounts lane elects its prober through `accounts.lock` under the runtime directory, not under `RIMZ_HOME`, and a caller that loses that election to a live room serves its cache as it stands without probing.
- A text diff of `rimz accounts list` between a release binary and a `testkit` build expects the Muse row to read `hooks missing` on the `testkit` side. `HOOK_ENV_NAMES` in `crates/rimz/src/agents/hook_types.rs` carries a `testkit`-only entry, and Muse's expected hook environment derives from it, so a `testkit` build never matches a hook that a release build installed.
- A walk or path comparison rooted at the RimZ home (a size walk, a prefix check, a relative link) is tested with a home that is itself a symlink, and a live check of one uses such a home too. The [accounts guide](../guide/accounts.md) supports a `~/.rimz` linked into a dotfiles checkout, and `fs::symlink_metadata` on that root reads the link, not the home behind it. `disk::usage` once walked nothing under such a home, so `rimz doctor --audit` reported the room as not created. `disk::usage::tests::measure_follows_a_symlinked_home_root_only` holds that walk.
- A probe that seeds an isolated home with a user's Markdown definitions also copies the skill library they name (`~/.agents/skills` into the home's `.rimz/skills`). A definition whose skill is missing fails resolution and is dropped, so the probe measures a home without it.
- It also writes the kind base `agents/<kind>.md`, with a nonempty body, for each kind the definitions run on that takes a system prompt. A definition without its kind base fails to load, and a kind base on a kind that takes none fails in turn.
- Kind bases live only under `agents/`. Under `subagents/`, a file named after a kind is refused as taking a kind's name.
- A Claude or Codex definition that the probe writes itself lists `tools`, which those kinds require. Every definition the probe writes carries a non-empty `description:`.
- A kind base takes no key beside `description:` but the launch tuning `fast`, `code-mode`, `effort`, and `auto-compact`. `tools`, `model`, and the rest go on a profile that names the kind with `agent:`.
- A live check of a change to definition or skill loading runs `rimz agents explain` on a healthy profile beside each broken item, and on each agent kind a broken item is named after, bare and as `<kind>-yolo` and `<kind>-fast`. `rimz agents validate` and `rimz agents profiles` never show which launches refuse.
- A hand-built `rimz packs` probe of a Git source runs under `env -i` with its own `HOME`, `RIMZ_HOME`, and short `XDG_RUNTIME_DIR`. It points `GIT_CONFIG_GLOBAL` at a file holding `[url "file://<fixture dir>/"]` with `insteadOf = https://github.com/rimio-ai/`. Write the fixture directory as an absolute path: a relative one fails the fetch.
- The probe tags each fixture repository (`rimz-crew`, `rimz-skills`) `v<version>`, the crate version that `rimz --version` prints before its `+` suffix. An official source with no ref resolves to that tag (`crates/rimz/src/packs/git.rs::Client::resolve`). The fixture in `crates/rimz/tests/integration/packs/git.rs` is the shape.

## Branch binaries against real state

- A live check whose command rereads transcripts (`rimz stats --refresh`) finds none under an isolated home, because the transcripts live in the real account and provider homes. Run each binary inside `bwrap --dev-bind / / --ro-bind ~/.rimz ~/.rimz --bind "$TMPDIR/cache-copy" ~/.rimz/cache --ro-bind ~/.claude ~/.claude --ro-bind ~/.codex ~/.codex --ro-bind ~/.config ~/.config -- <binary> ...`, where `cache-copy` is a copy of `~/.rimz/cache`. The binary reads the real transcripts and writes only the copy.
- `--refresh` refuses `--json`. For refolded JSON, remove `providers/provider-spending.json` from the copy and run `rimz stats --json`, which then finds no publication from today and refolds from the transcripts.
- To compare two binaries, give each its own cache copy with `providers/spending.json` removed as well as `providers/provider-spending.json`. Bind a frozen `cp -a` copy of the provider's session tree read-only over the real one (`--ro-bind "$TMPDIR/codex-sessions" ~/.codex/sessions`), so both walk identical files from scratch. Otherwise one binary can reuse its cursor while the other reparses rollouts that live agents are still writing.
- Add `--setenv XDG_RUNTIME_DIR /tmp/<8 chars>` (a fresh, short directory) when the measured process must own the spending walker. With the real runtime directory it joins the live room's spending service and measures nothing of its own. Confirm a `rimz-spending-w` thread (the walker's name, cut to 15 characters) in `/proc/<pid>/task/*/comm`.
- Build the head for such a comparison with `cargo build --release -p rimz --bin rimz --features testkit`. A debug build's cold fold took 1,933 s where the installed release base took 305 s.
- Compare the two `--json` outputs with `jq` after rounding every dollar field, since two cold folds can differ in the last digits of a float sum.
- Some commands of a branch binary write the room's own records: a `rimz lsp` query appends to the room's query usage log, and `rimz loop list`, `loop show`, and `loop logs` open `<room>/locks/lease-gate.lock` to probe any run lock there. Run a live check of such a command under `bwrap --dev-bind / / --bind "$TMPDIR/records" <room>/records --bind "$TMPDIR/locks" <room>/locks -- <binary> ...`. `<room>` is the room's state directory under `~/.rimz/ws/`, and each bound directory is a copy of the room's, so the check reads the real room and never writes it.
- A live check of a branch binary's `rimz sidebar snapshot` without `--no-produce` also writes outside the room's state directory. So does its `rimz sidebar frame`, which produces when the session has no published sidebar frame. Each republishes `live/agent_context/` and `lanes/` under `$XDG_RUNTIME_DIR/rimz/ws/<room>/`.
- When free space is under the `gc` floor, its heavy-lane refresh also spawns `rimz gc --tmp-floor`, which evicts ended agents' temp units in every room and appends assist records to `~/.rimz/logs/assists.log.jsonl`. Read `rimz doctor`'s `TEMP UNITS` row first: below the floor, the helper's evictions are real.
- Bind copies over those two runtime directories and `~/.rimz/logs`, as well as `~/.rimz/cache` and the room's `records`, `locks`, `cache`, `log` and `audit`. Where `log` or `audit` is too large to copy, bind it read-only (`--ro-bind`).
- A live check of a user-shell view (`rimz wait list`, `rimz loop list --all`) from an agent pane runs the binary from a script started with `setsid -f`. The script writes its output to files under `$TMPDIR` and touches a marker file last. The orphaned script has no agent among its ancestors.
- Unsetting `RIMZ_AGENT_*` and `TMUX_PANE` is not enough, because caller resolution falls back to process ancestry (`CallerIdentity::from_process_ancestry`). bwrap `--unshare-pid --proc /proc` cannot mount proc under scoped isolation.
