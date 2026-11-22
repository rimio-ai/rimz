use super::*;

use crate::cli::render;

pub(super) use rimz::launcher::stop::{newest_run_by_ref, newest_run_for_agent};

pub(super) fn print_run_line(run: &RunRecord) -> std::io::Result<()> {
    use std::io::Write;
    let status = run.status.label();
    writeln!(
        render::out(),
        "{} {} {} {}",
        render::paint(render::palette::muted(), "run:"),
        run.run_id,
        render::paint(render::status::run(run.status), status),
        run.prompt,
    )
}

pub(super) fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}
