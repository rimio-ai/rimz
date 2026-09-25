# Shared LSP

Your agents already navigate code semantically when they can. Claude's `LSP` tool starts a language server for the session and asks it for definitions, references, and call hierarchies; an agent without one falls back to grep and reads three files to find the one that matters. The server is the right idea: a `def` answer is exact where a grep answer is a guess, and it saves the tokens the guess would have cost.

The problem is who owns the server. A harness starts one per agent session and holds it until the session ends. `rust-analyzer` on this repository reaches 4.4 GB after its first index and 6.3 GB once references have been searched, so a three-seat team plus its subagents holds three or more copies of the same index over the same checkout, whether or not anyone is querying. And nothing on your machine says no: the next launch starts another one, and the kernel decides which process dies.

`rimz lsp` puts one server per checkout in front of every agent working in it, started at launch and only when memory allows. Agents query it read-only through the `rimz lsp` command; RimZ keeps its view of the disk current as files are saved, holds it alive while agents hold it, and stops it when the last one leaves or when memory runs short. The feature is off until you configure a server, and an agent without one works as agents work today.

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
2. If a server for this (checkout, name) is already running, the launch joins it and skips to step 5.
3. It checks memory under one machine-wide lock, so two launches cannot both pass on the same free gigabytes. The server may start when free memory, less what running servers are still expected to grow by, less this server's estimate, leaves a reserve of the larger of 10% of RAM and 8 GB. The estimate is the peak this server reached on this project before, learned from `~/.rimz/lsp-history.jsonl`, or the configured `memory-estimate` (8 GB) the first time.
4. It starts the server through a hidden broker process, one per server, which owns the server's stdin and stdout and answers queries over a Unix socket. The broker's socket and registry entry live under RimZ's runtime directory, so they do not survive a reboot.
5. Each agent's wrapper takes a lease on the server, and the launch goes on to open panes.

When the memory check fails, the launcher's terminal says so on one line and the launch proceeds without that server; the agents use grep:

```text
rimz: language server rust not started: needs 8 GB, 5.2 GB free after the 9.6 GB reserve (held: /held rust 6.3 GB); agents use grep
```

That line names who holds the memory, so you can `rimz lsp stop` a server a finished team left behind and relaunch.

A server never starts mid-session. Agents learn at launch whether one serves their checkout, and nothing about their tools changes afterward, so adding a server to the config reaches the next launch, never a running one. Subagents are the one launch that skips the check: a child works in its parent's checkout and joins its parent's server when there is one.

Each worktree is its own checkout, so agents in `-w feat-x` get a separate server from agents on the main tree, each indexing the files it will actually read.

## What your agents see

An agent launched with a server finds one paragraph in the system reminder RimZ appends, naming the servers and pointing at the `rimz-lsp` skill from your [skill library](./configuration.md#skills). The skill teaches the verbs below, the two exit codes that mean "use grep", and nothing else, so an agent spends no turn learning the tool. Without a server there is no paragraph.

An agent's query gets one of three answers. Exit 0 is an answer, and `no results` is a real one. Exit 3 means there is no server for this checkout, with the reason on one line: never started, memory short at launch, stopped by hand, stopped under memory pressure, checkout removed, or crashed. Exit 4 means the server is still indexing after the query's 30-second wait, with the elapsed time. The skill tells the agent to grep on 3 and 4 and carry on, so a server that stops mid-task costs the agent one failed call, not a stalled turn. Agents are never messaged about a stop.

Claude's native `LSP` tool is switched off whenever a server is configured, even for a launch whose server was refused for memory, so no private index starts outside the budget. `rimz agents validate` warns when a profile still lists `LSP` in its tools. OpenCode and Grok have no verified switch for their own servers, so with those two the shared server is an addition, not a replacement.

## Ask the server yourself

The same command works from your shell, in the checkout, and it is the fastest way to check what the agents are getting. A target is a position (`path:line:col`, one-based) or an exact symbol name; `Type::method` names a method in its container.

```sh
rimz lsp def MuxBackend                      # where it is defined
rimz lsp refs GcReport                       # every reference, with the source line
rimz lsp hover crates/rimz/src/lib.rs:1:1    # type and documentation at a position
rimz lsp impl MuxBackend                     # implementations of a trait
rimz lsp callers sweep                       # incoming calls
rimz lsp callees sweep                       # outgoing calls
rimz lsp symbols crates/rimz/src/lib.rs      # a file's outline
rimz lsp find Mux                            # workspace symbol search
```

A name that matches several symbols lists the candidates with positions instead of guessing; rerun with one of them. Add `--json` for the raw LSP result, and `--server <name>` when a checkout has more than one server and the file's extension does not settle it. The checkout is the one enclosing your current directory (or `--root`). Everything about targets, output, and flags is in the [reference](../reference/cli/lsp.md#queries).

The server answers from the disk, kept current by watching saved files, not from anyone's editor buffer. An unsaved change is invisible to it, which is what you want when several agents share one view.

## See what is running, and stop it

`rimz lsp list` shows every shared server on the machine, whichever room started it: its checkout, name, state (`starting`, `indexing`, `ready`, or `stopped` with the reason), current and peak memory, how many queries it has answered and how long since the last, and how many agents hold a lease. `rimz doctor` carries the same table under `LSP`, plus the last memory refusal, so when an agent says it is on grep you can see why in one place.

A server ends in one of five ways, and a stopped server is never restarted while the agents that leased it are still running:

- **Its last agent leaves.** Leases are held by the agents' wrapper processes and released when they exit; the broker also reaps a dead owner within five seconds. The last release starts a 60-second grace so a restart can rejoin, then the server exits. There is no idle timeout: a team that queries once a day keeps its server as long as it lives.
- **You stop it.** `rimz lsp stop --server rust` from the checkout, or `rimz lsp stop --all` for the machine. The agents keep running; their next query exits 3 with `stopped by hand` and they fall back to grep.
- **Memory runs short.** Every broker samples free memory every five seconds. When it drops below 5% of RAM, one broker is elected to stop servers until it recovers, taking first the servers no agent has queried, longest-running first, then the least recently queried. The victim's whole process group goes, including any `cargo` children, and a record of the kill lands in `~/.rimz/logs/lsp.log.jsonl`. Servers also carry a high `oom_score_adj`, so if the kernel acts first it takes a server before an agent.
- **The checkout goes.** Removing a worktree, by `rimz worktree remove` or a sweep, stops its servers, and the broker checks that its root still exists every five seconds as a backstop.
- **The server crashes.** The broker records it and answers queries with the reason.

Whichever way it ended, the entry stays visible in `rimz lsp list` as `stopped: <reason>` until the last lease is released, and closing those agents and relaunching gets a fresh server. To turn sharing off, remove the `[lsp.servers.<name>]` table; the next launch starts nothing and Claude's native tool is no longer denied.

## Make a server a precondition

By default a server is enrichment: `policy = "optional"` means a memory refusal costs the agents their navigation and nothing else. For a task where grep is not good enough, set `policy = "required"` on the entry. A required launch that fails the memory check waits before opening panes, first come first served among required launches, and the launcher's terminal prints its queue position and remaining time each time they change. At `wait-timeout` (10 minutes by default) the launch fails with the memory needed, the memory free, and who holds the rest. Optional launches never queue behind required ones.

`required` gates the start only. A required server killed later for memory degrades exactly as an optional one does, because RimZ never stops agents to keep a server alive.

Two machine-only keys size the budget: `reserve-percent` and `reserve-min` set the memory a launch must leave untouched, and `kill-floor-percent` sets where the watchdog begins stopping servers. Their defaults are in the [reference](../reference/cli/lsp.md#configuration); lower the reserve only on a machine whose agents leave headroom you can see in `rimz lsp list`.

## See also

- [Shared LSP reference](../reference/cli/lsp.md): every verb, flag, exit code, and configuration field with its default.
- [Subagents](./subagents.md): children join their parent's server, and why a native per-child index was the case that made this worth building.
- [Worktrees](./worktrees.md): each worktree is a separate checkout with its own server.
- [Security and trust](./security.md#project-trust): how a project's server entry enters the trust hash and what an untrusted clone may launch.
- [Language server internals](../internals/lsp.md): the admission formula, the broker, the watchdog, and the learned-cost record.
