//! File previews shared by explicit configuration consent surfaces.

use std::io::Write;
use std::path::Path;

use similar::TextDiff;

use super::{paint, palette};

pub(crate) fn preview_file_diff(
    out: &mut (impl Write + ?Sized),
    path: &Path,
    original: Option<&str>,
    candidate: &str,
) -> std::io::Result<()> {
    let path = path.display().to_string();
    let text = match original {
        Some(original) => {
            let rendered = TextDiff::from_lines(original, candidate)
                .unified_diff()
                .context_radius(3)
                .header(&path, &path)
                .to_string();
            if rendered.is_empty() {
                format!("--- {path}\n+++ {path}\n@@ no changes @@\n")
            } else {
                rendered
            }
        }
        None => {
            let mut text = format!("--- /dev/null\n+++ {path}\n@@ new file @@\n");
            for line in candidate.lines() {
                text.push('+');
                text.push_str(line);
                text.push('\n');
            }
            text
        }
    };
    for line in text.lines() {
        writeln!(out, "    {}", color_diff_line(line))?;
    }
    Ok(())
}

fn color_diff_line(line: &str) -> String {
    let style = if line.starts_with("+++") || line.starts_with("---") {
        palette::accent().bold()
    } else if line.starts_with('+') {
        palette::good()
    } else if line.starts_with('-') {
        palette::alarm()
    } else if line.starts_with("@@") {
        palette::warn().bold()
    } else {
        palette::faint()
    };
    paint(style, line)
}
