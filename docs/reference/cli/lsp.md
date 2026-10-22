# Shared language servers

`rimz lsp` queries shared language servers and attaches editors to them. Agent launches and editor attachment register servers for a checkout; requests start or restart dormant servers when memory permits. For lifecycle and memory accounting, see the [internals](../../internals/lsp.md).

## Queries

```sh
rimz lsp def MuxBackend
rimz lsp def Store::open QueryErr
rimz lsp refs GcReport
rimz lsp hover Store::open
rimz lsp impl MuxBackend
rimz lsp callers sweep_locked
rimz lsp callees Store::open
rimz lsp symbols crates/rimz/src/lib.rs
rimz lsp find MuxBackend --server rust --json
```

| Verb | Target | Result |
| --- | --- | --- |
| `def` | position, exact symbol name, or `path::Symbol` | definition locations |
| `refs` | position, exact symbol name, or `path::Symbol` | reference locations, inside the checkout by default |
| `hover` | position, exact symbol name, or `path::Symbol` | type and documentation |
| `impl` | position, exact symbol name, or `path::Symbol` | implementation locations, inside the checkout by default |
| `callers` | position, exact symbol name, or `path::Symbol` | incoming call hierarchy items, inside the checkout by default |
| `callees` | position, exact symbol name, or `path::Symbol` | outgoing call hierarchy items, inside the checkout by default |
| `symbols` | file path | document symbol outline |
| `find` | search string | workspace symbols matching the server's search, inside the checkout by default |
| `check` | one or more Markdown notes files | anchor verdicts and coverage summary per file |
| `show` | one or more file anchors or positions | numbered source blocks from disk |

`def`, `refs`, `hover`, `impl`, `callers`, and `callees` accept three TARGET forms: `path:line:col`, `path::Symbol`, or an exact symbol name. `symbols` accepts file paths only, and `find` accepts symbol-name search strings. Navigation positions use one-based line and column; omitting the column exits 1. When the outline names an item on that line, the error gives its kind, name, and complete position; otherwise it gives the format hint. `show` also accepts `path:line`. Paths are checkout-relative or absolute. `def` prints `path:line:col (start-end)` followed by the trimmed source snippet when available. The position points at the name; the span covers the full definition, with both server range lines converted to one-based lines. The span is omitted for single-line, empty, or absent full ranges and plain Location answers. `refs`, `impl`, `callers`, and `callees` group entries under a path line, then indent each `line:col` by two spaces, followed by two spaces and the source snippet or call item name when available. `refs` and `callers` place production entries first, then a `tests:` line and test entries; a mixed file appears in both groups. Within each group, checkout files precede external files, then sort by path and position (call items break ties by name). `impl` and `callees` keep that file order without separating tests. Source snippets are trimmed to 100 characters, ending with `…` when shortened. Outlines indent children, and hover prints markup without leading blank lines. Empty answers for resolved targets print `no results`.

JSON preserves the server's navigation answer: LocationLinks retain `targetUri`, `targetSelectionRange`, and `targetRange` rather than becoming Locations. A pasted `path:line:col (start-end)` inside a notes code span is a line-only anchor for `check`, covering `start-end`.

All five list verbs (`refs`, `impl`, `callers`, `callees`, `find`) show at most 50 locations by default, after scope filtering. File headers and the `tests:` boundary do not count toward the cap. Truncated lists start with `{total} {noun} (showing {shown})`, using `references`, `implementations`, `callers`, `callees`, or `symbols`, and end with `{rest} more; add --limit N or --all`, before any hidden-external footer. For `refs`, `callers`, and `find`, the header is `{total} {noun}, {tests} in tests (showing {shown}, tests last)` when every entry was classified without outline failures and some are tests, or `{total} {noun} (showing {shown}, tests last)` when classification stopped early or an outline failed. With no tests found in a complete, failure-free walk, or under `--no-tests`, the plain header applies. Untruncated lists have neither a count header nor a cap footer. `find` keeps one line per symbol. Candidate hints on exits 5 and 6 retain their fixed 20-line display regardless of list flags. JSON answers are always full and uncapped.

`refs`, `callers`, and `find` classify test code by a path component, a file stem, or a module in the server's document symbols that is named `tests`, `testkit`, or `test_support`, or ends in `_tests`. The rule is by name, not by `#[cfg(test)]`, so it includes the `testkit` modules compiled outside tests: the `testkit`-feature fixtures and the test-only counters that integration and performance tests read. Classification walks the existing output order only until the cap's worth of production entries is found, requesting at most one outline per visited non-test file. Classification outlines wait at most 5 s for server readiness. The first unavailable or malformed outline disables further outline requests; that entry and all remaining entries use only the path rule, and the header omits the test count because module counts are unknown. `--all` classifies every entry, which can require many outlines for a broad `find`.

`refs` and `callers` accept `--no-tests` to hide test-code locations before counting and limiting results. This classifies every entry, with the same outline-failure fallback. When test locations are hidden, a final footer reports `N test references hidden; drop --no-tests to show them` (`callers` for that verb), counting exactly the hidden entries, also after an outline failure. Without the flag, test locations remain included after production entries. `find` has no `--no-tests` flag. `--no-tests` conflicts with `--json`, which always preserves the complete server answer without classification.

Symbol names strip a trailing `()`, leading `crate`, `self`, and `super` segments, and generic arguments (`<…>`) from each segment. The last `::` segment is matched exactly against workspace symbols. The remaining segments must occur in order in the candidate's checkout-relative file path and container, but may skip intermediate segments: `launch_reminders::render`, `harness::launch_reminders::render`, and `rimz::harness::launch_reminders::render` can identify the same function. File extensions, `src` components, and final `mod`, `lib`, or `main` file stems do not contribute segments. Outside-checkout candidates contribute only their container. Wrong qualifiers never fall back to a bare-name match. A name may start with a file, as a notes anchor writes it (`crates/rimz/src/store/mod.rs::Store::open`): when the text before the first `::` has a file extension and no whitespace, the rest resolves as above among the symbols located in a file that path names, as `show` resolves an anchor's path: the checkout file at exactly that path when it exists, else every checkout file the path is a component-wise suffix of; `.` components are dropped, and a path outside the checkout or through `..` names nothing. The filter also applies to the member fallback's qualifier, and the file's extension picks the server. A named file without the symbol is not found, listing the exact-name candidates from every file; a path naming several files stays ambiguous when several distinct matches remain after exact-container preference.

Among matching symbols, items directly under the last written container take precedence. With a file head and no qualifier, top-level items take precedence; without an exact-container match, all subsequence matches still count. A bare name without a file or qualifier gets no preference. For example, `crates/rimz/src/agents/spending/session_roles.rs::load` names the top-level function, while `crates/rimz/src/agents/spending/session_roles.rs::tests::load` names the test helper.

When a qualified name has no workspace-symbol match, exact-name candidates are first narrowed to the named files or to files matching the leading module qualifiers, then enriched from their outlines and matched again against the same written chain and file filter. This lets a listed name such as `tui::tests::RecordingWriter::flush` resolve even when the workspace index omits the inline module and the workspace has many same-named symbols. Enrichment requires at most 20 files in the narrowed set and a hierarchical outline node for every candidate in that set. So for a name defined in more than 20 files, such as `new`, a chain through an inline module resolves only when it names the file's own module (`pacing::tests::FakeSource::new`) or starts with the file (`crates/rimz/src/sidebar_pane/pixel/pacing.rs::tests::FakeSource::new`); one that skips the module (`pixel::tests::FakeSource::new`, `tests::FakeSource::new`) is not found. If no match is recovered, the full exact-name set is tried under the same 20-file limit before the qualifier is resolved through workspace symbols and its outline's direct children are searched. This accepts fields (`Type::field`), variant fields (`Enum::Variant::field`), and inline-module items (`query::tests::foo`) when the qualifier is indexed and the server supplies a hierarchical outline. The qualifier itself gets no outline fallback. Several matching qualifiers are searched together, up to 20; distinct members remain ambiguous. A qualifier matching more than 20 symbols, such as a bare `tests`, skips the fallback and reports not found: add a path segment (`memory::tests::foo`). Existing workspace-symbol matches take precedence; qualify a field with its type (`Type::field`) to distinguish it from a same-named free function or another type's method. A field and a method on the same type still need positions to distinguish them.

Matches sharing one definition count as one symbol, including re-exports; distinct definitions list candidates instead of guessing. Rerun with a listed name or position; a listed name that comes back ambiguous again (the same item in several crates, several trait impls on one type) needs the position. Candidate lines in `find`, flat `symbols`, not-found, and ambiguous output use `{kind} {qualified name}  {path:line:col}`. Qualified names start after the first `src` component, include the container, and are accepted as input. Before resolving ambiguous matches, their names gain the outline's containers, including inline modules, when they span at most 20 files and every match can be located in a hierarchical outline. Otherwise names and matching keep the server's workspace-symbol information, without exact-container preference. `find` and not-found suggestions keep workspace-symbol names. Not-found considers exact-name candidates even when the qualifier matches none. Candidates sort by descending count of qualifier segments present in their path, then qualified name and position. Only the first 30 are resolved and collapsed, with at most four requests in flight; remaining candidates are counted but not resolved. If any remain unresolved, ambiguous matches stay ambiguous even when the resolved head shares one definition, and counts say `up to`: resolved groups plus unresolved candidates. Text shows at most 20 resolved candidates, followed by the remaining count; JSON includes every resolved group (at most 30).

`find` sorts case-insensitively by exact name, prefix, substring, then other matches, with qualified name and position breaking ties. Text puts production entries first, then a `tests:` line and test entries; within each group, checkout results precede external results, preserving this ranking within each scope. JSON keeps the original ranking.

All eight verbs require one or more targets. Targets run sequentially in argument order, including duplicates. With several targets, text labels each block `==> TARGET <==` using the argument as written, with one blank line between blocks. A target's failure does not omit later blocks. One target keeps the unlabelled output. Quote a search containing spaces to pass it as one `find` target. Query flags apply to every target:

| Flag | Effect |
| --- | --- |
| `--server <NAME>` | Select a configured server name. Otherwise a file's extension selects among checkout entries, or the sole entry is used. Ambiguous selection names this flag in the error. |
| `--json` | One target: print the structured LSP result on exit 0. Exits 5 and 6 print an object with `outcome` (`not-found` or `ambiguous`), the written `name`, and `candidates`, each containing the qualified `name`, kind word `kind`, and `position` (`path:line:col`). `total` is resolved groups plus unresolved candidates; `truncated` is true exactly when candidates remain unresolved. Both keys are always present. Several targets: print an array in argument order, with `target`, `outcome`, and `exit` on each object. Answers have `outcome: "answer"` and the raw result in `result`; not-found and ambiguous objects keep their other fields; errors have `outcome: "error"` and a message in `error`. |
| `--external` | `refs`, `impl`, `callers`, `callees`, `find`: include results outside the checkout. Default text output hides these results and ends with `<n> outside the checkout hidden; add --external to show them` when any were hidden. `def`, `hover`, and `symbols` are never filtered and reject this flag. `--json` is always unfiltered. |
| `--limit <N>` | List verbs only: show at most N locations (N ≥ 1; default 50). Conflicts with `--all` and `--json`. |
| `--no-tests` | `refs` and `callers` only: hide test-file and test-module locations before limiting. Conflicts with `--json`. |
| `--all` | List verbs only: show every location in the selected scope, without a cap. Conflicts with `--limit` and `--json`. `def`, `hover`, and `symbols` reject both list-limit flags. |

`find --external` shows everything the server's search returned; it does not widen the search itself. Rust-analyzer's default search covers the workspace and its path dependencies, not registry crates or the standard library.

The checkout comes from cwd or the global `--root`, using the most deeply enclosing registered checkout when present. Different worktrees have different servers even though they share a room. Queries wait up to 110 seconds for indexing, which stays under a 120-second agent shell timeout; there is no CLI wait-duration flag.

## Read source by anchor

```sh
rimz lsp show crates/rimz/src/lsp/check.rs::run crates/rimz/src/lsp/query.rs::render_with_source
rimz lsp show 'crates/rimz/src/lsp/check.rs::run ~20' --full
```

`show <ANCHOR>... [--full]` accepts positions (`path:line:col`) and the [check anchor grammar](#check-anchors-in-a-notes-file), including `path::Type::method`, line ranges, and repository qualifiers. Quote anchors containing spaces. A pasted `path:line:col (start-end)` reads the stated lines. Positions select the item whose name contains the position, otherwise the innermost enclosing item. `path:line` and `path:~line` select the innermost item covering any character of that line; when none does or no outline is available, they read the line itself. Use `path:N-N` to read only line N regardless of enclosing items. Symbol hints select every matching outline item overlapping the hint within three lines. With no hint, or no overlapping hits, every matching item prints in outline order, each as its own block. A type anchor can therefore print both its struct and its impl blocks.

Each block starts with checkout-relative `path:START-END`, then disk lines numbered with a right-aligned six-character line number and a tab. Blocks follow argument order, separated by one blank line. Items longer than 200 lines show their direct children's name-line snippets and spans instead, under `(outline: N lines; --full prints the body)`. `--full` prints the body; items without children, explicit line ranges, and bare lines with no enclosing item always print their bodies. An unsaved owning editor adds `(unsaved in editor)` after the header range; the body still comes from disk.

An end past the file prints through the last disk line, with the header stating the printed range; outline children whose name lines fall past the file are omitted. Failures print `<argument>  <status>  <detail>` and do not omit later arguments. Missing files or symbols, external anchors, a start of zero or past the file, and a start after the end exit 5; ambiguous paths exit 6; missing server configuration is `unchecked`, exit 3. Server errors retain query exit codes. Bare lines (`path:N` or `path:~N`) instead fall back to disk on missing configuration or any outline failure, with exit 0 for an existing line and exit 5 for zero or past EOF. Mixed failures use query severity (3, 4, 1, 6, 5, 0). An invalid argument exits 2 before any query. `show` has no `--json` flag.

## Check anchors in a notes file

`rimz lsp check <FILE>... [--json] [--fix [--hints]]` checks notes' file, symbol, and line references before hand-off. At least one FILE is required (otherwise exit 2). Each FILE is cwd-relative or absolute; anchors use the checkout enclosing cwd (or global `--root`). There is no `--server` flag. Without `--fix`, files are never changed. Flags apply to every file; a repeated FILE is checked again.

Only inline code spans count, never fenced blocks or prose. A path must have a filename extension holding at least one letter before the first colon, so `127.0.0.1:8080` is not an anchor. A path component holding `..` must be made only of dots, so `..Default::default()` is not an anchor while `../src/config.rs::f` is. Bare paths, bare symbols, and module names such as `config::Error` are ignored. Accepted anchors are `src/config.rs::Type::method`, `src/config.rs::Type.field`, and line-only forms such as `src/config.rs:42`, `src/config.rs:~42`, `src/config.rs:42:3`, or `src/config.rs:42-45`.

Symbol chains use either `::` or `.`. Generic arguments are removed; decorations end the symbol at whitespace, `(`, `{`, `[`, `,`, `~`, `#`, `!`, `=`, or `;`; trailing colons and dots are removed. Thus `config.rs::ConfigErr::Definition(DefinitionErr)` checks `ConfigErr::Definition`. A line hint inside the span after the symbol, or immediately following the span, accepts `(~42)`, `(42)`, `(~42-45)`, `(42-45)`, `~42`, `:~42`, `:42`, and `:42:3`. Symbol hints must overlap a matching outline range widened by three lines at each end. A single number more than three lines past the selected item's start, but not past its end, is `line-inside`: give that line its own `path:~N` anchor. The selected item is the one `--fix` would refresh; when a hint overlaps several items and none is selected, this rule does not apply. Ranges and positions are exempt. Every line of a line-only hint must exist in the file; it needs no server.

Anchors into other repositories prepend `owner/repo@ref:` to any accepted local form: `ghostty-org/ghostty@v1.3.1:src/terminal/kitty/graphics_storage.zig::ImageStorage.delete` or `kovidgoyal/kitty@c73326a:kitty/graphics.c:~120`. Hints following the span work too. Owner and repo are exactly two nonempty segments containing only ASCII letters, digits, `-`, `_`, or `.`; ref is nonempty and contains no whitespace or colon (slashes and dots are allowed). Host-qualified forms are not supported. A valid qualifier followed by a valid local anchor is always `external`, even when it names this repository: no checkout path or server is queried. Use unqualified anchors for this checkout; bare upstream paths still resolve against it and can be `missing-path`.

An exact checkout-relative path that is a regular file on disk resolves first, ignored or not; otherwise a path resolves by a unique component-boundary suffix among tracked and untracked, non-ignored files only. A deleted file whose removal is not yet staged is `missing-path`. A unique suffix resolves but is reported `short-path`; `--fix` completes it to the whole checkout-relative path for symbol and line-only anchors alike. A file whose whole path cannot be written as an anchor (a directory name holding whitespace, a colon, or a backtick) is cited by its short path: a unique suffix for it is `ok`, and `--fix` leaves it as written. Several suffix matches are `ambiguous-path`; lengthen the path instead of guessing, or, for a symbol anchor, use `--fix` when the symbol singles out one file. Symbols match an outline node and an ordered subsequence of its ancestors, ignoring generic arguments. A container's final whitespace-separated token also matches, so a method under `impl Type<T>` matches `Type::method`. Matches directly under the last written container take precedence; with only a file head, top-level items take precedence. Without such an exact match, every subsequence match still counts. This preference also applies to `show` and hint fixes, before any hint is considered: a hint on a nested namesake needs that item's container in the anchor. Multiple remaining nodes are accepted; a hint need only match one.

| Status | Meaning |
| --- | --- |
| `ok` | Exact path with a resolved symbol or valid line-only anchor. |
| `short-path` | Only a unique suffix matches a file whose whole path an anchor can spell; use the whole checkout-relative path. |
| `missing-path` | No checkout file matches. |
| `ambiguous-path` | Several files match the suffix. |
| `missing-symbol` | No outline node matches the chain. |
| `line-outside` | The hint misses every matching range, or lies outside the file. |
| `line-inside` | A single-number hint points more than three lines into the selected item; cite the line with its own `path:~N` anchor. |
| `unchecked` | No configured server covers the extension; verify it by hand. |
| `external` | Repo-qualified citation outside this checkout; recognised but not verified. |

Text prints one line per failing or unchecked anchor: `<notes>:<line>  <status>  <anchor as written, hint included>  <detail>`. There is no per-anchor line for `ok` or `external`. Details show candidate files, near-miss symbols and ranges, file length, or the missing server; `short-path` says `resolves to <whole checkout-relative path>`. For `line-inside`, the detail names the line, the item's kind, name and range, and the checkout-relative `path:~N` anchor to use instead. The last line is `<N> anchors in <notes>: <ok> ok, <failed> failed, <unchecked> unchecked, <external> external`, including zero counts. Exit 7 means at least one failure, including `short-path` and `line-inside`; exit 0 includes unchecked or external anchors and files with no anchors. An unreadable note exits 1. With one FILE, a file error prints only to stderr, with exit 1, 3 (server unavailable), or 4 (still indexing).

With several FILEs, stdout contains each file's block in argument order, without headers or blank separators. A file error prints one stdout line, `<FILE as given>  error  <message>`, and checking continues; per-file errors leave stderr empty. The call exits with the first applicable code in severity order 3, 4, 1, 7, 0. A checkout that cannot be listed fails the whole call with stderr only and exit 1, regardless of file count. After a server reports unavailable or indexing, the call reuses that failure rather than asking it again.

With several FILEs, `--json` prints a pretty-printed array in argument order. A checked file has the single-file object described below plus `exit` (0 or 7). A file error has exactly `notes` (FILE as given), `exit` (1, 3, or 4), and `error` (the message). One FILE still prints an object, without `exit`. Under `--fix`, each file's rewrite is atomic independently; a failed file does not prevent the others from being fixed and reported. A file is rewritten only after its anchors are judged, so a file that reports an error is unchanged.

A checked file's JSON object includes every anchor, including `ok` and `external`. Its base keys are `notes`, `checkout`, `anchors`, and `summary`. Each anchor has `line`, `text`, `status`, `path`, `symbol`, `hint`, `range`, `candidates`, `files`, and `detail`. Paths are checkout-relative or null; a `short-path` anchor carries its resolved whole path. Symbols are segment arrays or null for line-only anchors. Hints and ranges are inclusive one-based `[start, end]` pairs or null. Candidates have `name`, `kind`, and `range`; files lists ambiguous paths and is empty for `short-path`. External anchors have null `path` and `range`, empty `candidates` and `files`, and the qualifier as written in `detail` (for example, `ghostty-org/ghostty@v1.3.1`); `symbol` and `hint` come from the local part, and `text` preserves the whole span and any absorbed trailing hint. Summary always has `anchors`, `ok`, `failed`, `unchecked`, and `external` counts; `anchors` is the sum of the other four counts.

`--fix` updates existing symbol hints when exactly one checkout file matches and the hint overlaps exactly one matching outline item within three lines. A symbol with only one matching item is also fixed when its hint has drifted farther. It changes hint digits, including hints just after a code span, and short paths. An unqualified anchor in a code span that maps exactly to the notes source has a unique suffix completed to its whole checkout-relative path, for symbol and line-only anchors alike; this path completion needs no outline request or saved-editor check. When a short path names several checkout files, completion requires a symbol anchor, exactly one candidate outline matching the symbol chain, and no candidate with an unsaved owning editor. In either case the path text inside the span becomes the file's checkout-relative path, and a symbol anchor's hint is then refreshed, or with `--hints` inserted, as if the full path had been written. A hint never picks between several defining files. Single-line hints (`~N`, `:N`, `(N)`) use the full range's start, except those flagged `line-inside`, whose hint bytes stay unchanged even when their path is completed. Ranges (`~N-M`, `(N-M)`) use its start and end. Positions (`:N:C`) use the symbol's selection line and column, or the full range's start for flat outlines. Prefixes, parentheses, tildes, and dashes stay as written.

External anchors, missing paths, and ambiguous paths `--fix` cannot complete (no symbol, none or several defining files, or a candidate with an unsaved owning editor) are left alone. Unique short paths can still be completed on line-only anchors, anchors with missing or unchecked symbols, anchors without hints, and files with unsaved editor changes; their line hints are unchanged, and symbol hint updates still require a saved, uniquely resolved item. When several items match a symbol, a hint overlapping several is left alone, and a hint overlapping one is fixed to that item's range, an impl block included, unless it is `line-inside`. A hint overlapping none is fixed by the rule `--hints` inserts by: to a type's own range when the others are its impl blocks, otherwise left alone. Code spans whose normalized content cannot be mapped exactly to the notes source are also left alone. Single numbers at most three lines past the item's start or outside the item are still refreshed; deeper inside, through its end, they are left as written and flagged `line-inside`. The notes target is replaced atomically; a symlink stays a symlink. No changed path or hint means no write, including on a second run.

`--hints` is valid only with `--fix`; alone it is a usage error, exit 2. It also inserts a hint on every symbol anchor that has none, under the same conditions as a refresh: no repository qualifier, one checkout file, a configured server, no unsaved owning editor, and a code span that maps exactly to the notes source. The anchor must name one item after exact-container preference: either one outline item remains, or exactly one matching item's own name is the anchor's last segment and every other match is a container matched only by its final token. So `lib.rs::Type` names the type even when `impl Type` blocks also match, and gets the type's range. The hint is the item's full range as `(START-END)`, or `(N)` for a single-line item, inserted after one space following the span's closing backtick: `` `lib.rs::Type` `` becomes `` `lib.rs::Type` (1-3) ``. No other byte changes. An anchor that names several items keeps no hint and is reported; qualify it with a longer chain. Anchors that already have hints are refreshed exactly as without `--hints`.

Text prints completed paths and changed and inserted hints first as `<notes>:<line>  fixed  <before>  <after>`, one line per anchor; `before` is the anchor as written and `after` carries the full path and any hint change. With `--hints`, each anchor that names several items follows as `<notes>:<line>  ambiguous-symbol  <anchor>  <detail>`, the detail listing every matching item with its kind and range. Then come the normal verdicts for the updated notes, then `<K> anchors left without a hint: several items match` when K is at least one (`1 anchor left ...` for one), then the summary. `--fix --json` adds `fixes`, an array of objects with `line`, `before`, and `after`; without `--fix` this key is absent. `--hints` also adds `ambiguous`, an array of objects with `line`, `text`, and `candidates` in the anchor shape, empty when every anchor names one item; without `--hints` this key is absent. An ambiguous anchor still counts as `ok`. Exit codes reflect the updated notes, so fixing every failure exits 0.

## Attach

```sh
rimz lsp attach --server rust
rimz lsp attach --server rust --root /path/to/checkout --stdio
rimz lsp attach --version
```

`attach [--server NAME] [--stdio] [--version]` bridges an editor's stdin and stdout to one shared server. Stdio is the only transport; `--stdio` is optional. Except for `--version`, stdout contains only Content-Length-framed LSP messages; diagnostics and admission waits go to stderr. The first input frame must be `initialize`, otherwise the command fails with `expected initialize`. `--version` prints `rimz lsp attach <version>` and exits 0 without reading input or loading configuration.

The global `--root` takes precedence over the editor's `initialize.rootUri`, then its first workspace folder, then cwd. Editor roots must be file URIs. The most deeply enclosing live registered checkout is used when present; otherwise workspace resolution supplies the worktree root. `--server NAME` selects a configured server. Without it, exactly one configured server must have a matching root marker; zero or several matches fail with an error naming `--server`.

Attachment joins an existing broker or admits a new one using the configured trust, executable, and memory checks. Creating a required server can wait up to `wait-timeout`, printing queue position and remaining time to stderr every five seconds. A stopped entry is retried for five seconds before refusal. An optional server's memory refusal happens inside the LSP session, as failed requests and an editor message, not as a launch-time exit 3.

Editor EOF exits 0. Broker refusal (including an older broker without attachment support), connection failure, or the broker closing first exits 3 with `language server <name> for <root> is <reason>` on stderr; after a terminal stop the reason is `stopped: <stop reason>`, such as `stopped: checkout removed`. A server that did not start, or a required-admission queue timeout, also exits 3, with the admission diagnostic as written. A read error or malformed frame on the editor's side exits 1 with `editor stream failed: <error>`. Disconnecting releases the editor's lease; it does not stop a server still leased by other clients. See [editor setup](../../guide/lsp.md#use-it-from-your-editor) for supported Rust configurations.

## Shim

```sh
rimz lsp shim --server rust
rimz lsp shim --server rust --dir /path/to/bin
```

`shim --server NAME [--dir DIR]` writes executable `DIR/rimz-lsp-NAME` and prints its path. DIR defaults to `~/.local/bin` and is created if absent. The shell script invokes the resolved RimZ executable with `lsp attach --server NAME`, forwarding all arguments. It replaces an existing file only when it starts with RimZ's shell-script header and attachment-shim marker; other files are refused. It does not change editor settings or start a server. Remove the generated file to uninstall it.

## List and stop

```sh
rimz lsp list
rimz lsp list --all
rimz lsp list --json
rimz lsp stop --server rust
rimz lsp stop /path/to/checkout --server rust
rimz lsp stop --all
```

`list` covers the current room: entries whose recorded repository is the room's repository, or the repository of the current checkout. The room is the pane's pinned room, else the one resolved from cwd, and the global `--root` overrides both. Every worktree of the room's repository counts. By default it shows running servers and alarms (`dormant: crashed` or `dormant: memory pressure`). `--all` also shows room entries that hold no memory. Doctor is the machine-wide view. Dead entries are swept machine-wide before selecting the room. An entry whose broker died while a lease holder lives is kept on disk but not listed; the next query in its checkout brings it back. An entry this build cannot read, such as one a newer RimZ wrote, is not listed.

The first line reports the room's running count and total RSS + swap (no total when zero run), host free memory after reserve with available RAM, nonzero free swap and reserve, and the estimated memory a start needs per server name. Needs is the largest estimate across that server's room entries, learned from the largest of the last five matching project/server/settings peaks, falling back to each entry's last admitted estimate. A failed host reading omits the free-memory clause; no room entries omits needs. Other repositories with running servers follow, largest total first, with their total, running count, and a `rimz --root … lsp list` command. Paths under your home use `~/`.

Text columns are STATE, CHECKOUT, SERVER, RSS, SWAP, TOTAL, PEAK, LAST, and LEASES; RSS through LEASES are right-aligned. Running servers sort first, then alarms, then the rest; ties sort by checkout and server name. Hidden rows fold into `N not running: count reason, …; add --all to list them`, ordered by descending count then label. `--all` has no fold line. An empty room reads `no language server registered in this room` instead of a table.

STATE pairs a glyph with a word, preserved without color: `✓ ready`, `▸ starting`, `▸ indexing`, `· not started`, `· dormant: idle`, `· dormant: recycled`, `✗ dormant: crashed`, `· dormant: memory pressure`, or `· stopped: released`. A never-started server shows `not started`; other dormant entries show `dormant: <reason>`, and terminal shutdown shows `stopped: <reason>`.

RSS is current resident process-tree memory; SWAP is the tree's swapped-out memory (zero off Linux); TOTAL is RSS + SWAP from the same reading as the summary. These three show `-` when not running. PEAK keeps the recorded peak on dormant rows and includes the live resident tree peak when running; zero reads `-`. All four use decimal byte units. The recorded peak sums the kernel's per-process resident high-water marks over the tree, with the five-second RSS sample as a floor; swap is not included, so TOTAL can exceed PEAK. A running entry whose server process is gone or whose pid has been reused reads zero in RSS, SWAP, and TOTAL. LAST is compact time since the last query (`45s ago`, `11m ago`, `1h ago`, `3d ago`), or `-` with no query. Request and restart counts live in `status`.

`--json` returns every room entry, with or without `--all`, keeping registry order rather than the human table's sort. The entry shape is unchanged: process identities, nonce, state, estimate, timestamps, counters, and leases; peak RSS remains in KiB as recorded, including while dormant.

`stop [CHECKOUT]` defaults to the current checkout (or global `--root`). `--server <NAME>` selects one server when several exist. `--all` stops every room entry, the same membership as `list`, and conflicts with both CHECKOUT and `--server`. Dead entries are swept first; a failure stopping one entry does not prevent attempts on the others. It has no `--json` flag. A hand stop frees server memory and leaves `dormant: stopped by hand`; the next query can restart it. Stopping a `dormant: crashed` or `dormant: memory pressure` entry acknowledges the recorded reason: it becomes `stopped by hand` and the dormant-since time is kept. Stopping any other dormant entry leaves its reason unchanged.

Output reflects the state read before stopping: a running row prints `stopped SERVER (CHECKOUT)`, an alarm prints `acknowledged SERVER (CHECKOUT), was LABEL`, and other rows print nothing. If no line was printed and nothing failed, `nothing to stop: …` explains that no server is registered for the checkout, or names the selected server and its no-memory state. With `--all` it says no server is registered in this room, or no server in this room holds memory. Successful no-ops exit 0. With `--server`, known names are the checkout project's configured servers (including untrusted definitions) plus names registered anywhere on the machine. An unknown name exits 2 with the sorted, deduplicated `configured:` names, or `none configured`; a missing CHECKOUT exits 2 naming the supplied path. Several checkout rows without `--server` exit 1 naming the root and servers. Name validation reads machine and project configuration best-effort: if it cannot be read, the name check is skipped and stopping still uses registered rows. A known or unchecked name with no row takes the same nothing-to-stop path.

## Status

```sh
rimz lsp status
rimz lsp status /path/to/checkout --server rust
rimz lsp status --server rust --json
```

`status [CHECKOUT]` inspects one server without starting it. CHECKOUT defaults to cwd or the global `--root`; the most deeply enclosing registered checkout is used. Dead entries are swept first. `--server <NAME>` selects a server and is required when several entries match (exit 1 naming the root and servers). It uses the same best-effort configured/registered name check and `configured:` error as `stop`: an unknown name or missing CHECKOUT exits 2; no matching entry exits 3, including a configured name with no row. Configuration read failures skip name validation rather than blocking inspection.

Text shows checkout, server, kind, state and its age, next-query behavior, memory (running rows only, RSS + swap), peak, last query (or `never`), requests, restarts, leases, check-on-save when known, retained crash and stderr, then broker and server pids. Restarts exclude the first lazy start. State age uses the dormant or terminal stop stamp, ready stamp when ready, and start stamp while starting or indexing. Each attached editor shows its pid, client name (or `unnamed`), and seconds since attachment, followed by indented buffer paths relative to the checkout where possible. `(owner)` marks the holder whose buffer the server uses; `(unsaved in editor)` means changed since open or save, not a comparison with disk. A buffer already unsaved when opened is not detected, and editing back to disk text does not clear the marker.

`next query` says ready servers answer at once, starting or indexing servers wait up to 110 seconds then exit 4, and terminal stopped rows exit 3 with their reason. Dormant rows show needs and the free-memory fragment: a fitting start starts the server, a short start exits 4 unless idle servers can be stopped to make room. If host memory cannot be read, it says the start depends on memory admission. Needs uses the same history rule as `list`. The current machine reserve policy supplies the free-memory reading; an older broker can retain its spawn-time policy until it respawns.

When a crash cause is retained, text adds `last crash` with the exit code or signal (or error when neither is known) and age, plus `stderr` with up to five trailing lines. The next server start clears the cause. Crashed query errors direct you to this status command.

`--json` returns the server's current registry entry, including `attached`: each editor has `pid`, optional `name`, `since_ms`, and `open`; each open buffer has `uri`, `owner`, and `dirty`. Buffer text is not included. The [editor model](../../internals/lsp.md#editors) defines ownership and marker semantics.

`last_crash` is null or an object with `at_ms`, nullable `exit_code` and `signal`, `stderr_tail` (at most 8 KiB), and nullable `error`. Exit status is absent when the server never spawned or was still alive when a broker-side error ended its lifetime.

## Exit codes

With several query targets, exit 0 means every target answered; otherwise the most severe outcome wins, in the order 3 > 4 > 1 > 7 > 6 > 5 > 0. Per-target errors appear on stdout as `error: <message>` in their block (or in their JSON object), with empty stderr. A server returning 3 or 4, including a memory refusal, is not queried again during the call; its later targets reuse that error. An unknown query `--server` exits 2 before any target runs, naming the configured servers and any additional servers registered at this checkout. Configuration/registry loading, rendering, and write failures still abort the call. The stderr descriptions below apply to single-target queries.

| Exit | Meaning |
| --- | --- |
| 0 | Answered, including `no results` for a resolved symbol; successful list, status, stop, shim, or attach version probe; attach ended on editor EOF; check has no failing anchors (unchecked or external anchors are allowed). |
| 1 | Command failed, including a file extension no configured server handles, multiple matching servers, a column-less position, or protocol errors; details on stderr. An uncovered extension fails only that target and lists each configured server's extensions. Queries, checks, and source reads with multiple matching servers name the checkout and servers and suggest `--server NAME`. |
| 2 | Invalid command line, flag, or argument. |
| 3 | Queries/status: no server for the checkout, or a server stopped for a reason other than memory pressure, with a reason and grep fallback on stderr. Attach: admission timeout/refusal, broker refusal, or broker connection closing/failing, with the diagnostic described above. |
| 4 | Not ready: still indexing after the wait bound (with elapsed time), memory admission refused, or memory pressure stopped the server during the query. Use grep for this question and retry on the next. For `check`, rerun once a minute, at most ten times; then report the anchors as unchecked. |
| 5 | Symbol name not found; candidates with that last segment are listed on stdout. |
| 6 | Symbol name ambiguous; candidates are listed on stdout, each with an accepted name. |
| 7 | Anchor check found failures; verdicts on stdout, nothing on stderr. |

Exits 5 and 6 write nothing to stderr. Not-found text is `not found: {written name}`, followed by `; {N} other symbol(s) named {last segment}:` and candidate lines when candidates exist. Ambiguous text starts `ambiguous: {N} symbols named {written name}; rerun with one of these names or a position`. When candidates remain unresolved, `{N}` becomes `up to {N}` in either header. The footer `{rest} more; narrow with a qualifier or use find` counts resolved groups beyond the displayed 20 plus all unresolved candidates.

An absent configured server reports `not running`. When the checkout's broker died while an agent or editor still holds its lease, the query first respawns it, waiting up to 5 s, and then answers as for any dormant server; with no live holder it reports `not running` or `crashed` and starts nothing. A broker-side memory refusal reports `language server <server> for <root> not started: memory short, needs <estimate>, <free> free (<available> available + <swap> swap, <reserve> reserved); use grep for this question and retry on the next`, leaves the broker dormant, and exits 4. The figures come from the refusal itself; older or malformed replies still exit 4 without figures. Free memory is available RAM plus free swap minus the reserve, saturating at zero; zero swap omits the swap clause. Doctor's refusal row and required-launch waits and timeouts use the same free-memory fragment. A query stopped by memory pressure reports `language server <server> for <root> stopped: memory pressure; use grep for this question and retry on the next`, also exit 4. Diagnostics do not determine these results. Other stop-reason errors use `(stopped: <reason>)`, including `idle`, `evicted`, and `team done`; an ordinary query to a dormant server requests a restart instead.

Other failures follow the [CLI error conventions](../cli.md). With one target, `--json` does not change the stderr form of exits 3 and 4.

## Configuration

No servers are configured by default. Put server entries in `~/.rimz/config.toml` or trusted project `.rimz/config.toml`. A project entry replaces the same-named machine entry whole, rather than merging its fields. Project memory-policy keys are refused with a fix pointing to the per-machine config.

```toml
[lsp] # machine config only
reserve-percent = 10
reserve-min = "8G"
idle-timeout = "10m"
recycle-factor = 2
recycle-min = "2G"

[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, hover = { dropGlue = { enable = false } }, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
policy = "optional"
wait-timeout = "10m"
memory-estimate = "8G"
```

The Rust options disable checking that would contend with agent builds, include functions in workspace search (rust-analyzer defaults to types only), raise its 128-result search cap, and drop the `needs Drop` section from hover output. RimZ prints hover markdown as the server sends it, and rust-analyzer has no setting for its `Implements notable traits` section, so that section still appears. Without that tuning, symbol-name queries can miss functions; a position query does not need workspace search.

| Machine `[lsp]` field | Default | Meaning |
| --- | --- | --- |
| `reserve-percent` | `10` | Percentage of total RAM reserved at admission and maintained by the watchdog; free swap counts toward the free figure it is compared with. |
| `reserve-min` | `"8G"` | Minimum reserve; the larger of this and the percentage wins. |
| `idle-timeout` | `"10m"` | Stop a ready server after this long since readiness or the last query, whichever is later, with no query in flight. Units: `s`, `m`, `h`; checked every five seconds. |
| `recycle-factor` | `2` | Non-negative integer multiplier of the first-ready RSS + swap footprint. `0` disables recycling; `1` is accepted. |
| `recycle-min` | `"2G"` | Minimum recycle ceiling; the larger of this and factor times the first-ready footprint wins. `"0"` lets the factor alone set the ceiling. |

A ready server whose RSS + swap strictly exceeds that ceiling becomes `dormant: recycled` once no agent query or editor request is waiting on the server. The check runs every five seconds and also applies with editors attached. The next query or editor request restarts it through memory admission and can wait for re-indexing. Without a non-zero first-ready footprint, that lifetime does not recycle. Off Linux, swap reads zero.

| `[lsp.servers.<name>]` field | Default | Meaning |
| --- | --- | --- |
| `command` | required | Nonempty executable argv, not a shell command. |
| `kind` | detected | `rust-analyzer`, `ty`, `pyright`, `basedpyright`, `ruff`, or `generic`. An explicit value wins; otherwise the first command token whose basename before its first `@` is `rust-analyzer`, `ty`, `pyright-langserver`, `basedpyright-langserver`, or `ruff` selects the kind, falling back to `generic`. This includes version-pinned tokens such as `ty@0.0.84` and `ruff@0.6.0`. Unknown values refuse config load. |
| `extensions` | required | Nonempty file extensions without dots, used for selection and saved-file watching. |
| `root-markers` | required | Nonempty checkout-relative paths; any existing marker enables this server for the checkout. |
| `init-options` | absent | TOML table of initialization options passed to the server. |
| `editor-check-on-save` | `false` | Rust-analyzer only. Override `checkOnSave` to `true` while any editor is attached, restoring `init-options` after the last detach. Other kinds refuse admission when enabled. Joins project trust; enabling it uses a separate learned memory-history key. |
| `policy` | `"optional"` | `optional` starts on the first query; `required` admits and starts a new server before panes open. Both restart lazily after a stop. |
| `wait-timeout` | `"10m"` | Required-server queue deadline, with duration units such as `20s` or `10m`. |
| `memory-estimate` | `"8G"` | Admission estimate before matching peak history exists. |

`init-options` is sent verbatim as `initializationOptions`, except for the opt-in `editor-check-on-save` override. Ty reads flat top-level keys: `logLevel`, `logFile`, `untrustedWorkspace`, `diagnosticMode`, `experimental`, `showSyntaxErrors`, `configuration`, `configurationFile`, `disableLanguageServices`, `inlayHints`, `completions`, and `pythonExtension`. Do not wrap them in a `ty` table. For Pyright it is a section-keyed object: `python` and `pyright`; for basedpyright: `python`, `basedpyright`, and legacy `pyright`. Analysis belongs under `python.analysis` for Pyright and `basedpyright.analysis` for basedpyright. Ruff accepts `settings` or `globalSettings` at the top level, including Ruff's per-workspace `settings` array shape. Other top-level keys on these four kinds refuse admission with the accepted keys in the error. Rust-analyzer and generic accept any table.

Configuration requests walk the requested dotted section from this object; only rust-analyzer strips its own section prefix and returns the whole object for `rust-analyzer`. Ty's requested section `ty` therefore answers `null`, which ty treats as defaults; its flat initialization options take precedence. `kind` does not change the trust hash or learned memory-history key. Status reports the resolved kind; legacy entries may have no kind.

With `editor-check-on-save` enabled, status also shows `check on save` as `on (editor attached)` or `off (no editor attached)`. JSON includes `editor_check_on_save`, the live override state, or `null` when the option is disabled or the entry predates it. Attachment counts from the successful handshake, not from opening a file. Dormant servers apply the current override on their next initialization.

Server names use ASCII letters, digits, `-`, or `_`; `reserve-percent` ranges from 0 to 100. Sizes use decimal units (`8G` is 8 GB); `8GiB` is binary. Invalid machine-wide policy (`idle-timeout`, `reserve-percent`, `reserve-min`, or `recycle-min`) refuses launch only when a required server's root markers match the checkout; otherwise it warns once and skips all servers. Memory admission requires available memory plus free swap to cover the server estimate plus the reserve, with each capped by cgroup headroom, but not running servers' future memory use. Query-time admission can evict older idle servers ([admission rules](../../internals/lsp.md#admission)). Required waits print position and remaining time in the launcher's terminal, not the sidebar. Joining an existing broker, even dormant, skips eager admission; later stops and restarts treat required and optional alike.

Project server names, commands, and initialization options join the [trust hash](../../internals/harness/trust.md). Untrusted project declarations warn and stay inert regardless of policy; same-named machine entries stay in effect. An optional server's configuration error, including a missing executable, warns with the fix and skips that server; a required server's configuration error refuses launch. Each warning prints and appends a diagnostic refusal once per launch. Editor attach still refuses on an optional server's configuration error, with exit 3. `rimz doctor` shows servers and the last refusal or queue timeout.

Configured Claude launches deny the native `LSP` tool even while the shared server is dormant or query-time admission is refused, avoiding private servers outside the budget. `rimz agents validate` warns when a profile declares `LSP` and machine servers exist. OpenCode and Grok have no verified native-server suppression here. Existing agents are not reconfigured by editing this table; new launches read it.
