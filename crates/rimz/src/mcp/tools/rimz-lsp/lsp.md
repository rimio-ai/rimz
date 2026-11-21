Ask the checkout's shared language server what code means. It resolves a name as the compiler does, through imports, re-exports, and macros, where grep also matches comments, strings, and same-named methods of other types; keep grep for text (a log message, a string literal, a config key).

Verbs: `def` (definition and line span), `hover` (type and docs), `refs` (every use site), `callers` (functions whose bodies call it), `callees` (functions it calls), `impl` (implementations of a trait), `symbols` (a file's outline), `find` (workspace symbols by fuzzy name), `show` (numbered source), `check` (the code anchors in Markdown files).

Every verb takes several targets: pass all of a question's targets in one call. A server still starting or indexing holds each call up to two minutes, a wait a loop of calls pays every time.

A target of `def`, `hover`, `refs`, `callers`, `callees`, or `impl` is one of:
- a name as written in code, `Parser::parse`; any segment but the last may be dropped, from the front or the middle, and a wrong segment fails;
- an anchor, `path::Symbol`;
- `path:line:name`, a grep hit's `path:line` plus one identifier on that line; a name that occurs several times there fails, listing each occurrence as a position;
- a position, `path:line:col`, 1-based, the column anywhere on the name's last segment.
The last two reach a local binding, or an item of a dependency or the standard library, where your code declares or uses it. Every name and position a result prints is itself a valid target. A path is absolute or relative to the checkout root.

`show` prints numbered source for a name, an anchor, `path:line:col`, `path:line`, or a line range `path:N-N`. A position or a `path:line` prints the item enclosing it, the way to read the function around a `refs` hit. An item over 200 lines prints as an outline unless `full`. One target `show` cannot parse fails the whole call.

`refs` on a trait method also lists every implementing method; `refs` on one implementing method lists neither the trait method nor the other implementations, so before a signature change run `refs` on the trait method.

A list stops at 50 results: pass `all` on the first call when the question is about every site, or `limit` for another cap. `refs`, `callers`, and `find` list test files and modules last, under a `tests:` line, so the cap cuts them first; `no_tests` drops them from `refs` and `callers`. `external` adds dependencies and the standard library to `refs`, `callers`, `callees`, `impl`, and `find` (on `find`, only path dependencies); `def` and `hover` follow a position into dependencies without it.

`check` reads the anchors in a Markdown file's code spans: a symbol anchor, `path::Symbol`, passes when that file holds the symbol, and a line anchor, `path:~N`, when the file has that line. Any number right after a symbol anchor (`(N)`, `~N`, `:N`) is read as its line-span hint, so put other numbers elsewhere in the sentence. `fix` completes a short path and refreshes stale hints; `fix` with `hints` also writes a hint after every anchor that names one item. An `owner/repo@ref:` prefix marks an external anchor, left unchecked. A pass means the item exists, not that it holds the code you describe; verdicts are a recommendation.

Otherwise each target of a multi-target call gets its own block and outcome, and the exit code is the most severe: rerun only the targets whose block failed. With no language server, use grep. `server` picks one of several language servers for the checkout; `check` ignores it.
