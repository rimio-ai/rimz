# MCP server

Local contract for `crates/rimz/src/mcp/`: the protocol loop, the tool catalog and schemas, and the wording files under `tools/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Architecture, provider limits, and the reasons behind them live in [mcp.md](../../../../docs/internals/mcp.md), especially [The wording](../../../../docs/internals/mcp.md#the-wording) and [Where long material goes](../../../../docs/internals/mcp.md#where-long-material-goes).

## Writing tools

- **One home per fact.** A field doc comment holds what the field is, its format, its default, and which trigger or other field it needs; a tool description holds only what spans fields (when to reach for the tool, how to pick among its choices, what is particular to its answer) plus tool-search terms (worktree, pull request, cron, signal, condition); `STARTED_SENTENCE` in `wording.rs` holds what every start shares; a guide under `guides/` holds craft and reference material few calls need, and a description pointing to one never condenses it.
- **Flat choices.** A choice of one among several is a required enum plus a required value, never an object of optional keys with a refusal that counts them ([The wording](../../../../docs/internals/mcp.md#the-wording)).
- **One spelling.** The same value uses the same field name and format in every tool that takes it.
- **Reuse the carried tool.** A tool that carries another tool's call takes the tool's name and its arguments and reuses that tool's parser; it never copies the fields.
- **No always-refused field.** A schema lists no field that every call is refused for.
- **No cross-tool duplication.** A tool description does not restate what another tool's description says.
- **Keep safety rules in the description.** Every safety or misfire rule stays in the tool description, never only in a guide. Before a trim, list each sentence that tells the caller when to do or not do something, and find each one in the trimmed text.
- **Measure before and after.** Run `rimz mcp tools` before and after a wording change.
