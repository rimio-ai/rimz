//! `rimz notes` — write and list Markdown notes in the home-wide note store.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rimz::agents::single_line_description;
use rimz::harness::notes::{self, AddRequest, ListRequest, NotesErr};

use super::render::{self, Table, cell, palette};
use super::usage::UsageError;

#[derive(Debug, Args)]
pub struct NotesArgs {
    #[command(subcommand)]
    command: NotesSubcmd,
}

#[derive(Debug, Subcommand)]
enum NotesSubcmd {
    /// Save stdin, or --file, as a note and print its absolute path.
    Add {
        /// The note's kind, which names its folder (handoff, reflection, ...).
        kind: String,
        /// The note's title, which names its file.
        #[arg(allow_hyphen_values = true)]
        title: String,
        /// Read the body from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Wrap the body in one fenced code block, for a log or other text.
        #[arg(long)]
        fence: bool,
        /// Add a header key of your own (repeatable); RimZ's own keys win.
        #[arg(long, value_name = "KEY=VALUE")]
        meta: Vec<String>,
    },
    /// List the newest matching notes by write time, oldest first.
    List {
        /// Keep notes matching `field=value` terms; repeatable, `--filter help` lists fields.
        #[arg(long = "filter", value_name = "QUERY")]
        filters: Vec<String>,
        /// Print one absolute path per listed note and nothing else.
        #[arg(short = 'q', long, conflicts_with = "json")]
        quiet: bool,
        /// Show the newest N matches; 0 shows every match.
        #[arg(short = 'n', long, value_name = "N", default_value_t = notes::LIST_DEFAULT)]
        limit: usize,
        /// Print a JSON array instead of the table.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: NotesArgs, globals: &super::GlobalFlags) -> Result<()> {
    match args.command {
        NotesSubcmd::Add {
            kind,
            title,
            file,
            fence,
            meta,
        } => add(
            AddRequest {
                kind,
                title,
                body: Vec::new(),
                fence,
                meta,
                root: globals.root.clone(),
            },
            file,
        ),
        NotesSubcmd::List {
            filters,
            quiet,
            limit,
            json,
        } => list(ListRequest { filters, limit }, json, quiet),
    }
}

fn add(mut request: AddRequest, file: Option<PathBuf>) -> Result<()> {
    request.body = match &file {
        Some(path) => std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        None => {
            let mut body = Vec::new();
            std::io::stdin()
                .read_to_end(&mut body)
                .context("reading the note body from stdin")?;
            body
        }
    };
    let added = notes::add(&request, Path::new(".")).map_err(usage_or_error)?;
    let mut stderr = render::err();
    for key in &added.dropped_meta {
        let _ = writeln!(
            stderr,
            "{} --meta {key} dropped: RimZ writes that header key itself",
            render::paint(palette::warn().bold(), "warning:"),
        );
    }
    if let Some(error) = &added.unresolved_project {
        let _ = writeln!(
            stderr,
            "{} no project header: {error}",
            render::paint(palette::warn().bold(), "warning:"),
        );
    }
    render::finish(writeln!(std::io::stdout(), "{}", added.path.display()))
}

fn list(request: ListRequest, json: bool, quiet: bool) -> Result<()> {
    if notes::asks_filter_help(&request.filters) {
        return render::finish(render_filter_help(&mut render::out()));
    }
    let listing = notes::list(&request).map_err(usage_or_error)?;
    let mut stderr = render::err();
    let warning = render::paint(palette::warn().bold(), "warning:");
    for (path, reason) in &listing.skipped {
        let _ = writeln!(stderr, "{warning} skipped {}: {reason}", path.display());
    }
    for key in &listing.unknown_keys {
        let _ = writeln!(
            stderr,
            "{warning} no note carries the key `{key}`, so nothing matches"
        );
    }
    let hidden = (listing.hidden > 0).then(|| {
        let unit = if listing.hidden == 1 { "note" } else { "notes" };
        format!(
            "{} older {unit} not shown; pass -n to see more",
            listing.hidden
        )
    });
    if json || quiet {
        if json {
            render::json_pretty(&listing.notes)?;
        } else {
            let mut out = render::out();
            for note in &listing.notes {
                render::finish(writeln!(out, "{}", note.path.display()))?;
            }
        }
        if let Some(hidden) = hidden {
            let _ = writeln!(stderr, "{hidden}");
        }
        return Ok(());
    }
    let mut table = Table::new(["WRITTEN", "KIND", "AUTHOR", "TITLE", "PATH"]);
    for note in &listing.notes {
        // A header value read back may hold a line break or a control character.
        let key = |name: &str| {
            note.keys
                .get(name)
                .and_then(|value| single_line_description(value))
                .unwrap_or_else(|| "-".into())
        };
        table.row([
            cell(key("written")),
            cell(key("kind")),
            cell(key("author")),
            cell(key("title")),
            cell(note.path.display().to_string()),
        ]);
    }
    let mut out = render::out();
    render::finish(table.render(&mut out))?;
    match hidden {
        Some(hidden) => render::finish(writeln!(out, "{hidden}")),
        None => Ok(()),
    }
}

/// `--filter help`: every field a note filter names, then the grammar in brief.
fn render_filter_help(w: &mut impl Write) -> std::io::Result<()> {
    let mut table = Table::new(["FIELD", "TYPE", "VALUES"]);
    for field in notes::filter_fields() {
        table.row([cell(field.field), cell(field.kind), cell(field.values)]);
    }
    table.render(w)?;
    writeln!(w)?;
    for line in notes::FILTER_GRAMMAR {
        writeln!(w, "{line}")?;
    }
    Ok(())
}

/// Input the command line got wrong exits 2; everything else is a failure.
fn usage_or_error(error: NotesErr) -> anyhow::Error {
    match error {
        NotesErr::NoSlug { .. }
        | NotesErr::BadMeta { .. }
        | NotesErr::HeaderTooLarge { .. }
        | NotesErr::EmptyFilter
        | NotesErr::Term(_)
        | NotesErr::BadTerm { .. } => UsageError::new(error.to_string()).into(),
        error => error.into(),
    }
}
