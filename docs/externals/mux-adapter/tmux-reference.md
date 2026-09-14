# tmux upstream reference

> RimZ's side of the seam (the `MuxBackend` trait, the managed server endpoint, the sidebar hook, the presence watch, the room options RimZ writes, and the version floor it enforces) lives in [multiplexers.md → tmux backend](../../internals/multiplexers.md#tmux-backend). This page mirrors the upstream surface only.

This page mirrors the tmux surface RimZ binds to: the client and server model, the command verbs the backend adapter runs, the format language, hooks, options, the session environment, and the control-mode protocol. The baseline is **tmux 3.7c**, tag [`3.7c`](https://github.com/tmux/tmux/releases/tag/3.7c) at commit `e476c1230b95`, published as a GitHub release on 2026-08-17 (the tag commit is dated 2026-07-23); the man page, `CHANGES`, and source were read on 2026-09-14. Source paths below are relative to the repository root at that tag, and the installed `tmux -V` on the capture host prints `tmux 3.7c`. Where the man page and the source disagree, the page follows the source and flags the disagreement at the claim.

Coverage is depth on what the backend wires and breadth as an index: the commands the adapter runs, the format variables it reads, the options and hooks it sets, and the control-mode lines it parses get full shapes; the rest of each catalog is listed so a contributor wiring something new knows it exists.

## Upstream sources

tmux ships no documentation beyond the man page, so `tmux.1` is the reference; the wiki lags it, and control-mode wire shapes are defined only in source.

| Surface | Source |
| --- | --- |
| Release baseline | <https://github.com/tmux/tmux/releases/tag/3.7c> |
| Man page | <https://github.com/tmux/tmux/blob/3.7c/tmux.1>, <https://man.openbsd.org/tmux.1> |
| Changelog | <https://github.com/tmux/tmux/blob/3.7c/CHANGES> |
| Command synopses and flags | `cmd-*.c` (`.args` and `.usage` in each `cmd_entry`) |
| Option scopes, choices, defaults | `options-table.c` |
| Format variables and modifiers | `format.c` |
| Control-mode wire shapes | `control.c`, `control-notify.c`, `cmd-queue.c` (`cmdq_guard`), `client.c` (`client_exit_message`) |
| Layout strings | `layout-custom.c` (`layout_dump`, `layout_append`, `layout_parse`) |
| Wiki | <https://github.com/tmux/tmux/wiki/Control-Mode>, <https://github.com/tmux/tmux/wiki/Formats> |

## Server model and invocation

One server process per socket owns every session, window, and pane. Clients are separate processes that talk to the server over the socket; they attach to render and detach without disturbing it. The server starts on the first command that needs one and exits when no sessions remain (`exit-empty`, default `on`).

- **Sockets.** The default socket is `<$TMUX_TMPDIR or /tmp>/tmux-<uid>/default`. `-L <name>` picks another name in that directory, and `-S <path>` gives a full path and overrides `-L`. A non-default socket is created under umask 177, owner read and write only (`server.c`, `server_create_socket`). SIGUSR1 makes the server re-create a deleted socket. A `-S` path in a private directory is a private server.
- **`$TMUX`.** Panes receive `TMUX=<socket-path>,<server-pid>,<session-id>`, where the third field is the numeric session id without its `$` (`environ.c`, `environ_for_session`). The man page documents the value only as internal information. A client started with `$TMUX` set refuses attach-shaped commands with `sessions should be nested with care, unset $TMUX to force`; clear the variable in the child environment for a deliberate nested or control-mode attach. `$TMUX_PANE` carries the pane's `%id`.
- **The client's terminal is stdin.** An attach-shaped command opens its terminal from stdin, not `/dev/tty`. With stdin piped or null it fails `open terminal failed: not a terminal`, exit 1 (probed on 3.5a). Commands that never attach are unaffected, so a subprocess wrapper that nulls stdin cannot attach by accident.
- **Version string.** `tmux -V` prints `tmux 3.7c`; point releases add a letter to the same minor. OpenBSD's base tmux prints `tmux openbsd-X.Y`, which a numeric parser rejects.
- **Command sequences.** One client argv carries several commands separated by standalone `;` tokens (`tmux set -t s mouse on ';' set -t s history-limit 100000`): one fork, one server round trip. A parse error anywhere, such as an unknown command, fails the whole sequence before anything runs. A runtime failure, such as a bad target, stops at the failing command with earlier commands applied, exit 1, and stderr naming the failure (probed on 3.5a; the man page states only the runtime half).
- **No server.** Any command with no live server exits 1. stderr is `no server running on <path>` when the socket file exists but its server died, and `error connecting to <path> (No such file or directory)` when the socket was never created.

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
| `-v` | log to files; SIGUSR2 toggles server logging |

### Targets and ids

Most commands take `-t` (and some `-s`). Each target kind resolves in order:

| Kind | Forms |
| --- | --- |
| target-session | `$id`, exact name, name prefix, fnmatch pattern; a leading `=` forces exact match; several matches are an error |
| target-window | `session:window`, where window is `{start}`/`^`, `{end}`/`$`, `{last}`/`!`, `{next}`/`+`, `{previous}`/`-`, `{current}`/`@`, an offset like `+2`, an index, an `@id`, an exact name, a name prefix, or an fnmatch pattern |
| target-pane | `session:window.pane`, where pane is an index, a `%id`, `{active}`/`@`, or a position token (`{top-left}`, `{up-of}`, …); a bare `%id` is absolute; `{mouse}` and `{marked}` name the last mouse-event pane and the marked pane |

Sessions, windows, and panes carry server-unique ids, `$N`, `@N`, and `%N`, fixed for the object's life. They come from monotonic per-server counters and are not reused after close (probed: kill `%1`, and the next split is `%2`); they restart from zero only when a new server starts. Names permit `:` and `.` and may be empty (only `#(` is forbidden, 3.7a), but those characters keep their target-grammar meaning, so scripts target ids and ask for them with `-P -F '#{pane_id}'`.

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

Dates are GitHub release publication dates. The floors below cover the surfaces this page documents in depth; each is from `CHANGES` unless marked.

| Surface | Landed |
| --- | --- |
| `split-window` / `new-window` `-e VAR=val` | 3.0 |
| pane options (`set-option -p`, `show-options -p`) | 3.0 |
| `after-<command>` hooks as array options | 3.0 |
| `new-session -e`, `new-window -S` | 3.2 |
| `display-popup` | 3.2 (`-s`/`-S`/`-b`/`-T`/`-e`/`-B` 3.3; `-k` 3.6) |
| `extended-keys` | 3.2 (`always` 3.2a; revamped to mode 2 in 3.5) |
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
| `pane_start_time` format variable | does not exist in any release ([formats](#the-variables-rimz-reads)) |

Behaviour changes inside 3.5 through 3.7c that alter what a caller observes:

| Release | Change |
| --- | --- |
| 3.7c | `split-window ''` (a single empty command) creates an empty pane again, as it did before 3.7 (`cmd-split-window.c`); `new-window -n ''` keeps an empty name instead of deriving one (`spawn.c`); `new-pane` unzooms the window before creating a floating pane; `detach-on-destroy previous`/`next` pick sessions in name order and never the session being destroyed (`server-fn.c`); `message-format` uses `message-style` again |
| 3.7b | the end of a synchronized update (`sync` terminal feature) triggers a redraw, which 3.7 and 3.7a could skip |
| 3.7a | names may be empty and contain `#[`, `:`, and `.` |
| 3.7 | `history-limit` changes update existing panes; `new-session -A -c` applies the working directory on the attach path; read-only checks tighten on `attach-session`, `detach-client`, and `switch-client`; `paste-buffer` passes buffers through `vis(3)` unless `-S`; pane titles and window and session names are sanitized of C0 and invisible characters; default `update-environment` gains `WAYLAND_DISPLAY` and `XDG_*` variables; `refresh-client -l` loses its forward-to-pane argument in favour of the `get-clipboard` option; several control-mode exit hangs are fixed |
| 3.6 | bracketed paste stays byte-preserving while `extended-keys` is on (observed on the wire; `CHANGES` has no entry); `capture-pane` preserves tabs |
| 3.5a | `#()`, `run-shell`, and `if-shell` return to `/bin/sh`; popups keep `default-shell`; BSpace and Shift encodings under extended keys are corrected |
| 3.5 | `escape-time` default drops from 500 to 10 ms; extended keys always request mode 2 and use a new internal key representation |

Through 3.7c, client input `ESC[27u` (CSI-u Escape with the modifier omitted) is not recognized as Escape and reaches panes raw, while `ESC[27;1u` and the xterm form are translated. Earlier boundaries a contributor may meet: 3.3 made `command-prompt` and `confirm-before` block by default (`-b` restores async), and 3.2 moved window and pane hooks off session scope ([hooks](#hooks)), renamed `refresh-client -F` to `-f`, and made `window_flags` escape `#` (`window_raw_flags` is the raw form).

### Unreleased in 3.8-rc

tmux 3.8-rc (prerelease, 2026-09-09) is not the baseline, but its `CHANGES FROM 3.7c TO 3.8` section changes surfaces this page covers in depth. Read at <https://github.com/tmux/tmux/blob/master/CHANGES> on 2026-09-14:

- The `active-pane` client flag is removed.
- Layout strings move to a JSON subset that includes floating panes. The old format is still accepted, and control clients receive old layouts unless they set a new `new-layouts` client flag.
- Control mode queues notifications so none is sent inside `%begin`/`%end`, and bounds buffered command replies for a client that stops reading.
- The `mouse` option defaults to `on`.
- `new-window`, `respawn-pane`, and `respawn-window` gain `-E` (empty pane); `kill-pane -a`, `kill-window -a`, and `kill-session -a` gain `-f` filters; `set-hook` gains `-B` format monitors, `-T`, and `-E`, with many new hooks.

## Command surface

A `shell-command` argument to `new-session`, `new-window`, `split-window`, `respawn-window`, or `respawn-pane` may be several argv tokens, which tmux executes directly without `sh -c`. A single token goes through `/bin/sh -c`.

Generated usage strings (`tmux list-commands`, error output) omit some accepted flags. At 3.7c, `split-window` omits `-E` and `-m`, `new-pane` omits `-E`, `-L`, and `-m`, and `list-clients` omits `-r`; the `.args` strings in each `cmd-*.c` are authoritative. Synopses below follow `.args`.

### Sessions and clients

**`new-session [-AdDEPX] [-c start-dir] [-e VAR=val]… [-f flags] [-F format] [-n window-name] [-s name] [-t group] [-x cols] [-y rows] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-d` | create detached; size comes from `default-size` (80x24) unless `-x`/`-y` are given |
| `-x` / `-y` | initial size; also sets the new session's `default-size` option (`cmd-new-session.c`); `-` uses the current client's size |
| `-e VAR=val` | seed the session environment, repeatable; the first window's panes already inherit it |
| `-c` | start directory |
| `-P [-F]` | print the created session (`#{session_name}:` by default) |
| `-A` | attach instead when the session exists (see below) |
| `-t group` | join a session group (shared window set) |
| `-E` | skip `update-environment` |

`-A` on an existing session takes the attach path, and that path ignores `-d`: it honours only `-D` (like `attach -d`), `-X` (like `attach -x`), and since 3.7 `-c`. So `new-session -A -d` against a live session attaches and blocks, or with no terminal on stdin fails `open terminal failed: not a terminal`, exit 1 (probed on 3.5a). The attach path also ignores `-e`, `-x`, and `-y` (probed). Creating a session that exists without `-A` fails `duplicate session: <name>`, exit 1. The no-attach ensure idiom is `has-session -t =<name> || new-session -d -s <name>`, or `new-session -d` with `duplicate session` treated as success.

**`attach-session [-dErx] [-c dir] [-f flags] [-t session]`** (alias `attach`). `-d` detaches other clients, `-x` detaches them and sends SIGHUP, `-E` skips `update-environment`, and `-r` is read-only (the same as `-f read-only,ignore-size`). `-f` takes a comma-separated flag list, and a leading `!` clears a flag on an attached client:

| Client flag | Effect |
| --- | --- |
| `read-only` | keys limited to detach and switch bindings; commands follow per-command checks ([read-only](#read-only-clients)) |
| `ignore-size` | excluded from window size negotiation |
| `active-pane` | the client keeps its own active pane (removed in 3.8-rc) |
| `no-detach-on-destroy` | switch to another session instead of detaching when the session is destroyed (3.6) |
| `no-output` | control mode: no pane output |
| `pause-after=<secs>` | control mode: pause panes that fall behind |
| `wait-exit` | control mode: wait for an empty line after `%exit` |

**`detach-client [-aP] [-E shell-command] [-s session] [-t client]`**. `-s` detaches every client on the session, `-a` every client but the target, `-P` sends SIGHUP to the client's parent, and `-E` replaces the client process with a command.

**`kill-session [-aCg] [-t session]`**. An absent target exits 1 with `can't find session: <name>`. `-a` kills every other session, `-g` every session in the target's group (3.7), and `-C` clears alerts in the session's windows instead of killing anything. **`kill-server`** ends the server, every session, and every client.

**`has-session [-t session]`** reports through its exit code alone and never starts a server.

**`list-sessions [-r] [-F format] [-f filter] [-O order]`** and **`list-clients [-r] [-F format] [-f filter] [-O order] [-t session]`** print one line per session or attached client. `-f` keeps rows whose filter format is true, `-O` sorts, and `-r` reverses (3.7). In a `list-clients` row, pane and window variables resolve against the client's current session, so `#{pane_id}` is the active pane of that session's current window.

Index: `rename-session`, `switch-client`, `lock-client`/`lock-session`, `server-access [-adlrw] user` (socket access list, 3.3), `list-commands` (command syntax), `refresh-client` ([control mode](#client-flags-and-flow-control)).

### Windows and panes

**`new-window [-abdkPS] [-c dir] [-e VAR=val]… [-F format] [-n name] [-t window] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-d` | do not make the new window current |
| `-a` / `-b` | insert after / before the target index, shifting later windows |
| `-P [-F]` | print the new window; `-F '#{window_id} #{pane_id}'` returns both ids |
| `-n` | name the window and turn off `automatic-rename` for it (`spawn.c`), so the name stays fixed |
| `-S` | select an existing window with that name instead of failing (3.2) |
| `-k` | replace the window at the target index |

The window closes when its command exits unless `remain-on-exit` keeps the pane.

**`split-window [-bdeEfhIklPvZ] [-c dir] [-e VAR=val]… [-F format] [-l size] [-m message] [-p percentage] [-R inactive-style] [-s style] [-S active-style] [-t pane] [shell-command…]`**

| Flag | Effect |
| --- | --- |
| `-h` / `-v` | left and right / top and bottom (default) |
| `-b` | put the new pane before the target (left of or above it) |
| `-l <n>` / `-l <n>%` | size in cells / percentage (`-p` is the older percentage form) |
| `-f` | span the full window height or width |
| `-d` | leave the active pane unchanged |
| `-P [-F]` | print the new pane; the default format is index-shaped, so ask for `#{pane_id}` |
| `-E`, or a single empty command `''` | create an empty pane with no command; `display-message -I` writes into it |
| `-I` | create an empty pane and forward stdin into it |
| `-k` / `-m` | keep the pane after its command exits until a key is pressed / with that message |
| `-Z` | keep the window zoomed |

A split works on a detached session with no client attached.

**`new-pane [-bdeEfhIkLlPvZ] [-c dir] [-e VAR=val]… [-F format] [-l size] [-m message] [-p percentage] [-R inactive-style] [-s style] [-S active-style] [-t pane] [-x width] [-y height] [-X x] [-Y y] [shell-command…]`** creates a floating pane (3.7). It shares `split-window`'s implementation (`cmd-split-window.c`); `-x`/`-y` size the pane, `-X`/`-Y` place it, and `-L` makes an ordinary tiled pane instead. Floating panes sit above the tiled layout and behave as panes, not modal popups. In 3.7c they move and resize only with the mouse, and cannot be swapped, converted to tiled panes, or restored from a custom layout (`CHANGES FROM 3.6b TO 3.7`). `pane_floating_flag` marks one in formats, and layout strings list them separately ([layout strings](#layout-strings)).

**`respawn-pane [-k] [-c dir] [-e VAR=val]… [-t pane] [shell-command…]`** restarts a pane's command in place, keeping its `%id` and geometry. Without `-k` it fails on a pane whose command is still running; `-k` kills the running command first. With no command it reruns the pane's start command. `respawn-window` is the window-level form.

**`select-pane [-DdeLlMmRUZ] [-T title] [-t pane]`** makes a pane active within its window only; it does not change the session's current window (probed on 3.5a, and `cmd-select-pane.c` writes no current window). A cross-window jump is `select-window -t @win ';' select-pane -t %pane`, and only `switch-client` crosses sessions. `-L`/`-R`/`-U`/`-D` move by direction, `-l` goes to the last pane, `-e`/`-d` enable or disable input, `-T` sets the title, and `-m`/`-M` set or clear the marked pane.

**`select-window [-lnpT] [-t window]`** accepts `@id` targets; `-l`, `-n`, and `-p` pick the last, next, and previous window, and `-T` acts like `last-window` when the target is already current.

**`swap-window [-d] [-s src] [-t dst]`** exchanges two windows' positions, including into an occupied index, and `-d` keeps the current window current. `move-window [-abdkr]` relocates a window instead, and `-r` renumbers a session's windows.

**`rename-window [-t window] new-name`** sets the name and turns off that window's `automatic-rename` (`cmd-rename-window.c`). Setting `automatic-rename` on again derives the name from the active pane.

**`kill-pane [-a] [-t pane]`** kills a pane and its process, and the last pane's death closes its window. `-a` kills every other pane. `kill-window [-a]` works the same way on windows.

**`list-panes [-asr] [-F format] [-f filter] [-O order] [-t target]`** lists one window's panes by default, a session's with `-s`, and the server's with `-a` (the target is ignored). `-O` sorts and `-r` reverses (3.7). **`list-windows [-ar] [-F format] [-f filter] [-O order] [-t session]`** is the window-level form.

Index: `break-pane`, `join-pane`, `move-pane`, `swap-pane`, `rotate-window`, `link-window`/`unlink-window`, `last-pane`/`last-window`, `next-window`/`previous-window`, `display-panes`, copy mode and its `send-keys -X` commands, and the `choose-tree`/`choose-client`/`choose-buffer` modes.

### Layout and sizing

**`resize-pane [-DLMRTUZ] [-x width] [-y height] [-t pane] [adjustment]`** sets a pane's size. `-x`/`-y` take an absolute size in cells or a percentage of the window; `-L`/`-R`/`-U`/`-D` move the border by `adjustment` cells (default 1). `-Z` toggles the window's zoom on the target pane (`window_zoomed_flag`), `-M` starts a mouse resize, and `-T` trims lines below the cursor that are in history.

**`resize-window [-aADLRU] [-x width] [-y height] [-t window] [adjustment]`** sets a window's size and, as a side effect, sets that window's `window-size` option to `manual` (`cmd-resize-window.c`). Unset the option (`set-option -wu -t @win window-size`) to return the window to client-driven sizing. `-a` and `-A` size the window to the smallest or largest session containing it.

**`select-layout [-Enop] [-t pane] [layout-name]`** applies a preset or a layout string. The seven presets are `even-horizontal`, `even-vertical`, `main-horizontal`, `main-horizontal-mirrored`, `main-vertical`, `main-vertical-mirrored`, and `tiled` (`layout-set.c`). A layout string is the format `window_layout` prints ([layout strings](#layout-strings)); tmux rejects one whose checksum is wrong, whose cells cannot fit the window's panes, or that carries a floating-pane suffix (`layout-custom.c`, `layout_parse`). `-E` spreads the target pane and its neighbours evenly, `-n`/`-p` step through presets, and `-o` restores the previous layout.

Sizing options that interact with these commands: `window-size` (`largest`, `smallest`, `manual`, `latest`; default `latest`), `aggressive-resize`, `default-size`, and the `ignore-size` client flag.

### Pane I/O

**`capture-pane [-aCeFHJLMNpPqT] [-b buffer] [-E end] [-S start] [-t pane]`**

| Flag | Effect |
| --- | --- |
| `-p` | write to stdout (otherwise into a paste buffer) |
| `-S` / `-E` | first / last line: 0 is the top visible line, negatives reach into history, `-` means history start / visible end; the default is the visible screen |
| `-e` | include SGR escape sequences for text attributes and colours |
| `-C` | escape non-printable characters as octal `\xxx` |
| `-J` | join wrapped lines and keep trailing spaces (implies `-T`) |
| `-N` | keep trailing spaces |
| `-T` | stop at the last used cell of each line |
| `-a` / `-q` | capture the alternate screen / tolerate its absence |
| `-M` | capture the active mode's screen, such as copy mode (3.6) |
| `-P` | capture only the start of an incomplete escape sequence |
| `-L` / `-F` | prefix each line with its number / its flags (`-` none, `D` unused, `O` output, `P` prompt, `X` extended cells, `H` hyperlinks) |
| `-H` | capture only the hyperlinks in the selected lines |

The man page synopsis omits `-T`; the flag is accepted (`cmd-capture-pane.c`).

**`send-keys [-FHKlMRX] [-c client] [-N count] [-t pane] key…`** looks each argument up as a key name (`C-c`, `M-a`, `Enter`, `Escape`, `F1`, `NPage`, `User0`…) and sends an argument that is not a key name as its characters. `-l` disables lookup and sends every argument as literal UTF-8; `--` protects arguments that start with `-` (probed); `-H` sends hex bytes; `-N` repeats; `-R` resets the terminal state; `-X` sends a copy-mode command.

**`load-buffer [-w] [-b buffer] [-t client] path`** reads a file into a named paste buffer, and a `path` of `-` reads stdin (`file.c`, `file_read`). `-w` also sends the buffer to the client's clipboard. **`delete-buffer [-b buffer]`** removes one buffer.

**`paste-buffer [-dprS] [-s separator] [-b buffer] [-t pane]`** inserts a paste buffer into a pane:

| Flag | Effect |
| --- | --- |
| (default) | replace each LF with CR |
| `-s <sep>` / `-r` | use another separator / no replacement (LF stays LF) |
| `-S` | skip the `vis(3)` control-character sanitization that 3.7 applies by default (`cmd-paste-buffer.c`); tmux before 3.7 rejects `-S` |
| `-p` | add bracketed-paste markers when the application requested bracketed paste mode |
| `-d` | delete the buffer after pasting |

**`display-message [-aCIlNpv] [-c client] [-d delay] [-F format] [-t pane] [message]`**. `-p` prints the expanded format to stdout, which makes it the general way to evaluate a format against a target; a window target resolves to that window's active pane. `-a` lists every format variable with its value, `-v` logs each expansion step (how a missing variable is found), `-I` forwards stdin into an empty pane, and `-C` keeps the pane updating while a message shows (3.6). `-F` supplies the format when no positional message is given; `cmd-display-message.c` accepts it while the man page synopsis omits it.

**`pipe-pane [-IOo] [-t pane] [shell-command]`** streams a pane's output (`-O`, the default) or input (`-I`) through a shell command. With no command it closes the pipe, and `-o` opens one only when none is open. `pane_pipe_pid` holds the pipe process id (3.7).

### Key bindings and scripting

**`bind-key [-nr] [-N note] [-T key-table] key [command [argument…]]`** binds a key to a tmux command in a key table. `-n` is `-T root`, so the binding fires without the prefix from any pane on the server; key tables are server-global. `-r` makes the key repeatable. A key named `UserN` is the sequence defined at index N of the `user-keys` option (`key-string.c`).

**`run-shell [-bCE] [-c dir] [-d delay] [-t pane] [shell-command [argument…]]`** runs a command through `/bin/sh -c`, or a tmux command with `-C`, after expanding formats. It blocks the command queue until done unless `-b`. `-E` sends stderr to the output as well (3.6), and arguments after the command expand as `#{1}`, `#{2}`, and so on (3.7).

**`if-shell [-bF] [-t pane] condition command [command]`** runs the first command when a shell condition exits 0, otherwise the second. With `-F` the condition is a format, true when it expands to something other than empty or `0`, and no shell runs. The branches are tmux command strings, parsed when the condition resolves.

**`wait-for [-L|-S|-U] channel`** blocks the client until another client runs `wait-for -S` on the channel; `-L` and `-U` lock and unlock it.

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

**`show-options [-AgHpqsvw] [-t target] [option]`**. `-v` prints the value alone (`show-options -gv base-index`), `-A` includes inherited values, `-H` includes hooks, and `-q` suppresses errors for unset options.

**`set-environment [-Fghru] [-t session] name [value]`** and **`show-environment [-ghs] [-t session] [name]`**. Name and value are separate argv tokens; a single `NAME=value` argument fails `variable name contains =` (probed). `-g` targets the global environment, `-r` marks a variable for removal from new panes, `-u` unsets it, `-h` hides it from panes, and `show-environment -s` prints shell `export` lines.

**`set-hook [-agpRuw] [-t target] hook [command]`** and **`show-hooks`** set and show hooks, which are array options in the same scope tables ([hooks](#hooks)). `-R` runs a hook immediately.

## Formats

Formats are tmux's read surface: every `-F`, filter, hook command, and `#()` expands through them. An unknown variable expands to the empty string without an error, so a misspelled variable is silent and a multi-column `-F` row with a printable separator gets an empty column instead of shifted ones. `display-message -p` evaluates a format, `-a` lists variables, and `-v` traces the expansion.

### Language

`#{name}` expands a variable or an option value (`#{automatic-rename}`, `#{@user_option}`). Short aliases are `#S` session_name, `#W` window_name, `#I` window_index, `#D` pane_id, `#P` pane_index, `#T` pane_title, `#F` window_flags, and `#H`/`#h` host; `##` is a literal `#`. Modifiers go inside the braces before a colon, and several combine with `;` (`#{T;=10:status-left}`).

| Modifier | Meaning |
| --- | --- |
| `#{?cond,yes,no}` | conditional; since 3.6 repeated condition and value pairs, an optional final default, and an empty implicit default; escape `,` and `}` as `#,` and `#}` |
| `==`, `!=`, `<`, `>`, `<=`, `>=` | string comparison: `#{==:a,b}` |
| `&&`, `\|\|`, `!`, `!!` | boolean and, or (N-ary since 3.6), not, and canonical boolean |
| `m:`, `m/r:`, `m/i:` | fnmatch match, regular expression match, ignore case |
| `C:` | search pane content, yielding a line number |
| `e\|op\|f\|digits:` | arithmetic (`+ - * / m %` and comparisons; `f` for floating point) |
| `a:`, `c:` | ASCII character from a number; colour to RGB hex |
| `t:`, `t/f/<fmt>:`, `t/p:` | epoch to time string; custom strftime; abbreviated past time |
| `b:`, `d:` | basename, dirname |
| `q:`, `q/h:`, `q/a:` | escape for `sh(1)`; also escape `#`; escape as tmux command arguments |
| `E:`, `T:` | expand the result again; also expand strftime sequences |
| `S:`, `W:`, `P:`, `L:` | loop over sessions, windows, panes, clients; a second format applies to the current item; `/i`, `/n`, `/t`, `/r` sort by index, name, activity, or reverse (3.6); `loop_last_flag` marks the last item, and `W:` sets `next_window_*`/`prev_window_*` (3.7) |
| `N/w:`, `N/s:` | whether a window or session with this name exists |
| `s/pat/rep/:` | regex substitution (any delimiter, `i` flag) |
| `=N:`, `=/N/marker:` | truncate to N columns (negative from the end), with a marker |
| `pN:`, `n:`, `w:` | pad; length; display width |
| `l:` | literal, no expansion |
| `R:value,count` | repeat (3.6) |
| `#(command)` | last line of a shell command's output; cached, refreshed at most once a second, never blocks (empty until the first run completes); runs `/bin/sh` with the global environment |

### The variables RimZ reads

| Variable | Value | Notes |
| --- | --- | --- |
| `session_name` | session name (`#S`) | changes with `rename-session` |
| `session_id` | `$N` | server-unique |
| `window_id` | `@N` | server-unique, monotonic |
| `window_name` | window name | fixed once named by `new-window -n` or `rename-window` (per-window `automatic-rename` off) |
| `window_index` | position | `renumber-windows` rewrites it; ids never change |
| `window_width` / `window_height` | cells | |
| `window_zoomed_flag` | 1 if a pane is zoomed | |
| `window_layout` | layout string | [layout strings](#layout-strings) |
| `pane_id` | `%N` (`#D`) | server-unique, monotonic; exported as `$TMUX_PANE` |
| `pane_index` | position in the window | changes as panes close |
| `pane_active` | 1 if the window's active pane | one per window, so N windows report N active panes |
| `pane_floating_flag` | 1 if the pane floats above the tiled layout | 3.7; earlier releases expand it empty |
| `pane_left` / `pane_top` / `pane_width` / `pane_height` | geometry in cells | relative to the window |
| `pane_at_left` / `pane_at_top` / `pane_at_bottom` / `pane_at_right` | 1 if the pane touches that window edge | |
| `pane_current_command` | foreground process name | the name, not its argv |
| `pane_current_path` | foreground process working directory | |
| `pane_pid` | PID of the pane's first process | the spawned shell or command, never the foreground child |
| `pane_title` | title set by OSC 0/2 or `select-pane -T` | writable by applications while `allow-set-title` is on; sanitized of C0 and invisible characters (3.7) |
| `pane_start_command` / `pane_start_path` | command and directory the pane was created with | empty for a pane created with no command while `default-command` is empty (the default), that is a pane running `default-shell` (`spawn.c`) |
| `pane_dead` / `pane_dead_status` / `pane_dead_signal` / `pane_dead_time` | exit facts for a pane kept by `remain-on-exit` | |
| `client_name` / `client_tty` / `client_pid` / `client_session` | client identity | |
| `client_width` / `client_height` | client terminal size | |
| `client_activity` | last activity time, epoch seconds | |
| `client_flags` | comma-separated client flags (`attached`, `focused`, `control-mode`, `ignore-size`, `read-only`, …) | |
| `client_control_mode` | 1 for a control client | |
| `client_termname` | client terminal name (`$TERM`) | |
| `socket_path` / `pid` / `start_time` / `version` | server facts | `start_time` is the server's start |

**There is no `pane_start_time`.** No release through 3.7c defines a per-pane start-time variable: `format.c` has no such name, and `display-message -v` logs `format 'pane_start_time' not found` while the column expands empty (probed on 3.5a and 3.7b). The nearest facts are `pane_pid` and the never-reused `%id` itself; a process start time comes from the operating system for `pane_pid`.

### Output sanitization

Command output to a client that is not marked UTF-8 is sanitized: every byte outside printable ASCII, tab included, becomes `_`, and each UTF-8 character becomes one `_` per display column (`server-client.c`, `server_client_print`; `utf8.c`, `utf8_sanitize`). A client is UTF-8 when it runs with `-u`, when `$TMUX` is set, or when the first non-empty of `LC_ALL`, `LC_CTYPE`, and `LANG` contains `UTF-8` or `UTF8` (`tmux.c`, `main`). A parser that must work under any locale uses printable separators and substitutes the separator out of free-form fields (`#{s/,/_/g:pane_title}`).

### Catalog

The man page's FORMATS table lists over 200 variables at 3.7c, in these families: `buffer_*` (including `buffer_full`), `client_*` (geometry, flags, tty, uid, `client_theme`), `command_*`, copy mode (`copy_cursor_*`, `selection_*`, `search_*`, `scroll_position`), `cursor_*`, `history_*` (`history_size`, `history_limit`, `history_bytes`), `hook_*`, `mouse_*`, `pane_*` (geometry, edges, flags, modes, `pane_pipe_pid`, `pane_pb_state`/`pane_pb_progress` for OSC 9;4 progress, 3.7), `session_*` (counts, times, groups, `session_attached`, alerts), `window_*` (geometry, flags, counts, `window_layout`), `next_window_*`/`prev_window_*`, `loop_last_flag`, `sixel_support`, and the server variables.

## Hooks

Hooks run tmux commands on triggers. They are array options stored in the same scope tables as options, global or per session, window, or pane: `set-hook -g name[i] command` sets one member, members run in index order, and setting a hook without an index clears the array and sets member 0. A hook's command is parsed by tmux, not by a shell, so a shell command inside it needs its own quoting layer.

**After hooks.** A command that has an `after-<command>` hook fires it when the command completes, except when the command itself ran from a hook, so a hook that runs `split-window` cannot trigger `after-new-window`. The man page says "most" commands have one; at 3.7c the after hooks are `after-bind-key`, `after-capture-pane`, `after-copy-mode`, `after-display-message`, `after-display-panes`, `after-kill-pane`, `after-list-buffers`, `after-list-clients`, `after-list-keys`, `after-list-panes`, `after-list-sessions`, `after-list-windows`, `after-load-buffer`, `after-lock-server`, `after-new-session`, `after-new-window`, `after-paste-buffer`, `after-pipe-pane`, `after-queue`, `after-refresh-client`, `after-rename-session`, `after-rename-window`, `after-resize-pane`, `after-resize-window`, `after-save-buffer`, `after-select-layout`, `after-select-pane`, `after-select-window`, `after-send-keys`, `after-set-buffer`, `after-set-environment`, `after-set-hook`, `after-set-option`, `after-show-environment`, `after-show-messages`, `after-show-options`, `after-split-window`, and `after-unbind-key` (`options-table.c`).

**Named hooks.** The table column is the option scope from `options-table.c` (`OPTIONS_TABLE_HOOK`, `OPTIONS_TABLE_WINDOW_HOOK`, `OPTIONS_TABLE_PANE_HOOK`); after hooks are session hooks. Since 3.2, window hooks live in the window table and pane hooks in the window or pane table, so a window or pane hook set on a session never fires; `set-hook -g` reaches every global table.

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

Control-mode notifications double as hooks under the same names without `%` or arguments, except `%exit`. Inside a hook command, the `hook_*` variables (`hook`, `hook_client`, `hook_session`, `hook_window`, `hook_pane`, and their `_name` forms) describe the firing context.

## Options

Options live in four scope tables, server, session, window, and pane, each with a global and a local layer; a local value shadows the global. The table covers the options the backend writes, with scope, choices, and default read from `options-table.c` at 3.7c. The values RimZ writes are in [configuration → multiplexer room options](../../guide/configuration.md#multiplexer-room-options) and [multiplexers.md → room options](../../internals/multiplexers.md#room-options).

| Option | Scope | Values | Default | Meaning |
| --- | --- | --- | --- | --- |
| `focus-events` | server | on, off | off | request focus reporting from the terminal and pass focus events to applications; enables `pane-focus-*` hooks; clients re-attach to pick up a change |
| `set-clipboard` | server | on, external, off | external | `on` accepts OSC 52 from applications into a buffer and forwards it to the terminal; `external` only forwards; forwarding needs the terminal's `Ms` capability or the `clipboard` feature |
| `get-clipboard` | server | off, buffer, request, both | buffer | how an application's clipboard read is answered (3.7); `CHANGES` says the default is off, `options-table.c` sets `buffer` |
| `extended-keys` | server | on, off, always | off | `on` honours application requests for modifyOtherKeys mode 1 or 2; `always` sends extended keys unrequested |
| `extended-keys-format` | server | csi-u, xterm | xterm | encoding of extended keys: `C-S-a` is `^[[65;6u` (csi-u) or `^[[27;6;65~` (xterm) (3.5) |
| `terminal-features[]` | server | `<terminal-pattern>:<feature>:…` | `xterm*:clipboard:ccolour:cstyle:focus:title`, `screen*:title`, `rxvt*:ignorefkeys` | features tmux assumes for outer terminals whose `TERM` matches, when detection cannot find them; features include `256`, `RGB`, `clipboard`, `extkeys`, `focus`, `hyperlinks`, `mouse`, `osc7`, `sixel`, `sync` (synchronized output), `title`, `usstyle` |
| `user-keys[]` | server | escape sequences | empty | each index N names a key `UserN` that `bind-key` can bind; a config file expands `\e`, an argv value needs the literal byte |
| `escape-time` | server | milliseconds | 10 | how long tmux waits after ESC to tell a key from an escape sequence (500 before 3.5); extended while a partial paste end or forwarded request is pending (3.7) |
| `mouse` | session | on, off | off | mouse events become bindable keys; clicks select panes |
| `history-limit` | session | lines | 2000 | scrollback per pane; since 3.7 a change also trims existing panes (the option's help text still says new panes only, while `CHANGES`, `options.c`, and a live probe agree on the new behaviour) |
| `renumber-windows` | session | on, off | off | renumber windows when one closes, from `base-index` |
| `set-titles` | session | on, off | off | set the outer terminal's title for attached clients whose terminal can set titles |
| `set-titles-string` | session | format | `#S:#I:#W - "#T" #{session_alerts}` | the title `set-titles` writes |
| `allow-passthrough` | window, pane | off, on, all | off | honour the `\ePtmux;…\e\\` passthrough escape; `on` only while the pane is visible, `all` always (3.3; `all` 3.4) |
| `aggressive-resize` | window | on, off | off | size a window to the clients currently viewing it, not every client whose session contains it |
| `window-size` | window | largest, smallest, manual, latest | latest | how a window's size follows its clients; `resize-window` sets `manual` |
| `automatic-rename` | window | on, off | on | derive the window name from the active pane's command |
| `pane-border-status` | window | off, top, bottom | off | draw a text row on each pane border |
| `pane-border-format` | window, pane | format | active-pane marker and `#{pane_index} "#{pane_title}"` | the text in that row |
| `pane-border-lines` | window | single, double, heavy, simple, number, spaces | single | border characters; `simple` is plain ASCII; `spaces` 3.6 |
| `default-size` | session | `WxH` | 80x24 | size of a detached session's windows; `new-session -x`/`-y` set it |

**User options.** A name starting with `@` is a user option: any scope, any string value, readable in formats as `#{@name}`, and not writable by applications through escape sequences. Scope flags on `set-option` matter for user options because tmux cannot infer their table.

Other options a contributor will reach for: `base-index` (first window index), `remain-on-exit` (`on`, `off`, `failed`, `key`; `key` 3.7) with `remain-on-exit-format`, `detach-on-destroy` (`off`, `on`, `no-detached`, `previous`, `next`), `destroy-unattached` (`off`, `on`, `keep-last`, `keep-group`), `exit-empty`/`exit-unattached` (server lifetime), `allow-rename` (applications rename windows by escape, default off), `allow-set-title` (applications set `pane_title`, default on), `automatic-rename-format`, `focus-follows-mouse` (session, 3.7), `update-environment[]` ([environment](#global-and-session-environment)), `default-terminal` (default `screen` unless the build sets another), `default-command`/`default-shell`, `popup-style`/`popup-border-style`/`popup-border-lines` (3.3), `synchronize-panes`, `monitor-activity`/`monitor-bell`/`monitor-silence`, and the 3.6 and 3.7 additions `pane-scrollbars`, `pane-scrollbars-position`, `pane-scrollbars-style`, `tiled-layout-max-columns`, `codepoint-widths[]`, `variation-selector-always-wide`, `copy-mode-position-*`, `copy-mode-selection-style`, `copy-mode-line-numbers` with its styles, `initial-repeat-time`, `input-buffer-size`, `default-client-command`, `tree-mode-preview-*`, and `message-format`.

## Global and session environment

The server copies the environment it starts with into the **global environment**, and each session keeps a **session environment**. A new pane's process receives the global environment merged with the session environment (session wins), plus `TMUX`, `TMUX_PANE`, and `TERM` from `default-terminal`. The merge happens when the pane is created, so environment changes never reach running processes.

- `new-session -e`, `new-window -e`, `split-window -e`, and `respawn-pane -e`/`respawn-window -e` set variables at creation, so even a session's first window sees them.
- `set-environment -t <session> NAME VALUE` reaches only panes created afterwards.
- `update-environment[]` (session option, fnmatch patterns allowed) copies the listed variables from the attaching client into the session environment on `new-session` and on every attach; a variable the client lacks is marked for removal. The default list is `DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE` (`options-table.c`), so an attach from a new SSH connection rewrites the `SSH_*` values for later panes. `-E` on `attach-session` or `new-session` skips the copy.
- A variable set with `-h` is hidden: it stays in the table for formats and is never exported to panes.

## Control mode

`tmux -C` makes a client a line protocol endpoint: commands go in on stdin, and command replies and asynchronous notifications come out on stdout. `-CC` also puts the terminal in raw mode and wraps the stream in a `\eP1000p` DCS opening and a closing `\e\\` (the iTerm2 integration shape); plain `-C` writes no terminal markers. A typical observer attaches with `tmux -C attach-session -f ignore-size,no-output -t <session>`.

### Protocol shape

- **Reply blocks.** Each stdin line is a command or `;` sequence, and each produces one reply block: `%begin <time> <number> <flags>`, the output lines, then `%end` on success or `%error` on failure with the same three arguments. `time` is epoch seconds, `number` identifies the command, and `flags` is 1 when the command came from this control client and 0 otherwise (`cmd-queue.c`, `cmdq_guard`). The man page documents `flags` as unused.
- **Notifications never appear inside a reply block.** Any `%` line outside `%begin`…`%end` is a notification, so a reader can buffer a reply block whole. (3.8-rc adds queuing to enforce this under more paths; see [unreleased](#unreleased-in-38-rc).)
- **An unparseable command still gets a block:** `%begin`, `parse error: …`, `%error`.
- **An empty stdin line detaches the client.** With the `wait-exit` flag, the client waits for an empty line after `%exit` before it exits. Closing stdin also ends the client.
- **Size.** Control clients have no size and take no part in size negotiation until `refresh-client -C WxH` (or `-C @win:WxH` per window) gives them one.
- **Exit.** The last line is `%exit [reason]`, written by the client. Reasons are `detached`, `detached (from session <name>)`, `detached and SIGHUP`, `detached and SIGHUP (from session <name>)`, `lost tty`, `terminated`, `too far behind`, `exited` (the server had no sessions), `server exited`, and `server exited unexpectedly` (`client.c`, `client_exit_message`; `control.c`).

### Notification catalog

Wire shapes are from `control-notify.c` and `control.c` at 3.7c. How the backend classifies each line is in [state.md → what triggers a mux-derived event](../../internals/sidebar/state.md#what-triggers-a-mux-derived-event).

| Notification | Wire shape | Sent when |
| --- | --- | --- |
| `%window-add` | `%window-add @id` | a window is linked into the client's session |
| `%window-close` | `%window-close @id` | a window in the client's session closes |
| `%window-renamed` | `%window-renamed @id name` | a window in the client's session is renamed |
| `%unlinked-window-add` / `-close` / `-renamed` | `%unlinked-window-… @id [name]` | the same events for a window not in the client's session, judged per client |
| `%layout-change` | `%layout-change @id <layout> <visible-layout> <raw-flags>` | a window's layout changes (split, close, resize); older releases sent fewer fields, so parse at least the id and the layout |
| `%window-pane-changed` | `%window-pane-changed @id %id` | a window's active pane changes |
| `%session-window-changed` | `%session-window-changed $id @id` | a session's current window changes |
| `%session-changed` | `%session-changed $id name` | this client switches session |
| `%client-session-changed` | `%client-session-changed <client> $id name` | another client switches session |
| `%session-renamed` | `%session-renamed $id name` | a session is renamed; the man page documents the name alone, so parse the id first |
| `%sessions-changed` | `%sessions-changed` | a session is created or destroyed |
| `%client-detached` | `%client-detached <client>` | a client detaches (3.2) |
| `%pane-mode-changed` | `%pane-mode-changed %id` | a pane enters or leaves a mode such as copy mode |
| `%output` | `%output %id <value>` | pane output; bytes below 0x20 and `\` are escaped as octal `\nnn`, other bytes pass raw, so a line can split a UTF-8 sequence |
| `%extended-output` | `%extended-output %id <age-ms> … : <value>` | pane output under `pause-after`; ignore fields between the age and the lone `:` |
| `%pause` / `%continue` | `%pause %id` / `%continue %id` | pause-mode flow control |
| `%subscription-changed` | `%subscription-changed name $id @id <window-index> %id … : value` | a `refresh-client -B` format's value changed; window subscriptions put `-` in the pane field, session subscriptions in the window, index, and pane fields; checked at most once a second |
| `%paste-buffer-changed` / `%paste-buffer-deleted` | `%paste-buffer-… name` | a paste buffer changes / is deleted (3.4) |
| `%config-error` | `%config-error <error>` | a configuration file error (3.4) |
| `%message` | `%message <text>` | `display-message` without `-p` targets this client |
| `%exit` | `%exit [reason]` | the client is about to exit |

### Layout strings

A layout string, as in `%layout-change`, `window_layout`, and `select-layout`, is a four-hex-digit checksum, a comma, then a cell tree. Each cell is `WxH,X,Y`, followed by `,<pane>` for a leaf, where `<pane>` is the pane's `%id` number, or by `{…}` for left-to-right children or `[…]` for top-to-bottom children, separated by commas (`layout-custom.c`, `layout_append`). Example: `b25d,208x60,0,0{104x60,0,0,1,103x60,105,0,2}`.

Since 3.7, a window with floating panes appends them after the tiled tree inside angle brackets, one leaf cell each: `<WxH,X,Y,<pane>,WxH,X,Y,<pane>>`. The checksum covers the whole string (`layout_dump`). tmux's own parser does not read the suffix: `layout_parse` builds the tiled tree and fails `invalid layout` on anything after it, so `select-layout` rejects the string `window_layout` prints for a window with a floating pane, and a client parser that stops at the tiled tree fails the same way. `CHANGES FROM 3.6b TO 3.7` lists restoring custom layouts with floating panes as not yet available. 3.8-rc replaces this format with a JSON subset for clients that opt in ([unreleased](#unreleased-in-38-rc)).

### Client flags and flow control

- `no-output` suppresses pane output entirely, both `%output` and `%extended-output`.
- Without `pause-after`, tmux disconnects a client whose buffered output ages past five minutes (`CONTROL_MAXIMUM_AGE 300000` ms in `control.c`), with exit reason `too far behind`. A `no-output` client still receives notifications and should keep reading.
- `pause-after=<secs>` switches output to `%extended-output` and pauses a pane (`%pause`) whose output falls that far behind, instead of disconnecting. `refresh-client -A %id:<state>` controls one pane: `continue` resumes it (`%continue`), `pause` pauses it now, `off` stops its output to this client, and `on` turns output back on. When every client has turned a pane off, tmux stops reading that pane's pty, which applies backpressure to the application.
- `refresh-client -B name:what:format` subscribes to a format. `what` is empty for the attached session, a `%id`, `%*` for every pane in the session, an `@id`, or `@*` for every window. Changes arrive as `%subscription-changed`, checked at most once a second, and `-B name` alone unsubscribes.
- `refresh-client -f <flags>` changes flags on a live client, `-r %id:<report>` lets a control client answer OSC 10 and 11 colour queries for a pane (3.5), and `-l` requests the terminal clipboard into a paste buffer.

### Read-only clients

The `read-only` flag is not a command sandbox. It limits key bindings, and each command adds its own checks for commands sent by a read-only client: 3.7 restricts `attach-session`, `detach-client`, and `switch-client` so a read-only user can only detach their own client, and `send-keys` refuses a read-only target client unless it sends a copy-mode command with `-X`. Other commands on a read-only control client's stdin can still change server state, so a watcher that must not mutate anything restricts what it writes to stdin.
