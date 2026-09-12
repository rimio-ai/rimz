//! Provider accounts at the command line: the `--account <kind>=<name>`
//! selection `rimz start` and `rimz reset` pass to a room's birth.

use anyhow::{Result, bail};
use rimz::ids::{AgentKind, LoginName, RoomLogins};

/// One `--account <kind>=<name>` flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountFlag {
    kind: AgentKind,
    name: LoginName,
}

pub(crate) fn parse_account_flag(raw: &str) -> std::result::Result<AccountFlag, String> {
    let Some((kind, name)) = raw.split_once('=') else {
        return Err(format!("expected `<kind>=<name>`, got `{raw}`"));
    };
    let Some(definition) = rimz::agents::find_definition(kind) else {
        return Err(format!("unknown agent kind `{kind}`"));
    };
    Ok(AccountFlag {
        kind: AgentKind::new_unchecked(definition.spec().kind),
        name: name.parse().map_err(|err| format!("{err}"))?,
    })
}

/// The room selection the flags request; naming one kind twice is refused
/// rather than letting the later flag silently win.
pub(crate) fn requested_logins(flags: &[AccountFlag]) -> Result<RoomLogins> {
    let mut logins = RoomLogins::new();
    for flag in flags {
        if let Some(first) = logins.insert(flag.kind.clone(), flag.name.clone()) {
            bail!(
                "--account names {} twice (`{first}` and `{}`); pass one account per kind",
                flag.kind,
                flag.name
            );
        }
    }
    Ok(logins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_flags_parse_kind_and_name_and_refuse_a_repeated_kind() {
        let work = parse_account_flag("claude=work").expect("claude=work");
        let default = parse_account_flag("codex=default").expect("codex=default");
        assert_eq!(
            requested_logins(&[work.clone(), default]).expect("one per kind"),
            RoomLogins::from([
                (AgentKind::new_unchecked("claude"), "work".parse().unwrap()),
                (
                    AgentKind::new_unchecked("codex"),
                    LoginName::default_login()
                ),
            ])
        );
        assert!(parse_account_flag("claude").is_err());
        assert!(parse_account_flag("nope=work").is_err());
        assert!(parse_account_flag("claude=Work").is_err());

        let personal = parse_account_flag("claude=personal").expect("claude=personal");
        let err = requested_logins(&[work, personal]).unwrap_err();
        assert!(err.to_string().contains("names claude twice"), "{err}");
    }
}
