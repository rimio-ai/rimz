//! The name grammar loop tasks, workflows, and workflow nodes share.

/// A name outside the shared grammar.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("schedule name `{name}` must be non-empty and use only letters, digits, `-`, or `_`")]
pub struct NameErr {
    pub name: String,
}

/// Validate a schedule name: non-empty and limited to a filesystem- and
/// shell-safe charset.
pub fn validate_name(name: &str) -> Result<(), NameErr> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if ok {
        Ok(())
    } else {
        Err(NameErr {
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        validate_name("morning").expect("ok");
        validate_name("morning-claude_1").expect("ok");
        for name in ["", "bad name", "bad/name"] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }
}
