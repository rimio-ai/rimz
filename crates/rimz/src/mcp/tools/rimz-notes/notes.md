Save a Markdown note that outlives this session, which a later session or another agent in any room or checkout reads by its path, and return that path; cite it word for word wherever you point at the note. RimZ stamps the note's header with who wrote it and where. `kind` is what the note is: `handoff`, `molt`, `debug`, `reflection`, or one word of your own; `title` is its title.

- A file you already have (a report, a subagent's output, a log under `$TMPDIR`): pass it as `file` (an absolute path, or one relative to the directory the session started in) in place of `body`, and from then on cite the note's path in place of the file's.
- Text that is not Markdown (a log, command output): add `fence`, which wraps the body in one code block.
- A fact to find the note by later (a branch, a PR number): add it to `meta`, as in `{"pr": "482"}`; a `rimz notes list` filter names it.
