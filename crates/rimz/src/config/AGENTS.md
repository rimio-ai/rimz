# Configuration

Local contract for `crates/rimz/src/config/`: machine settings, profile and team definitions, and `rimz config` editing. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). The `//!` header of [`config.rs`](../config.rs) describes the config tiers and load modes.

- A new config key stored as a string that a user can type as a bare number (a byte size, a duration, a budget) joins the always-string list in `crates/rimz/src/config/edit.rs::parse_set_value` and gets its own branch in `validate_set_value`, pinned beside the `gc.tmp_floor_min` cases in `crates/rimz/src/config/edit/tests.rs`. Otherwise `rimz config set <key> 1024` is refused.
- A config command that accepts a table key (`rimz config unset`) applies every rule keyed on a leaf or an entry path to each such path under the table, and each rule gets a test that names a parent key: a test of the named key alone passes while the parent path skips the rule.
- A change to a file under `crates/rimz/src/config/templates/` runs `cargo xtask test 'config::template_tests'`: the test uncomments the template's default lines and parses the result, so a prose comment line that begins with `[` reads as a table header.
