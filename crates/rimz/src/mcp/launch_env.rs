//! What a launch serves, handed to a server it does not start itself: a provider that runs
//! one fixed `rimz mcp serve --launch-env` entry for every session of the account takes
//! each launch's selection from two variables the launch sets. The provider-env compiler
//! sets them on a launch whose adapter serves from them and clears them on every other.

use std::path::PathBuf;

use super::catalog::{Capability, LoadMode};

/// The served capabilities by name, comma-separated, each with an optional `:always` or
/// `:deferred`: the vocabulary of a profile's `mcps` list. Always the explicit list; absent
/// or empty serves nothing.
pub const ENV_SERVE: &str = "RIMZ_MCP_SERVE";

/// The directory the agent launched in, where every tool call runs.
pub const ENV_CWD: &str = "RIMZ_MCP_CWD";

/// One served capability and its load mark.
pub type Served = (Capability, Option<LoadMode>);

/// What a launch serves, from its environment: `None` when it serves nothing.
#[derive(Debug, PartialEq, Eq)]
pub struct LaunchSelection {
    pub served: Vec<Served>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{ENV_SERVE} entry `{0}` is not a RimZ capability (rimz-lsp, rimz-wait, rimz-subagents, \
     rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-control, rimz-notes) \
     with an optional `:always` or `:deferred`, named once"
)]
pub struct LaunchEnvErr(String);

/// `served` as the value of [`ENV_SERVE`].
pub fn render(served: &[Served]) -> String {
    served
        .iter()
        .map(|(capability, mode)| match mode {
            Some(mode) => format!("{}:{}", capability.name(), mode.name()),
            None => capability.name().to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The capabilities and marks a value of [`ENV_SERVE`] names.
pub fn parse(value: &str) -> Result<Vec<Served>, LaunchEnvErr> {
    let mut served: Vec<Served> = Vec::new();
    for entry in value.split(',').map(str::trim) {
        let invalid = || LaunchEnvErr(entry.to_owned());
        let (name, mode) = match entry.split_once(':') {
            Some((name, mark)) => (name, Some(LoadMode::from_name(mark).ok_or_else(invalid)?)),
            None => (entry, None),
        };
        let capability = Capability::from_name(name).ok_or_else(invalid)?;
        if served.iter().any(|(earlier, _)| *earlier == capability) {
            return Err(invalid());
        }
        served.push((capability, mode));
    }
    Ok(served)
}

/// The selection the two variables carry, read through `var`: `Ok(None)` when
/// [`ENV_SERVE`] is absent or empty.
pub fn read(var: impl Fn(&str) -> Option<String>) -> Result<Option<LaunchSelection>, LaunchEnvErr> {
    let Some(value) = var(ENV_SERVE).filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    Ok(Some(LaunchSelection {
        served: parse(&value)?,
        cwd: var(ENV_CWD)
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_round_trips_and_a_bad_entry_is_named() {
        let served = vec![
            (Capability::Lsp, Some(LoadMode::Always)),
            (Capability::Wait, None),
            (Capability::Notes, Some(LoadMode::Deferred)),
        ];
        let value = render(&served);
        assert_eq!(value, "rimz-lsp:always,rimz-wait,rimz-notes:deferred");
        assert_eq!(parse(&value), Ok(served));
        assert_eq!(
            parse(" rimz-lsp , rimz-control:always"),
            Ok(vec![
                (Capability::Lsp, None),
                (Capability::Control, Some(LoadMode::Always))
            ])
        );
        for bad in ["rimz", "*", "github", "rimz-lsp:eager", "rimz-lsp:", ""] {
            let value = format!("rimz-wait,{bad}");
            assert_eq!(parse(&value), Err(LaunchEnvErr(bad.to_owned())), "{value}");
        }
        assert_eq!(
            parse("rimz-lsp,rimz-lsp:always"),
            Err(LaunchEnvErr("rimz-lsp:always".to_owned()))
        );
        assert_eq!(
            LaunchEnvErr("rimz-x".to_owned()).to_string(),
            "RIMZ_MCP_SERVE entry `rimz-x` is not a RimZ capability (rimz-lsp, rimz-wait, \
             rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, \
             rimz-control, rimz-notes) with an optional `:always` or `:deferred`, named once"
        );
    }

    #[test]
    fn absent_or_empty_serves_nothing() {
        fn vars(
            serve: Option<&'static str>,
            cwd: Option<&'static str>,
        ) -> impl Fn(&str) -> Option<String> {
            move |name| match name {
                ENV_SERVE => serve.map(str::to_owned),
                ENV_CWD => cwd.map(str::to_owned),
                _ => None,
            }
        }
        assert_eq!(read(vars(None, Some("/w"))), Ok(None));
        assert_eq!(read(vars(Some(" "), Some("/w"))), Ok(None));
        assert_eq!(
            read(vars(Some("rimz-wait"), Some("/w"))),
            Ok(Some(LaunchSelection {
                served: vec![(Capability::Wait, None)],
                cwd: Some(PathBuf::from("/w")),
            }))
        );
        assert_eq!(
            read(vars(Some("rimz-wait"), Some(""))).map(|read| read.map(|read| read.cwd)),
            Ok(Some(None))
        );
    }
}
