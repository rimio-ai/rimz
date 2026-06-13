use serde::Serialize;
use serde_json::json;

use crate::cli::render;
use rimz::remote::aliases::RemoteAlias;

#[derive(Serialize)]
struct ListEntryJson<'a> {
    name: &'a str,
    target: &'a str,
    reconnect: bool,
    no_resume: bool,
    mux: Option<&'a str>,
}

pub(super) fn print(entries: &[RemoteAlias], json: bool) -> std::io::Result<()> {
    if json {
        let rendered = render_list_json(entries);
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }
    if entries.is_empty() {
        return Ok(());
    }
    human_table(entries).render(&mut render::out())
}

fn render_list_json(entries: &[RemoteAlias]) -> String {
    let rows: Vec<ListEntryJson<'_>> = entries
        .iter()
        .map(|entry| ListEntryJson {
            name: &entry.name,
            target: &entry.target,
            reconnect: entry.reconnect,
            no_resume: entry.no_resume,
            mux: entry.mux.map(|mux| mux.as_str()),
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "remotes": rows })).expect("rendered JSON serializes")
}

fn human_table(entries: &[RemoteAlias]) -> render::Table {
    let mut table = render::Table::new(["NAME", "TARGET", "RECONNECT", "RESUME", "MUX"]);
    for entry in entries {
        let reconnect = if entry.reconnect {
            "reconnect"
        } else {
            "no-reconnect"
        };
        let no_resume = if entry.no_resume {
            "no-resume"
        } else {
            "resume"
        };
        let mux = entry
            .mux
            .map(|mux| mux.as_str().to_owned())
            .unwrap_or_else(|| "-".to_owned());
        let reconnect_style = if entry.reconnect {
            render::palette::GOOD
        } else {
            render::palette::MUTED
        };
        table.row([
            render::cell(entry.name.as_str()).fg(render::palette::ACCENT),
            render::cell(entry.target.as_str()),
            render::cell(reconnect).fg(reconnect_style),
            render::cell(no_resume).fg(render::palette::BODY),
            render::cell(mux).dash(),
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::MuxName;

    #[test]
    fn list_renderers_emit_public_shapes() {
        let entries = vec![
            RemoteAlias {
                name: "dev".to_owned(),
                target: "dev-box:query-engine".to_owned(),
                reconnect: true,
                no_resume: false,
                mux: None,
            },
            RemoteAlias {
                name: "prod".to_owned(),
                target: "agent@prod-box:~/code/query-engine".to_owned(),
                reconnect: false,
                no_resume: true,
                mux: Some(MuxName::Tmux),
            },
        ];
        insta::assert_snapshot!(render_list_json(&entries), @r#"
        {
          "remotes": [
            {
              "mux": null,
              "name": "dev",
              "no_resume": false,
              "reconnect": true,
              "target": "dev-box:query-engine"
            },
            {
              "mux": "tmux",
              "name": "prod",
              "no_resume": true,
              "reconnect": false,
              "target": "agent@prod-box:~/code/query-engine"
            }
          ]
        }
        "#);
        let entries = vec![RemoteAlias {
            name: "prod".to_owned(),
            target: "prod-box:query-engine".to_owned(),
            reconnect: true,
            no_resume: false,
            mux: Some(MuxName::Zellij),
        }];
        // Render the table with ANSI stripped so the snapshot is the plain,
        // aligned text; `print` re-styles on the real stdout via `render::out`.
        let mut buf: Vec<u8> = Vec::new();
        human_table(&entries)
            .render(&mut anstream::StripStream::new(&mut buf))
            .expect("table renders to an in-memory buffer");
        let rendered = String::from_utf8(buf).expect("table output is utf-8");
        insta::assert_snapshot!(rendered, @r"
        NAME  TARGET                 RECONNECT  RESUME  MUX
        prod  prod-box:query-engine  reconnect  resume  zellij
        ");
    }
}
