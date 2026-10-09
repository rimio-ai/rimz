# Shared LSP

Your agents already navigate code semantically when they can. Claude's `LSP` tool starts a language server for the session and asks it for definitions, references, and call hierarchies; an agent without one falls back to grep and reads three files to find the one that matters. The server is the right idea: a `def` answer is exact where a grep answer is a guess, and it saves the tokens the guess would have cost.

The problem is who owns the server. A harness starts one per agent session and holds it until the session ends. `rust-analyzer` on this repository reaches 4.4 GB after its first index and 6.3 GB once references have been searched, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, whether or not anyone is querying. And nothing on your machine says no: the next launch starts another one, and the kernel decides which process dies.

`rimz lsp` puts one server per checkout in front of every agent working in it, started on the first query and only when memory allows. Agents query it read-only through the `rimz lsp` command; RimZ keeps its view of the disk current as files are saved and frees its memory when it is idle or the team finishes. The next query can start it again without relaunching agents. The feature is off until you configure a server, and an agent without one works as agents work today.

## Configure a server

A server is one `[lsp.servers.<name>]` table in your per-machine `~/.rimz/config.toml`. The recommended servers are rust-analyzer for Rust and ty for Python, one shared server per language in a checkout. No server is configured by default.

Rust LSP example:

```toml
[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, hover = { dropGlue = { enable = false } }, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
```

`command` is an argv, not a shell line, and the executable has to be on the path of the shell that launches agents. `root-markers` decide which checkouts get this server: any listed path present at the checkout root enables it, so a repository without a `Cargo.toml` never starts `rust-analyzer`. `extensions` pick the server when a query names a file, and they decide which saved files the server is told about.

The `init-options` matter for Rust. `checkOnSave = false` stops `rust-analyzer` running `cargo check` on startup and on every save: the queries expose no diagnostics, so the check would spend 57 CPU seconds and a 3.7 GB spike for nothing, and it would fight your agents' own builds for the checkout's `target/` lock. The `workspace.symbol` block makes symbol-name queries find functions, which `rust-analyzer` leaves out of workspace search by default, and lifts its 128-result cap that otherwise hides the symbol you asked for; leave `workspace.symbol.search.scope` unset, since `workspace_and_dependencies` searches dependencies instead of your code and breaks symbol-name queries. `hover.dropGlue` switches off the `needs Drop` line that `rust-analyzer` adds to a type's hover; its `Implements notable traits` line has no setting and stays. A server for another language needs no such tuning unless its defaults do work the queries never read.

For Python, use ty 0.0.84 or newer. Older versions can return `no results` for files no editor holds open.

```toml
[lsp.servers.ty]
command = ["ty", "server"]
extensions = ["py"]
root-markers = ["pyproject.toml", "ty.toml"]
```

If you use uv, `command = ["uvx", "ty", "server"]` works too, including a pinned token such as `ty@0.0.84`. No `init-options` are needed: ty defaults to checking only open files, avoiding diagnostic work the agent queries do not read. The server kind is detected from the command; set `kind = "ty"` for a wrapper script. `rimz lsp status --server ty` shows the detected kind.

A repository can ship its own entry in its project config, so a team gets the same server without each member writing the table. Because `command` and `init-options` run and configure a process, they join the [trust hash](./security.md#project-trust): the entry stays inert until you run `rimz trust grant`. Launch warns about an untrusted entry and continues; a same-named machine entry stays in effect. A trusted project entry replaces the same-named machine entry whole, not field by field. The memory policy below stays machine-only, because a repository cannot decide how much of your machine it may take.

Every field and its default is in the [reference](../reference/cli/lsp.md#configuration).

## What a launch does on your machine

Nothing runs until you launch agents or attach an editor. Every launch that opens panes, `rimz agents`, `rimz teams`, a `-p` run, a resume, and rebirth recovery, does the following for its checkout before the first pane opens:

1. It resolves which configured servers apply, by root markers, and checks their configuration and executables. An optional server's configuration error warns with the fix and skips that server; a required server's error refuses launch. Untrusted project entries warn and stay inert. A machine-wide LSP policy error skips all servers with a warning unless a required server matches the checkout, in which case launch refuses.
2. If a broker for this (checkout, name) already exists, dormant or running, the launch joins it. Otherwise it starts a small hidden broker, leaving the language server dormant by default. The broker's socket and registry entry live under RimZ's runtime directory, so they do not survive a reboot.
3. Each agent's wrapper takes a lease on the broker, and the launch goes on to open panes. A [required server](#make-a-server-a-precondition) instead starts eagerly before panes open.

The first query asks the broker to check memory under one machine-wide lock, so two starts cannot both pass on the same free gigabytes. The server may start when free memory, less what running servers are still expected to grow by, less this server's estimate, leaves a reserve of the larger of 10% of RAM and 8 GB. The estimate is learned from this project's recent peaks in `~/.rimz/lsp-history.jsonl`, or the configured `memory-estimate` (8 GB) before any history exists. If needed, RimZ frees other servers in least-recently-queried order, protecting ones still indexing, ready for less than five minutes, or queried within two minutes.

If memory still does not fit, the query reports `not started: memory short` and the agent uses grep. The broker stays dormant; a later query retries. There is no optional-server memory refusal at launch, and no need to relaunch agents after freeing memory.

Adding a server to the config still reaches the next launch, not an existing agent. Subagents skip broker creation: a child works in its parent's checkout and joins its parent's broker, including a dormant one that its queries can wake.

Each worktree is its own checkout, so agents in `-w feat-x` get a separate server from agents on the main tree, each indexing the files it will actually read.

## What your agents see

An agent launched with a shared server available finds one paragraph in the system reminder RimZ appends, naming dormant and running servers and pointing at the `rimz-lsp` skill from your [skill library](./configuration.md#skills). It explains that a first query may wait for startup. The skill teaches the verbs below and the two exit codes that mean "use grep", so an agent spends no turn learning the tool. Without a shared server available there is no paragraph.

An agent's query gets one of five lookup and availability outcomes. Exit 0 is an answer, and `no results` for a resolved symbol is a real one. Exit 3 means the server is unavailable, with the reason on one line, such as no server for this checkout or insufficient memory to start. Exit 4 means startup or indexing exceeded the query's 110-second wait, with elapsed time for that server start. Exit 5 means the name was not found as written; the output lists any symbols with the same last segment. Exit 6 means the name is ambiguous and lists the matching symbols. A dormant server is not itself a failure: the query wakes it and answers normally if it becomes ready in time. Agents can use grep on 3 and 4 and try a later query; they are never messaged about a stop.

Claude's native `LSP` tool is switched off whenever a server is configured, even while it is dormant or a query cannot start it for lack of memory, so no private index starts outside the budget. `rimz agents validate` warns when a profile still lists `LSP` in its tools. OpenCode and Grok have no verified switch for their own servers, so with those two the shared server is an addition, not a replacement.

## Ask the server yourself

The same command works from your shell, in the checkout, and it is the fastest way to check what the agents are getting. A target is a position (`path:line:col`, one-based) or an exact symbol name. Write `Store::open` for a method, `Type::field` for a field, `launch_reminders::render` for a module function, or `rimz::store::Store` for a crate-qualified name. A method of the same name wins over a field; use the field's position in that case. You can leave intermediate segments out, but incorrect segments are refused rather than guessed. A trailing `()` is accepted too. A name may also start with its file, the way a code anchor in notes does: `crates/rimz/src/store/mod.rs::Store::open` looks for `Store::open` only in that file, and a shorter path such as `store/mod.rs::Store::open` names every file ending with it unless a file sits at exactly that path.

```sh
rimz lsp def MuxBackend                      # where it is defined
rimz lsp def Store::open QueryErr             # definitions for several names
rimz lsp refs GcReport                       # references grouped by file, with source snippets
rimz lsp hover Store::open                   # type and documentation; Type::method names a method
rimz lsp impl MuxBackend                     # implementations of a trait
rimz lsp callers sweep_locked                # incoming calls
rimz lsp callees Store::open                 # outgoing calls
rimz lsp symbols crates/rimz/src/lib.rs      # a file's outline
rimz lsp find Mux                            # workspace symbol search
```

A name that matches several symbols lists the candidates instead of guessing; rerun with one of the listed qualified names or its position. A wrong qualifier lists possible names the same way, so you can repair the query in one rerun. `refs`, `impl`, `callers`, `callees`, and `find` list only results inside the checkout and end with how many were left out, if any; `--external` shows them. Add `--json` for structured output (the raw LSP result on success, an outcome and candidates for not-found or ambiguous names), and `--server <name>` when a checkout has more than one server and the file's extension does not settle it. The checkout is the one enclosing your current directory (or `--root`). Everything about targets, output, and flags is in the [reference](../reference/cli/lsp.md#queries).

Definition results include the full item's line span when the server supplies it, so you can read the whole definition without guessing a window around its name. Copy the position and parenthesized span into a notes code span to check those lines before hand-off.

When tracing several symbols, pass them together to the same verb rather than making one call per name; each gets a `==> TARGET <==` block, even when another target fails.

Broad queries can return hundreds of locations. Lists show the first 50, with the total and remaining count whenever there are more. Add `--limit N` for a larger head or `--all` for the complete list in the selected scope. References, implementations, callers, and callees group entries by file; `find` keeps its relevance order. Checkout results come before external ones. Source snippets stop at 100 characters with `…` when shortened. JSON stays complete and cannot be combined with either limit flag.

For files no attached editor holds, the server answers from disk, kept current by watching saved files. Attaching your editor lets agents query your unsaved buffers too.

Reading a definition with a fixed line window can miss half the body or include unrelated code. Use `rimz lsp show` with a file's symbol anchor to read the item's current source, or pass several anchors to read them together. It resolves each anchor against the server outline, then prints numbered lines from disk. Large items show their children's signatures first; add `--full` to read the whole body. An unsaved-editor marker means the outline may be ahead of the disk text. The [source-reading reference](../reference/cli/lsp.md#read-source-by-anchor) lists position and line-range forms too.

```sh
rimz lsp show crates/rimz/src/lsp/check.rs::run crates/rimz/src/lsp/query.rs::render_with_source
```

## Use it from your editor

Your editor already runs rust-analyzer for completion, navigation, and diagnostics. Point it at `rimz lsp attach` instead to share the index your agents use, and let their answers include the changes you have not saved yet. Configure the `rust` server above first, then replace the editor's server command using its setting below. The editor must run on the host, or in a sandbox that binds the host's `XDG_RUNTIME_DIR` so it can reach the shared server.

| Editor | Setting |
| --- | --- |
| VS Code | Run `rimz lsp shim --server rust`, then set `"rust-analyzer.server.path": "<printed path>"`. VS Code's rust-analyzer server settings are ignored. |
| Zed | Set `lsp.rust-analyzer.binary = { path = "rimz", arguments = ["lsp", "attach", "--server", "rust"] }`. |
| Neovim | Set `cmd = { "rimz", "lsp", "attach", "--server", "rust" }`, with `root_dir` set to the checkout; `--root` is optional. |
| Helix | Set `language-server.rust-analyzer = { command = "rimz", args = ["lsp", "attach", "--server", "rust"] }`. |

For Python, attach to the `ty` server configured above for navigation and type checking. Keep Ruff as your editor's own server for linting and formatting: Ruff does not answer the agents' navigation queries. Ruff can also be shared, but a second shared server on `py` requires `--server` on Python queries and makes `rimz lsp check` unable to choose a server for Python anchors.

| Python editor | Setting |
| --- | --- |
| Neovim | In the ty LSP configuration, set `cmd = { "rimz", "lsp", "attach", "--server", "ty" }`, with `root_dir` set to the checkout. |
| Helix | Set `language-server.ty = { command = "rimz", args = ["lsp", "attach", "--server", "ty"] }` and include `ty` in the Python language's `language-servers` list alongside the editor's own Ruff server. |
| Zed | Python binary overrides have not been verified with attachment; this guide does not promise ty, Pyright, basedpyright, or Ruff attachment in Zed yet. |

Pyright and basedpyright remain alternatives to ty. Choose one shared Python server per checkout; to use Pyright instead, replace the ty entry with:

```toml
[lsp.servers.pyright]
command = ["pyright-langserver", "--stdio"]
extensions = ["py"]
root-markers = ["pyproject.toml", "pyrightconfig.json"]
init-options = { python = { analysis = { typeCheckingMode = "strict" } } }
```

For basedpyright, use `command = ["basedpyright-langserver", "--stdio"]` and put analysis settings under `init-options.basedpyright.analysis`. Use the corresponding editor LSP configuration and replace `ty` in the attachment arguments with your RimZ server name. Both kinds are detected from their commands; set `kind` explicitly for a wrapper script.

Python extensions in VS Code are outside this integration. Commands that need the editor to apply an edit or answer a server prompt, such as Pyright's organize-imports command, are unsupported. Ruff still supports formatting and code actions that return edits directly. Python cache and virtual-environment directories are not watched; after installing a package into `.venv`, run `rimz lsp stop --server ty` (or the configured server name) so the next request starts a fresh server.

For VS Code, the shim is an executable file at `~/.local/bin/rimz-lsp-rust` that runs RimZ with the attachment arguments; the command prints the path to paste into the setting. The other editors invoke RimZ directly, so `rimz` must be on their PATH. Restart the editor's language server after changing its configuration. Attachment uses the checkout reported by the editor, or the global `--root` if supplied. It joins the shared server or creates one when absent, without needing an agent launch first, under the same memory and trust policy.

RimZ owns the server settings: editor initialization options and later server-setting changes are ignored. Change `init-options` in your RimZ server configuration instead. To get cargo-check diagnostics only while an editor is attached, set `editor-check-on-save = true` on your rust-analyzer server entry and keep `checkOnSave = false` inside `init-options`. The first editor turns checking on; the last editor leaving restores your configured setting, without restarting the server. This is off by default. Set `cargo = { targetDir = true }` inside `init-options` too, keeping any other options you use: the separate target directory avoids contention with agent builds, though checking still costs CPU and memory. For checks even without an editor, set `checkOnSave = true` inside `init-options` instead.

While attached, your unsaved buffers are visible to agents. Definition, reference, and implementation output replaces the disk source line with `(unsaved in editor)` for a buffer changed since it was opened or saved. This marker is advisory: an already-unsaved buffer when first opened is not detected, and editing back to the disk text does not clear it. Raw JSON output is unchanged.

If two editors open the same file, the first opener's buffer supplies answers for both editors and the agents; their local buffers are not merged or overwritten. The second editor receives diagnostics only when its text matches the diagnostic snapshot, and stale diagnostics are cleared when its text diverges. The owner keeps its diagnostics until the server refreshes them. Closing the first editor's file transfers ownership to the next opener. `rimz lsp status --server rust` shows attached editors, their files, and which buffer owns each file.

Rust-analyzer's Run, Debug, Impl, and Reference lenses and hover actions work in VS Code. Neovim shows lenses when lens display is enabled, but warns once when you activate an action it cannot run unless a plugin implements it. Helix has no code-lens UI.

An attached editor does not keep an idle server's memory resident. The next request restarts it and restores held buffers. If a stop or memory refusal cancels outstanding requests, the editor gets one explanatory message; a stop with no outstanding requests produces no popup. VS Code also receives the server's stopped status.

To return to the editor's own server, remove the command override above and restart its language server. For VS Code, also remove the generated shim with `rm ~/.local/bin/rimz-lsp-rust` (or the path printed for a custom directory). Disconnecting releases the editor's lease without stopping agents' server access.

### Check notes before hand-off

A note can name a symbol that does not exist or point at an old line. Before handing it off, check its inline-code anchors against the checkout's files and shared server outlines:

```sh
rimz lsp check notes.md
```

When a hand-off spans several notes files, check them together; each gets its own result, even if another cannot be read:

```sh
rimz lsp check notes.md plan.md
```

When a symbol still exists but its line hint has drifted, refresh the hint instead of editing each number by hand. The same pass repairs a common slip: using a bare file name such as `install.rs` where the whole checkout-relative path is needed. The check flags such a suffix whether one file matches (`short-path`) or several do (`ambiguous-path`).

```sh
rimz lsp check notes.md --fix
```

This rewrites existing hint digits and completes a unique short path to the full path from the checkout root for symbol and line-only anchors alike, then checks the updated notes. When several files share a short path, completion needs the symbol to single out one file and no candidate with unsaved editor changes; otherwise the path stays flagged. When several items match a symbol, the hint must select exactly one within three lines; a unique item can be fixed even when its hint has drifted farther, and so can a type whose other matches are its impl blocks. It preserves hint syntax and leaves hints that cannot select one item or come from unsaved editor changes alone; completing a unique path does not depend on editor contents. Review the notes diff to keep or undo the changed paths and hints. A second run with nothing to change leaves the file untouched.

Anchors written without a line span can get one in the same pass:

```sh
rimz lsp check notes.md --fix --hints
```

Each symbol anchor that names one item gains its line span after the closing backtick, such as `` `config.rs::ConfigErr` (40-62) ``; a type with impl blocks counts as one item and gets the type's own range. An anchor that matches several items, such as a function and a method of the same name, keeps no span and is listed as `ambiguous-symbol` with every competing item; lengthen its chain (`config.rs::Loader::load`) and rerun. Anchors that already carry hints are only refreshed.

Fix remaining flagged paths, symbols, or lines and rerun. A path suffix that still names several files after `--fix` needs more of the path. The summary counts what passed, failed, remained unchecked, or was external; `unchecked` means no server covers that language, so verify those anchors by hand. Cite another repository with `owner/repo@ref:path::Symbol` or `owner/repo@ref:path:~N`: these count as `external`, without being verified or flagged as failures. The [reference](../reference/cli/lsp.md#check-anchors-in-a-notes-file) defines the accepted anchors and JSON output.

## See what is running, and stop it

`rimz lsp list` shows the shared servers of the room you run it in, one row per checkout and server, and `rimz lsp list --all` shows every shared server on the machine, whichever room registered it. Each row carries its state (`not started`, `starting`, `indexing`, `ready`, `dormant: <reason>`, or briefly `stopped` during shutdown), checkout, name, current and peak memory for running servers, query count and time since the last query, restarts, and leases. Alive rows sort first, and a crash stands out with a failure glyph and tone. RSS and PEAK show `-` when not running. `not started` means the server has never run; other dormant entries name the stop reason. `rimz doctor` shows every machine row under `LSP`, with state, checkout, server, RSS, and leases, plus the last refusal, so when an agent says it is on grep you can see why in one place.

If a configured server cannot start, run `rimz doctor` in that checkout. It checks only servers whose root markers match, reuses the status of running servers, and briefly starts the others to ask for their startup reply before terminating them. Checks run together with a ten-second wait; a slow reply warns rather than counting as a failure. A failed check shows the exit, recent stderr, and a fix, including the `rustup component add` command when the pinned toolchain lacks its analyzer. Untrusted project servers ask for `rimz trust` instead of running. `rimz lsp status` retains the last broker crash cause even when a later doctor check succeeds.

A server can free its memory while agents remain:

- **Nobody queries it.** After ten minutes without a query, a ready server becomes `dormant: idle`. Set machine `[lsp] idle-timeout` to change that duration. Startup, indexing, and queries in flight are protected; the check runs every five seconds.
- **The team finishes.** Flipping a cohort to `Done` stops its checkout's servers to `dormant: team done`, without changing the flip's output or stopping agents.
- **You stop it.** Run `rimz lsp stop --server rust` from the checkout, or `rimz lsp stop --all` for the machine. The server becomes `dormant: stopped by hand`; the next query can restart it.
- **Another query needs the memory.** Admission can evict an older, idle server to make room, leaving it `dormant: evicted`.
- **Memory runs short.** Every broker samples free memory every five seconds. Below 5% of RAM, one broker stops servers in least-recently-queried order until memory recovers, without the age protections used for admission. The victim's whole process group goes, including any `cargo` children, and a record lands in `~/.rimz/logs/lsp.log.jsonl`. Servers carry a high `oom_score_adj` so the kernel favors them over agents if it acts first.
- **The server crashes.** The broker records the crash and becomes dormant, ready for a later query to retry. Until then `rimz lsp list` and `rimz doctor` show it as `dormant: crashed`; run `rimz lsp stop` to acknowledge it, which turns the entry into `dormant: stopped by hand`. A `dormant: memory pressure` entry clears the same way.

Each restart checks memory again; `RESTARTS` counts starts after a stop, not the first lazy start. The entry remains available while agents or attached editors hold leases. Once the last one leaves, a 60-second grace lets a replacement rejoin before the broker exits, even if dormant. Removing the checkout ends its broker too. To turn sharing off for future launches, remove the `[lsp.servers.<name>]` table; Claude's native tool is then no longer denied. Existing agents keep their leases until they exit.

## Make a server a precondition

By default a server is enrichment: `policy = "optional"` defers startup and memory admission to the first query. For a task where you want memory secured before agents begin, set `policy = "required"` on the entry. When creating a new broker, required policy admits and starts the server eagerly, without waiting for its index. A required launch that fails the memory check waits before opening panes, first come first served among required launches, and the launcher's terminal prints its queue position and remaining time each time they change. At `wait-timeout` (10 minutes by default) the launch fails with the memory needed, the memory free, and who holds the rest. Optional launches never queue behind required ones.

`required` gates initial launch admission only. Joining an existing broker, including a dormant one, does not repeat that check. After a stop, both policies restart on a query and can refuse for memory, because RimZ never stops agents to keep a server alive.

Machine-only keys control memory: `reserve-percent` and `reserve-min` set the memory a start must leave untouched, `kill-floor-percent` sets where the watchdog begins stopping servers, and `idle-timeout` controls how long an unused ready server stays resident. Their defaults are in the [reference](../reference/cli/lsp.md#configuration); lower the reserve only on a machine whose agents leave headroom you can see in `rimz lsp list`.

## See also

- [Shared LSP reference](../reference/cli/lsp.md): every verb, flag, exit code, and configuration field with its default.
- [Subagents](./subagents.md): children join their parent's server, and why a native per-child index was the case that made this worth building.
- [Worktrees](./worktrees.md): each worktree is a separate checkout with its own server.
- [Security and trust](./security.md#project-trust): how a project's server entry enters the trust hash and what an untrusted clone may launch.
- [Language server internals](../internals/lsp.md): the admission formula, the broker, the watchdog, and the learned-cost record.
