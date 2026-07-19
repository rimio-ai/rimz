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
    auto_forward: bool,
}

pub(super) fn print(entries: &[RemoteAlias], json: bool) -> anyhow::Result<()> {
    if json {
        return render::json_pretty(&list_json(entries));
    }
    if entries.is_empty() {
        return Ok(());
    }
    human_table(entries).render(&mut render::out())?;
    Ok(())
}

fn list_json(entries: &[RemoteAlias]) -> serde_json::Value {
    let rows: Vec<ListEntryJson<'_>> = entries
        .iter()
        .map(|entry| ListEntryJson {
            name: &entry.name,
            target: &entry.target,
            reconnect: entry.reconnect,
            no_resume: entry.no_resume,
            mux: entry.mux.map(|mux| mux.as_str()),
            auto_forward: entry.auto_forward,
        })
        .collect();
    json!({ "remotes": rows })
}

fn human_table(entries: &[RemoteAlias]) -> render::Table {
    let mut table = render::Table::new(["NAME", "TARGET", "RECONNECT", "RESUME", "MUX", "FORWARD"]);
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
        let forward = if entry.auto_forward { "auto" } else { "off" };
        let reconnect_style = if entry.reconnect {
            render::palette::good()
        } else {
            render::palette::muted()
        };
        table.row([
            render::cell(entry.name.as_str()).fg(render::palette::accent()),
            render::cell(entry.target.as_str()),
            render::cell(reconnect).fg(reconnect_style),
            render::cell(no_resume).fg(render::palette::body()),
            render::cell(mux).dash(),
            render::cell(forward).fg(render::palette::body()),
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::MuxName;

    fn render_list_json(entries: &[RemoteAlias]) -> String {
        serde_json::to_string_pretty(&list_json(entries)).expect("rendered JSON serializes")
    }

    #[test]
    fn list_renderers_emit_public_shapes() {
        let entries = vec![
            RemoteAlias {
                name: "dev".to_owned(),
                target: "dev-box:query-engine".to_owned(),
                reconnect: true,
                no_resume: false,
                mux: None,
                auto_forward: true,
            },
            RemoteAlias {
                name: "prod".to_owned(),
                target: "agent@prod-box:~/code/query-engine".to_owned(),
                reconnect: false,
                no_resume: true,
                mux: Some(MuxName::Tmux),
                auto_forward: false,
            },
        ];
        insta::assert_snapshot!(render_list_json(&entries), @r#"
        {
          "remotes": [
            {
              "auto_forward": true,
              "mux": null,
              "name": "dev",
              "no_resume": false,
              "reconnect": true,
              "target": "dev-box:query-engine"
            },
            {
              "auto_forward": false,
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
            auto_forward: true,
        }];
        // Render the table with ANSI stripped so the snapshot is the plain,
        // aligned text; `print` re-styles on the real stdout via `render::out`.
        let mut buf: Vec<u8> = Vec::new();
        human_table(&entries)
            .render(&mut anstream::StripStream::new(&mut buf))
            .expect("table renders to an in-memory buffer");
        let rendered = String::from_utf8(buf).expect("table output is utf-8");
        insta::assert_snapshot!(rendered, @r"
        NAME  TARGET                 RECONNECT  RESUME  MUX     FORWARD
        prod  prod-box:query-engine  reconnect  resume  zellij  auto
        ");
    }
}
