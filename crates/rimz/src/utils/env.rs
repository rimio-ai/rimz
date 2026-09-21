//! Environment variables RimZ reads as on/off switches.
//!
//! The offline switches are documented as `VAR=1`, so a reader who wants the
//! feature back reaches for `VAR=0` — which a bare presence check reads as a
//! second "on". [`flag_enabled`] parses the value instead, and the values that
//! mean off are the ones a user would write. A new user-facing on/off variable
//! belongs here; `var_os(..).is_some()` is for markers another program sets,
//! `TMUX` and `ZELLIJ` among them.

use std::ffi::{OsStr, OsString};

/// The values that turn a RimZ switch off when the variable is still exported.
const OFF_VALUES: &[&str] = &["", "0", "false"];

/// `true` when `name` is set to anything other than empty, `0`, or `false`.
///
/// An unset variable is off, and so is a value the shell left empty, which is
/// what `VAR=` and an exported-but-unassigned variable both produce.
pub fn flag_enabled(name: impl AsRef<OsStr>) -> bool {
    flag_value_enabled(std::env::var_os(name))
}

/// The parse behind [`flag_enabled`], taking the value so it is testable
/// without mutating this process's environment.
fn flag_value_enabled(value: Option<OsString>) -> bool {
    value.is_some_and(|value| {
        let value = value.to_string_lossy();
        !OFF_VALUES.contains(&value.trim().to_ascii_lowercase().as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(raw: Option<&str>) -> Option<OsString> {
        raw.map(OsString::from)
    }

    #[test]
    fn off_values_and_an_unset_variable_read_as_off() {
        for raw in ["", "0", "false", "False", "FALSE", " 0 "] {
            assert!(
                !flag_value_enabled(value_of(Some(raw))),
                "`{raw}` should not enable a flag"
            );
        }
        assert!(!flag_value_enabled(None));
    }

    #[test]
    fn any_other_value_reads_as_on() {
        for raw in ["1", "true", "yes", "on", "cache-only"] {
            assert!(
                flag_value_enabled(value_of(Some(raw))),
                "`{raw}` should enable a flag"
            );
        }
    }
}
