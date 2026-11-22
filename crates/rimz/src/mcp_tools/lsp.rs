//! `lsp`: the job of `rimz lsp <verb>`, answered with the text the command
//! prints.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use super::Tools;
use crate::lsp::check::Mode;
use crate::lsp::invoke::{self, QueryContext};
use crate::lsp::query::{ListOptions, Scope, Verb};
use crate::mcp::catalog::{LspArgs, LspVerb};
use crate::mcp::{Answer, Failure};

pub(super) fn run(tools: &Tools, args: &LspArgs) -> Result<Answer, Failure> {
    let dir = tools.dir(args.cwd.as_deref())?;
    let context = QueryContext::open(&dir, None, tools.pinned_room.clone())?;
    let server = args.server();
    context
        .validate_server(server)
        .map_err(|err| Failure::new(err.to_string()))?;
    let mut text = String::new();
    let emit = |chunk: &str| {
        text.push_str(chunk);
        Ok(())
    };
    let answered = match args.verb {
        LspVerb::Show => {
            let shown = invoke::show(&context, &args.targets, args.full, server)
                .map_err(|err| Failure::new(err.to_string()))?;
            return match shown.error {
                Some(error) => Err(Failure::new(error)),
                None => Ok(Answer::Text(shown.text)),
            };
        }
        LspVerb::Check => {
            let mode = match (args.fix, args.hints) {
                (false, _) => Mode::Check,
                (true, false) => Mode::Fix,
                (true, true) => Mode::FixHints,
            };
            let files: Vec<PathBuf> = args.targets.iter().map(PathBuf::from).collect();
            invoke::check(&context, &files, false, mode, emit)?
        }
        verb => invoke::query(
            &context,
            query_verb(verb),
            &args.targets,
            server,
            false,
            options(args),
            emit,
        )?,
    };
    match answered.error {
        Some(error) => Err(Failure::new(error.to_string())),
        None => Ok(Answer::Text(text)),
    }
}

fn query_verb(verb: LspVerb) -> Verb {
    match verb {
        LspVerb::Def => Verb::Def,
        LspVerb::Hover => Verb::Hover,
        LspVerb::Refs => Verb::Refs,
        LspVerb::Callers => Verb::Callers,
        LspVerb::Callees => Verb::Callees,
        LspVerb::Impl => Verb::Impl,
        LspVerb::Symbols => Verb::Symbols,
        LspVerb::Find | LspVerb::Show | LspVerb::Check => Verb::Find,
    }
}

/// The scope and limit a listing verb takes, as `rimz lsp` sets them from
/// its flags; every other verb reads the checkout.
fn options(args: &LspArgs) -> ListOptions {
    let listing = matches!(
        args.verb,
        LspVerb::Refs | LspVerb::Callers | LspVerb::Impl | LspVerb::Callees | LspVerb::Find
    );
    if !listing {
        return Scope::Checkout.into();
    }
    let scope = if args.external {
        Scope::External
    } else {
        Scope::Checkout
    };
    let mut options = ListOptions::from(scope);
    options.limit = if args.all {
        None
    } else {
        args.limit
            .and_then(|limit| NonZeroUsize::new(usize::try_from(limit).ok()?))
            .or(options.limit)
    };
    options.no_tests = matches!(args.verb, LspVerb::Refs | LspVerb::Callers) && args.no_tests;
    options
}
