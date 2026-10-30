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
    preview_diff(
        out,
        original.map(|text| (path, text)),
        Some((path, candidate)),
    )
}

pub(in crate::cli) fn preview_diff(
    out: &mut (impl Write + ?Sized),
    original: Option<(&Path, &str)>,
    candidate: Option<(&Path, &str)>,
) -> std::io::Result<()> {
    let text = match (original, candidate) {
        (Some((old_path, original)), Some((new_path, candidate))) => {
            let old_path = old_path.display().to_string();
            let new_path = new_path.display().to_string();
            let rendered = TextDiff::from_lines(original, candidate)
                .unified_diff()
                .context_radius(3)
                .header(&old_path, &new_path)
                .to_string();
            if rendered.is_empty() {
                format!("--- {old_path}\n+++ {new_path}\n@@ no changes @@\n")
            } else {
                rendered
            }
        }
        (None, Some((path, candidate))) => {
            let path = path.display();
            let mut text = format!("--- /dev/null\n+++ {path}\n@@ new file @@\n");
            for line in candidate.lines() {
                text.push('+');
                text.push_str(line);
                text.push('\n');
            }
            text
        }
        (Some((path, original)), None) => {
            let mut text = format!(
                "--- {}\n+++ /dev/null\n@@ deleted file @@\n",
                path.display()
            );
            for line in original.lines() {
                text.push('-');
                text.push_str(line);
                text.push('\n');
            }
            text
        }
        (None, None) => return Ok(()),
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
