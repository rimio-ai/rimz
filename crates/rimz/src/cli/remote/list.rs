use std::collections::BTreeMap;
use std::io::Write;

use serde::Serialize;
use serde_json::json;

use crate::cli::render;
use rimz::remote::RemoteTarget;
use rimz::remote::aliases::RemoteAlias;

#[derive(Serialize)]
struct ListEntryJson<'a> {
    name: &'a str,
    target: &'a str,
    reconnect: bool,
    resume: bool,
    mux: Option<&'a str>,
    auto_forward: bool,
}

pub(super) fn print(entries: &[RemoteAlias], json: bool) -> anyhow::Result<()> {
    if json {
        return render::json_pretty(&list_json(entries));
    }
    if entries.is_empty() {
        writeln!(
            render::out(),
            "no saved remotes; save one with: rimz remote add <name> [user@]host:<session-or-path>"
        )?;
        return Ok(());
    }
    write_human_table(entries, &mut render::out())?;
    Ok(())
}

fn list_json(entries: &[RemoteAlias]) -> serde_json::Value {
    let rows: Vec<ListEntryJson<'_>> = entries
        .iter()
        .map(|entry| ListEntryJson {
            name: &entry.name,
            target: &entry.target,
            reconnect: entry.reconnect,
            resume: entry.resume,
            mux: entry.mux.map(|mux| mux.as_str()),
            auto_forward: entry.auto_forward,
        })
        .collect();
    json!({ "remotes": rows })
}

fn write_human_table(entries: &[RemoteAlias], out: &mut impl Write) -> std::io::Result<()> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for entry in entries {
        let destination = RemoteTarget::parse(&entry.target)
            .ok()
            .map(|target| target.ssh_destination().as_str().to_owned());
        groups.entry(destination).or_default().push(entry);
    }
    for (destination, entries) in groups {
        let mut table = render::Table::new(["", "", ""]).without_headers().indent(2);
        table.section(destination.as_deref().unwrap_or("Invalid targets"));
        for entry in entries {
            let mut cells = vec![
                render::cell(entry.name.as_str()).fg(render::palette::accent()),
                render::cell(entry.target.as_str()).fg(render::palette::muted()),
            ];
            let settings = settings(entry);
            if !settings.is_empty() {
                cells.push(render::cell(settings).fg(render::palette::body()));
            }
            table.row(cells);
        }
        table.render(out)?;
    }
    Ok(())
}

pub(super) fn settings(entry: &RemoteAlias) -> String {
    let mut words = Vec::new();
    if let Some(mux) = entry.mux {
        words.push(mux.as_str());
    }
    if !entry.reconnect {
        words.push("no-reconnect");
    }
    if !entry.resume {
        words.push("no-resume");
    }
    if !entry.auto_forward {
        words.push("no-auto-forward");
    }
    words.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::MuxName;

    fn render_list_json(entries: &[RemoteAlias]) -> String {
        serde_json::to_string_pretty(&list_json(entries)).expect("rendered JSON serializes")
    }

    #[test]
    fn list_uses_positive_resume_and_omits_default_settings() {
        let entries = vec![RemoteAlias {
            name: "dev".to_owned(),
            target: "dev-box:room".to_owned(),
            reconnect: true,
            resume: true,
            mux: None,
            auto_forward: true,
        }];
        let json = list_json(&entries);
        assert_eq!(json["remotes"][0]["resume"], true);
        assert!(json["remotes"][0].get("no_resume").is_none());
        let mut buf = Vec::new();
        write_human_table(&entries, &mut anstream::StripStream::new(&mut buf)).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "\ndev-box\n  dev  dev-box:room\n"
        );
    }

    #[test]
    fn list_renderers_emit_public_shapes() {
        let entries = vec![
            RemoteAlias {
                name: "dev".to_owned(),
                target: "dev-box:query-engine".to_owned(),
                reconnect: true,
                resume: true,
                mux: None,
                auto_forward: true,
            },
            RemoteAlias {
                name: "prod".to_owned(),
                target: "agent@prod-box:~/code/query-engine".to_owned(),
                reconnect: false,
                resume: false,
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
              "reconnect": true,
              "resume": true,
              "target": "dev-box:query-engine"
            },
            {
              "auto_forward": false,
              "mux": "tmux",
              "name": "prod",
              "reconnect": false,
              "resume": false,
              "target": "agent@prod-box:~/code/query-engine"
            }
          ]
        }
        "#);
        let entries = vec![
            entries[0].clone(),
            RemoteAlias {
                name: "docs".to_owned(),
                target: "agent@prod-box:~/code/docs".to_owned(),
                ..entries[0].clone()
            },
            RemoteAlias {
                name: "invalid".to_owned(),
                target: "missing-target".to_owned(),
                ..entries[0].clone()
            },
            entries[1].clone(),
            RemoteAlias {
                name: "tools".to_owned(),
                target: "dev-box:workspace/tools".to_owned(),
                mux: Some(MuxName::Zellij),
                ..entries[0].clone()
            },
            RemoteAlias {
                name: "v6".to_owned(),
                target: "user@[::1]:~/code/with:colon".to_owned(),
                ..entries[0].clone()
            },
        ];
        // Render the table with ANSI stripped so the snapshot is the plain,
        // aligned text; `print` re-styles on the real stdout via `render::out`.
        let mut buf: Vec<u8> = Vec::new();
        write_human_table(&entries, &mut anstream::StripStream::new(&mut buf))
            .expect("table renders to an in-memory buffer");
        let rendered = String::from_utf8(buf).expect("table output is utf-8");
        insta::assert_snapshot!(rendered, @"

        Invalid targets
          invalid  missing-target

        agent@prod-box
          docs  agent@prod-box:~/code/docs
          prod  agent@prod-box:~/code/query-engine  tmux, no-reconnect, no-resume, no-auto-forward

        dev-box
          dev    dev-box:query-engine
          tools  dev-box:workspace/tools  zellij

        user@[::1]
          v6  user@[::1]:~/code/with:colon
        ");
    }
}
