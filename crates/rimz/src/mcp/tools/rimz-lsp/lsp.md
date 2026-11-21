Ask the checkout's language server what code means. It resolves a name as the compiler does, through imports, re-exports, and macros, down to a binding's inferred type. Grep also matches comments, strings, and same-named methods of other types, so keep grep for text: a log message, a string literal, a config key.

Verbs: `def` (definition and line span), `hover` (type and docs), `refs` (every use site), `callers` (functions whose bodies call it), `callees` (functions it calls), `impl` (implementations of a trait or interface), `symbols` (a file's outline), `find` (workspace symbols by fuzzy name), `show` (numbered source), `check` (the anchors in a Markdown file).

Pass all of a question's targets in one call. A server still starting or indexing holds each call up to two minutes, a wait a loop of calls pays every time.

A target of `def`, `hover`, `refs`, `callers`, `callees`, or `impl` is one of four forms (every name and position a result prints is one):
- a name as you would write it in code, `Store::open`; any segment but the last may be dropped, from the front or the middle (`rimz::Store::open`);
- an anchor, `path::Symbol`;
- `path:line:name`, a grep hit's `path:line` plus one identifier on that line; a name that occurs several times there fails, listing each occurrence as a position;
- a position, `path:line:col`, 1-based, the column anywhere on the name's last segment.
The last two reach a local binding, or a dependency's item where your code declares or uses it. A path is absolute or relative to the checkout root.

`show` also takes `path:line` and a range `path:N-N`. A position or a `path:line` prints the item enclosing it, the way to read the function around a `refs` hit. An item over 200 lines prints as an outline unless `full`.

`refs`, `callers`, `callees`, `impl`, and `find` list results inside your checkout; `external` adds dependencies and the standard library (on `find`, only path dependencies). `def` and `hover` follow a position into dependencies without it. `refs` on a trait method also lists every implementing method; `refs` on one implementing method lists neither, so before a signature change run `refs` on the trait method. A list stops at 50 results: pass `all` on the first call when the question is about every site, or `limit` for another cap. `refs`, `callers`, and `find` list tests last, under a `tests:` line, so the cap cuts them first; `no_tests` drops them from `refs` and `callers`.

`check` reads the anchors in a Markdown file's code spans: a symbol anchor, `path::Symbol`, passes when that file holds the symbol, and a line anchor, `path:~N`, when the file has that line. Any number right after a symbol anchor (`(N)`, `~N`, `:N`) is read as its line-span hint, so put other numbers elsewhere in the sentence. `fix` completes a short path and refreshes stale hints; `fix` with `hints` also writes a hint after every anchor that names one item. A pass means the item exists, not that it holds the code you describe.

Each target gets its own block, and a failed call's last line is the most severe `exit code N`: rerun only the targets whose block failed. `no results` is an answer. 1: do what the message says. 3: no server; use grep for the rest of the task. 4: not ready; the message says to rerun or use grep. 3 or 4 on `check`: report the anchors unchecked. 5: no such name; rerun with a listed candidate. 6: several matches; rerun with the one you mean. 7: `check` flagged anchors; fix those you agree are wrong. `server` picks the language server where several serve the checkout; `check` ignores it.
