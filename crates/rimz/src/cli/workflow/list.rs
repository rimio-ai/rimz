use std::collections::BTreeMap;
use std::io::IsTerminal;

use jiff::Timestamp;
use rimz::config::GlyphRole;
use rimz::store::workflow::RunState;
use unicode_width::UnicodeWidthStr;

use super::*;

struct Row<'a> {
    record: &'a RunRecord,
    address: String,
    glyph: String,
    state: RunState,
    asking: bool,
    node: String,
    detail: String,
    age: String,
    since: Timestamp,
}

impl Row<'_> {
    fn needs_you(&self) -> bool {
        self.state == RunState::Parked || self.asking
    }
}

pub(super) fn run(json: bool, globals: &super::super::GlobalFlags) -> Result<()> {
    render(&resolve::all()?, None, json, globals)
}

pub(super) fn render(
    runs: &[resolve::Located],
    workflow: Option<&str>,
    json: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let now = Timestamp::now();
    let live = runs
        .iter()
        .filter(|run| !run.record.state.is_terminal())
        .collect::<Vec<_>>();
    let details = live
        .iter()
        .map(|run| {
            let spec = workflow::load(&run.paths.home).ok();
            workflow_view::detail(&run.record, spec.as_ref(), now)
        })
        .collect::<Vec<_>>();
    if json {
        let rows = live
            .iter()
            .zip(&details)
            .map(|(run, detail)| json_row(&run.record, detail))
            .collect::<Vec<_>>();
        return render::json_pretty(&rows);
    }
    let mut out = render::out();
    if live.is_empty() {
        if let Some(name) = workflow {
            writeln!(out, "no active or parked runs of {name}")?;
        } else {
            writeln!(
                out,
                "no active or parked runs; start one with rimz workflow run <name>"
            )?;
        }
        return Ok(());
    }
    let caller = resolve::caller(globals).map(|workspace| workspace.project_root);
    let mut rooms = BTreeMap::new();
    let machine = super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    for (run, detail) in live.into_iter().zip(details) {
        let record = &run.record;
        let asking = workflow_view::asking(record).is_some();
        let role = if record.state == RunState::Parked || asking {
            GlyphRole::StatusWaiting
        } else if record.wait.is_some() {
            GlyphRole::StatusSleeping
        } else {
            GlyphRole::StatusWorking
        };
        rooms
            .entry(&record.project_root)
            .or_insert_with(Vec::new)
            .push(Row {
                record,
                address: record.address().to_string(),
                glyph: glyph(role),
                state: record.state,
                asking,
                node: record.node.clone(),
                detail,
                age: render::age_short(workflow_view::since(record), now),
                since: workflow_view::since(record),
            });
    }
    let mut rooms = rooms.into_iter().collect::<Vec<_>>();
    rooms.sort_by_key(|(root, _)| (Some(*root) != caller.as_ref(), *root));
    let several = rooms.len() > 1;
    let width = std::io::stdout()
        .is_terminal()
        .then(|| render::terminal_columns(100));
    for (index, (root, rows)) in rooms.iter_mut().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        if several {
            writeln!(
                out,
                "{}",
                render::paint(render::palette::header(), &render::home_relative_path(root))
            )?;
        }
        rows.sort_by(|a, b| {
            let a_needs = a.needs_you();
            let b_needs = b.needs_you();
            b_needs
                .cmp(&a_needs)
                .then_with(|| {
                    if a_needs {
                        a.since.cmp(&b.since)
                    } else {
                        a.address.cmp(&b.address)
                    }
                })
                .then(a.address.cmp(&b.address))
        });
        let needs = rows.partition_point(Row::needs_you);
        for (title, section) in [("NEEDS YOU", &rows[..needs]), ("ACTIVE", &rows[needs..])] {
            if section.is_empty() {
                continue;
            }
            if title == "ACTIVE" && needs > 0 {
                writeln!(out)?;
            }
            writeln!(out, "{}", render::paint(render::palette::header(), title))?;
            write_rows(&mut out, section, width)?;
            if title == "NEEDS YOU" {
                writeln!(
                    out,
                    "  → {}",
                    resolve::run_command(section[0].record, "show", globals, None, &[], &[])
                )?;
            }
        }
    }
    Ok(())
}

fn json_row(record: &RunRecord, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "address": record.address().to_string(),
        "workflow": record.workflow,
        "scope": record.scope.path,
        "state": record.state,
        "node": record.node,
        "since": record.entered_at,
        "detail": detail,
    })
}

fn write_rows(out: &mut impl Write, rows: &[Row], width: Option<usize>) -> std::io::Result<()> {
    let name = rows
        .iter()
        .map(|row| row.glyph.width() + 1 + row.address.width())
        .max()
        .unwrap_or(0);
    let node = rows.iter().map(|row| row.node.width()).max().unwrap_or(0);
    let age = rows.iter().map(|row| row.age.width()).max().unwrap_or(0);
    let detail_width = width.map(|width| width.saturating_sub(2 + name + node + age + 6));
    let mut table = render::Table::new(["", "", "", ""])
        .without_headers()
        .indent(2);
    for row in rows {
        let detail = detail_width.map_or_else(
            || row.detail.clone(),
            |width| render::clip_to_width(&row.detail, width),
        );
        table.row([
            render::cell(&row.glyph)
                .fg(render::status::workflow(row.state, row.asking))
                .suffix(&row.address, render::palette::body()),
            render::cell(&row.node),
            render::cell(detail),
            render::cell(&row.age).fg(render::palette::muted()),
        ]);
    }
    table.render(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_rows_clip_detail_before_age() {
        let record = serde_json::from_value(serde_json::json!({
            "version":1, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/feature"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"parked", "node":"decision", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1,
        }))
        .unwrap();
        let rows = [Row {
            record: &record,
            address: "review#feature".into(),
            glyph: "?".into(),
            state: RunState::Parked,
            asking: false,
            node: "decision".into(),
            detail: "user: This decision needs a long explanation".repeat(3),
            age: "47m".into(),
            since: Timestamp::UNIX_EPOCH,
        }];
        let mut out = anstream::StripStream::new(Vec::new());
        write_rows(&mut out, &rows, Some(60)).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            text.contains('…') && text.trim_end().ends_with("47m"),
            "{text}"
        );
        assert!(text.lines().all(|line| line.width() <= 60), "{text}");
    }
}
