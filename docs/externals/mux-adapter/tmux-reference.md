# tmux upstream reference

> The [tmux backend documentation](../../internals/multiplexers.md#tmux-backend) owns RimZ's mapping. This page describes upstream tmux.

This reference covers tmux's client/server model, commands, formats, hooks, options, environment, and control-mode protocol.

Baseline: **tmux 3.8**, tag [`3.8`](https://github.com/tmux/tmux/releases/tag/3.8), commit [`7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53`](https://github.com/tmux/tmux/commit/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53), checked 2026-10-12. GitHub records the release publication as 2026-09-09. The tag's current commit is dated 2026-10-08. This reference follows that commit's source. Historical terminal captures retain their stated versions. Where documentation and source disagree, the source takes precedence and the claim notes the disagreement.

Commands and protocol shapes used by an adapter receive full entries. The remaining catalogs identify related surfaces and their source.

## Upstream sources

The release man page documents commands and options. Source defines the control-mode wire shapes. Source links pin the checked commit because the tag commit postdates the release publication.

| Surface | Source |
| --- | --- |
| Release baseline | <https://github.com/tmux/tmux/releases/tag/3.8> |
| Man page | <https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tmux.1>, <https://man.openbsd.org/tmux.1> |
| Changelog | <https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/CHANGES> |
| Command synopses and flags | [`cmd-*.c`](https://github.com/tmux/tmux/tree/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53) (`.args` and `.usage` in each `cmd_entry`) |
| Option scopes, choices, defaults | [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c) |
| Format variables and modifiers | [format.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/format.c) |
| Control-mode wire shapes | [control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c), [control-notify.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control-notify.c), [cmd-queue.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-queue.c) (`cmdq_guard`), [client.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/client.c) (`client_exit_message`) |
| Layout strings | [layout-custom.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/layout-custom.c) (`layout_dump`, `layout_append`, `layout_parse`) |
| Wiki | <https://github.com/tmux/tmux/wiki/Control-Mode>, <https://github.com/tmux/tmux/wiki/Formats> |

## Server model and invocation

One server process per socket owns every session, window, and pane. Clients are separate processes that talk to the server over the socket. They attach to render and detach without disturbing it. The server starts on the first command that needs one and exits when no sessions remain (`exit-empty`, default `on`).

- Sockets. The default socket is `<$TMUX_TMPDIR or /tmp>/tmux-<uid>/default`. `-L <name>` picks another name in that directory, and `-S <path>` gives a full path and overrides `-L`. A non-default socket is created under umask 177, owner read and write only ([server.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/server.c), `server_create_socket`). SIGUSR1 makes the server re-create a deleted socket. A `-S` path in a private directory is a private server.
- `$TMUX`. Panes receive `TMUX=<socket-path>,<server-pid>,<session-id>`, where the third field is the numeric session id without its `$` ([environ.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/environ.c), `environ_for_session`). The man page documents the value only as internal information. A client started with `$TMUX` set refuses attach-shaped commands with `sessions should be nested with care, unset $TMUX to force`. Clear the variable in the child environment for a deliberate nested or control-mode attach. `$TMUX_PANE` carries the pane's `%id`.
- The client's terminal is stdin. An attach-shaped command opens its terminal from stdin, not `/dev/tty`. With stdin piped or null it fails `open terminal failed: not a terminal`, exit 1 (probed on 3.5a). Commands that never attach are unaffected, so a subprocess wrapper that nulls stdin cannot attach by accident.
- Version string. `tmux -V` reports the build version, such as `tmux 3.8`. Point releases add a letter to the same minor. OpenBSD's base tmux prints `tmux openbsd-X.Y`, which a numeric parser rejects.
- Command sequences. One client argv carries several commands separated by standalone `;` tokens (`tmux set -t s mouse on ';' set -t s history-limit 100000`): one fork, one server round trip. A parse error anywhere, such as an unknown command, fails the whole sequence before anything runs. A runtime failure, such as a bad target, stops at the failing command with earlier commands applied, exit 1, and stderr naming the failure (probed on 3.5a. The man page states only the runtime half).
- Stdin scripts. `source-file -` reads commands from a non-attached, non-control client's stdin without packing the script into argv ([cmd-source-file.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-source-file.c), [file.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/file.c)). Each line is its own command group: a runtime error stops the rest of that line, later lines still run, and the client exits 1 with stderr lacking line numbers. A parse error runs nothing and prints `-:LINE: ...` on stdout, with stderr empty. `display-message -p` output, including inside `if-shell -F`, stays in execution order, so begin/end markers can bracket each line. Single-quote each word and escape an embedded quote as `'\''`; reject newlines and carriage returns for byte-preserving words, since even quoted backslash-newline folds (probed on 3.5, 3.7c, and 3.8).
- No server. A command that requires an existing server exits 1 when none is running. stderr is `no server running on <path>` when the socket file exists but its server died, and `error connecting to <path> (No such file or directory)` when the socket was never created. A denied connect also exits 1, but prints `error connecting to <path> (Permission denied)` or `error connecting to <path> (Operation not permitted)` instead. The parenthesized reason is `strerror(errno)`, not proof that the server is absent ([client.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/client.c), `client_main`).

Global flags that matter to a wrapper:

| Flag | Effect |
| --- | --- |
| `-S <path>` / `-L <name>` | socket path / socket name |
| `-f <file>` | configuration file |
| `-N` | never start a server |
| `-D` | run the server in the foreground (disables `exit-empty`) |
| `-C` / `-CC` | control mode ([control mode](#control-mode)) |
| `-T <features>` | client terminal features |
| `-u` | mark the client UTF-8 ([output sanitization](#output-sanitization)) |
| `-v` | log to files, SIGUSR2 toggles server logging |

### Targets and ids

Most commands take `-t` (and some `-s`). Each target kind resolves in order:

| Kind | Forms |
| --- | --- |
| target-session | `$id`, exact name, name prefix, fnmatch pattern, a leading `=` forces exact match, several matches are an error |
| target-window | `session:window`, where window is `{start}`/`^`, `{end}`/`$`, `{last}`/`!`, `{next}`/`+`, `{previous}`/`-`, `{current}`/`@`, an offset like `+2`, an index, an `@id`, an exact name, a name prefix, or an fnmatch pattern |
| target-pane | `session:window.pane`, where pane is an index, a `%id`, `{active}`/`@`, or a position token (`{top-left}`, `{up-of}`, …), a bare `%id` is absolute, `{mouse}` and `{marked}` name the last mouse-event pane and the marked pane |

Sessions, windows, and panes carry server-unique ids, `$N`, `@N`, and `%N`, fixed for the object's life. They come from monotonic per-server counters and are not reused after close (probed: kill `%1`, and the next split is `%2`). They restart from zero only when a new server starts. Names permit `:` and `.` and may be empty (only `#(` is forbidden, 3.7a), but those characters keep their target-grammar meaning, so scripts target ids and ask for them with `-P -F '#{pane_id}'`.

## Releases and version floors

| Release | Date | | Release | Date |
| --- | --- | --- | --- | --- |
| 3.2 | 2021-04-13 | | 3.6 | 2025-11-26 |
| 3.2a | 2021-06-10 | | 3.6a | 2025-12-05 |
| 3.3 | 2022-06-01 | | 3.6b | 2026-05-20 |
| 3.3a | 2022-06-09 | | 3.7 | 2026-06-26 |
| 3.4 | 2024-02-13 | | 3.7a | 2026-07-01 |
| 3.5 | 2024-09-27 | | 3.7b | 2026-07-01 |
| 3.5a | 2024-10-05 | | 3.7c | 2026-08-17 |
| | | | 3.8 | 2026-09-09 |

Dates are GitHub release publication dates. The floors below cover the surfaces this page documents in depth. Each is from `CHANGES` unless marked.

| Surface | Landed |
| --- | --- |
| `split-window` / `new-window` `-e VAR=val` | 3.0 |
| pane options (`set-option -p`, `show-options -p`) | 3.0 |
| `after-<command>` hooks as array options | 3.0 |
| `new-session -e`, `new-window -S` | 3.2 |
| `display-popup` | 3.2 (`-s`/`-S`/`-b`/`-T`/`-e`/`-B` 3.3, `-k` 3.6) |
| `extended-keys` | 3.2 (`always` 3.2a, revamped to mode 2 in 3.5) |
| `client-active`, `window-resized` hooks | 3.2 |
| control mode: pause mode, `%extended-output`, `refresh-client -B` subscriptions, `-f` flag spelling | 3.2 (`no-output` since 3.0 as `refresh-client -F`) |
| client flags on `attach-session -f` (`ignore-size`, `read-only`, …) | 3.2 |
| `allow-passthrough` | 3.3, default `off` (`all` 3.4) |
| `extended-keys-format` | 3.5 |
| `escape-time` default 10 ms (was 500 ms) | 3.5 |
| `command-error` hook, `refresh-client -r` | 3.5 |
| `main-horizontal-mirrored`, `main-vertical-mirrored` layouts | 3.5 |
| `capture-pane -M`, `run-shell -E`, `display-message -C` | 3.6 |
| `new-pane` floating panes, `pane_floating_flag` | 3.7 |
| `-O` sort and `-r` reverse on `list-panes`, `list-windows`, `list-sessions`, `list-clients` | 3.7 |
| `paste-buffer -S` (and `vis(3)` sanitization by default) | 3.7 |
| `{current}` / `{active}` target tokens | 3.7 |
| `history-limit` changes apply to existing panes | 3.7 |
| JSON layouts, `new-layouts`, hook monitors, event waits, `capture-pane -I` | 3.8 |
| `pane_start_time` format variable | absent in 3.8 ([formats](#the-variables-rimz-reads)) |

Behaviour changes inside 3.5 through 3.8 that alter what a caller observes:

| Release | Change |
| --- | --- |
| 3.8 | JSON layouts replace default layout output. Control clients retain old layouts unless `new-layouts` is set. `active-pane` is removed. `CHANGES` says `mouse` defaults to on, but the source defaults to off unless the build overrides `TMUX_MOUSE`. Hooks gain monitors and events. Floating panes gain keyboard controls and layout restoration. Control replies have a 64 MiB buffer limit. |
| 3.7c | `split-window ''` (a single empty command) creates an empty pane again, as it did before 3.7 ([cmd-split-window.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-split-window.c)), `new-window -n ''` keeps an empty name instead of deriving one ([spawn.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/spawn.c)), `new-pane` unzooms the window before creating a floating pane, `detach-on-destroy previous`/`next` pick sessions in name order and never the session being destroyed ([server-fn.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/server-fn.c)), `message-format` uses `message-style` again |
| 3.7b | the end of a synchronized update (`sync` terminal feature) triggers a redraw, which 3.7 and 3.7a could skip |
| 3.7a | names may be empty and contain `#[`, `:`, and `.` |
| 3.7 | `history-limit` changes update existing panes, `new-session -A -c` applies the working directory on the attach path, read-only checks tighten on `attach-session`, `detach-client`, and `switch-client`, `paste-buffer` passes buffers through `vis(3)` unless `-S`, pane titles and window and session names are sanitized of C0 and invisible characters, default `update-environment` gains `WAYLAND_DISPLAY` and `XDG_*` variables, `refresh-client -l` loses its forward-to-pane argument in favour of the `get-clipboard` option, several control-mode exit hangs are fixed |
| 3.6 | bracketed paste stays byte-preserving while `extended-keys` is on (observed on the wire, `CHANGES` has no entry), `capture-pane` preserves tabs |
| 3.5a | `#()`, `run-shell`, and `if-shell` return to `/bin/sh`, popups keep `default-shell`, BSpace and Shift encodings under extended keys are corrected |
| 3.5 | `escape-time` default drops from 500 to 10 ms, extended keys always request mode 2 and use a new internal key representation |

Through 3.7c, client input `ESC[27u` (CSI-u Escape with the modifier omitted) is not recognized as Escape and reaches panes raw, while `ESC[27;1u` and the xterm form are translated. Earlier boundaries a contributor may meet: 3.3 made `command-prompt` and `confirm-before` block by default (`-b` restores async), and 3.2 moved window and pane hooks off session scope ([hooks](#hooks)), renamed `refresh-client -F` to `-f`, and made `window_flags` escape `#` (`window_raw_flags` is the raw form).

<a id="unreleased-in-38-rc"></a>

### Changes released in 3.8

These changes ship in stable 3.8. See [layouts](#layout-strings), [hooks](#hooks), and [flow control](#client-flags-and-flow-control).

The [3.7c to 3.8 changelog](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/CHANGES) also records these compatibility changes:

- `display-panes` becomes a mode and removes `-b`.
- Copy-mode `refresh-from-pane` is replaced by `refresh-on`, `refresh-off`, `refresh-toggle`, and `refresh-now`.
- Command templates escape single quotes in `%%`. `%1` remains unquoted.
- `server-access` accepts groups as well as users.

## Command surface

A `shell-command` argument to `new-session`, `new-window`, `split-window`, `respawn-window`, or `respawn-pane` may be several argv tokens, which tmux executes directly without `sh -c`. A single token runs through the pane shell with `-c`. New panes use `default-shell`, while respawn keeps the pane's shell ([spawn.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/spawn.c)).

Command synopses below follow the release source's `.args` strings. Generated usage can omit accepted flags. For example, `split-window` accepts `-E` although its usage omits it ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-split-window.c)).

### Sessions and clients

**`new-session [-AdDEPX] [-c start-dir] [-e VAR=val]… [-f flags] [-F format] [-n window-name] [-s name] [-t group] [-x cols] [-y rows] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-d` | create detached, size comes from `default-size` (80x24) unless `-x`/`-y` are given |
| `-x` / `-y` | initial size, also sets the new session's `default-size` option ([cmd-new-session.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-new-session.c)), `-` uses the current client's size |
| `-e VAR=val` | seed the session environment, repeatable, the first window's panes already inherit it |
| `-c` | start directory |
| `-P [-F]` | print the created session (`#{session_name}:` by default) |
| `-A` | attach instead when the session exists (see below) |
| `-t group` | join a session group (shared window set) |
| `-E` | skip `update-environment` |

`-A` on an existing session takes the attach path, and that path ignores `-d`: it honours only `-D` (like `attach -d`), `-X` (like `attach -x`), and since 3.7 `-c`. So `new-session -A -d` against a live session attaches and blocks, or with no terminal on stdin fails `open terminal failed: not a terminal`, exit 1 (probed on 3.5a). The attach path also ignores `-e`, `-x`, and `-y` (probed). Creating a session that exists without `-A` fails `duplicate session: <name>`, exit 1. The no-attach ensure idiom is `has-session -t =<name> || new-session -d -s <name>`, or `new-session -d` with `duplicate session` treated as success.

**`attach-session [-dErx] [-c dir] [-f flags] [-t session]`** (alias `attach`). `-d` detaches other clients, `-x` detaches them and sends SIGHUP, `-E` skips `update-environment`, and `-r` is read-only (the same as `-f read-only,ignore-size`). `-f` takes a comma-separated flag list, and a leading `!` clears a flag on an attached client:

| Client flag | Effect |
| --- | --- |
| `read-only` | keys limited to detach and switch bindings, commands follow per-command checks ([read-only](#read-only-clients)) |
| `ignore-size` | excluded from window size negotiation |
| `active-pane` | removed in 3.8 |
| `new-layouts` | control mode: use JSON layout strings (3.8) |
| `no-detach-on-destroy` | switch to another session instead of detaching when the session is destroyed (3.6) |
| `no-output` | control mode: no pane output |
| `pause-after=<secs>` | control mode: pause panes that fall behind |
| `wait-exit` | control mode: wait for an empty line after `%exit` |

**`detach-client [-aP] [-E shell-command] [-s session] [-t client]`**. `-s` detaches every client on the session, `-a` every client but the target, `-P` sends SIGHUP to the client's parent, and `-E` replaces the client process with a command.

**`kill-session [-aCg] [-f filter] [-t session]`**. An absent target exits 1 with `can't find session: <name>`. `-a` kills every other session, `-g` every session in the target's group (3.7), and `-C` clears alerts in the session's windows instead of killing anything. `-f` filters the sessions selected by `-a` (3.8). **`kill-server`** ends the server, every session, and every client.

**`has-session [-t session]`** reports through its exit code alone and never starts a server.

**`list-sessions [-r] [-F format] [-f filter] [-O order]`** and **`list-clients [-r] [-F format] [-f filter] [-O order] [-t session]`** print one line per session or attached client. `-f` keeps rows whose filter format is true, `-O` sorts, and `-r` reverses (3.7). In a `list-clients` row, pane and window variables resolve against the client's current session, so `#{pane_id}` is the active pane of that session's current window.

Index: `rename-session`, `switch-client`, `lock-client`/`lock-session`, `server-access` (socket access list, 3.3), `list-commands` (command syntax), `refresh-client` ([control mode](#client-flags-and-flow-control)).

### Windows and panes

**`new-window [-abdEkPS] [-c dir] [-e VAR=val]… [-F format] [-n name] [-t window] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-d` | do not make the new window current |
| `-a` / `-b` | insert after / before the target index, shifting later windows |
| `-P [-F]` | print the new window, `-F '#{window_id} #{pane_id}'` returns both ids |
| `-n` | name the window and turn off `automatic-rename` for it ([spawn.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/spawn.c)), so the name stays fixed |
| `-S` | select an existing window with that name instead of failing (3.2) |
| `-k` | replace the window at the target index |
| `-E` | create an empty pane (3.8) |

The window closes when its command exits unless `remain-on-exit` keeps the pane.

**`split-window [-bdeEfhIkPvWZ] [-B border-lines] [-T title] [-c dir] [-e VAR=val]… [-F format] [-l size] [-m message] [-p percentage] [-R inactive-style] [-s style] [-S active-style] [-t pane] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-h` / `-v` | left and right / top and bottom (default) |
| `-b` | put the new pane before the target (left of or above it) |
| `-l <n>` / `-l <n>%` | size in cells / percentage (`-p` is the older percentage form) |
| `-f` | span the full window height or width |
| `-d` | leave the active pane unchanged |
| `-P [-F]` | print the new pane, the default format is index-shaped, so ask for `#{pane_id}` |
| `-E`, or a single empty command `''` | create an empty pane with no command, `display-message -I` writes into it |
| `-I` | create an empty pane and forward stdin into it |
| `-k` / `-m` | keep the pane after its command exits until a key is pressed / with that message |
| `-Z` | keep the window zoomed |
| `-B` / `-T` | set border lines / pane title (3.8) |
| `-W` | wait for the pane command to exit (3.8) |

A split works on a detached session with no client attached.

**`new-pane [-AbCDEdefhIkKLMOPvWZ] [-B border-lines] [-c dir] [-e VAR=val]… [-F format] [-l size] [-m message] [-p percentage] [-R inactive-style] [-s style] [-S active-style] [-t pane] [-T title] [-x width] [-y height] [-X x] [-Y y] [shell-command…]`** creates a floating pane. It shares `split-window`'s implementation ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-split-window.c)). `-x`/`-y` set size, `-X`/`-Y` set position, and `-L` creates a tiled pane.

| Flag | Floating-pane effect in 3.8 |
| --- | --- |
| `-A` | remain above a zoomed pane |
| `-O` | create a modal pane that blocks interaction with other panes and remains above zoom |
| `-C` | close a modal pane when the mouse clicks outside it |
| `-D` | close a modal pane with Escape or C-c |
| `-K` | send every key, including the prefix, to the pane |
| `-M` | start mouse dragging |
| `-W` | wait for the pane command to exit |

In 3.8, `move-pane` and `resize-pane` support floating panes. `break-pane` floats a tiled pane, `join-pane` tiles a floating pane, and `swap-pane` supports both. Custom JSON layouts can restore floating panes. These operations were unavailable in 3.7 ([changelog](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/CHANGES)).

**`respawn-pane [-Ek] [-c dir] [-e VAR=val]… [-t pane] [shell-command…]`** restarts a pane's command in place, keeping its `%id` and geometry. Without `-k` it fails on a pane whose command is still running. `-k` kills the running command first. With no command it reruns the pane's start command. `respawn-window` is the window-level form. Both accept `-E` to create an empty pane in 3.8.

**`select-pane [-DdeLlMmRUZ] [-T title] [-t pane]`** makes a pane active within its window only. It does not change the session's current window (probed on 3.5a, and [cmd-select-pane.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-select-pane.c) writes no current window). A cross-window jump is `select-pane -t %pane ';' select-window -t @win`, and only `switch-client` crosses sessions. `-L`/`-R`/`-U`/`-D` move by direction, `-l` goes to the last pane, `-e`/`-d` enable or disable input, `-T` sets the title, and `-m`/`-M` set or clear the marked pane.

**`select-window [-lnpT] [-t window]`** accepts `@id` targets. `-l`, `-n`, and `-p` pick the last, next, and previous window, and `-T` acts like `last-window` when the target is already current.

**`swap-window [-d] [-s src] [-t dst]`** exchanges two windows' positions, including into an occupied index, and `-d` keeps the current window current. `move-window [-abdkr]` relocates a window instead, and `-r` renumbers a session's windows.

**`rename-window [-t window] new-name`** sets the name and turns off that window's `automatic-rename` ([cmd-rename-window.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-rename-window.c)). Setting `automatic-rename` on again derives the name from the active pane.

**`kill-pane [-a] [-f filter] [-t pane]`** kills a pane and its process, and the last pane's death closes its window. `-a` kills every other pane. `kill-window [-a] [-f filter]` works the same way on windows. With `-a`, `-f` filters the objects killed (3.8).

**`list-panes [-asr] [-F format] [-f filter] [-O order] [-t target]`** lists one window's panes by default, a session's with `-s`, and the server's with `-a` (the target is ignored). `-O` sorts and `-r` reverses (3.7). **`list-windows [-ar] [-F format] [-f filter] [-O order] [-t session]`** is the window-level form.

Index: `break-pane`, `join-pane`, `move-pane`, `swap-pane`, `rotate-window`, `link-window`/`unlink-window`, `last-pane`/`last-window`, `next-window`/`previous-window`, `display-panes`, copy mode and its `send-keys -X` commands, and the `choose-tree`/`choose-client`/`choose-buffer` modes.

### Layout and sizing

**`resize-pane [-MTZ] [-D lines] [-L columns] [-R columns] [-U lines] [-x width] [-y height] [-t pane] [adjustment]`** sets a pane's size. `-x`/`-y` take an absolute size in cells or a percentage of the window. `-L`/`-R`/`-U`/`-D` move a border by their optional argument, falling back to `adjustment` (default 1). `-Z` toggles the window's zoom on the target pane (`window_zoomed_flag`), `-M` starts a mouse resize, and `-T` trims lines below the cursor that are in history.

**`resize-window [-aADLRU] [-x width] [-y height] [-t window] [adjustment]`** sets a window's size and, as a side effect, sets that window's `window-size` option to `manual` ([cmd-resize-window.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-resize-window.c)). Unset the option (`set-option -wu -t @win window-size`) to return the window to client-driven sizing. `-a` and `-A` size the window to the smallest or largest session containing it.

**`select-layout [-Enop] [-t pane] [layout-name]`** applies a preset or a layout string. The seven presets are `even-horizontal`, `even-vertical`, `main-horizontal`, `main-horizontal-mirrored`, `main-vertical`, `main-vertical-mirrored`, and `tiled` ([layout-set.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/layout-set.c)). A layout string is the format `window_layout` prints ([layout strings](#layout-strings)). Tmux accepts JSON layouts and legacy checksummed layouts, and rejects invalid geometry or syntax ([layout-custom.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/layout-custom.c), `layout_parse`). `-E` spreads the target pane and its neighbours evenly, `-n`/`-p` step through presets, and `-o` restores the previous layout.

Sizing options that interact with these commands: `window-size` (`largest`, `smallest`, `manual`, `latest`. Default `latest`), `aggressive-resize`, `default-size`, and the `ignore-size` client flag.

A sizing client gives the current window its terminal rows minus the session's `status` lines (`off`: 0, `on`: 1, `2` through `5`: that many). The window keeps that size after the client detaches ([resize.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/resize.c), [status.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/status.c), `status_line_size`; probed on 3.8).

### Pane I/O

**`capture-pane [-aCeFHIJLMNpPqRT] [-b buffer] [-E end] [-S start] [-t pane]`**

| Flag | Effect |
| --- | --- |
| `-p` | write to stdout (otherwise into a paste buffer) |
| `-S` / `-E` | first / last line: 0 is the top visible line, negatives reach into history, `-` means history start / visible end, the default is the visible screen |
| `-e` | include SGR escape sequences for text attributes and colours |
| `-C` | escape non-printable characters as octal `\xxx` |
| `-J` | join wrapped lines and keep trailing spaces (implies `-T`) |
| `-N` | keep trailing spaces |
| `-T` | stop at the last used cell of each line |
| `-a` / `-q` | capture the alternate screen / tolerate its absence |
| `-M` | capture the active mode's screen, such as copy mode (3.6) |
| `-P` | capture only the start of an incomplete escape sequence |
| `-L` / `-F` | prefix each line with its number / its flags (`-` none, `D` unused, `O` output, `P` prompt, `X` extended cells, `H` hyperlinks, `W` wrapped) |
| `-H` | capture only the hyperlinks in the selected lines |
| `-I` | prefix each line with epoch seconds when it entered history, or 0 when no time is recorded (3.8) |
| `-R` | dump grid, line, and cell diagnostics as text |

The accepted capture flags are defined in [cmd-capture-pane.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-capture-pane.c).

**`send-keys [-FHKlMRX] [-c client] [-N count] [-t pane] key…`** looks each argument up as a key name (`C-c`, `M-a`, `Enter`, `Escape`, `F1`, `NPage`, `User0`…) and sends an argument that is not a key name as its characters. `-l` disables lookup and sends every argument as literal UTF-8. `--` protects arguments that start with `-` (probed). `-H` sends hex bytes. `-N` repeats. `-R` resets the terminal state. `-X` sends a copy-mode command.

**`load-buffer [-w] [-b buffer] [-t client] path`** reads a file into a named paste buffer, and a `path` of `-` reads stdin ([file.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/file.c), `file_read`). `-w` also sends the buffer to the client's clipboard. **`delete-buffer [-b buffer]`** removes one buffer.

**`paste-buffer [-dprS] [-s separator] [-b buffer] [-t pane]`** inserts a paste buffer into a pane:

| Flag | Effect |
| --- | --- |
| (default) | replace each LF with CR |
| `-s <sep>` / `-r` | use another separator / no replacement (LF stays LF) |
| `-S` | skip the `vis(3)` control-character sanitization that 3.7 applies by default ([cmd-paste-buffer.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-paste-buffer.c)), tmux before 3.7 rejects `-S` |
| `-p` | add bracketed-paste markers when the application requested bracketed paste mode |
| `-d` | delete the buffer after pasting |

**`display-message [-aCIjlNpv] [-c client] [-d delay] [-F format] [-t pane] [message]`**. `-p` prints the expanded format to stdout, which makes it the general way to evaluate a format against a target. A window target resolves to that window's active pane. `-a` lists every format variable with its value, `-v` logs each expansion step (how a missing variable is found), `-I` forwards stdin into an empty pane, and `-C` keeps the pane updating while a message shows (3.6). `-F` supplies the format when no positional message is given. `-j` parses the expanded message as JSON and serializes it, or fails on invalid JSON ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-display-message.c)).

**`pipe-pane [-IOo] [-t pane] [shell-command]`** streams a pane's output (`-O`, the default) or input (`-I`) through a shell command. With no command it closes the pipe, and `-o` opens one only when none is open. `pane_pipe_pid` holds the pipe process id (3.7).

### Key bindings and scripting

**`bind-key [-nr] [-N note] [-T key-table] key [command [argument…]]`** binds a key to a tmux command in a key table. `-n` is `-T root`, so the binding fires without the prefix from any pane on the server. Key tables are server-global. `-r` makes the key repeatable. A key named `UserN` is the sequence defined at index N of the `user-keys` option ([key-string.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/key-string.c)).

**`run-shell [-bCE] [-c dir] [-d delay] [-t pane] [shell-command [argument…]]`** runs a command through `/bin/sh -c`, or a tmux command with `-C`, after expanding formats. It blocks the command queue until done unless `-b`. `-E` sends stderr to the output as well (3.6), and arguments after the command expand as `#{1}`, `#{2}`, and so on (3.7).

**`if-shell [-bF] [-t pane] condition command [command]`** runs the first command when a shell condition exits 0, otherwise the second. With `-F` the condition is a format, true when it expands to something other than empty or `0`, and no shell runs. The branches are tmux command strings, parsed when the condition resolves.

**`wait-for [-ELSUlv] [-F format] [-w waiter] name`** waits on a channel by default. `-S` wakes channel waiters. `-L` and `-U` lock and unlock a channel. In 3.8, `-E` waits for an event and `-v` prints its payload. `-F` filters event payloads, `-l` lists waiters, and `-w` wakes one named waiter ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-wait-for.c)).

**`display-popup [-BCEkN] [-b border-lines] [-c client] [-d dir] [-e VAR=val] [-h height] [-w width] [-x x] [-y y] [-s style] [-S border-style] [-T title] [-t pane] [shell-command…]`** shows a modal overlay running a command (3.2). `-E` closes it when the command exits, `-EE` only on success, `-C` closes an open popup, and `-k` lets any key close it after the command exits (3.6). Sizes and positions accept `%`. Popups run under `default-shell`.

### Option, hook, and environment commands

**`set-option [-aFgopqsuUw] [-t target] option [value]`** (alias `set`). The scope flag picks the option table: `-s` server, none session, `-w` window (`set-window-option` is the same), `-p` pane. `-g` addresses the global table of that scope. For built-in options tmux infers the table from the name, so scope flags matter for user options (`@name`) and to force pane over window scope. A local value shadows the global one.

| Flag | Effect |
| --- | --- |
| `-u` | unset the local value, revealing the global |
| `-U` | for a pane option, also unset it in every pane of the window |
| `-q` | suppress unknown-option and ambiguous-option errors |
| `-o` | set only when not already set |
| `-a` | append (styles get a comma) |
| `-F` | expand formats in the value |

**`show-options [-AgHpqsvw] [-F format] [-t target] [option]`**. `-v` prints the value alone (`show-options -gv base-index`), `-A` includes inherited values, `-H` includes hooks, and `-q` suppresses errors for unset options.

**`set-environment [-Fghru] [-t session] name [value]`** and **`show-environment [-ghs] [-t session] [name]`**. Name and value are separate argv tokens. A single `NAME=value` argument fails `variable name contains =` (probed). `-g` targets the global environment, `-r` marks a variable for removal from new panes, `-u` unsets it, `-h` hides it from panes, and `show-environment -s` prints shell `export` lines.

**`set-hook [-agpERTuw] [-B name:what:format] [-t target] [hook] [command]`** and **`show-hooks [-Bgpw] [-F format] [-t target] [hook]`** manage hooks. `-R` runs a hook immediately. Monitors and user events are described under [hooks](#hooks). `show-hooks` and `show-options` accept `-F` output formats in 3.8 ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-show-options.c)).

## Formats

Formats are tmux's read surface: every `-F`, filter, hook command, and `#()` expands through them. An unknown variable expands to the empty string without an error, so a misspelled variable is silent and a multi-column `-F` row with a printable separator gets an empty column instead of shifted ones. `display-message -p` evaluates a format, `-a` lists variables, and `-v` traces the expansion.

### Language

`#{name}` expands a variable or an option value (`#{automatic-rename}`, `#{@user_option}`). Short aliases are `#S` session_name, `#W` window_name, `#I` window_index, `#D` pane_id, `#P` pane_index, `#T` pane_title, `#F` window_flags, and `#H`/`#h` host. `##` is a literal `#`. Modifiers go inside the braces before a colon, and several combine with `;` (`#{T;=10:status-left}`).

| Modifier | Meaning |
| --- | --- |
| `#{?cond,yes,no}` | conditional, since 3.6 repeated condition and value pairs, an optional final default, and an empty implicit default, escape `,` and `}` as `#,` and `#}` |
| `==`, `!=`, `<`, `>`, `<=`, `>=` | string comparison: `#{==:a,b}` |
| `&&`, `\|\|`, `!`, `!!` | boolean and, or (N-ary since 3.6), not, and canonical boolean |
| `m:`, `m/r:`, `m/i:` | fnmatch match, regular expression match, ignore case |
| `C:` | search pane content, yielding a line number |
| `e\|op\|f\|digits:` | arithmetic (`+ - * / m %` and comparisons, `f` for floating point) |
| `a:`, `c:` | ASCII character from a number, colour to RGB hex |
| `t:`, `t/f/<fmt>:`, `t/p:` | epoch to time string, custom strftime, abbreviated past time |
| `b:`, `d:` | basename, dirname |
| `q:`, `q/h:`, `q/a:` | escape for `sh(1)`, also escape `#`, escape as tmux command arguments |
| `E:`, `T:` | expand the result again, also expand strftime sequences |
| `S:`, `W:`, `P:`, `L:` | loop over sessions, windows, panes, clients, a second format applies to the current item, `/i`, `/n`, `/t`, `/r` sort by index, name, activity, or reverse (3.6), `loop_last_flag` marks the last item, and `W:` sets `next_window_*`/`prev_window_*` (3.7) |
| `N/w:`, `N/s:` | whether a window or session with this name exists |
| `s/pat/rep/:` | regex substitution (any delimiter, `i` flag) |
| `=N:`, `=/N/marker:` | truncate to N columns (negative from the end), with a marker |
| `pN:`, `n:`, `w:` | pad, length, display width |
| `l:` | literal, no expansion |
| `R:value,count` | repeat (3.6) |
| `A/count:` | animate a sequence of frames (3.8) |
| `O:`, `V:` | loop over options / environment variables (3.8) |
| `I:` | client terminal information (3.8) |
| `t/d:`, `c/f:`, `c/b:` | time difference in seconds / foreground colour / background colour (3.8) |
| `#(command)` | last line of a shell command's output, cached, refreshed at most once a second, never blocks (empty until the first run completes), runs `/bin/sh` with the global environment |

The 3.8 format additions are specified in [tmux.1](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tmux.1) and [format.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/format.c). The `m` modifier also supports multiple terms and fuzzy matching.

### The variables RimZ reads

| Variable | Value | Notes |
| --- | --- | --- |
| `session_name` | session name (`#S`) | changes with `rename-session` |
| `session_id` | `$N` | server-unique |
| `window_id` | `@N` | server-unique, monotonic |
| `window_name` | window name | fixed once named by `new-window -n` or `rename-window` (per-window `automatic-rename` off) |
| `window_index` | position | `renumber-windows` rewrites it, ids never change |
| `window_width` / `window_height` | cells | |
| `window_zoomed_flag` | 1 if a pane is zoomed | |
| `window_layout` | layout string | [layout strings](#layout-strings) |
| `pane_id` | `%N` (`#D`) | server-unique, monotonic, exported as `$TMUX_PANE` |
| `pane_index` | position in the window | changes as panes close |
| `pane_active` | 1 if the window's active pane | one per window, so N windows report N active panes |
| `pane_floating_flag` | 1 if the pane floats above the tiled layout | 3.7, earlier releases expand it empty |
| `pane_left` / `pane_top` / `pane_width` / `pane_height` | geometry in cells | relative to the window |
| `pane_at_left` / `pane_at_top` / `pane_at_bottom` / `pane_at_right` | 1 if the pane touches that window edge | |
| `pane_current_command` | foreground process name | the name, not its argv |
| `pane_current_path` | foreground process working directory | |
| `pane_pid` | PID of the pane's first process | the spawned shell or command, never the foreground child |
| `pane_title` | title set by OSC 0/2 or `select-pane -T` | defaults to the host name, writable by applications while `allow-set-title` is on, sanitized of C0 and invisible characters (3.7) |
| `pane_start_command` / `pane_start_path` | command and directory the pane was created with | a pane spawned with one shell-command argument (`split-window 'a b c'`) reports it as one double-quoted string, `"a b c"`, a pane spawned with an argv (`respawn-pane -k a b c`) reports the words joined by spaces, unquoted (probed on 3.7c), empty for a pane created with no command while `default-command` is empty (the default), that is a pane running `default-shell` ([spawn.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/spawn.c)) |
| `pane_dead` / `pane_dead_status` / `pane_dead_signal` / `pane_dead_time` | exit facts for a pane kept by `remain-on-exit` | |
| `client_name` / `client_tty` / `client_pid` / `client_session` | client identity | |
| `client_width` / `client_height` | client terminal size | |
| `client_activity` | last activity time, epoch seconds | |
| `client_flags` | comma-separated client flags (`attached`, `focused`, `control-mode`, `ignore-size`, `read-only`, …) | |
| `client_control_mode` | 1 for a control client | |
| `client_termname` | client terminal name (`$TERM`) | |
| `socket_path` / `pid` / `start_time` / `version` | server facts | `start_time` is the server's start |

**There is no `pane_start_time`.** tmux 3.8 defines no per-pane start-time variable: [format.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/format.c) has no such name, and `display-message -v` logs `format 'pane_start_time' not found` while the column expands empty (probed on 3.5a and 3.7b). The nearest facts are `pane_pid` and the never-reused `%id` itself. A process start time comes from the operating system for `pane_pid`.

### Output sanitization

Command output to a client that is not marked UTF-8 is sanitized: every byte outside printable ASCII, tab included, becomes `_`, and each UTF-8 character becomes one `_` per display column ([server-client.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/server-client.c), `server_client_print`, [utf8.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/utf8.c), `utf8_sanitize`). A client is UTF-8 when it runs with `-u`, when `$TMUX` is set, or when the first non-empty of `LC_ALL`, `LC_CTYPE`, and `LANG` contains `UTF-8` or `UTF8` ([tmux.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tmux.c), `main`). A parser that must work under any locale uses printable separators and substitutes the separator out of free-form fields (`#{s/,/_/g:pane_title}`).

### Catalog

The man page's FORMATS table lists over 200 variables at 3.8, in these families: `buffer_*` (including `buffer_full`), `client_*` (geometry, flags, tty, uid, `client_theme`), `command_*`, copy mode (`copy_cursor_*`, `selection_*`, `search_*`, `scroll_position`), `cursor_*`, `history_*` (`history_size`, `history_limit`, `history_bytes`), `hook_*`, `mouse_*`, `pane_*` (geometry, edges, flags, modes, `pane_pipe_pid`, `pane_pb_state`/`pane_pb_progress` for `OSC 9;4` progress, 3.7), `session_*` (counts, times, groups, `session_attached`, alerts), `window_*` (geometry, flags, counts, `window_layout`), `next_window_*`/`prev_window_*`, `loop_last_flag`, `sixel_support`, and the server variables.

New adapter-facing variables in 3.8 are defined in [format.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/format.c):

| Variable | Value |
| --- | --- |
| `pane_start_command_list` | start arguments individually shell-quoted and joined by spaces |
| `pane_last_output_time`, `pane_output_generation` | last output time and generation incremented when output is processed |
| `history_added`, `history_collected`, `history_generation` | lines added, oldest lines discarded at the limit, and generation for clears or reflows |
| `pane_command_running`, `pane_command_status` | OSC 133 command state and last exit status |
| `pane_command_start_time`, `pane_command_end_time`, `pane_command_duration`, `pane_last_prompt_time` | OSC 133 times and command duration in seconds |
| `pane_modal_flag`, `window_modal_pane` | modal state and window's modal pane |
| `pane_private_modes` | enabled DECSET private modes |
| `window_manual_width`, `window_manual_height` | manual window dimensions |
| `client_colours` | client colour count |

## Hooks

Hooks run tmux commands on triggers. They are array options stored globally or per session, window, or pane. `set-hook -g name[key] command` sets one member. Numeric keys run first in numeric order, followed by named keys in lexical order. Setting a hook without a key clears the array and sets member 0 ([options.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options.c)). A hook's command is parsed by tmux, not by a shell, so a shell command inside it needs its own quoting layer.

**After hooks.** A command that has an `after-<command>` hook fires it when the command completes, except when the command itself ran from a hook, so a hook that runs `split-window` cannot trigger `after-split-window`. The man page says "most" commands have one. At 3.8 the after hooks are `after-bind-key`, `after-capture-pane`, `after-copy-mode`, `after-display-message`, `after-display-panes`, `after-kill-pane`, `after-list-buffers`, `after-list-clients`, `after-list-keys`, `after-list-panes`, `after-list-sessions`, `after-list-windows`, `after-load-buffer`, `after-lock-server`, `after-new-session`, `after-new-window`, `after-paste-buffer`, `after-pipe-pane`, `after-refresh-client`, `after-rename-session`, `after-rename-window`, `after-resize-pane`, `after-resize-window`, `after-save-buffer`, `after-select-layout`, `after-select-pane`, `after-select-window`, `after-send-keys`, `after-set-buffer`, `after-set-environment`, `after-set-hook`, `after-set-option`, `after-show-environment`, `after-show-messages`, `after-show-options`, `after-split-window`, `after-swap-window`, and `after-unbind-key` ([options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c)).

**Named hooks.** The table column is the option scope from [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c) (`OPTIONS_TABLE_HOOK`, `OPTIONS_TABLE_WINDOW_HOOK`, `OPTIONS_TABLE_PANE_HOOK`). After hooks are session hooks. Since 3.2, window hooks live in the window table and pane hooks in the window or pane table, so a window or pane hook set on a session never fires. `set-hook -g` reaches every global table.

| Hook | Fires when | Table |
| --- | --- | --- |
| `alert-activity` / `alert-bell` / `alert-silence` | a window alert fires (`monitor-*` options) | session |
| `client-active` | a client becomes the latest active client of its session (3.2) | session |
| `client-attached` / `client-detached` | a client attaches / detaches | session |
| `client-focus-in` / `client-focus-out` | a client's terminal gains / loses focus | session |
| `client-light-theme` / `client-dark-theme` | a client's terminal reports a theme change (3.6) | session |
| `client-resized` | a client is resized | session |
| `client-session-changed` | a client switches session | session |
| `command-error` | a command fails (3.5) | session |
| `session-created` / `session-closed` / `session-renamed` | session lifecycle | session |
| `session-window-changed` | a session's current window changes | session |
| `window-linked` / `window-unlinked` | a window is linked into / unlinked from a session | session |
| `window-renamed` | a window is renamed | window |
| `window-layout-changed` | a window's layout changes | window |
| `window-pane-changed` | a window's active pane changes | window |
| `window-resized` | a window is resized, after `client-resized` (3.2) | window |
| `pane-died` | a pane's command exits and `remain-on-exit` keeps it | window, pane |
| `pane-exited` | a pane's command exits and the pane closes | window, pane |
| `pane-focus-in` / `pane-focus-out` | a pane gains / loses focus (requires `focus-events on`) | window, pane |
| `pane-mode-changed` | a pane enters or leaves a mode | window, pane |
| `pane-set-clipboard` | an application sets the clipboard via OSC 52 | window, pane |
| `pane-title-changed` | a pane's title changes | window, pane |

Additional hooks in 3.8 come from [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c):

| Hook | Fires when | Table |
| --- | --- | --- |
| `client-created`, `client-closed` | client lifecycle | session |
| `marked-pane-changed` | marked pane is set or cleared | session |
| `session-added-to-group`, `session-removed-from-group` | group membership changes | session |
| `window-created`, `window-closed` | window lifecycle | window |
| `window-zoomed`, `window-unzoomed` | zoom changes | window |
| `pane-activity`, `pane-bell` | output arrives / bell arrives | window, pane |
| `pane-created` | a pane is created or respawned | window, pane |
| `pane-mode-entered`, `pane-mode-exited` | mode lifecycle | window, pane |
| `pane-prompt-opened`, `pane-prompt-closed` | pane prompt lifecycle | window, pane |
| `pane-moved`, `pane-resized` | a pane changes window / size | window, pane |
| `pane-command-started`, `pane-command-finished`, `pane-shell-prompt` | OSC 133 command or prompt events | window, pane |

After hooks gain `after-swap-window` in 3.8. `after-queue` is removed in 3.8. It exists in the [3.7c option table](https://github.com/tmux/tmux/blob/3.7c/options-table.c) but not in 3.8.

Hooks and control notifications share an event system in 3.8. Their names are not interchangeable: `window-layout-changed` produces `%layout-change`, for example ([control-notify.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control-notify.c)). Hook context includes `hook`, `hook_client`, `hook_session`, `hook_window`, `hook_pane`, `hook_session_name`, and `hook_window_name`.

Event payload keys become `hook_*` formats ([hooks.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/hooks.c)). OSC 133 command events carry `command_status`, `command_start_time`, `command_end_time`, and `command_duration` when those values are available ([input.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/input.c)). `wait-for -E -v pane-command-finished` waits for one event and prints its payload. These signals depend on the application emitting OSC 133.

| Monitor or event command | Effect |
| --- | --- |
| `set-hook -B '@name:what:format' [command]` | check the format once per second and run the hook when its value changes |
| `set-hook -T -B '@name:what:format' [command]` | run only when the changed value is true |
| `set-hook -u -B @name` | remove a monitor |
| `show-hooks -B` | list monitors |
| `set-hook -E @name` | fire a user event |
| `wait-for -E -v @name` | wait for a user event and print its payload |

Monitor names and user event names must start with `@`. The first monitor check records a baseline without firing. Later changes trigger the hook. Monitors run only in their creation scope and are not inherited. `what` uses the [subscription selectors](#client-flags-and-flow-control). Monitor formats include `hook_last`, `hook_value`, `hook_monitor_format`, and `hook_monitor_target`. Hook option formats include `hook_fire_count` and `hook_fire_time` ([man page](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tmux.1)).

## Options

Server options have one table. Session and window options inherit global defaults, and pane options inherit window values. Local values override inherited values. The table lists selected adapter-facing options, with scope, choices, and defaults from [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c) at 3.8. The values RimZ writes are in [configuration → multiplexer room options](../../guide/configuration.md#multiplexer-room-options) and [multiplexers.md → room options](../../internals/multiplexers.md#room-options).

| Option | Scope | Values | Default | Meaning |
| --- | --- | --- | --- | --- |
| `theme` | server | detect, terminal, light, dark | detect | select built-in colours or preserve terminal colours with `terminal` (3.8) |
| `clear-on-attach` | server | on, off | on | off preserves pre-attach terminal contents in scrollback by avoiding the alternate screen (3.8) |
| `focus-events` | server | on, off | off | request focus reporting from the terminal and pass focus events to applications, enables `pane-focus-*` hooks, clients re-attach to pick up a change |
| `set-clipboard` | server | on, external, off | external | `on` accepts OSC 52 from applications into a buffer and forwards it to the terminal, `external` only forwards, forwarding needs the terminal's `Ms` capability or the `clipboard` feature |
| `get-clipboard` | server | off, buffer, request, both | buffer | how an application's clipboard read is answered (3.7), `CHANGES` says the default is off, [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c) sets `buffer` |
| `extended-keys` | server | on, off, always | off | `on` honours application requests for modifyOtherKeys mode 1 or 2, `always` sends extended keys unrequested |
| `extended-keys-format` | server | csi-u, xterm | xterm | encoding of extended keys: `C-S-a` is `^[[65;6u` (csi-u) or `^[[27;6;65~` (xterm) (3.5) |
| `terminal-features[]` | server | `<terminal-pattern>:<feature>:…` | `xterm*:clipboard:ccolour:cstyle:focus:title`, `screen*:title`, `rxvt*:ignorefkeys` | features tmux assumes for outer terminals whose `TERM` matches, when detection cannot find them, features include `256`, `RGB`, `clipboard`, `extkeys`, `focus`, `hyperlinks`, `mouse`, `osc7`, `sixel`, `sync` (synchronized output), `title`, `usstyle` |
| `user-keys[]` | server | escape sequences | empty | each index N names a key `UserN` that `bind-key` can bind, a config file expands `\e`, an argv value needs the literal byte |
| `escape-time` | server | milliseconds | 10 | how long tmux waits after ESC to tell a key from an escape sequence (500 before 3.5), extended while a partial paste end or forwarded request is pending (3.7) |
| `mouse` | session | on, off | off unless built with `TMUX_MOUSE=1` | `CHANGES` says on, but [tmux.h](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tmux.h) defines `TMUX_MOUSE` as 0 by default |
| `history-limit` | session | lines | 2000 | scrollback per pane, since 3.7 a change also trims existing panes (the option's help text still says new panes only, while `CHANGES`, [options.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options.c), and a live probe agree on the new behaviour) |
| `renumber-windows` | session | on, off | off | renumber windows when one closes, from `base-index` |
| `set-titles` | session | on, off | off | set the outer terminal's title for attached clients whose terminal can set titles |
| `set-titles-string` | session | format | `#S:#I:#W - "#T" #{session_alerts}` | the title `set-titles` writes |
| `allow-passthrough` | window, pane | off, on, all | off | honour the `\ePtmux;…\e\\` passthrough escape, `on` only while the pane is visible, `all` always (3.3, `all` 3.4) |
| `aggressive-resize` | window | on, off | off | size a window to the clients currently viewing it, not every client whose session contains it |
| `window-size` | window | largest, smallest, manual, latest | latest | how a window's size follows its clients, `resize-window` sets `manual` |
| `automatic-rename` | window | on, off | on | derive the window name from the active pane's command |
| `pane-border-status` | window, pane | off, top, bottom, top-floating, bottom-floating | off | draw a text row on each pane border |
| `pane-border-format` | window, pane | format | active-pane marker and `#{pane_index} "#{pane_title}"` | the text in that row |
| `pane-border-lines` | window, pane | single, double, heavy, simple, number, spaces | single | border characters, `simple` is plain ASCII, `spaces` 3.6 |
| `default-size` | session | `WxH` | 80x24 | size of a detached session's windows, `new-session -x`/`-y` set it |

In 3.8, `terminal-features` accepts `utf8` and `appesc`. Append `@` to a feature name to disable it, such as `sync@` ([source](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/tty-features.c)). Styles accept theme colour names, colour formats, `dim=`, `link=`, and `nolink` ([changelog](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/CHANGES)).

`pane-scrollbars` adds `auto-hide`, controlled by `pane-scrollbars-timeout`. The changelog says auto-hide replaces modal behaviour, but `modal` remains an accepted choice in [options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c).

**User options.** A name starting with `@` is a user option: any scope, any string value, readable in formats as `#{@name}`, and not writable by applications through escape sequences. Scope flags on `set-option` matter for user options because tmux cannot infer their table.

Other options a contributor will reach for: `base-index` (first window index), `remain-on-exit` (`on`, `off`, `failed`, `key`, `failed-key`. `key` arrived in 3.7, `failed-key` in 3.8) with `remain-on-exit-format`, `detach-on-destroy` (`off`, `on`, `no-detached`, `previous`, `next`), `destroy-unattached` (`off`, `on`, `keep-last`, `keep-group`), `exit-empty`/`exit-unattached` (server lifetime), `allow-rename` (applications rename windows by escape, default off), `allow-set-title` (applications set `pane_title`, default on), `automatic-rename-format`, `focus-follows-mouse` (session, 3.7), `update-environment[]` ([environment](#global-and-session-environment)), `default-terminal` (default `screen` unless the build sets another), `default-command`/`default-shell`, `popup-style`/`popup-border-style`/`popup-border-lines` (3.3), `synchronize-panes`, `monitor-activity`/`monitor-bell`/`monitor-silence`, and the 3.6 and 3.7 additions `pane-scrollbars`, `pane-scrollbars-position`, `pane-scrollbars-style`, `tiled-layout-max-columns`, `codepoint-widths[]`, `variation-selector-always-wide`, `copy-mode-position-*`, `copy-mode-selection-style`, `copy-mode-line-numbers` with its styles, `initial-repeat-time`, `input-buffer-size`, `default-client-command`, `tree-mode-preview-*`, and `message-format`.

## Global and session environment

The server copies the environment it starts with into the **global environment**, and each session keeps a **session environment**. A new pane's process receives the global environment merged with the session environment (session wins), plus `TMUX`, `TMUX_PANE`, and `TERM` from `default-terminal`. The merge happens when the pane is created, so environment changes never reach running processes.

- `new-session -e`, `new-window -e`, `split-window -e`, and `respawn-pane -e`/`respawn-window -e` set variables at creation, so even a session's first window sees them.
- `set-environment -t <session> NAME VALUE` reaches only panes created afterwards.
- `update-environment[]` (session option, fnmatch patterns allowed) copies the listed variables from the attaching client into the session environment on `new-session` and on every attach. A variable the client lacks is marked for removal. The default list is `DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE` ([options-table.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/options-table.c)), so an attach from a new SSH connection rewrites the `SSH_*` values for later panes. `-E` on `attach-session` or `new-session` skips the copy.
- A variable set with `-h` is hidden: it stays in the table for formats and is never exported to panes.

## Control mode

`tmux -C` makes a client a line protocol endpoint: commands go in on stdin, and command replies and asynchronous notifications come out on stdout. `-CC` also puts the terminal in raw mode and wraps the stream in a `\eP1000p` DCS opening and a closing `\e\\` (the iTerm2 integration shape). Plain `-C` writes no terminal markers. A typical observer attaches with `tmux -C attach-session -f ignore-size,no-output -t <session>`.

### Protocol shape

- Reply blocks. Each stdin line contains a command or `;` sequence. Each executed command produces its own reply block: `%begin <time> <number> <flags>`, the output lines, then `%end` on success or `%error` on failure with the same three arguments. `time` is epoch seconds, `number` identifies the command, and `flags` is 1 when the command came from this control client and 0 otherwise ([cmd-queue.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/cmd-queue.c), `cmdq_guard`). The man page documents `flags` as unused.
- Notifications are queued outside reply blocks in 3.8. Older versions could interleave notifications with `%begin`/`%end`. See [control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c).
- An unparseable command still gets a block: `%begin`, `parse error: …`, `%error`.
- An empty stdin line detaches the client. With the `wait-exit` flag, the client waits for an empty line after `%exit` before it exits. Closing stdin also ends the client.
- Size. Control clients have no size and take no part in size negotiation until `refresh-client -C WxH` (or `-C @win:WxH` per window) gives them one.
- Exit. The last line is `%exit [reason]`, written by the client. Reasons are `detached`, `detached (from session <name>)`, `detached and SIGHUP`, `detached and SIGHUP (from session <name>)`, `lost tty`, `terminated`, `too far behind`, `exited` (the server had no sessions), `server exited`, and `server exited unexpectedly` ([client.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/client.c), `client_exit_message`, [control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c)).

### Notification catalog

Wire shapes are from [control-notify.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control-notify.c) and [control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c) at 3.8. How the backend classifies each line is in [state.md → what triggers a mux-derived event](../../internals/sidebar/state.md#what-triggers-a-mux-derived-event).

Notifications from a batched command list arrive in command order (probed on 3.7c). A `select-pane` on a hidden window reports `%window-pane-changed` without `%session-window-changed`; selecting the window afterward reports `%session-window-changed` with the target pane already active. The `pane_active` subscription changes follow these notifications.

| Notification | Wire shape | Sent when |
| --- | --- | --- |
| `%window-add` | `%window-add @id` | a window is linked into the client's session |
| `%window-close` | `%window-close @id` | a window in the client's session closes |
| `%window-renamed` | `%window-renamed @id name` | a window in the client's session is renamed |
| `%unlinked-window-add` / `-close` / `-renamed` | `%unlinked-window-… @id [name]` | the same events for a window not in the client's session, judged per client |
| `%layout-change` | `%layout-change @id <layout> <visible-layout> <raw-flags>` | a window's layout changes (split, close, resize), older releases sent fewer fields, so parse at least the id and the layout |
| `%window-pane-changed` | `%window-pane-changed @id %id` | a window's active pane changes |
| `%session-window-changed` | `%session-window-changed $id @id` | a session's current window changes |
| `%session-changed` | `%session-changed $id name` | this client switches session |
| `%client-session-changed` | `%client-session-changed <client> $id name` | another client switches session |
| `%session-renamed` | `%session-renamed $id name` | a session is renamed, the man page documents the name alone, so parse the id first |
| `%sessions-changed` | `%sessions-changed` | a session is created or destroyed |
| `%client-detached` | `%client-detached <client>` | a client detaches (3.2) |
| `%pane-mode-changed` | `%pane-mode-changed %id` | a pane enters or leaves a mode such as copy mode |
| `%output` | `%output %id <value>` | pane output, bytes below 0x20 and `\` are escaped as octal `\nnn`, other bytes pass raw, so a line can split a UTF-8 sequence |
| `%extended-output` | `%extended-output %id <age-ms> … : <value>` | pane output under `pause-after`, ignore fields between the age and the lone `:` |
| `%pause` / `%continue` | `%pause %id` / `%continue %id` | pause-mode flow control |
| `%subscription-changed` | `%subscription-changed name $id @id <window-index> %id … : value` | a `refresh-client -B` format's value changed, window subscriptions put `-` in the pane field, session subscriptions in the window, index, and pane fields, checked at most once a second |
| `%paste-buffer-changed` / `%paste-buffer-deleted` | `%paste-buffer-… name` | a paste buffer changes / is deleted (3.4) |
| `%config-error` | `%config-error <error>` | a configuration file error (3.4) |
| `%message` | `%message <text>` | `display-message` without `-p` targets this client |
| `%exit` | `%exit [reason]` | the client is about to exit |

### Layout strings

tmux 3.8 emits JSON layout strings by default. `window_layout` describes the saved unzoomed layout, while `window_visible_layout` describes the visible layout. A control client without `new-layouts` receives the legacy format, including format evaluations in that client's context ([format.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/format.c)). Ordinary command clients receive JSON.

The JSON schema comes from [layout-custom.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/layout-custom.c):

| Key | Meaning |
| --- | --- |
| `V` | top-level version, currently `2` |
| `L` | top-level root cell |
| `t` | cell type: `h` horizontal, `v` vertical, `p` pane |
| `w`, `h`, `x`, `y` | cell width, height, horizontal position, vertical position. Positions may be negative. |
| `c` | child cells for a group |
| `I` | pane ID as `%N`, ignored when parsing a layout |
| `i` | pane index |
| `a` | true for the active pane |
| `l` | position in the last-pane list, when visited and inactive |
| `z` | z-index for a floating pane |

A single-pane schema example, derived from the serializer:

```json
{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":0,"I":"%0"}}
```

`select-layout` accepts both formats. The legacy format is deprecated in the source. It starts with a four-hex-digit checksum and comma, then a cell tree. Each cell is `WxH,X,Y`, followed by `,<pane-number>` for a leaf, `{…}` for horizontal children, or `[…]` for vertical children.

Legacy restoration ignores the written pane numbers: it assigns panes in native pane-list order to depth-first leaves. Excess leaves are pruned from the bottom-right; too few leaves fail. Containers may nest directly inside containers of the same orientation. JSON restoration instead binds by `i`, not `I`, so it can preserve a mapping whose pane-list and depth-first orders differ ([layout-custom.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/layout-custom.c)).

Historical 3.7c output appended floating panes as `<WxH,X,Y,<pane>,…>`. Its parser rejected that suffix, so floating layouts could not round-trip. Version 3.8 legacy output omits the suffix and carries no floating marker. Read `pane_floating_flag` separately when using that format. JSON layouts carry `z` on floating leaves and remove the restoration restriction ([changelog](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/CHANGES)).

A custom tiled legacy layout at 3.7c counts floating panes during assignment and can absorb them into tiled leaves; a rewrite that must preserve floating state must refuse such windows ([3.7c layout-custom.c](https://github.com/tmux/tmux/blob/e476c1230b958df0cb12977517d24b3dc931375b/layout-custom.c)).

### Client flags and flow control

- `new-layouts` selects JSON layouts for a control client in 3.8. Without it, layout notifications retain the legacy format.
- `no-output` suppresses pane output entirely, both `%output` and `%extended-output`.
- Without `pause-after`, tmux disconnects a client whose buffered output ages past five minutes (`CONTROL_MAXIMUM_AGE 300000` ms in [control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c)), with exit reason `too far behind`. A `no-output` client still receives notifications and should keep reading.
- In 3.8, buffered command replies reaching 64 MiB disconnect the client with `too far behind`, independently of pane-output age ([control.c](https://github.com/tmux/tmux/blob/7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53/control.c)).
- `pause-after=<secs>` switches output to `%extended-output` and pauses a pane (`%pause`) whose output falls that far behind, instead of disconnecting. `refresh-client -A %id:<state>` controls one pane: `continue` resumes it (`%continue`), `pause` pauses it now, `off` stops its output to this client, and `on` turns output back on. When every client has turned a pane off, tmux stops reading that pane's pty, which applies backpressure to the application.
- `refresh-client -B name:what:format` subscribes to a format. `what` is empty for the attached session, a `%id`, `%*` for every pane in the session, an `@id`, or `@*` for every window. Changes arrive as `%subscription-changed`, checked at most once a second, and `-B name` alone unsubscribes.
- `refresh-client -f <flags>` changes flags on a live client, `-r %id:<report>` lets a control client answer OSC 10 and 11 colour queries for a pane (3.5), and `-l` requests the terminal clipboard into a paste buffer.

### Read-only clients

The `read-only` flag is not a command sandbox. It limits key bindings, and each command adds its own checks for commands sent by a read-only client: 3.7 restricts `attach-session`, `detach-client`, and `switch-client` so a read-only user can only detach their own client, and `send-keys` refuses a read-only target client unless it sends a copy-mode command with `-X`. Other commands on a read-only control client's stdin can still change server state, so a watcher that must not mutate anything restricts what it writes to stdin.
