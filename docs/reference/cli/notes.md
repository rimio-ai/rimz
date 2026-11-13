# Notes CLI

`rimz notes` saves Markdown notes in one store under the RimZ home and lists them back. A note is any text an agent or a person wants to keep past the session that wrote it: a handoff, a reflection, a build log. It needs no room, no checkout, and no running multiplexer, so a plain shell, a headless check, and an agent in any room all write to the same store.

```sh
rimz notes add <kind> <title> [--file <path>] [--fence] [--meta <key>=<value>]...
rimz notes list [--filter <query>]... [-n <count>] [-q | --json]
```

Both commands are on the [routine list](../agent-support.md) an installed agent runs without asking.

## The store

Notes live under `notes/` in the RimZ home, one folder per kind and one per day inside it:

```text
~/.rimz/notes/<kind>/<YYYYMMDD>/<title>.md
```

`rimz paths` prints the store's location as its `notes` row ([Paths CLI](./paths.md)). The day folder is the local date of the moment the note was written, the same instant its `written` header key records.

RimZ writes each note once and never edits, moves, renames, or deletes one. Neither `rimz gc` nor `rimz uninstall` touches the store. Space is freed by deleting notes or whole day folders by hand; `list` reads whatever is left.

Every folder RimZ creates in the store is mode `0700` and every note is mode `0600`, so only the owning user can read them.

## Names

The kind names the folder and the title names the file, both by the same slug rule: each run of ASCII letters and digits is lowercased, runs are joined by one `-`, and every other character is dropped. The slug is then cut to 48 characters, and a `-` left at the end of the cut is dropped too. A kind or title with no ASCII letter or digit (`..`, `/`, `---`) is refused, so no name can reach outside its day folder.

A title whose slug a note in the same day folder already holds gets the next free suffix after the cut: `ship-notes.md`, then `ship-notes-2.md`, `ship-notes-3.md`. Two notes written at once under one title both land, each under its own name, and neither replaces the other.

The title is free text and one shell word, so quote a title with spaces. Put `--` before the kind when the title could read as one of the command's flags: `rimz notes add -- diary '--fence or not'`.

## Add

`add` reads the body on stdin, or from the file `--file` names, and prints the absolute path of the published note on stdout, alone on one line.

| Flag | Meaning |
| --- | --- |
| `--file <PATH>` | Read the body from this file instead of stdin. Stdin is then ignored. |
| `--fence` | Wrap the body in one fenced code block, for a log or other text that is not Markdown. |
| `--meta <KEY>=<VALUE>` | Add a header key of your own. Repeat it for more keys. |

The body is copied byte for byte and never parsed: it may hold its own `---` header, Markdown, or bytes that are not UTF-8. A body that is empty or only whitespace is refused before anything is written; a UTF-8 body counts every Unicode space (`U+00A0`, `U+2003`) as whitespace.

`--file` exists for an agent whose sandbox lets it run `rimz` but not write outside its workspace: the agent writes the text to a file it can reach and names it, and RimZ copies it into the store. A path that does not exist or cannot be read fails with an error naming it and writes no note.

`--fence` works with either source. The fence is one backtick longer than the longest run of backticks in the body, and never shorter than three, so a body that holds its own code fences stays inside the block. A newline is added before the closing fence when the body does not end with one. The empty-body check runs on the body before any fence.

A `--meta` key starts with a lowercase ASCII letter and holds only lowercase letters, digits, `_`, and `-`, so no key reads as a YAML number; its value is one line. Any other key, a value with a line break, or an argument with no `=` is a usage error and writes no note. A key that names one of RimZ's own header keys is dropped with one warning on stderr, even when RimZ has no value of its own for it. A key given twice keeps its last value, in the position of its first.

`add` exits `0` with the path, `2` for a refused kind, title, or `--meta` argument or a header past 1 MiB, and `1` for an empty body, an unreadable `--file`, or a failed write. A refused add leaves nothing behind in the store.

## The header

Each note opens with a YAML header between `---` lines, then the body. RimZ writes its keys in this order, each only when it has a value for it, followed by the `--meta` keys in the order given. Each key is written bare and each value as one quoted string (`kind: "handoff"`), with any character YAML forbids inside quotes (`U+007F`, `U+0080`–`U+009F`, `U+FFFE`, `U+FFFF`) written as a `\uXXXX` escape that reads back as the character, so a line-oriented reader can match `^written:` without a YAML parser.

| Key | Value | Present when |
| --- | --- | --- |
| `kind` | The kind's slug, the folder's name. | Always. |
| `title` | The title as given. | Always. |
| `written` | The moment of writing, RFC 3339 local time with its offset (`2026-11-12T14:03:07.412+01:00`). | Always. |
| `author` | The writing agent's instance name (`level-spark`), from `RIMZ_AGENT_NAME`. | The writer is a RimZ agent. |
| `profile` | The agent's profile, from `RIMZ_AGENT_PROFILE`. | The agent was launched from a profile. |
| `team` | The agent's team, from `RIMZ_TEAM`. | The agent is a team member. |
| `role` | The agent's team role, from `RIMZ_AGENT_ROLE`. | The agent holds a role. |
| `model` | The model the agent was launched with, from `RIMZ_AGENT_MODEL`. A model switched mid-session is not seen. | The launch named a model. |
| `project` | The project root, from `RIMZ_PROJECT_ROOT`, else the project the current directory resolves to. The global `--root` names it ahead of both. | The writer is inside a project. |
| `worktree` | The checkout the writer runs in, from `RIMZ_WORKTREE_PATH`: the project's main checkout or a linked worktree, so it can equal `project`. | The writer runs in a room pane, or RimZ started it as a headless check, a start-throttle check, a loop task's workflow script, or a worktree hook. |
| `channel` | The agent's channel, from `RIMZ_CHANNEL`. | The agent is on a named channel. |
| `loop-task` | The loop task that started the writer, from `RIMZ_LOOP_TASK`. | A loop task started the writer as a headless check, a start-throttle check, or a workflow script. An agent a loop fire launches carries `workflow-run` instead. |
| `workflow-run` | The workflow run the writer belongs to, from `RIMZ_WORKFLOW_RUN`. | A workflow step launched it, including a headless check. |

These facts come from the writer's environment alone; RimZ reads no room record to fill them. A variable set to the empty string counts as absent, and a plain shell writes `kind`, `title`, `written`, and usually `project`. When resolving the project fails (a broken RimZ home, a failing `git`), the note is still written without `project`, and one warning on stderr says why; stdout still holds the path alone.

## List

`list` prints the newest 20 notes in the store that match its filter, by write time, oldest first, then by path, so the newest note is the last row. Every note is its own row: two notes with the same title or author never fold into one.

| Flag | Meaning |
| --- | --- |
| `--filter <QUERY>` | Keep notes matching every term of the query. Repeat it to add terms. `--filter help` prints the fields and the grammar and reads no note. |
| `-n, --limit <N>` | Show the newest `N` matches; default 20. `0` shows every match. |
| `-q, --quiet` | Print one absolute path per listed note and nothing else. Conflicts with `--json`. |
| `--json` | Print a JSON array instead of the table. |

```sh
rimz notes list --filter kind=handoff
rimz notes list --filter 'kind=handoff author=otter age<2d' -q
rimz notes list --filter pr=482 -q -n 1
rimz notes list --filter 'kind=handoff title=*expiry* age<2d' -q -n 1
```

### Filter the list

A query is terms separated by spaces, each `<field><op><value>`, in the grammar [`rimz agents list --filter`](./agents.md#filter-the-list) uses. Every term of every `--filter` must match. The operators are `=` and `!=` on every field, and `>`, `>=`, `<`, and `<=` on `age` and `written` alone. `=` takes a comma list and matches when any value does; `!=` matches when none does. `none` matches a note without the key and `*` any note that has it; a `*` inside a value stands for any run of characters. Quote a value holding a space, a comma, or a quote as a JSON string (`topic="two words"`); a value that is literally `none` is quoted too (`label="none"`). A `*` is a wildcard quoted or not: `label="*"` matches any note that has `label`, as `label=*` does, and no query matches a literal `*` alone.

Each RimZ header key is a field under its own name, matched ignoring ASCII case (`kind`, `LOOP-TASK`). Any other field is a `--meta` key, written flat (`pr=482`, not `meta.pr=482`) and matched exactly as the header writes it. Values are compared case-sensitively, with these exceptions:

| Field | Rule |
| --- | --- |
| `kind` | The value is slugged the way `add` names its folder, so `Reflection` finds `reflection`. A value with a `*` is matched as written. A value with no ASCII letter or digit is refused. The header's `kind` decides, not the folder the note sits in. |
| `title` | Matched ignoring case. |
| `author` | One leading `@` is ignored, so `@otter` and `otter` match the same notes. |
| `project` | Both the value and the stored path are made absolute and normalized (`.` and `..` resolved by text) before they are compared; symlinks are not resolved. A value with a `*` is matched as written. |
| `written` | An RFC 3339 instant (`2026-11-12T09:00:00Z`). `written>=<instant>` keeps a note written at that instant. |
| `age` | How long ago the note was written, as a duration (`30s`, `5m`, `1h`, `2d`). `age` is no header key: it is not printed and shadows a `--meta` key of that name. |

A term naming a field that is no RimZ key, not `age`, and carried by no note in the store matches nothing, under any operator: the listing is empty, one warning on stderr names each such key, and the command exits `0`. A malformed term, an ordered operator on another field, or a value its field cannot read exits `2` with a message naming the term.

### What the limit hides

When the limit leaves older matches out, a closing line says how many: `137 older notes not shown; pass -n to see more`. In the table it is the last line on stdout. Under `-q` and `--json` it goes to stderr, so stdout holds only the result. Nothing prints when nothing is hidden, and an empty result under `-q` prints nothing at all. A store that does not exist yet lists nothing and exits `0`.

The table shows `WRITTEN`, `KIND`, `AUTHOR`, `TITLE`, and `PATH`, each value on one line: a line break or run of whitespace reads as one space and other control characters are dropped. `--json` keeps every value as written. Each `--json` element carries the note's `path`, every RimZ header key the note holds under its own name, and the `--meta` keys in a separate `meta` object:

```json
[
  {
    "path": "/home/me/.rimz/notes/handoff/20261112/ship-notes.md",
    "author": "level-spark",
    "kind": "handoff",
    "project": "/home/me/src/myrepo",
    "title": "Ship notes",
    "written": "2026-11-12T14:03:07.412+01:00",
    "meta": {
      "ticket": "42"
    }
  }
]
```

A `.md` file in the store that does not open with a RimZ header, whose header does not close within the file's first 1 MiB or is not UTF-8, or that has no readable `written` key is skipped with one warning on stderr naming it, and the listing goes on. `add` refuses a header that would not close within that 1 MiB, so every note it writes lists. Files without the `.md` extension, including a half-written temporary file left by an interrupted add, are skipped without a word.
