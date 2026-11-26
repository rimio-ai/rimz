# Configuration

Local contract for `crates/rimz/src/config/`: machine settings, profile and team definitions, and `rimz config` editing. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). The `//!` header of [`config.rs`](../config.rs) describes the config tiers and load modes. [templates/AGENTS.md](./templates/AGENTS.md) extends this one for the config templates.

- A new config key stored as a string that a user can type as a bare number (a byte size, a duration, a budget) joins the always-string list in `crates/rimz/src/config/edit.rs::parse_set_value` and gets its own branch in `validate_set_value`, pinned beside the `gc.tmp_floor_min` cases in `crates/rimz/src/config/edit/tests.rs`. Otherwise `rimz config set <key> 1024` is refused.
