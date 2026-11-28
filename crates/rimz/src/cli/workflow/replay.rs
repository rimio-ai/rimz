//! `rimz workflow try`: a workflow script replayed over answers written by hand, which starts
//! nothing.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rimz::harness::block::{self, Failure};
use rimz::harness::workflow_script::{self, Caps, End, Log, Mark, Outcome, Pass, Progress};
use serde_json::{Value, json};

use super::render;
use crate::cli::usage::UsageError;

pub(super) fn run(path: &Path, args: Option<&str>, answers: Option<&Path>) -> Result<()> {
    let args = args
        .map(|raw| {
            serde_json::from_str::<Value>(raw)
                .map_err(|error| UsageError::new(format!("`--args` is not JSON: {error}")))
        })
        .transpose()?;
    let (script, dir) = if path.is_dir() {
        (path.join("workflow.js"), Some(path))
    } else {
        (path.to_path_buf(), None)
    };
    let text = std::fs::read_to_string(&script)
        .with_context(|| format!("reading {}", script.display()))?;
    let answers = match answers {
        Some(answers) => {
            let lines = std::fs::read_to_string(answers)
                .with_context(|| format!("reading {}", answers.display()))?;
            workflow_script::answers(&lines)
                .map_err(|reason| anyhow!("{}: {reason}", answers.display()))?
        }
        None => Vec::new(),
    };
    let log = Log::Answers(answers.clone());
    let name = script
        .file_name()
        .map_or_else(|| "workflow.js".into(), |name| name.to_string_lossy());
    let replay = workflow_script::replay(&Pass {
        name: &name,
        text: &text,
        args: args.as_ref(),
        log: &log,
        caps: Caps::PASS,
        dir,
        reader: None,
        covered: answers.len().checked_sub(1),
        steps: None,
    })
    .map_err(|reason| anyhow!("{}: {reason}", script.display()))?;
    let mut out = render::out();
    writeln!(out, "{}: {}", replay.meta.name, replay.meta.description)?;
    let mut marks = replay.marks.iter().peekable();
    for call in &replay.calls {
        write_marks(&mut out, &mut marks, call.number - 1)?;
        let summary = block::summary(call.block, &call.object);
        let gap = if summary.is_empty() { "" } else { " " };
        writeln!(out, "{} {}{gap}{summary}", call.number, call.block.as_str())?;
        let answer = usize::try_from(call.number - 1)
            .ok()
            .and_then(|index| answers.get(index))
            .filter(|_| !replay.open.contains(&call.number));
        match answer {
            Some(Outcome::Result(value)) => writeln!(out, "  answer {value}")?,
            Some(Outcome::Failure(failure)) => {
                writeln!(
                    out,
                    "  fails {}: {}",
                    failure.cause.as_str(),
                    failure.reason
                )?;
            }
            None => writeln!(out, "  no answer")?,
        }
    }
    write_marks(&mut out, &mut marks, u64::MAX)?;
    match &replay.end {
        End::Pending => {
            let first = replay.open.first().copied().unwrap_or_default();
            writeln!(
                out,
                "waits on call {first}, the first with no answer; answer it with a line like one of these:"
            )?;
            if let Some(call) = replay.calls.iter().find(|call| call.number == first) {
                let result = block::template(call.block, &call.object);
                writeln!(out, "  {}", json!({ "result": result }))?;
            }
            writeln!(out, "  {}", json!({ "failure": { "cause": "..." } }))?;
        }
        End::Returned(value) => writeln!(out, "returns {value}")?,
        End::Failed(failure) => writeln!(out, "fails{}", failed(failure))?,
        End::Threw(reason) => writeln!(out, "throws: {reason}")?,
    }
    if replay.used < answers.len() {
        writeln!(
            out,
            "answers {} to {} went unused: the script ran no further",
            replay.used + 1,
            answers.len()
        )?;
    }
    Ok(())
}

/// Each mark not written yet that the script made with at most `made` calls behind it, each line
/// of its text under its own prefix, so a line break in the text cannot pass for a call.
fn write_marks<'a>(
    out: &mut impl Write,
    marks: &mut std::iter::Peekable<impl Iterator<Item = &'a Mark>>,
    made: u64,
) -> std::io::Result<()> {
    while let Some(mark) = marks.next_if(|mark| mark.after <= made) {
        let (prefix, text): (_, &str) = match &mark.progress {
            Progress::Phase(title) => ("phase", title),
            Progress::Log(line) => ("log", line),
        };
        for line in text.split('\n') {
            writeln!(out, "{prefix} {line}")?;
        }
    }
    Ok(())
}

fn failed(failure: &Failure) -> String {
    let at = match (failure.step, failure.block) {
        (Some(step), Some(block)) => format!(" at call {step} ({})", block.as_str()),
        (Some(step), None) => format!(" at call {step}"),
        (None, Some(block)) => format!(" in a refused `{}` call", block.as_str()),
        (None, None) => String::new(),
    };
    format!("{at}: {}: {}", failure.cause.as_str(), failure.reason)
}
