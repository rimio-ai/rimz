use serde::Serialize;
use serde_json::json;

use rimz::remote::aliases::RemoteAlias;

#[derive(Serialize)]
struct ListEntryJson<'a> {
    name: &'a str,
    target: &'a str,
    reconnect: bool,
    no_resume: bool,
    mux: Option<&'a str>,
}

pub(super) fn print(entries: &[RemoteAlias], json: bool) {
    if json {
        let rendered = render_list_json(entries);
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return;
    }
    let rendered = render_list_human(entries);
    if rendered.is_empty() {
        return;
    }
    #[expect(clippy::print_stdout, reason = "human listing")]
    {
        println!("{rendered}");
    }
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

fn render_list_human(entries: &[RemoteAlias]) -> String {
    let mut buf = String::new();
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
        use std::fmt::Write as _;
        writeln!(
            buf,
            "{}\t{}\t{}\t{}\t{}",
            entry.name, entry.target, reconnect, no_resume, mux,
        )
        .expect("write to string");
    }
    buf.trim_end().to_owned()
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
        insta::assert_snapshot!(
            render_list_human(&entries),
            @"prod	prod-box:query-engine	reconnect	resume	zellij"
        );
    }
}
