use super::*;

pub(crate) fn follow(
    workspace: &rimz::ResolvedWorkspace,
    target: Option<&str>,
    worktree: Option<&str>,
    tail: Option<usize>,
    all: bool,
    json: bool,
    flat: bool,
) -> Result<()> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing state paths")?;
    let read = |last| {
        chat_view_with_mode(
            workspace,
            &paths,
            target,
            worktree,
            last,
            all,
            ViewMode {
                hidden: Hidden::for_json(json),
                flat,
            },
        )
    };
    let initial = read(tail)?;
    let baseline = if tail.is_some() {
        read(None)?.entries.len()
    } else {
        initial.entries.len()
    };
    if json {
        for entry in selected_lines(&initial) {
            render::finish(write_json_line(&entry))?;
        }
    } else if !selected_lines(&initial).is_empty() {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_render(render_lines_to(
            &mut out,
            &initial,
            &tz,
            Prose::for_stdout(),
        ))?;
    }

    let tz = crate::cli::machine_config().time_zone();
    let mut seen = baseline;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let view = read(None)?;
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
            finish_render(render_lines_since_to(
                &mut out,
                &view,
                seen - new_entries.len(),
                &tz,
                Prose::for_stdout(),
            ))?;
        }
    }
}

pub(crate) fn finish_render(write: Result<()>) -> Result<()> {
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
