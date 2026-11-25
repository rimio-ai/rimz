# Wait traps

- **Truncated log files (`file`):** a rerun that truncates and regrows a watched file within a second hides lines written before the old end. Remove the log before starting a command that rewrites it.
- **Hanging `check` commands:** a `check` command that hangs delays its `check_in` notice, because the notice waits for the running probe to return. Give the command a timeout of its own.
- **Bounding a `when` wait:** `when` takes no `check_in`. To bound a condition that may never come true, arm an `in` wait beside it and `stop` whichever is left when one fires.
