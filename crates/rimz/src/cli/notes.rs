//! `rimz notes` — write and list Markdown notes in the home-wide note store.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use rimz::agents::single_line_description;
use rimz::harness::notes::{self, AddRequest, ListFilter, NotesErr};

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
    /// List notes by write time, oldest first.
    List {
        /// Keep notes of this kind.
        #[arg(long)]
        kind: Option<String>,
        /// Keep notes written at or after this RFC 3339 instant.
        #[arg(long, value_name = "INSTANT")]
        since: Option<Timestamp>,
        /// Keep notes whose author is this agent instance name.
        #[arg(long)]
        author: Option<String>,
        /// Keep notes written from this project root.
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
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
            kind,
            since,
            author,
            project,
            json,
        } => list(
            ListFilter {
                kind,
                since,
                author,
                project,
            },
            json,
        ),
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
    let added = notes::add(&request).map_err(usage_or_error)?;
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

fn list(filter: ListFilter, json: bool) -> Result<()> {
    let listing = notes::list(&filter).map_err(usage_or_error)?;
    let mut stderr = render::err();
    for (path, reason) in &listing.skipped {
        let _ = writeln!(
            stderr,
            "{} skipped {}: {reason}",
            render::paint(palette::warn().bold(), "warning:"),
            path.display()
        );
    }
    if json {
        return render::json_pretty(&listing.notes);
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
    render::finish(table.render(&mut render::out()))
}

/// Input the command line got wrong exits 2; everything else is a failure.
fn usage_or_error(error: NotesErr) -> anyhow::Error {
    match error {
        NotesErr::NoSlug { .. } | NotesErr::BadMeta { .. } | NotesErr::HeaderTooLarge { .. } => {
            UsageError::new(error.to_string()).into()
        }
        error => error.into(),
    }
}
