//! The refactor ledger's review tables, read from its Markdown so `survey`
//! can tell a reviewed admission from an unreviewed one and a held module
//! from a fresh candidate.
//!
//! The ledger is prose owned by people; atlas reads two of its tables and
//! nothing else. `## Admission intents` rows carry one upward edge each,
//! spelled `` `from` → `to` `` with an optional `to::{a,b}` brace group;
//! `## Module verdicts` rows carry a module at survey-rank granularity, a
//! status, the SHA a `holds` verdict reviewed, and the scoped-commit count
//! that reopens it. Anything the parser cannot read lands in `problems`
//! rather than failing the survey: a malformed ledger row is a finding, not
//! a crash.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::module_is_within;

pub(super) const LEDGER_FILE: &str = "docs/contributing/refactor-ledger.md";

const INTENTS_HEADING: &str = "## Admission intents";
const VERDICTS_HEADING: &str = "## Module verdicts";
const HOLDS_STATUS: &str = "holds";

/// One reviewed upward edge: `from` and `to` are crate module paths as the
/// ledger spells them, `intent` the verdict cell verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Intent {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) intent: String,
}

/// One `holds` verdict: the module in survey-rank spelling (`store/snapshot`),
/// the SHA reviewed, and the scoped-commit count that reopens it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Hold {
    pub(super) module: String,
    pub(super) sha: String,
    pub(super) reopen_at: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(super) struct Ledger {
    pub(super) intents: Vec<Intent>,
    pub(super) holds: Vec<Hold>,
    /// Rows atlas could not read, each with the line that carries it.
    pub(super) problems: Vec<String>,
}

/// Reads the ledger at `path`; `None` when the repository keeps no ledger,
/// in which case every admission reads as unreviewed.
pub(super) fn load(path: &Path) -> Result<Option<Ledger>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(parse(&text)))
}

pub(super) fn parse(text: &str) -> Ledger {
    let mut ledger = Ledger::default();
    for (line, cells) in table_rows(text, INTENTS_HEADING) {
        let Some(edge) = cells.first() else {
            continue;
        };
        let Some(intent) = cells.get(2) else {
            ledger.problems.push(format!(
                "{LEDGER_FILE}:{line}: admission row has no intent cell"
            ));
            continue;
        };
        let Some((from, to)) = edge.split_once('→') else {
            ledger.problems.push(format!(
                "{LEDGER_FILE}:{line}: admission edge `{edge}` has no `→`"
            ));
            continue;
        };
        let from = strip_code(from);
        for to in expand_braces(&strip_code(to)) {
            ledger.intents.push(Intent {
                from: from.clone(),
                to,
                intent: intent.clone(),
            });
        }
    }
    for (line, cells) in table_rows(text, VERDICTS_HEADING) {
        let (Some(module), Some(status)) = (cells.first(), cells.get(1)) else {
            continue;
        };
        if !status.starts_with(HOLDS_STATUS) {
            continue;
        }
        let module = strip_code(module);
        let sha = cells
            .get(2)
            .map(|cell| strip_code(cell))
            .unwrap_or_default();
        let reopen_at = cells
            .get(3)
            .and_then(|cell| cell.split_whitespace().next())
            .and_then(|count| count.parse::<usize>().ok());
        match (sha.is_empty() || sha == "—", reopen_at) {
            (false, Some(reopen_at)) => ledger.holds.push(Hold {
                module,
                sha,
                reopen_at,
            }),
            _ => ledger.problems.push(format!(
                "{LEDGER_FILE}:{line}: `{module}` holds without a sha and a reopen count"
            )),
        }
    }
    ledger
}

impl Ledger {
    /// The ledger row that reviews the edge `from → to`, both crate module
    /// paths: the row whose `from` contains the importing module and whose
    /// `to` contains the provider, most specific `to` first.
    pub(super) fn intent_for(&self, from: &str, to: &str) -> Option<&Intent> {
        self.intents
            .iter()
            .filter(|intent| {
                module_is_within(from, &intent.from) && module_is_within(to, &intent.to)
            })
            .max_by_key(|intent| (intent.to.len(), intent.from.len()))
    }

    pub(super) fn hold_for(&self, module: &str) -> Option<&Hold> {
        self.holds.iter().find(|hold| hold.module == module)
    }
}

/// Commits that touched `paths` after `sha`, the ledger's reopen measure
/// (`git log --oneline <sha>.. -- <path> | wc -l`).
pub(super) fn commits_since(root: &Path, sha: &str, paths: &[&Path]) -> Result<usize> {
    let mut command = Command::new("git");
    command
        .args(["rev-list", "--count", &format!("{sha}..HEAD"), "--"])
        .args(paths)
        .current_dir(root);
    let output = command
        .output()
        .context("running git rev-list for the ledger")?;
    if !output.status.success() {
        bail!(
            "git rev-list {sha}..HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("git rev-list --count printed no number")
}

/// Body rows of the first Markdown table under `heading`, each as its
/// trimmed cells with the 1-based line that carries it. The header and the
/// `---` separator are skipped; the table ends at the first non-row line.
fn table_rows(text: &str, heading: &str) -> Vec<(usize, Vec<String>)> {
    let mut lines = text.lines().enumerate();
    if !lines.any(|(_, line)| line.trim() == heading) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut in_table = false;
    for (index, line) in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            if in_table {
                break;
            }
            continue;
        }
        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_owned())
            .collect::<Vec<_>>();
        if !in_table {
            // Header row, then the separator on the next line.
            in_table = true;
            continue;
        }
        if cells
            .iter()
            .all(|cell| cell.trim_matches(':').chars().all(|ch| ch == '-'))
        {
            continue;
        }
        rows.push((index + 1, cells));
    }
    rows
}

fn strip_code(cell: &str) -> String {
    cell.trim().trim_matches('`').trim().to_owned()
}

/// `sidebar::{heartbeat,timing}` → `sidebar::heartbeat`, `sidebar::timing`;
/// anything without a brace group passes through whole.
fn expand_braces(module: &str) -> Vec<String> {
    let Some((prefix, rest)) = module.split_once('{') else {
        return vec![module.to_owned()];
    };
    let Some((group, suffix)) = rest.split_once('}') else {
        return vec![module.to_owned()];
    };
    group
        .split(',')
        .map(|member| format!("{prefix}{}{suffix}", member.trim()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = "# Ledger

## Module verdicts

Prose about holds.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `store` | landed pass-1 | — | — | seam reviewed |
| `store/snapshot` | holds | abc1234 | 30 commits | reviewed in pass 9 |
| `config` | holds | — | 30 | forgot the sha |

## Admission intents

Prose about intents.

| from → to | sites at baseline | intent | reason / seam |
| --- | ---: | --- | --- |
| `store` → `agents` | 134 | keep | intended direction |
| `store` → `sidebar::{heartbeat,timing,wakeup}` | 4 | closed | pass 4 |
| `store` → `diag::record` | — | keep | cycle |
| `harness` → `sidebar::refresh` | 2 | keep | account-cache writers |
| `harness` → `sidebar::refresh::pr` | 4 | closed | pass 8 |
| broken row without an arrow | 1 | keep | typo |

## Pass log
";

    #[test]
    fn parses_intents_with_brace_groups_and_reports_malformed_rows() {
        let ledger = parse(LEDGER);

        let edges = ledger
            .intents
            .iter()
            .map(|intent| format!("{} → {} {}", intent.from, intent.to, intent.intent))
            .collect::<Vec<_>>();
        assert_eq!(
            edges,
            [
                "store → agents keep",
                "store → sidebar::heartbeat closed",
                "store → sidebar::timing closed",
                "store → sidebar::wakeup closed",
                "store → diag::record keep",
                "harness → sidebar::refresh keep",
                "harness → sidebar::refresh::pr closed",
            ]
        );
        assert_eq!(ledger.problems.len(), 2, "{:?}", ledger.problems);
        assert!(ledger.problems[0].contains("has no `→`"));
        assert!(ledger.problems[1].contains("`config` holds without a sha"));
    }

    #[test]
    fn holds_need_a_sha_and_a_reopen_count() {
        let ledger = parse(LEDGER);

        assert_eq!(
            ledger.holds,
            [Hold {
                module: "store/snapshot".to_owned(),
                sha: "abc1234".to_owned(),
                reopen_at: 30,
            }]
        );
        assert!(ledger.hold_for("store/snapshot").is_some());
        assert!(ledger.hold_for("store").is_none());
    }

    #[test]
    fn intent_lookup_matches_module_prefixes_and_prefers_the_specific_edge() {
        let ledger = parse(LEDGER);

        assert_eq!(
            ledger
                .intent_for("store::snapshot", "agents::catalog")
                .map(|i| i.intent.as_str()),
            Some("keep")
        );
        assert_eq!(
            ledger
                .intent_for("harness::schedule", "sidebar::refresh::pr")
                .map(|i| i.to.as_str()),
            Some("sidebar::refresh::pr")
        );
        assert_eq!(
            ledger
                .intent_for("harness", "sidebar::refresh::account")
                .map(|i| i.to.as_str()),
            Some("sidebar::refresh")
        );
        assert!(ledger.intent_for("store", "diag").is_none());
        assert!(ledger.intent_for("message", "harness::spec").is_none());
    }

    #[test]
    fn a_missing_ledger_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(load(&dir.path().join("missing.md")).unwrap(), None);
    }
}
