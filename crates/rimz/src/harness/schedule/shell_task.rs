//! A shell row's inline graph: the one-node workflow a shell task's fire runs, and the
//! timeout a scheduled run takes when the row and the machine config name none.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::config::{MachineConfig, TaskEntry};

pub const SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL: &str = "2h";

/// The time limit a command task stores when it is given none: `loop.default-timeout`, else
/// [`SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL`].
pub fn default_timeout(machine: &MachineConfig) -> String {
    machine
        .r#loop
        .default_timeout
        .clone()
        .unwrap_or_else(|| SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL.to_owned())
}

/// The node a command fire's run starts at and ends on.
pub const COMMAND_NODE: &str = "command";

/// The inline definition one fire of task `name` runs: a `test` check whose script execs `argv`
/// under the row's `timeout`, routing a zero exit to `@done` and anything else to `@failed`.
/// `sh` only execs the argv, every word single-quoted, so each reaches the process as written;
/// a `command` step would confine every word holding `/` to the definition's directory.
pub fn command_definition(
    name: &str,
    entry: &TaskEntry,
    argv: &[String],
) -> Result<crate::harness::workflow::InlineDefinition> {
    let timeout = entry
        .timeout
        .as_deref()
        .with_context(|| format!("loop task `{name}` has a command without `timeout`"))?;
    let on = toml::Table::from_iter(
        [
            ("pass", "@done"),
            ("fail", "@failed"),
            ("timeout", "@failed"),
            ("died", "@failed"),
        ]
        .map(|(outcome, target)| (outcome.to_owned(), toml::Value::from(target))),
    );
    let check = toml::Table::from_iter([
        ("summary".to_owned(), "the task's command".into()),
        (
            "shell".to_owned(),
            format!("exec {}", super::command_line(argv)).into(),
        ),
        ("timeout".to_owned(), timeout.into()),
        ("on".to_owned(), on.into()),
    ]);
    let workflow = toml::Table::from_iter([
        ("summary".to_owned(), format!("loop task {name}").into()),
        ("scope".to_owned(), "worktree".into()),
        ("start".to_owned(), COMMAND_NODE.into()),
    ]);
    let spec = toml::Table::from_iter([
        ("workflow".to_owned(), workflow.into()),
        (
            "test".to_owned(),
            toml::Table::from_iter([(COMMAND_NODE.to_owned(), check.into())]).into(),
        ),
    ]);
    Ok(crate::harness::workflow::InlineDefinition {
        text: toml::to_string(&spec)?,
        assets: BTreeMap::new(),
    })
}
