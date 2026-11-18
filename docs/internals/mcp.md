# The MCP server

`rimz mcp serve` gives a Claude or Codex agent RimZ's own commands as MCP tools. Its user-facing contract (the tools, their arguments, the protocol methods) is the [`rimz mcp` reference](../reference/cli/mcp.md). This page covers how it is built and what it must keep true.

## A thin front over one-shots

The server runs no RimZ logic itself. Each tool call becomes one `rimz` child process, a one-shot, with an argv built from the call's arguments, and the result is that child's output. So a tool behaves exactly as its command does, under the agent's own environment, and a server started before `rimz update` drives the new binary, since `crates/rimz/src/proc/mod.rs::rimz_exe` resolves the room's link at each spawn.

Because the one-shot does the work, the server's modules open no store, load no config, and resolve no room. `cargo xtask invariants` holds that boundary with a textual guard over `crates/rimz/src/mcp/` and `crates/rimz/src/cli/mcp.rs` (`ensure_thin_mcp_server` in `xtask/src/invariants.rs`). The process stays small: an idle server holds the parked wrapper's 8 MiB private-memory budget (`mcp_server::an_idle_server_stays_inside_its_memory_budget` in the integration suite).

## The catalog

`crates/rimz/src/mcp/catalog.rs::TOOLS` is the one table of tools. Each `Tool` carries its capability, description, the command words it runs, its argument schema, and the function that turns arguments into an `Invocation` (argv, working directory, stdin bytes). Everything that lists tools or capabilities reads it: `tools/list`, `--only` parsing through `crates/rimz/src/mcp/catalog.rs::Capability`, `crates/rimz/src/mcp/catalog.rs::serve_args` for the arguments that start a server with chosen capabilities, the reference page's table (`mcp::catalog::tests::the_reference_table_lists_every_tool_under_its_capability`), and the routine-command check (`agents::conformance::every_mcp_tool_command_is_a_routine_command`), which keeps every command a tool runs under an entry of `ROUTINE_RIMZ_PREFIXES`.

Each tool's arguments are a private serde struct with `deny_unknown_fields`, and its schema is derived from that struct with schemars, inlined into one object with `null` dropped from optional fields' types and enums. Deserializing the call's arguments is the shape check. A struct's `ToolArgs::refusal` adds a refusal only where the command would misread arguments rather than refuse them: a `wait` with no trigger (`rimz wait` bare lists the waits and exits 0), a `self_tune` value with no setting (it would slide into the setting slot), a `notes_add` with both a body and a file (the file wins and the body is dropped), and a meta key containing `=` (the CLI splits at the first one). Other rules that span flags (which flag fits which verb, one trigger per wait) stay the command's, so the server cannot drift from the CLI.

The argv builder joins every flag value as `--flag=value` and puts every positional after `--`. An agent-supplied string that starts with `-` therefore arrives as data: a note titled `--force`, an LSP target `--help`.

## The serve loop

`crates/rimz/src/mcp/server.rs::serve` reads newline-delimited JSON-RPC from stdin on the main thread and answers every request inline except `tools/call`, which runs on a thread of its own so calls overlap. Stdout carries protocol lines only, each written whole under the stdout lock.

Calls in flight sit in a map keyed by a sequence number, each with the request id, the `Child` once it has spawned, and a cancelled flag. The reading thread lists a call before it starts the call's thread, so a `notifications/cancelled` read after the call, or the server's end, always finds it: a call cancelled before its spawn kills its one-shot as soon as the spawn returns (`Calls::attach`), and a closed map lists no new call, so nothing spawns once the server is ending. The worker reads the child's stdout to its end (stderr on a side thread, a `notes_add` body written on another), then removes its entry and reaps the child. A listed child is never reaped, so its pid cannot be reused while `notifications/cancelled` or the server's end may still kill it. A call whose entry was cancelled or killed by the server's end gets no reply.

stdin's end, SIGINT, and SIGTERM close the map, which kills every listed one-shot, and the server exits 0.

Under `--parent-env`, which Codex's registration passes, `serve` reads the parent's environment once at start (`crate::proc::environ` on the parent pid, Linux and macOS) and keeps each variable the server's own environment lacks; every one-shot gets those beneath its inherited environment. Codex starts a server with only the names its `env_vars` lists, and `crates/rimz/src/harness/launch.rs::spawn_env_names` collects them before the login shell's startup files run, so the parent's environment is where the rest live. A failed read logs one warning and adds nothing (`crates/rimz/src/mcp/server.rs::inherited_from`). A provider ends a server by signalling its process group; a watcher that `rimz wait` detached leads its own group and survives (`mcp_server::a_wait_armed_through_the_server_fires_after_its_group_is_terminated`).

The Codex app-server transport in `crates/rimz/src/agents/adapters/codex/app_server/transport.rs` frames the same protocol, but it sits in `agents`, a later layer than `mcp` in `refactor-target.toml`, and keeps responses only, so the server writes its own few lines of framing.
