# tmux upstream reference

> The Rimz-side contracts live in [multiplexers.md](../../internals/sidebar/multiplexers.md) — the `MuxBackend` seam, the managed sidebar pane, the control-mode presence watch, and the room options. This doc mirrors the upstream surface itself.

This is the single home for the **tmux upstream surface** Rimz binds to — the client/server and socket model, the command verbs the backend adapter drives, the format language, hooks, options, the session environment, and the control-mode protocol. It is a hand-maintained mirror of the tmux(1) man page cross-checked against the installed binary and live probes on a scratch `-S` server, captured at **tmux 3.5a** (2026-06; upstream latest is **3.6b**, 2026-05-20). Where the man page and the wire disagree, the wire wins and the disagreement is flagged.

Coverage is **depth on what Rimz wires, breadth as an index**: the commands `TmuxBackend` runs, the format variables `list-panes`/`list-clients` read, the twelve room options, the `after-new-window` hook, and the control-mode notifications `PresenceWatch` filters are documented in full; the rest of each catalog is listed so a contributor wiring a new surface knows it exists.

## Upstream sources

Re-fetch these to refresh this mirror. The man page is the canonical reference (tmux ships no other); the wiki's Control Mode page lags it, and the notification wire shapes are source-level.

| Surface | Source |
| --- | --- |
| Man page (master) | <https://man.openbsd.org/tmux.1>, <https://github.com/tmux/tmux/blob/master/tmux.1> |
| Changelog (version floors) | <https://raw.githubusercontent.com/tmux/tmux/master/CHANGES> |
| Control mode wiki | <https://github.com/tmux/tmux/wiki/Control-Mode> |
| Formats wiki | <https://github.com/tmux/tmux/wiki/Formats> |
| Notification wire shapes | `control.c`, `control-notify.c`, `cmd-queue.c`, `client.c` in <https://github.com/tmux/tmux> |
| Installed binary | `man tmux`, `tmux -V`, `display-message -a`/`-v` probes on a `tmux -S <tempdir>` scratch server |

## Server model and invocation

One server process per socket owns every session, window, and pane; clients are separate processes that talk to it over the socket, attach to render, and detach without disturbing it. The server starts on the first command that needs it and exits when no sessions remain (`exit-empty on`, the default).

- **Sockets.** The default socket is `<$TMUX_TMPDIR or /tmp>/tmux-<uid>/default`. `-L <name>` picks a different socket name in that directory; `-S <path>` a full path (ignores `-L`, created umask 177). SIGUSR1 makes the server re-create a deleted socket. One `-S` tempdir socket is one private server — the integration-test isolation seam (`TmuxBackend::with_socket`).
- **`$TMUX`.** `<socket-path>,<server-pid>,<session-index>` — the shape is stable but the man page documents it only as "some internal information"; field 1 is the control socket (`control_socket_from_env`). `$TMUX_PANE` carries the pane's `%id`. A client started with `$TMUX` set refuses attach-shaped commands ("sessions should be nested with care"); drop the variable from the child env for a deliberate nested or control-mode attach.
- **The client's terminal is stdin.** An attach-shaped command opens its terminal from stdin rather than `/dev/tty`: with stdin piped or null it fails `open terminal failed: not a terminal`, exit 1 (probed 3.5a). Commands that never attach are unaffected — a subprocess wrapper with stdin nulled can never accidentally attach; it errors instead.
- **Version probe.** `tmux -V` prints `tmux 3.5a` — point releases carry a letter suffix on the same minor. OpenBSD's base tmux prints `tmux openbsd-X.Y` instead (it versions with the OS), which a numeric parser rejects — `rimz doctor` renders the raw string in that case.
- **Command sequences.** One client argv carries several commands joined by standalone `;` tokens (`tmux set -t s mouse on ';' set -t s history-limit 100000`) — one fork, one server round-trip. Semantics (probed 3.5a): a parse error anywhere — an unknown verb — fails the whole sequence before *anything* runs; a runtime failure (a bad target) stops execution at the failing command with **earlier commands applied**, exit 1, stderr naming the failure.
- **No-server errors.** Any command without a live server exits 1: stderr `no server running on <path>` when the socket exists but its server died, `error connecting to <path> (No such file or directory)` when it was never created. Both read as "no sessions" — distinct from Zellij's exit-0-empty contract.
- Flags index: `-f <config>` config file, `-N` never autostart a server, `-D` foreground server (turns off `exit-empty`), `-C`/`-CC` control mode, `-T <features>` client terminal features, `-u` force UTF-8, `-v` file logging (SIGUSR2 toggles it server-side).

### Targets and ids

Most commands take `-t` (and sometimes `-s`). Resolution order, per kind:

- **target-session** — `$id`, exact name, name prefix, fnmatch pattern; a leading `=` forces exact-only. Multiple matches error.
- **target-window** — `session:window` where window is a special token (`{start}`/`^`, `{end}`/`$`, `{last}`/`!`, `{next}`/`+`, `{previous}`/`-`, offsets like `+2`), an index, an `@id`, an exact name, a name prefix, or an fnmatch pattern.
- **target-pane** — `session:window.pane` with pane index or `%id`, plus positional tokens (`{top-left}`, `{up-of}`, …); a bare `%id` is absolute and needs no qualifier. `{mouse}` and `{marked}` name the last-event and marked panes.

Sessions, windows, and panes carry server-unique ids — `$N`, `@N`, `%N` — unchanged for the object's life. They are allocated from monotonic per-server counters and are **not reused after close** (probed: kill `%1`, the next split is `%2`); name-shaped targets carry colon/period ambiguity that ids never do, so scripting prefers `-P -F '#{pane_id}'` over labels.

## Releases and version floors

| Release | Date | | Release | Date |
| --- | --- | --- | --- | --- |
| 3.2 | 2021-04-13 | | 3.5 | 2024-09-27 |
| 3.2a | 2021-06-10 | | 3.5a | 2024-10-05 |
| 3.3 | 2022-06-01 | | 3.6 | 2025-11-26 |
| 3.3a | 2022-06-09 | | 3.6a | 2025-12-05 |
| 3.4 | 2024-02-13 | | 3.6b | 2026-05-20 |

Floors for the surfaces Rimz uses (from CHANGES):

| Surface | Landed |
| --- | --- |
| `split-window` / `new-window` `-e VAR=val` | 3.0 |
| `new-session -e` | 3.2 |
| `display-popup` | 3.2 (`-s`/`-S`/`-b`/`-T`/`-e`/`-B` 3.3; `-k` 3.6) |
| `extended-keys` option | 3.2 (`always` value 3.2a; mode-2 revamp 3.5) |
| `extended-keys-format` option | **3.5** |
| `allow-passthrough` option | **3.3**, default `off` (`all` value 3.4; the escape is not option-gated before 3.3) |
| `escape-time` default 10ms (was 500ms) | 3.5 |
| `command-error` hook | 3.5 |
| `client-active`, `window-resized` hooks | 3.2 |
| `after-<command>` hooks (incl. `after-new-window`) | 3.0 (array-option hooks) |
| control mode: pause mode, `%extended-output`, subscriptions (`refresh-client -B`), `-f` flag spelling | 3.2 (`no-output` existed since 3.0 as `refresh-client -F`) |
| `refresh-client -r` (control client answers OSC 10/11 pane reports) | 3.5 |
| `new-window -S` (select-if-exists by name) | 3.2 |
| `pane_start_time` format variable | **does not exist in any release** ([formats](#the-variables-rimz-reads)) |

**The floor is option-driven:** `MIN_TMUX_VERSION` is 3.5.0 because the room options Rimz applies unconditionally include `allow-passthrough` (3.3) and `extended-keys-format` (3.5), and a batched option sequence fails at the first option the server does not know — the command surface alone would need only 3.2. A future option below the floor either moves the constant again or gates itself (`set-option -q` silences unknown-option errors without branching on `tmux -V`).

Behaviour changes inside the supported range: 3.5 cut `escape-time`'s default 500→10ms and revamped extended-keys (always requests mode 2 upstream, new internal key representation; 3.5a adjusts BSpace/Shift encoding); 3.5 ran `#()`/`run-shell`/`if-shell`/popups under `default-shell`, and 3.5a reverted all but popups to `/bin/sh`; 3.3 made `command-prompt`/`confirm-before` block by default (`-b` restores async); 3.2 moved window/pane hooks off session scope ([hooks](#hooks)), renamed `refresh-client -F` to `-f`, and made `window_flags` escape `#` (`window_raw_flags` is the raw form).

3.6 additions, forward-looking and none load-bearing for Rimz: pane scrollbars (`pane-scrollbars*` options), dark/light theme reporting (DEC mode 2031) with `client-light-theme`/`client-dark-theme` hooks, `capture-pane -M`, `display-popup -k`, N-ary `&&`/`||` plus `!` in formats, `buffer_full` and `sixel_support` variables, a `no-detach-on-destroy` client flag, `default-client-command`, `input-buffer-size`.

## Command surface

`shell-command` arguments to `new-session`, `new-window`, `split-window`, `respawn-window`, and `respawn-pane` may be **multiple argv tokens, executed directly without `sh -c`** — no quoting layer; the single-argument form goes through `/bin/sh -c`.

### Sessions and clients

**`new-session [-AdDEPX] [-c start-dir] [-e VAR=val]… [-f flags] [-F fmt] [-n window-name] [-s name] [-t group] [-x cols] [-y rows] [cmd…]`**

- `-d` births detached; the initial size comes from `default-size` (80×24) unless `-x`/`-y` give one, and giving `-x`/`-y` **also sets the session's `default-size` option** (probed). `-x -`/`-y -` use the current client's size.
- `-e` seeds the session environment at birth, repeatable — the first window's panes already inherit it (the identity-pin channel).
- `-P [-F fmt]` prints the created session (`#{session_name}:` by default).
- **`-A` attaches instead when the name exists — and on that path plain `-d` is ignored.** `-d` is the create-path flag; the attach path honors only `-D` (≙ attach `-d`, detach others) and `-X` (≙ attach `-x`), so `new-session -A -d` against a live session genuinely attaches and blocks, or fails `open terminal failed: not a terminal` (exit 1) without a terminal on stdin (probed 3.5a). `-A` on a live session also ignores `-e`/`-x`/`-y` (probed) — re-assert env with `set-environment` after. The no-attach ensure idiom is `has-session -t = || new-session -d`.
- `-t group` joins a session group (shared window set); `-E` skips `update-environment`.

**`attach-session [-dErx] [-c dir] [-f flags] [-t session]`** — `-d` detaches other clients, `-x` detach-and-SIGHUP them, `-E` skips `update-environment`, `-r` is read-only (alias for `-f read-only,ignore-size`). Client flags (`-f`, comma-separated; a leading `!` clears a flag on an already-attached client): `active-pane` (client-private active pane), `ignore-size` (excluded from size negotiation), and the control-mode trio `no-output`, `pause-after=secs`, `wait-exit` ([control mode](#control-mode)). A read-only client's *keys* are limited to detach/switch bindings — its stdin commands are not ([sharp edges](#client-flags-and-flow-control)).

**`detach-client [-aP] [-E cmd] [-s session] [-t client]`** — `-s` detaches every client on the session (Rimz's `detach`); `-P` SIGHUPs the client's parent; `-E` exec-replaces the client process.

**`kill-session [-aC] [-t session]`** — an absent target exits 1 with `can't find session: <name>` (alongside the two no-server shapes, the goal state of an idempotent kill). `-a` kills every *other* session. **`kill-server`** tears down the server, all sessions, all clients.

**`list-sessions [-F fmt] [-f filter]`** · **`list-clients [-F fmt] [-f filter] [-t session]`** — one line per session / attached client; `-f` keeps rows whose format evaluates non-zero. In a `list-clients` row the pane/window variables resolve against the client's attached session, so `#{pane_id}` is the active pane of that session's current window — per-client divergence exists only under the `active-pane` client flag.

Index: `has-session -t` (pure exit code), `rename-session`, `lock-client`/`lock-session`, `server-access [-adlrw] user` (socket ACL, 3.3), `list-commands` (machine-readable command syntax), `refresh-client` ([control mode](#client-flags-and-flow-control)).

### Windows and panes

**`new-window [-abdkPS] [-c dir] [-e VAR=val]… [-F fmt] [-n name] [-t window] [cmd…]`** — `-d` keeps the current window current; `-P -F '#{window_id}\t#{pane_id}'` prints the ids for follow-up targeting; `-n` names the window **and disables `automatic-rename` for it**, making the name a stable idempotency key (Rimz's resume/daemon windows probe `list-windows -F '#{window_name}'`); `-S` selects an existing window of that name instead of erroring (3.2); `-a`/`-b` insert after/before an index, shifting others; `-k` replaces an existing target. The window closes when its command exits unless `remain-on-exit` holds the corpse.

**`split-window [-bdfhIvPZ] [-c dir] [-e VAR=val]… [-l size] [-t pane] [cmd…] [-F fmt]`** — `-h` splits left-right, `-v` top-bottom (default); `-b` puts the new pane before (left of / above) the target — the sidebar-on-the-left shape; `-l <n>` fixes columns/lines, `-l <n>%` a percentage; `-f` spans the full window edge; `-d` leaves focus alone; `-P [-F]` prints the new pane (ask for `#{pane_id}` explicitly — the default format is index-shaped); an empty command `''` births a command-less pane writable via `display-message -I`. Splits mount fine on a detached session — no client required (the asymmetry with Zellij's detached-mount drop).

**`select-pane [-DdeLlMmRUZ] [-T title] [-t pane]`** — activates the pane *within its window only*: **it does not switch the session's current window** (probed 3.5a). A cross-window jump is `select-window -t @win ';' select-pane -t %pane`; only `switch-client` crosses sessions. `-e`/`-d` enable/disable input to the pane; `-T` sets the pane title; `-m`/`-M` set/clear the marked pane (the `{marked}` target); `-L/-R/-U/-D` move directionally.

**`select-window [-lnpT] [-t window]`** — accepts `@id` targets; `-l`/`-n`/`-p` are last/next/previous.

**`swap-window [-d] [-s src] [-t dst]`** — exchanges two windows' positions; succeeds into an occupied slot (probed) and `-d` keeps the current window current — the reorder primitive behind `lead_window`. `move-window [-abrdk]` relocates instead; `-r` renumbers a whole session.

**`kill-pane [-a] [-t pane]`** — kills the pane and its process; the last pane's death closes the window. `-a` kills every *other* pane. `kill-window [-a]` likewise.

**`list-panes [-as] [-F fmt] [-f filter] [-t target]`** — default one window; `-s` a whole session; `-a` every pane on the server (target ignored). One line per pane; a tab-separated multi-variable format is the stable cross-version read because missing variables empty their column rather than shifting it ([formats](#formats)).

**`capture-pane [-aAepPqCJNT] [-b buffer] [-E end] [-S start] [-t pane]`** — `-p` writes stdout (else into a paste buffer); `-S`/`-E` bound lines where 0 is the top of the visible screen, negatives reach into history, and `-` means history start / visible end; `-e` includes SGR colour/attribute escapes; `-C` octal-escapes non-printables; `-J` joins wrapped lines and preserves trailing spaces; `-N` preserves trailing spaces only; `-a` reads the alternate screen (`-q` to tolerate its absence).

**`send-keys [-FHKlMRX] [-c client] [-N count] [-t pane] key…`** — each argument is first looked up as a key name (`C-c`, `M-a`, `Enter`, `Escape`, `F1`, `NPage`…); an argument that is not a key name is sent as its characters. `-l` disables lookup entirely (literal UTF-8) — prefer it for typing text; `--` guards leading-dash arguments (probed); `-H` sends hex bytes; `-N` repeats; `-R` resets terminal state; `-X` drives copy mode.

**`display-message [-aIlNpv] [-c client] [-d delay] [-t pane] [message]`** — `-p` prints the expanded format to stdout: the universal "evaluate a format against this target" probe (the `window_width` read rides it; a `@window` target resolves to that window's active pane). `-a` dumps every format variable with its current value — the catalog probe; `-v` traces the expansion step by step — how a missing variable is caught.

**`display-popup [-BCE] [-b border-lines] [-c client] [-d dir] [-e VAR=val] [-h h] [-w w] [-x x] [-y y] [-s style] [-S border-style] [-T title] [-t pane] [cmd…]`** — a transient overlay running a command (3.2); `-E` closes on exit, `-EE` only on success, `-C` closes any open popup; sizes and positions accept `%`. Popups run under `default-shell` (3.5a). The surface the trust-gated popup integration will compile to.

**`pipe-pane [-IOo] [-t pane] [cmd]`** — streams the pane's output (`-O`, default) and/or input (`-I`) through a shell command; no argument closes the current pipe; `-o` opens only if none exists (toggle shape). Capture-pane snapshots; pipe-pane streams.

**`run-shell [-bC] [-c dir] [-d delay] [-t pane] [cmd]`** — runs a shell command (`/bin/sh -c`) or with `-C` a tmux command, formats expanded first; blocks the command queue until done unless `-b`. **`wait-for [-L|-S|-U] channel`** — bare form blocks the *client* until another client fires `wait-for -S` on the channel; `-L`/`-U` lock/unlock. The two synchronization verbs scripts get.

Index: `respawn-pane`/`respawn-window [-k] [-c] [-e]` (restart in place — the `remain-on-exit` partner), `resize-pane [-DLRUZTM] [-x -y]` (`-Z` zoom toggle), `resize-window`, `break-pane`, `join-pane`, `move-pane`, `rotate-window`, `link-window`/`unlink-window`, `select-layout` + the five preset layouts, `rename-window`, copy mode and its `send-keys -X` command set, the `choose-tree`/`choose-client`/`choose-buffer` interactive modes, and paste buffers (`set-buffer`, `load-buffer`, `save-buffer`, `paste-buffer`, `show-buffer`, `delete-buffer`).

### Option, hook, and environment commands

**`set-option [-aFgopqsuUw] [-t target] option value`** — the scope flag picks the table: `-s` server, none session, `-w` window (`set-window-option` is the same thing), `-p` pane; `-g` addresses the global table of that scope. For built-in options tmux infers the table from the option name when the flag is omitted (assuming `-w` for pane options) — explicit flags only matter for user options (`@name`) and for forcing pane-over-window scope. A local option shadows its global; `-u` unsets the local and reveals the global (`-U` also clears every pane in the window for pane options). `-q` silences unknown-option errors — the forward-compat tool for version-gated options; `-a` appends (styles get a comma inserted); `-o` sets only if unset; `-F` expands formats in the value.

**`show-options [-AgHpqsvw] [-t target] [option]`** — `-v` value only (`show-options -gv base-index` is the global-default read); `-A` includes options inherited from a parent scope; `-H` includes hooks.

**`set-environment [-Fhgru] [-t session] name [value]`** / **`show-environment [-hgs] [-t session] [name]`** — name and value are **separate argv tokens**; a single `NAME=value` argument errors `variable name contains =` (probed; only the `-e` birth flags use `=`). `-r` marks remove-on-spawn, `-h` hides (formats-only), `-g` targets the global table, `-s` formats output as shell exports.

**`set-hook [-agpRuw] [-t target] hook command`** / **`show-hooks`** — hooks are array options in the same scope tables; `-R` fires one immediately.

## Formats

The format language is tmux's read surface: every `-F` flag, filter, hook command, and `#()` goes through it. **An unknown or inapplicable variable expands to the empty string, never an error** — a tab-separated `-F` row degrades by emptying columns rather than shifting them, and a misspelled variable is silent (the `pane_start_time` lesson below). `display-message -p` evaluates, `-a` dumps the catalog, `-v` traces.

### Language

`#{variable}` with aliases `#S` session_name, `#W` window_name, `#I` window_index, `#D` pane_id, `#P` pane_index, `#T` pane_title, `#F` window_flags, `#H`/`#h` host; `##` is a literal `#`. Modifiers prefix inside the braces and compose:

- `#{?cond,yes,no}` conditional (nestable; `,`/`}` escape as `#,`/`#}`) · `#{==:a,b}` and `!=` `<` `>` `<=` `>=` string compare · `#{&&:a,b}`/`#{||:a,b}` boolean.
- `m:` fnmatch match, `m/r:` regex, `/i` ignore-case · `C:` search pane *content*, yielding a line number.
- `e|<op>[|f][|digits]:` arithmetic (`+ - * / m %`, comparisons; `f` floats) · `a:` ASCII char · `c:` colour → RGB hex.
- `t:` epoch → time string (`t/f/<strftime>` custom, `t/p` abbreviated past) · `b:`/`d:` basename/dirname · `q:` shell-quote (`q/h` escapes `#`).
- `E:` expand the result again, `T:` likewise plus strftime — how option values holding formats get evaluated.
- `S:`/`W:`/`P:`/`L:` loop the format over sessions/windows/panes/clients (windows/panes take a second variant for the current/active one) · `N/w:`/`N/s:` does a window/session of this name exist.
- `s/pat/rep/:` substitute (extended regex, any delimiter, `i` flag) · `=N:` truncate (negative from the end, `=/N/…:` adds a marker) · `pN:` pad · `n:` length · `w:` display width · `l:` literal (no expansion).
- `#(cmd)` inserts the last line of a shell command's output — cached, refreshed at most once a second, never blocks (a placeholder until the first completion); `/bin/sh` with the global environment.

### The variables Rimz reads

| Variable | Replaced with | Notes |
| --- | --- | --- |
| `session_name` | `#S` | mutable via `rename-session` |
| `session_id` | `$N` | server-unique |
| `window_id` | `@N` | the `view_id` grouping key; server-unique, monotonic |
| `window_name` | window label | sticky once `-n`-named (per-window `automatic-rename` off) — the resume/daemon idempotency key |
| `window_index` | position | `renumber-windows` rewrites it; ids never move |
| `window_width` / `window_height` | cells | the sidebar sizing read |
| `pane_id` | `%N` | `#D`; server-unique, monotonic; exported as `$TMUX_PANE` |
| `pane_active` | 1 if the window's active pane | one per window — N windows report N active panes (the per-view focus mark) |
| `pane_current_command` | live foreground process name | the process name, not its argv |
| `pane_current_path` | live cwd | |
| `pane_pid` | PID of the pane's **first** process | the spawned shell/command — never the live foreground child |
| `pane_title` | OSC 0/2 title | app-writable (`allow-set-title`); the sidebar identifies itself through it |
| `pane_start_command` / `pane_start_path` | spawn command / cwd | what the pane was born running |
| `pane_dead` / `pane_dead_status` / `pane_dead_signal` / `pane_dead_time` | remain-on-exit corpse facts | |
| `client_tty` / `client_session` / `client_name` / `client_control_mode` / `client_flags` | per-client facts | the `list-clients` row context |
| `socket_path` / `pid` / `start_time` / `version` | server facts | `start_time` is the **server's** start, not a pane's |

**There is no `pane_start_time`.** No release from 3.2 through 3.6b defines a per-pane process start-time variable — `display-message -v` reports `format 'pane_start_time' not found` and the column expands empty (probed 3.5a; absent from master `format.c`). The nearest live facts are `pane_pid` (first process) and the monotonic never-reused `%id` itself, which already rules out stale-id collisions within one server's lifetime; Rimz derives `pane_process_start` from `pane_pid` via `/proc` ([multiplexers.md → pane metadata](../../internals/sidebar/multiplexers.md#pane-metadata)).

Catalog breadth (~230 variables): `buffer_*`, `client_*` (geometry, flags, tty, uid), `command_*`, copy-mode state (`copy_cursor_*`, `selection_*`, `search_*`, `scroll_position`), `cursor_*`, `history_*` (`history_size`, `history_limit`, `history_bytes`), `hook_*` (firing context), `mouse_*`, `pane_*` (geometry, edges, flags, modes), `session_*` (counts, times, groups, `session_attached`), `window_*` (geometry, flags, counts, `window_zoomed_flag`, `window_layout`), and the server singletons. Full table: man FORMATS.

## Hooks

Commands run on triggers, stored as **array options** — they scope and stack exactly like options (global or per-session/window/pane; `set-hook -g name[i] cmd`; members run in index order; setting without an index resets the array to one member). The hook's command string is parsed by tmux, not a shell — quote the inner command accordingly (the `after-new-window` split carries its serve argv single-quoted inside the hook string).

- **Every command has an implicit after-hook** — `after-<command-name>` fires when the command completes, except when the command itself ran from a hook. This is the tab-template parity mechanism: a session-scoped `after-new-window` re-runs the sidebar split in every window opened later, and because the hook runs `split-window` (not `new-window`) it cannot recurse.
- **Control-mode notifications double as hooks** under the same names without `%` or arguments — `window-add` is `set-hook`-able — except `%exit`.
- Named hooks beyond the notifications: `alert-activity`/`alert-bell`/`alert-silence` (the `monitor-*` options), `client-active` (3.2), `client-attached`/`client-detached`/`client-focus-in`/`client-focus-out`/`client-resized`/`client-session-changed`, `command-error` (3.5), `pane-died` (remain-on-exit corpse) / `pane-exited` / `pane-focus-in`/`pane-focus-out` (need `focus-events on`) / `pane-set-clipboard`, `session-created`/`session-closed`/`session-renamed`, `window-linked`/`window-renamed`/`window-resized` (3.2, fires after `client-resized`)/`window-unlinked`.
- Scope gotcha since 3.2: window-shaped hooks (`window-layout-changed`, `window-linked`, `window-pane-changed`, `window-renamed`, `window-unlinked`) live in the **window** table and pane-shaped hooks (`pane-died`, `pane-exited`, `pane-focus-in/out`, `pane-mode-changed`, `pane-set-clipboard`) in the **pane** table — a session-scoped set silently misses them; `-g` reaches everything.
- The firing context rides the `hook_*` variables (`hook`, `hook_pane`, `hook_window`, `hook_session`, …) inside the hook's command.

## Options

Four scope tables — server, session, window, pane — each in a global and a local flavour; local shadows global. The room options Rimz applies at `ensure_session`, batched into one client call ([multiplexers.md → tmux backend](../../internals/sidebar/multiplexers.md#tmux-backend)); Rimz's value in bold:

| Option | Scope | Values | Why it matters |
| --- | --- | --- | --- |
| `focus-events` | server | **on** \| off | requests focus reporting from the terminal and forwards FocusIn/Out to apps; enables the `pane-focus-*` hooks; clients should re-attach after flipping |
| `set-clipboard` | server | **on** \| external \| off | `on` both accepts OSC 52 from apps (into a tmux buffer) and forwards to the outer terminal (needs terminfo `Ms`); `external` forwards only, ignoring app sets |
| `extended-keys` | server | **on** \| off \| always | modifyOtherKeys: `on` honours app requests for mode 1/2; `always` forces mode 1 onto non-requesters (3.2; revamped 3.5) |
| `extended-keys-format` | server | **csi-u** \| xterm | `C-S-a` → `^[[65;6u` (csi-u) vs `^[[27;6;65~` (xterm); **3.5+** |
| `escape-time` | server | ms, **0** | ESC-disambiguation delay; upstream default 10 since 3.5 (500 before) |
| `mouse` | session | **on** \| off | mouse events become bindable keys; click focuses panes |
| `history-limit` | session | lines, **100000** | scrollback cap **for panes created after the set** — existing panes keep their birth limit |
| `renumber-windows` | session | **on** \| off | closing a window renumbers indexes (respects `base-index`); `@id`s never move |
| `allow-passthrough` | pane (set at window scope) | off \| **on** \| all | the `\ePtmux;…\e\\` passthrough escape; **3.3+, default off**; `on` works only while the pane is visible, `all` always (3.4) |
| `aggressive-resize` | window | **on** \| off | size to the smallest/largest session currently *viewing* the window rather than merely linked to it |
| `pane-border-status` | window | **off** \| top \| bottom | a per-pane border text line (`pane-border-format`) |
| `pane-border-lines` | window | **simple** \| single \| double \| heavy \| number | border glyph set; `simple` is plain ASCII |

Neighbours a contributor will reach for: `default-size XxY` (detached birth geometry — **implicitly set by `new-session -x/-y`**, probed), `base-index` (first window index, conventionally global), `window-size largest|smallest|manual|latest`, `remain-on-exit [on|off|failed]` + `remain-on-exit-format`, `detach-on-destroy [on|off|no-detached|previous|next]`, `destroy-unattached [off|on|keep-last|keep-group]`, `exit-empty`/`exit-unattached` (server lifetime), `allow-rename` (apps renaming windows via escape, default off), `allow-set-title` (apps writing `pane_title`, default on), `automatic-rename[-format]`, `update-environment[]`, `default-terminal` + `terminal-features[]` (the modern per-terminal capability switchboard: `256`, `RGB`, `clipboard`, `extkeys`, `focus`, `hyperlinks`, `mouse`, `osc7`, `sixel`, `sync`, `title`, `usstyle`, …), `default-command`/`default-shell`, `popup-style`/`popup-border-style`/`popup-border-lines` (3.3), `synchronize-panes`, `monitor-activity`/`monitor-bell`/`monitor-silence`.

## Global and session environment

The server copies its spawn environment into the **global environment**; each session keeps a **session environment**. A new pane's process receives global merged with session (session wins), plus `TMUX`, `TMUX_PANE`, and `TERM` from `default-terminal`. The merge happens **at pane creation** — environment edits never reach live processes.

- `new-session -e` / `new-window -e` / `split-window -e` / `respawn-* -e` seed variables at birth, so even the first window inherits them.
- `set-environment -t <session> NAME VALUE` reaches only panes created afterwards — the idempotent re-assert for sessions born before a variable existed.
- `update-environment[]` (session option, fnmatch patterns allowed) copies the listed variables from the **attaching client** into the session env on `new-session` and every attach — variables absent on the client are marked for removal (`-r` semantics). The default list covers `DISPLAY`, the `SSH_*` set, `TERM_PROGRAM`, … — an attach from a new SSH connection silently rewrites them for future panes; `-E` on attach/new-session skips the mechanism.
- `-h` hidden variables live in the tables but are never exported — formats-only state.

## Control mode

`tmux -C` turns a client into a line-oriented protocol endpoint: commands go in on stdin, replies and asynchronous notifications come out on stdout. `-CC` additionally puts the tty in raw mode and brackets the stream with a `\eP1000p` DCS preamble and a closing `\e\\` (the iTerm2 integration shape); plain `-C` emits no terminal markers. [`PresenceWatch`](../../../crates/rimz/src/mux/tmux/presence.rs) holds `tmux -C attach-session -r -f no-output -t <session>` and reads notifications only.

### Protocol shape

- Each stdin line is a command (or `;`-sequence); each produces exactly one reply block: `%begin <time> <number> <flags>`, the output lines, then `%end` (success) or `%error` (failure) carrying **identical arguments**. `time` is epoch seconds; `number` increments per command — pair replies on it; `flags` is documented "currently not used" (the wire always says `1`).
- **A notification never appears inside a reply block** — the load-bearing guarantee: any `%`-line outside `%begin…%end` is an async event, and a reply block can be buffered atomically.
- **An empty stdin line detaches the client.** The `wait-exit` flag turns this into the drain handshake — after `%exit` the client waits for an empty line before exiting. Closing stdin tears the client down: the no-leak guarantee the presence watch leans on.
- An unparseable command still produces a block: `%begin` / `parse error: …` / `%error`.
- Control clients are sizeless and excluded from size negotiation until `refresh-client -C WxH` (or `@win:WxH`) gives them a size.
- The final line is `%exit [reason]`, emitted by the client itself; reasons: `detached (from session <name>)`, `detached and SIGHUP …`, `lost tty`, `terminated`, `too far behind`, `exited` (server had no sessions), `server exited`, `server exited unexpectedly`.

### Notification catalog (3.5a wire shapes)

✓ marks what `PresenceWatch` forwards as a presence nudge; everything else it reads and drops.

| Notification | Wire shape | Fired when | ✓ |
| --- | --- | --- | :---: |
| `%window-add` | `%window-add @id` | a window was linked into the client's session | ✓ |
| `%window-close` | `%window-close @id` | a linked window closed | ✓ |
| `%unlinked-window-add` / `-close` / `-renamed` | `… @id [name]` | the same events for windows **not** in the client's session — linked vs unlinked is judged per client against its own attached session | ✓ (add/close) |
| `%layout-change` | `%layout-change @id <layout> <visible-layout> <raw-flags>` | a split opened/closed/resized in a window; old releases sent two fields — accept ≥ 2 | ✓ |
| `%sessions-changed` | bare | a session was created or destroyed | ✓ |
| `%window-renamed` | `@id name` | | |
| `%window-pane-changed` | `@id %id` | a window's active pane changed | |
| `%session-changed` | `$id name` | this client switched session | |
| `%session-renamed` | **wire: `$id name`** — the man documents the name only; parse id-then-name | | |
| `%session-window-changed` | `$id @id` | a session's current window changed | |
| `%client-session-changed` | `<client> $id name` | another client switched session | |
| `%client-detached` | `<client>` | (3.2) | |
| `%output` | `%output %id <value>` | pane output; bytes < 0x20 and `\` escape as octal `\nnn`, bytes ≥ 0x80 pass raw — the escaping is byte-wise, so a line may split a UTF-8 sequence | suppressed |
| `%extended-output` | `%extended-output %id <age-ms> … : <value>` | replaces `%output` under `pause-after`; ignore anything between the age and the lone `:` | suppressed |
| `%pause` / `%continue` | `%id` | pause-mode flow control | |
| `%subscription-changed` | `name $id @id <win-idx> %id … : value` — window subs put `-` in the pane slot, session subs in window/index/pane | a `refresh-client -B` format changed; coalesced to ≤ 1/s | |
| `%pane-mode-changed` | `%id` | copy-mode enter/leave | filtered |
| `%paste-buffer-changed` / `%paste-buffer-deleted` | `name` | (deleted: 3.4) | |
| `%config-error` | `<error>` | config-file load errors (3.4) | |
| `%message` | `<text>` | `display-message` aimed at this client | |
| `%exit` | `[reason]` | last line before client exit | EOF |

**Layout strings** (`%layout-change`, `window_layout`, `select-layout`): a 4-hex-digit checksum, a comma, then a cell tree — each cell `WxH,X,Y` followed by `,<pane-number>` for a leaf (the bare number is the pane's `%id` digits), `{…}` for left-right children, or `[…]` for top-bottom children, children comma-separated. Example: `b25d,208x60,0,0{104x60,0,0,1,103x60,105,0,2}`.

### Client flags and flow control

- `no-output` suppresses pane output entirely — both `%output` and `%extended-output` — the topology-only diet.
- Without `pause-after`, a reader that stops draining is force-exited once any buffered output ages past **five minutes** (`CONTROL_MAXIMUM_AGE 300000` ms), exit message `too far behind` — even a `no-output` client should keep reading promptly.
- `pause-after=secs` switches output to `%extended-output` and, past the threshold, pauses the pane (`%pause`) instead of disconnecting the client. `refresh-client -A %id:state` drives it per pane: `continue` resumes (`%continue`), `pause` pauses now, `off` stops the pane's output for this client — when every client turns a pane off, tmux stops reading the pane's pty entirely (backpressure onto the application).
- `refresh-client -B name:what:format` subscribes to a format: `what` is empty (the attached session), a `%id`, `%*` (all panes in the session), an `@id`, or `@*`; changes arrive as `%subscription-changed` at most once a second; `-B name` alone unsubscribes. The push alternative to polling `list-panes` — the upgrade path the presence watch has not needed.
- `refresh-client -f flags` rewrites the flag set on a live client; `-r %id:<report>` lets a control client answer OSC 10/11-style pane queries (3.5); `-l` requests the outer terminal's clipboard into a paste buffer.
- **`read-only` restricts key input only — stdin commands still execute.** The man's "only keys bound to detach-client or switch-client have any effect" governs key bindings; a `-r` control client can still mutate the server through commands. The presence watch's actual safety property is that it writes nothing to stdin.
