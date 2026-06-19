//! `rimz list-themes` — print the bundled sidebar theme names.
//!
//! The names are the keys of the embedded Alacritty catalog (sorted), each
//! usable verbatim as `[theme] scheme` or `rimz config set
//! theme.scheme <name>`. `--json` emits the same list as an array for
//! scripting.

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use crate::cli::render;
use rimz::sidebar_pane::render::scheme;

#[derive(Debug, Args)]
pub struct ListThemesArgs {
    /// Emit machine-readable JSON instead of one name per line.
    #[arg(long)]
    json: bool,
}

pub fn run(args: ListThemesArgs, _globals: &GlobalFlags) -> Result<()> {
    let names = scheme::available_scheme_names();
    if args.json {
        let rendered = serde_json::to_string_pretty(&names).expect("theme name vec serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }
    use std::io::Write;
    let mut out = render::out();
    let accent = render::palette::ACCENT;
    for name in &names {
        writeln!(out, "{}{name}{}", accent.render(), accent.render_reset())?;
    }
    Ok(())
}
