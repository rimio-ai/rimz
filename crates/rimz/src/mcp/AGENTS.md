# MCP server

Local contract for `crates/rimz/src/mcp/`: the protocol loop, the tool catalog and schemas, and the wording files under `tools/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). [mcp.md](../../../../docs/internals/mcp.md) owns the architecture, the provider limits, and the reasons behind the rules below, above all in [The wording](../../../../docs/internals/mcp.md#the-wording) and [Where long material goes](../../../../docs/internals/mcp.md#where-long-material-goes).

## Writing tools

- **One home per fact.** A field's doc comment holds what the field is, its format, its default, and the trigger or other field it needs. A tool's description holds only what spans its fields (when to reach for the tool, how to pick among its choices, what is particular to its answer) and the words a tool search would match, and never restates another tool's description. What every start shares goes in `STARTED_SENTENCE` in `wording.rs`. A description that points to a guide never condenses it.
- **Safety rules stay in the description.** A sentence that tells the caller when to do or not do something stays in the tool's description, even where a guide covers its topic. Before you trim a description, list each such sentence and find each one in the trimmed text.
- **Flat choices.** A choice of one among several is a required enum plus a required value, never an object of optional keys with a refusal that counts them.
- **One spelling.** The same value takes the same field name and format in every tool that takes it.
- **No always-refused field.** A schema lists no field that every call is refused for.
- **Measure a wording change.** Run `rimz mcp tools` before and after any change to what a tool lists, the types behind its schema included, and diff the two `rimz mcp tools --json` listings: each field `description` the change drops names a fact to find in the new text.
