//! `rimz resolver` — manage the per-machine resolver allowlist.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;

use super::GlobalFlags;
use rimz::resolver::allowlist::{Allowlist, AllowlistEntry};

const DEFAULT_ORDER: u32 = 10;

#[derive(Debug, Args)]
pub struct ResolverArgs {
    #[command(subcommand)]
    command: ResolverSubcmd,
}

#[derive(Debug, Subcommand)]
enum ResolverSubcmd {
    /// Enrol a resolver on the allowlist.
    Add {
        id: String,
        #[arg(long, default_value_t = DEFAULT_ORDER)]
        order: u32,
        #[arg(long, default_value = "30s", value_parser = parse_budget)]
        budget: Duration,
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Remove a resolver from the allowlist.
    Remove { id: String },
    /// List enrolled resolvers, sorted by chain order.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Move a resolver to a new position relative to another.
    Reorder {
        id: String,
        /// Place `id` immediately before this resolver.
        #[arg(long, group = "pivot")]
        before: Option<String>,
        /// Place `id` immediately after this resolver.
        #[arg(long, group = "pivot")]
        after: Option<String>,
    },
}

pub fn run(args: ResolverArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        ResolverSubcmd::Add {
            id,
            order,
            budget,
            binary,
            display_name,
        } => {
            let id = id.parse().context("parsing resolver id")?;
            let mut list = Allowlist::load().context("loading allowlist")?;
            list.add(AllowlistEntry {
                id,
                order,
                budget_seconds: budget.as_secs(),
                binary,
                display_name,
            })?;
            list.save().context("saving allowlist")?;
            Ok(())
        }
        ResolverSubcmd::Remove { id } => {
            let id = id.parse().context("parsing resolver id")?;
            let mut list = Allowlist::load().context("loading allowlist")?;
            list.remove(&id)?;
            list.save().context("saving allowlist")?;
            Ok(())
        }
        ResolverSubcmd::List { json } => {
            let list = Allowlist::load().context("loading allowlist")?;
            print_list(list.entries(), json);
            Ok(())
        }
        ResolverSubcmd::Reorder { id, before, after } => {
            let id = id.parse().context("parsing resolver id")?;
            let mut list = Allowlist::load().context("loading allowlist")?;
            match (before, after) {
                (Some(b), None) => {
                    list.reorder_before(&id, &b.parse().context("parsing --before id")?)?
                }
                (None, Some(a)) => {
                    list.reorder_after(&id, &a.parse().context("parsing --after id")?)?
                }
                (None, None) => bail!("`reorder` needs `--before <id>` or `--after <id>`"),
                (Some(_), Some(_)) => unreachable!("clap group enforces exclusivity"),
            }
            list.save().context("saving allowlist")?;
            Ok(())
        }
    }
}

fn parse_budget(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
}

#[derive(Serialize)]
struct ListEntryJson<'a> {
    id: &'a str,
    order: u32,
    budget_seconds: u64,
    binary: Option<&'a str>,
    display_name: Option<&'a str>,
}

fn print_list(entries: &[AllowlistEntry], json: bool) {
    if json {
        let rows: Vec<ListEntryJson<'_>> = entries
            .iter()
            .map(|e| ListEntryJson {
                id: e.id.as_str(),
                order: e.order,
                budget_seconds: e.budget_seconds,
                binary: e.binary.as_deref().and_then(std::path::Path::to_str),
                display_name: e.display_name.as_deref(),
            })
            .collect();
        let rendered = serde_json::to_string_pretty(&json!({ "resolvers": rows }))
            .expect("rendered JSON serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return;
    }
    if entries.is_empty() {
        return;
    }
    for entry in entries {
        let binary = entry
            .binary
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_owned());
        #[expect(clippy::print_stdout, reason = "human listing")]
        {
            println!(
                "{:>4}\t{}\t{}s\t{}",
                entry.order, entry.id, entry.budget_seconds, binary,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(id: &str, order: u32, secs: u64) -> AllowlistEntry {
        AllowlistEntry {
            id: id.parse().unwrap(),
            order,
            budget_seconds: secs,
            binary: None,
            display_name: None,
        }
    }

    #[test]
    fn parse_budget_accepts_short_units() {
        assert_eq!(parse_budget("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_budget("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_budget("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_budget("30").is_err());
        assert!(parse_budget("30d").is_err());
    }

    #[test]
    fn list_json_emits_canonical_shape() {
        let entries = vec![
            entry("opus-policy", 10, 30),
            entry("slack-on-call", 20, 300),
        ];
        let rendered = render_list_json(&entries);
        insta::assert_snapshot!(rendered, @r#"
        {
          "resolvers": [
            {
              "binary": null,
              "budget_seconds": 30,
              "display_name": null,
              "id": "opus-policy",
              "order": 10
            },
            {
              "binary": null,
              "budget_seconds": 300,
              "display_name": null,
              "id": "slack-on-call",
              "order": 20
            }
          ]
        }
        "#);
    }

    #[test]
    fn list_human_emits_tab_separated_rows() {
        let entries = vec![entry("opus-policy", 10, 30)];
        let rendered = render_list_human(&entries);
        insta::assert_snapshot!(rendered, @"  10	opus-policy	30s	-");
    }

    fn render_list_json(entries: &[AllowlistEntry]) -> String {
        let rows: Vec<ListEntryJson<'_>> = entries
            .iter()
            .map(|e| ListEntryJson {
                id: e.id.as_str(),
                order: e.order,
                budget_seconds: e.budget_seconds,
                binary: e.binary.as_deref().and_then(std::path::Path::to_str),
                display_name: e.display_name.as_deref(),
            })
            .collect();
        serde_json::to_string_pretty(&json!({ "resolvers": rows })).unwrap()
    }

    fn render_list_human(entries: &[AllowlistEntry]) -> String {
        let mut buf = String::new();
        for entry in entries {
            let binary = entry
                .binary
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".to_owned());
            use std::fmt::Write;
            writeln!(
                buf,
                "{:>4}\t{}\t{}s\t{}",
                entry.order, entry.id, entry.budget_seconds, binary,
            )
            .unwrap();
        }
        buf.trim_end().to_owned()
    }
}
