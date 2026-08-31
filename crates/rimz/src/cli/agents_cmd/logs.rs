use super::*;

use crate::cli::render;
use crate::cli::render::prose::Prose;

pub(super) fn logs_agent(
    reference: String,
    tail: Option<usize>,
    follow: bool,
    all: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let target = agent_logs_target(&reference);
    if follow {
        return follow_agent_logs(&workspace, &target, tail, all, json);
    }
    let view = crate::cli::transcript::chat_view(&workspace, Some(&target), None, tail, all)?;
    let selected = crate::cli::transcript::selected_lines(&view);
    if json {
        render::finish(write_json_pretty(
            &serde_json::json!({ "entries": selected }),
        ))?;
    } else if selected.is_empty() {
        let mut out = render::err();
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::faint(),
                view.empty_message
                    .as_deref()
                    .unwrap_or("No conversation recorded yet.")
            )
        )?;
    } else {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_transcript_render(crate::cli::transcript::render_lines_to(
            &mut out,
            &view,
            &tz,
            Prose::for_stdout(),
        ))?;
    }
    Ok(())
}

fn agent_logs_target(reference: &str) -> String {
    if reference.starts_with('@') || reference.starts_with('#') {
        reference.to_owned()
    } else {
        format!("@{reference}")
    }
}

fn follow_agent_logs(
    workspace: &rimz::ResolvedWorkspace,
    target: &str,
    tail: Option<usize>,
    all: bool,
    json: bool,
) -> Result<()> {
    let initial = crate::cli::transcript::chat_view(workspace, Some(target), None, tail, all)?;
    let baseline = if tail.is_some() {
        crate::cli::transcript::chat_view(workspace, Some(target), None, None, all)?
            .entries
            .len()
    } else {
        initial.entries.len()
    };
    if json {
        for entry in crate::cli::transcript::selected_lines(&initial) {
            render::finish(write_json_line(&entry))?;
        }
    } else if !crate::cli::transcript::selected_lines(&initial).is_empty() {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_transcript_render(crate::cli::transcript::render_lines_to(
            &mut out,
            &initial,
            &tz,
            Prose::for_stdout(),
        ))?;
    }

    let tz = crate::cli::machine_config().time_zone();
    let mut seen = baseline;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let view = crate::cli::transcript::chat_view(workspace, Some(target), None, None, all)?;
        if view.entries.len() <= seen {
            continue;
        }
        let new_entries = view.entries[seen..].to_vec();
        seen = view.entries.len();
        if json {
            for entry in new_entries {
                render::finish(write_json_line(&entry.chat))?;
            }
        } else {
            let mut out = render::out();
            finish_transcript_render(crate::cli::transcript::render_lines_since_to(
                &mut out,
                &view,
                seen - new_entries.len(),
                &tz,
                Prose::for_stdout(),
            ))?;
        }
    }
}

fn finish_transcript_render(write: Result<()>) -> Result<()> {
    render::finish(write.map_err(|err| match err.downcast::<std::io::Error>() {
        Ok(err) => err,
        Err(err) => std::io::Error::other(err),
    }))
}

fn write_json_line(value: &impl serde::Serialize) -> std::io::Result<()> {
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
}

fn write_json_pretty(value: &impl serde::Serialize) -> std::io::Result<()> {
    let pretty = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{pretty}")
}
