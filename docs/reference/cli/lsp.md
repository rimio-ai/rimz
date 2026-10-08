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

Positions are `path:line:col`, with one-based line and column; omitting the column is an error. Paths are checkout-relative or absolute. `def` prints `path:line:col (start-end)` followed by the trimmed source snippet when available. The position points at the name; the span covers the full definition, with both server range lines converted to one-based lines. The span is omitted for single-line, empty, or absent full ranges and plain Location answers. `refs`, `impl`, `callers`, and `callees` group entries under a path line, then indent each `line:col` by two spaces, followed by two spaces and the source snippet or call item name when available. Checkout files precede external files, then sort by path and position (call items break ties by name). Source snippets are trimmed to 100 characters, ending with `…` when shortened. Outlines indent children, and hover prints markup. Empty answers for resolved targets print `no results`.

JSON preserves the server's navigation answer: LocationLinks retain `targetUri`, `targetSelectionRange`, and `targetRange` rather than becoming Locations. A pasted `path:line:col (start-end)` inside a notes code span is a line-only anchor for `check`, covering `start-end`.

All five list verbs (`refs`, `impl`, `callers`, `callees`, `find`) show at most 50 locations by default, after scope filtering. File headers do not count toward the cap. Truncated lists start with `{total} {noun} (showing {shown})`, using `references`, `implementations`, `callers`, `callees`, or `symbols`, and end with `{rest} more; add --limit N or --all`, before any hidden-external footer. Untruncated lists have neither a count header nor a cap footer. `find` keeps one line per symbol. Candidate hints on exits 5 and 6 retain their fixed 20-line display regardless of list flags. JSON answers are always full and uncapped.

`refs` and `callers` accept `--no-tests` to hide test-code locations before counting and limiting results. Test code means a path component, a file stem, or a module in the server's document symbols that is named `tests`, `testkit`, or `test_support`, or ends in `_tests`. The rule is by name, not by `#[cfg(test)]`, so it also hides the `testkit` modules compiled outside tests: the `testkit`-feature fixtures and the test-only counters that integration and performance tests read. When test locations are hidden, a final footer reports `N test references hidden; drop --no-tests to show them` (`callers` for that verb). Without the flag, test locations remain included. `--no-tests` conflicts with `--json`, which always preserves the complete server answer.

Symbol names strip a trailing `()`, leading `crate`, `self`, and `super` segments, and generic arguments (`<…>`) from each segment. The last `::` segment is matched exactly against workspace symbols. The remaining segments must occur in order in the candidate's checkout-relative file path and container, but may skip intermediate segments: `launch_reminders::render`, `harness::launch_reminders::render`, and `rimz::harness::launch_reminders::render` can identify the same function. File extensions, `src` components, and final `mod`, `lib`, or `main` file stems do not contribute segments. Outside-checkout candidates contribute only their container. Wrong qualifiers never fall back to a bare-name match. A name may start with a file, as a notes anchor writes it (`crates/rimz/src/store/mod.rs::Store::open`): when the text before the first `::` has a file extension and no whitespace, the rest resolves as above among the symbols located in a file that path names, as `show` resolves an anchor's path: the checkout file at exactly that path when it exists, else every checkout file the path is a component-wise suffix of; `.` components are dropped, and a path outside the checkout or through `..` names nothing. The filter also applies to the member fallback's qualifier, and the file's extension picks the server. A named file without the symbol is not found, listing the exact-name candidates from every file; a path naming several files that each hold it is ambiguous.

When a qualified name has no workspace-symbol match, its qualifier is resolved through workspace symbols and its outline's direct children are searched. This accepts fields (`Type::field`), variant fields (`Enum::Variant::field`), and inline-module items (`query::tests::foo`) when the qualifier is indexed and the server supplies a hierarchical outline. The qualifier itself gets no outline fallback. Several matching qualifiers are searched together, up to 20; distinct members remain ambiguous. A qualifier matching more than 20 symbols, such as a bare `tests`, skips the fallback and reports not found: add a path segment (`memory::tests::foo`). Existing workspace-symbol matches take precedence, so a method of the same name wins over a field; use the field's position in that case.

Matches sharing one definition count as one symbol, including re-exports; distinct definitions list candidates instead of guessing. Rerun with a listed name or position; a listed name that comes back ambiguous again (the same item in several crates, several trait impls on one type) needs the position. Candidate lines in `find`, flat `symbols`, not-found, and ambiguous output use `{kind} {qualified name}  {path:line:col}`. Qualified names start after the first `src` component, include the container, and are accepted as input. Not-found considers exact-name candidates even when the qualifier matches none. Candidates sort by descending count of qualifier segments present in their path, then qualified name and position. Only the first 30 are resolved and collapsed, with at most four requests in flight; remaining candidates are counted but not resolved. If any remain unresolved, ambiguous matches stay ambiguous even when the resolved head shares one definition, and counts say `up to`: resolved groups plus unresolved candidates. Text shows at most 20 resolved candidates, followed by the remaining count; JSON includes every resolved group (at most 30).

`find` sorts case-insensitively by exact name, prefix, substring, then other matches, with qualified name and position breaking ties. Text places checkout results before external results, preserving this ranking within each scope. JSON keeps the original ranking.

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

`show <ANCHOR>... [--full]` accepts positions (`path:line:col`) and the [check anchor grammar](#check-anchors-in-a-notes-file), including `path::Type::method`, line ranges, and repository qualifiers. Quote anchors containing spaces. A pasted `path:line:col (start-end)` reads the stated lines. Positions select the item whose name contains the position, otherwise the innermost enclosing item. Symbol hints select every matching outline item overlapping the hint within three lines. With no hint, or no overlapping hits, every matching item prints in outline order, each as its own block. A type anchor can therefore print both its struct and its impl blocks.

Each block starts with checkout-relative `path:START-END`, then disk lines numbered with a right-aligned six-character line number and a tab. Blocks follow argument order, separated by one blank line. Items longer than 200 lines show their direct children's name-line snippets and spans instead, under `(outline: N lines; --full prints the body)`. `--full` prints the body; items without children and line-only anchors always print their bodies. An unsaved owning editor adds `(unsaved in editor)` after the header range; the body still comes from disk.

An end past the file prints through the last disk line, with the header stating the printed range; outline children whose name lines fall past the file are omitted. Failures print `<argument>  <status>  <detail>` and do not omit later arguments. Missing files or symbols, external anchors, a start of zero or past the file, and a start after the end exit 5; ambiguous paths exit 6; missing server configuration is `unchecked`, exit 3. Server errors retain query exit codes. Mixed failures use query severity (3, 4, 1, 6, 5, 0). An invalid argument exits 2 before any query. `show` has no `--json` flag.

## Check anchors in a notes file

`rimz lsp check <FILE>... [--json] [--fix [--hints]]` checks notes' file, symbol, and line references before hand-off. At least one FILE is required (otherwise exit 2). Each FILE is cwd-relative or absolute; anchors use the checkout enclosing cwd (or global `--root`). There is no `--server` flag. Without `--fix`, files are never changed. Flags apply to every file; a repeated FILE is checked again.

Only inline code spans count, never fenced blocks or prose. A path must have a filename extension holding at least one letter before the first colon, so `127.0.0.1:8080` is not an anchor. A path component holding `..` must be made only of dots, so `..Default::default()` is not an anchor while `../src/config.rs::f` is. Bare paths, bare symbols, and module names such as `config::Error` are ignored. Accepted anchors are `src/config.rs::Type::method`, `src/config.rs::Type.field`, and line-only forms such as `src/config.rs:42`, `src/config.rs:~42`, `src/config.rs:42:3`, or `src/config.rs:42-45`.

Symbol chains use either `::` or `.`. Generic arguments are removed; decorations end the symbol at whitespace, `(`, `{`, `[`, `,`, `~`, `#`, `!`, `=`, or `;`; trailing colons and dots are removed. Thus `config.rs::ConfigErr::Definition(DefinitionErr)` checks `ConfigErr::Definition`. A line hint inside the span after the symbol, or immediately following the span, accepts `(~42)`, `(42)`, `(~42-45)`, `(42-45)`, `~42`, `:~42`, `:42`, and `:42:3`. Symbol hints must overlap a matching outline range widened by three lines at each end. Every line of a line-only hint must exist in the file; it needs no server.

Anchors into other repositories prepend `owner/repo@ref:` to any accepted local form: `ghostty-org/ghostty@v1.3.1:src/terminal/kitty/graphics_storage.zig::ImageStorage.delete` or `kovidgoyal/kitty@c73326a:kitty/graphics.c:~120`. Hints following the span work too. Owner and repo are exactly two nonempty segments containing only ASCII letters, digits, `-`, `_`, or `.`; ref is nonempty and contains no whitespace or colon (slashes and dots are allowed). Host-qualified forms are not supported. A valid qualifier followed by a valid local anchor is always `external`, even when it names this repository: no checkout path or server is queried. Use unqualified anchors for this checkout; bare upstream paths still resolve against it and can be `missing-path`.

An exact checkout-relative path that is a regular file on disk resolves first, ignored or not; otherwise a path resolves by a unique component-boundary suffix among tracked and untracked, non-ignored files only. A deleted file whose removal is not yet staged is `missing-path`. Lengthen an ambiguous suffix instead of guessing; for a symbol anchor, `--fix` lengthens it when the symbol singles out one file. Symbols match an outline node and an ordered subsequence of its ancestors, ignoring generic arguments. A container's final whitespace-separated token also matches, so a method under `impl Type<T>` matches `Type::method`. Multiple matching nodes are accepted; a hint need only match one.

| Status | Meaning |
| --- | --- |
| `ok` | Resolved path and symbol or valid line-only anchor. |
| `missing-path` | No checkout file matches. |
| `ambiguous-path` | Several files match the suffix. |
| `missing-symbol` | No outline node matches the chain. |
| `line-outside` | The hint misses every matching range, or lies outside the file. |
| `unchecked` | No configured server covers the extension; verify it by hand. |
| `external` | Repo-qualified citation outside this checkout; recognised but not verified. |

Text prints one line per failing or unchecked anchor: `<notes>:<line>  <status>  <anchor as written, hint included>  <detail>`. There is no per-anchor line for `ok` or `external`. Details show candidate files, near-miss symbols and ranges, file length, or the missing server. The last line is `<N> anchors in <notes>: <ok> ok, <failed> failed, <unchecked> unchecked, <external> external`, including zero counts. Exit 7 means at least one failure; exit 0 includes unchecked or external anchors and files with no anchors. An unreadable note exits 1. With one FILE, a file error prints only to stderr, with exit 1, 3 (server unavailable), or 4 (still indexing).

With several FILEs, stdout contains each file's block in argument order, without headers or blank separators. A file error prints one stdout line, `<FILE as given>  error  <message>`, and checking continues; per-file errors leave stderr empty. The call exits with the first applicable code in severity order 3, 4, 1, 7, 0. A checkout that cannot be listed fails the whole call with stderr only and exit 1, regardless of file count. After a server reports unavailable or indexing, the call reuses that failure rather than asking it again.

With several FILEs, `--json` prints a pretty-printed array in argument order. A checked file has the single-file object described below plus `exit` (0 or 7). A file error has exactly `notes` (FILE as given), `exit` (1, 3, or 4), and `error` (the message). One FILE still prints an object, without `exit`. Under `--fix`, each file's rewrite is atomic independently; a failed file does not prevent the others from being fixed and reported. A file is rewritten only after its anchors are judged, so a file that reports an error is unchanged.

A checked file's JSON object includes every anchor, including `ok` and `external`. Its base keys are `notes`, `checkout`, `anchors`, and `summary`. Each anchor has `line`, `text`, `status`, `path`, `symbol`, `hint`, `range`, `candidates`, `files`, and `detail`. Paths are checkout-relative or null; symbols are segment arrays or null for line-only anchors. Hints and ranges are inclusive one-based `[start, end]` pairs or null. Candidates have `name`, `kind`, and `range`; files lists ambiguous paths. External anchors have null `path` and `range`, empty `candidates` and `files`, and the qualifier as written in `detail` (for example, `ghostty-org/ghostty@v1.3.1`); `symbol` and `hint` come from the local part, and `text` preserves the whole span and any absorbed trailing hint. Summary always has `anchors`, `ok`, `failed`, `unchecked`, and `external` counts; `anchors` is the sum of the other four counts.

`--fix` updates existing symbol hints when exactly one checkout file matches and the hint overlaps exactly one matching outline item within three lines. A symbol with only one matching item is also fixed when its hint has drifted farther. It changes hint digits, including hints just after a code span, and short paths. A short path is completed when an unqualified symbol anchor in a code span that maps exactly to the notes source names several checkout files, exactly one of them has an outline item matching the symbol chain, and none has an unsaved owning editor: the path text inside the span becomes that file's checkout-relative path, and the anchor's hint is then refreshed, or with `--hints` inserted, as if the full path had been written. A hint never picks between several defining files. Single-line hints (`~N`, `:N`, `(N)`) use the full range's start; ranges (`~N-M`, `(N-M)`) use its start and end. Positions (`:N:C`) use the symbol's selection line and column, or the full range's start for flat outlines. Prefixes, parentheses, tildes, and dashes stay as written.

Line-only anchors, external anchors, missing paths, ambiguous paths `--fix` cannot complete (no symbol, none or several defining files, or a candidate with an unsaved owning editor), missing symbols, unchecked files, hints based on an unsaved owning editor, and, without `--hints`, anchors without hints are left alone. When several items match a symbol, a hint overlapping several is left alone, and a hint overlapping one is fixed to that item's range, an impl block included. A hint overlapping none is fixed by the rule `--hints` inserts by: to a type's own range when the others are its impl blocks, otherwise left alone. Code spans whose normalized content cannot be mapped exactly to the notes source are also left alone. Hints within the three-line checking tolerance are still refreshed. The notes target is replaced atomically; a symlink stays a symlink. No changed path or hint means no write, including on a second run.

`--hints` is valid only with `--fix`; alone it is a usage error, exit 2. It also inserts a hint on every symbol anchor that has none, under the same conditions as a refresh: no repository qualifier, one checkout file, a configured server, no unsaved owning editor, and a code span that maps exactly to the notes source. The anchor must name one item: either one outline item matches, or exactly one matching item's own name is the anchor's last segment and every other match is a container matched only by its final token. So `lib.rs::Type` names the type even when `impl Type` blocks also match, and gets the type's range. The hint is the item's full range as `(START-END)`, or `(N)` for a single-line item, inserted after one space following the span's closing backtick: `` `lib.rs::Type` `` becomes `` `lib.rs::Type` (1-3) ``. No other byte changes. An anchor that names several items keeps no hint and is reported; qualify it with a longer chain. Anchors that already have hints are refreshed exactly as without `--hints`.

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

`list` covers the current room: entries whose recorded repository is the room's repository, or the repository of the current checkout. The room is the pane's pinned room, else the one resolved from cwd, and the global `--root` overrides both. Every worktree of the room's repository counts. `--all` covers the whole machine, including entries registered without a repository. Text columns are STATE, CHECKOUT, SERVER, RSS, PEAK, REQUESTS, LAST, RESTARTS, and LEASES; RSS through LEASES are right-aligned. CHECKOUT uses `~/` for paths under your home. Running servers sort first, then servers needing attention, then the rest; ties sort by checkout and server name. Dead entries are swept before listing.

STATE pairs a glyph with a word, preserved without color: `✓ ready`, `▸ starting`, `▸ indexing`, `· not started`, `· dormant: idle`, `✗ dormant: crashed`, `! dormant: memory pressure`, or `· stopped: released`. A never-started server shows `not started`; other dormant entries show `dormant: <reason>`, and terminal shutdown shows `stopped: <reason>`.

RSS is current process-tree memory; PEAK is the larger of the recorded peak and the live tree peak, including current tree memory, displayed in decimal byte units. The recorded peak sums the kernel's per-process high-water marks over the tree, with the five-second RSS sample as a floor. RSS and PEAK show `-` when not running. LAST is compact time since the last query (`45s ago`, `11m ago`, `1h ago`, `3d ago`), or `-` with no query. RESTARTS counts starts after a stop, excluding the first lazy start.

`--json` keeps registry order rather than the human table's sort and returns registry entries, including process identities, nonce, state, estimate, timestamps, counters, and leases; peak RSS remains in KiB as recorded, including while dormant.

`stop [CHECKOUT]` defaults to the current checkout (or global `--root`). `--server <NAME>` selects one server when several exist. `--all` stops every machine entry and conflicts with both CHECKOUT and `--server`. Dead entries are swept first; a failure stopping one entry does not prevent attempts on the others. Stop prints one acknowledgment line per entry; no matching entries produce no lines. It has no `--json` flag. A hand stop frees server memory and leaves `dormant: stopped by hand`; the next query can restart it. Stopping a `dormant: crashed` or `dormant: memory pressure` entry acknowledges the alarm: the reason becomes `stopped by hand` and the dormant-since time is kept. Stopping any other dormant entry leaves its reason unchanged.

## Status

```sh
rimz lsp status
rimz lsp status /path/to/checkout --server rust
rimz lsp status --server rust --json
```

`status [CHECKOUT]` inspects one server without starting it. CHECKOUT defaults to cwd or the global `--root`; the most deeply enclosing registered checkout is used. Dead entries are swept first. `--server <NAME>` selects a server and is required when several entries match. No matching entry exits 3.

Text shows checkout, server name, state (the word `list` prints, so a never-started server reads `not started`), broker and server pids, request count, and lease count. Each attached editor shows its pid, client name (or `unnamed`), and seconds since attachment, followed by indented buffer paths relative to the checkout where possible. `(owner)` marks the holder whose buffer the server uses; `(unsaved in editor)` means changed since open or save, not a comparison with disk. A buffer already unsaved when opened is not detected, and editing back to disk text does not clear the marker.

When a crash cause is retained, text adds `last crash` with the exit code or signal (or error when neither is known) and age, plus `stderr` with up to five trailing lines. The next server start clears the cause. Crashed query errors direct you to this status command.

`--json` returns the server's current registry entry, including `attached`: each editor has `pid`, optional `name`, `since_ms`, and `open`; each open buffer has `uri`, `owner`, and `dirty`. Buffer text is not included. The [editor model](../../internals/lsp.md#editors) defines ownership and marker semantics.

`last_crash` is null or an object with `at_ms`, nullable `exit_code` and `signal`, `stderr_tail` (at most 8 KiB), and nullable `error`. Exit status is absent when the server never spawned or was still alive when a broker-side error ended its lifetime.

## Exit codes

With several query targets, exit 0 means every target answered; otherwise the most severe outcome wins, in the order 3 > 4 > 1 > 6 > 5. Per-target errors appear on stdout as `error: <message>` in their block (or in their JSON object), with empty stderr. A server returning 3 or 4 is not queried again during the call; its later targets reuse that error. Configuration/registry loading, rendering, and write failures still abort the call. The stderr descriptions below apply to single-target queries.

| Exit | Meaning |
| --- | --- |
| 0 | Answered, including `no results` for a resolved symbol; successful list, status, stop, shim, or attach version probe; attach ended on editor EOF; check has no failing anchors (unchecked or external anchors are allowed). |
| 1 | Command failed, including server-selection or protocol errors; details on stderr. |
| 2 | Invalid command line, flag, or argument. |
| 3 | Queries/status: no server for the checkout, memory admission refused, or terminal shutdown, with a reason and grep fallback on stderr. Attach: admission timeout/refusal, broker refusal, or broker connection closing/failing, with the diagnostic described above. |
| 4 | Still indexing after the wait bound. The stderr line gives elapsed time. |
| 5 | Symbol name not found; candidates with that last segment are listed on stdout. |
| 6 | Symbol name ambiguous; candidates are listed on stdout, each with an accepted name. |
| 7 | Anchor check found failures; verdicts on stdout, nothing on stderr. |

Exits 5 and 6 write nothing to stderr. Not-found text is `not found: {written name}`, followed by `; {N} other symbol(s) named {last segment}:` and candidate lines when candidates exist. Ambiguous text starts `ambiguous: {N} symbols named {written name}; rerun with one of these names or a position`. When candidates remain unresolved, `{N}` becomes `up to {N}` in either header. The footer `{rest} more; narrow with a qualifier or use find` counts resolved groups beyond the displayed 20 plus all unresolved candidates.

An absent configured server reports `not running`. A broker-side memory refusal reports `no language server for <root> (not started: memory short); use grep`, leaves the broker dormant, and is retried by a later query. Diagnostics do not determine this result. Stop-reason errors use `(stopped: <reason>)`, including `idle`, `evicted`, and `team done`; an ordinary query to a dormant server requests a restart instead.

Other failures follow the [CLI error conventions](../cli.md). With one target, `--json` does not change the stderr form of exits 3 and 4.

## Configuration

No servers are configured by default. Put server entries in `~/.rimz/config.toml` or trusted project `.rimz/config.toml`. A project entry replaces the same-named machine entry whole, rather than merging its fields. Project memory-policy keys are refused with a fix pointing to the per-machine config.

```toml
[lsp] # machine config only
reserve-percent = 10
reserve-min = "8G"
kill-floor-percent = 5
idle-timeout = "10m"

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
| `reserve-percent` | `10` | Percentage of total memory reserved at admission. |
| `reserve-min` | `"8G"` | Minimum reserve; the larger of this and the percentage wins. |
| `kill-floor-percent` | `5` | Below this percentage of available memory relative to total, the watchdog stops shared servers. |
| `idle-timeout` | `"10m"` | Stop a ready server after this long since readiness or the last query, whichever is later, with no query in flight. Units: `s`, `m`, `h`; checked every five seconds. |

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

Server names use ASCII letters, digits, `-`, or `_`; percentages range from 0 to 100. Sizes use decimal units (`8G` is 8 GB); `8GiB` is binary. Invalid machine-wide policy (`idle-timeout`, percentages, or `reserve-min`) refuses launch only when a required server's root markers match the checkout; otherwise it warns once and skips all servers. Memory admission accounts for running servers' committed growth and cgroup headroom; dormant entries reserve nothing. Query-time admission can evict older idle servers ([admission rules](../../internals/lsp.md#admission)). Required waits print position and remaining time in the launcher's terminal, not the sidebar. Joining an existing broker, even dormant, skips eager admission; later stops and restarts treat required and optional alike.

Project server names, commands, and initialization options join the [trust hash](../../internals/harness/trust.md). Untrusted project declarations warn and stay inert regardless of policy; same-named machine entries stay in effect. An optional server's configuration error, including a missing executable, warns with the fix and skips that server; a required server's configuration error refuses launch. Each warning prints and appends a diagnostic refusal once per launch. Editor attach still refuses on an optional server's configuration error, with exit 3. `rimz doctor` shows servers and the last refusal or queue timeout.

Configured Claude launches deny the native `LSP` tool even while the shared server is dormant or query-time admission is refused, avoiding private servers outside the budget. `rimz agents validate` warns when a profile declares `LSP` and machine servers exist. OpenCode and Grok have no verified native-server suppression here. Existing agents are not reconfigured by editing this table; new launches read it.
