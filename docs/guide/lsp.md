# Shared LSP

Your agents already navigate code semantically when they can. Claude's `LSP` tool starts a language server for the session and asks it for definitions, references, and call hierarchies; an agent without one falls back to grep and reads three files to find the one that matters. The server is the right idea: a `def` answer is exact where a grep answer is a guess, and it saves the tokens the guess would have cost.

The problem is who owns the server. A harness starts one per agent session and holds it until the session ends. `rust-analyzer` on this repository reaches 4.4 GB after its first index and 6.3 GB once references have been searched, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, whether or not anyone is querying. And nothing on your machine says no: the next launch starts another one, and the kernel decides which process dies.

`rimz lsp` puts one server per checkout in front of every agent working in it, started on the first query and only when memory allows. Agents query it read-only through the `rimz lsp` command; RimZ keeps its view of the disk current as files are saved and frees its memory when it is idle or the team finishes. The next query can start it again without relaunching agents. The feature is off until you configure a server, and an agent without one works as agents work today.

## Configure a server

A server is one `[lsp.servers.<name>]` table in your per-machine `~/.rimz/config.toml`. No server is configured by default.

Rust LSP example:

```toml
[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
```

`command` is an argv, not a shell line, and the executable has to be on the path of the shell that launches agents. `root-markers` decide which checkouts get this server: any listed path present at the checkout root enables it, so a repository without a `Cargo.toml` never starts `rust-analyzer`. `extensions` pick the server when a query names a file, and they decide which saved files the server is told about.

The `init-options` matter for Rust. `checkOnSave = false` stops `rust-analyzer` running `cargo check` on startup and on every save: the queries expose no diagnostics, so the check would spend 57 CPU seconds and a 3.7 GB spike for nothing, and it would fight your agents' own builds for the checkout's `target/` lock. The `workspace.symbol` block makes symbol-name queries find functions, which `rust-analyzer` leaves out of workspace search by default, and lifts its 128-result cap that otherwise hides the symbol you asked for. A server for another language needs no such tuning unless its defaults do work the queries never read.

A repository can ship its own entry in its project config, so a team gets the same server without each member writing the table. Because `command` and `init-options` run and configure a process, they join the [trust hash](./security.md#project-trust): the entry stays inert until you run `rimz trust grant`, and a launch on an untrusted clone that declares one refuses with the fix rather than starting agents without it. A trusted project entry replaces the same-named machine entry whole, not field by field. The memory policy below stays machine-only, because a repository cannot decide how much of your machine it may take.

Every field and its default is in the [reference](../reference/cli/lsp.md#configuration).

## What a launch does on your machine

Nothing runs until you launch agents. Every launch that opens panes, `rimz agents`, `rimz teams`, a `-p` run, a resume, and rebirth recovery, does the following for its checkout before the first pane opens:

1. It resolves which configured servers apply, by root markers, and checks each executable exists. A missing binary or an untrusted project entry refuses the launch here, with the fix.
2. If a broker for this (checkout, name) already exists, dormant or running, the launch joins it. Otherwise it starts a small hidden broker, leaving the language server dormant by default. The broker's socket and registry entry live under RimZ's runtime directory, so they do not survive a reboot.
3. Each agent's wrapper takes a lease on the broker, and the launch goes on to open panes. A [required server](#make-a-server-a-precondition) instead starts eagerly before panes open.

The first query asks the broker to check memory under one machine-wide lock, so two starts cannot both pass on the same free gigabytes. The server may start when free memory, less what running servers are still expected to grow by, less this server's estimate, leaves a reserve of the larger of 10% of RAM and 8 GB. The estimate is learned from this project's recent peaks in `~/.rimz/lsp-history.jsonl`, or the configured `memory-estimate` (8 GB) before any history exists. If needed, RimZ frees other servers in least-recently-queried order, protecting ones still indexing, ready for less than five minutes, or queried within two minutes.

If memory still does not fit, the query reports `not started: memory short` and the agent uses grep. The broker stays dormant; a later query retries. There is no optional-server memory refusal at launch, and no need to relaunch agents after freeing memory.

Adding a server to the config still reaches the next launch, not an existing agent. Subagents skip broker creation: a child works in its parent's checkout and joins its parent's broker, including a dormant one that its queries can wake.

Each worktree is its own checkout, so agents in `-w feat-x` get a separate server from agents on the main tree, each indexing the files it will actually read.

## What your agents see

An agent launched with a shared server available finds one paragraph in the system reminder RimZ appends, naming dormant and running servers and pointing at the `rimz-lsp` skill from your [skill library](./configuration.md#skills). It explains that a first query may wait for startup. The skill teaches the verbs below and the two exit codes that mean "use grep", so an agent spends no turn learning the tool. Without a shared server available there is no paragraph.

An agent's query gets one of five lookup and availability outcomes. Exit 0 is an answer, and `no results` for a resolved symbol is a real one. Exit 3 means the server is unavailable, with the reason on one line, such as no server for this checkout or insufficient memory to start. Exit 4 means startup or indexing exceeded the query's 30-second wait, with elapsed time for that server start. Exit 5 means the name was not found as written; the output lists any symbols with the same last segment. Exit 6 means the name is ambiguous and lists the matching symbols. A dormant server is not itself a failure: the query wakes it and answers normally if it becomes ready in time. Agents can use grep on 3 and 4 and try a later query; they are never messaged about a stop.

Claude's native `LSP` tool is switched off whenever a server is configured, even while it is dormant or a query cannot start it for lack of memory, so no private index starts outside the budget. `rimz agents validate` warns when a profile still lists `LSP` in its tools. OpenCode and Grok have no verified switch for their own servers, so with those two the shared server is an addition, not a replacement.

## Ask the server yourself

The same command works from your shell, in the checkout, and it is the fastest way to check what the agents are getting. A target is a position (`path:line:col`, one-based) or an exact symbol name. Write `Store::open` for a method, `launch_reminders::render` for a module function, or `rimz::store::Store` for a crate-qualified name. You can leave intermediate segments out, but incorrect segments are refused rather than guessed. A trailing `()` is accepted too.

```sh
rimz lsp def MuxBackend                      # where it is defined
rimz lsp refs GcReport                       # every reference, with the source line
rimz lsp hover Store::open                   # type and documentation; Type::method names a method
rimz lsp impl MuxBackend                     # implementations of a trait
rimz lsp callers sweep_locked                # incoming calls
rimz lsp callees Store::open                 # outgoing calls
rimz lsp symbols crates/rimz/src/lib.rs      # a file's outline
rimz lsp find Mux                            # workspace symbol search
```

A name that matches several symbols lists the candidates instead of guessing; rerun with one of the listed qualified names or its position. A wrong qualifier lists possible names the same way, so you can repair the query in one rerun. Callers and callees list only code inside the checkout and end with how many were left out, if any; `--external` shows them. Add `--json` for structured output (the raw LSP result on success, an outcome and candidates for not-found or ambiguous names), and `--server <name>` when a checkout has more than one server and the file's extension does not settle it. The checkout is the one enclosing your current directory (or `--root`). Everything about targets, output, and flags is in the [reference](../reference/cli/lsp.md#queries).

The server answers from the disk, kept current by watching saved files, not from anyone's editor buffer. An unsaved change is invisible to it, which is what you want when several agents share one view.

## See what is running, and stop it

`rimz lsp list` shows every shared server on the machine, whichever room registered it: its checkout, name, state (`dormant`, `starting`, `indexing`, `ready`, or briefly `stopped` during shutdown), current and peak memory for running servers, query count and time since the last query, restarts, and leases. Dormant entries name the stop reason when there is one. `rimz doctor` shows shared-server status under `LSP`, plus the last memory refusal, so when an agent says it is on grep you can see why in one place.

A server can free its memory while agents remain:

- **Nobody queries it.** After ten minutes without a query, a ready server becomes `dormant: idle`. Set machine `[lsp] idle-timeout` to change that duration. Startup, indexing, and queries in flight are protected; the check runs every five seconds.
- **The team finishes.** Flipping a cohort to `Done` stops its checkout's servers to `dormant: team done`, without changing the flip's output or stopping agents.
- **You stop it.** Run `rimz lsp stop --server rust` from the checkout, or `rimz lsp stop --all` for the machine. The server becomes `dormant: stopped by hand`; the next query can restart it.
- **Another query needs the memory.** Admission can evict an older, idle server to make room, leaving it `dormant: evicted`.
- **Memory runs short.** Every broker samples free memory every five seconds. Below 5% of RAM, one broker stops servers in least-recently-queried order until memory recovers, without the age protections used for admission. The victim's whole process group goes, including any `cargo` children, and a record lands in `~/.rimz/logs/lsp.log.jsonl`. Servers carry a high `oom_score_adj` so the kernel favors them over agents if it acts first.
- **The server crashes.** The broker records the crash and becomes dormant, ready for a later query to retry.

Each restart checks memory again; `RESTARTS` counts starts after a stop, not the first lazy start. The entry remains available while agents hold leases. Once the last agent leaves, a 60-second grace lets a replacement rejoin before the broker exits, even if dormant. Removing the checkout ends its broker too. To turn sharing off for future launches, remove the `[lsp.servers.<name>]` table; Claude's native tool is then no longer denied. Existing agents keep their leases until they exit.

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
