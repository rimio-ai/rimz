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
    let target = agent_logs_target(&reference);
    let workspace = crate::cli::transcript::resolve_view_workspace(Some(&target), None, globals)?;
    let hidden = crate::cli::transcript::Hidden::for_json(json);
    if follow {
        return crate::cli::transcript::follow(
            &workspace,
            Some(&target),
            None,
            tail,
            all,
            json,
            false,
        );
    }
    let view = crate::cli::transcript::chat_view_with_hidden(
        &workspace,
        Some(&target),
        None,
        tail,
        all,
        hidden,
    )?;
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
        crate::cli::transcript::finish_render(crate::cli::transcript::render_lines_to(
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

fn write_json_pretty(value: &impl serde::Serialize) -> std::io::Result<()> {
    let pretty = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{pretty}")
}
