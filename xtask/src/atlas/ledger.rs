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

use std::collections::BTreeMap;
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
struct Intent {
    from: String,
    to: String,
    intent: String,
}

/// One `holds` verdict: the module in survey-rank spelling (`store/snapshot`),
/// the SHA reviewed, and the scoped-commit count that reopens it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Hold {
    module: String,
    sha: Option<String>,
    reopen_at: usize,
    line: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(super) struct Ledger {
    intents: Vec<Intent>,
    holds: Vec<Hold>,
    /// Rows atlas could not read or resolve.
    pub(super) problems: Vec<String>,
    pub(super) restamps: Vec<Restamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Restamp {
    pub(super) module: String,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) line: usize,
}

pub(super) fn write_restamps(root: &Path, restamps: &[Restamp]) -> Result<()> {
    if restamps.is_empty() {
        return Ok(());
    }
    let path = root.join(LEDGER_FILE);
    let changed =
        || format!("{LEDGER_FILE} changed since the survey read it; rerun atlas survey --restamp");
    let raw = std::fs::read_to_string(&path).with_context(changed)?;
    let rows = table_rows(&raw, VERDICTS_HEADING);
    let mut rewritten = raw.clone();
    for restamp in restamps.iter().rev() {
        let (_, cells) = rows
            .iter()
            .find(|(line, _)| *line == restamp.line)
            .with_context(changed)?;
        if cells.first().map(|cell| strip_code(cell)).as_deref() != Some(&restamp.module)
            || !cells
                .get(1)
                .is_some_and(|cell| cell.starts_with(HOLDS_STATUS))
            || cells.get(2).map(|cell| strip_code(cell)).as_deref() != Some(&restamp.from)
        {
            bail!("{}", changed());
        }
        let mut lines = raw.split_inclusive('\n');
        let line_start: usize = lines.by_ref().take(restamp.line - 1).map(str::len).sum();
        let line = lines.next().with_context(changed)?;
        let cell_start: usize = line.split('|').take(3).map(|cell| cell.len() + 1).sum();
        let cell = line.split('|').nth(3).with_context(changed)?;
        let start = line_start + cell_start + cell.find(&restamp.from).with_context(changed)?;
        rewritten.replace_range(start..start + restamp.from.len(), &restamp.to);
    }
    std::fs::write(&path, rewritten).with_context(|| format!("writing {}", path.display()))
}

/// Reads and resolves every hold in the repository ledger, regardless of survey scope.
pub(super) fn load(root: &Path) -> Result<Option<Ledger>> {
    let path = root.join(LEDGER_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut ledger = parse(&text);
    let mut resolutions = BTreeMap::new();
    for hold in &mut ledger.holds {
        let Some(from) = hold.sha.take() else {
            continue;
        };
        match resolutions
            .entry(from.clone())
            .or_insert_with(|| resolve(root, &from).map_err(|error| format!("{error:#}")))
        {
            Ok(to) => {
                hold.sha = Some(to.clone());
                if *to != from {
                    ledger.restamps.push(Restamp {
                        module: hold.module.clone(),
                        from,
                        to: to.clone(),
                        line: hold.line,
                    });
                }
            }
            Err(error) => ledger
                .problems
                .push(format!("`{}` holds at {from}: {error}", hold.module)),
        }
    }
    Ok(Some(ledger))
}

fn parse(text: &str) -> Ledger {
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
                sha: Some(sha),
                reopen_at,
                line,
            }),
            _ => ledger.problems.push(format!(
                "{LEDGER_FILE}:{line}: `{module}` holds without a sha and a reopen count"
            )),
        }
    }
    ledger
}

impl Ledger {
    pub(super) fn row_counts(&self) -> (usize, usize) {
        (self.intents.len(), self.holds.len())
    }

    /// Whether the module's hold has reopened: `None` when no resolved hold
    /// covers it, else whether its commits since the review reached the count.
    pub(super) fn reopened(
        &self,
        root: &Path,
        module: &str,
        paths: &[&Path],
    ) -> Result<Option<bool>> {
        let Some(hold) = self.hold_for(module) else {
            return Ok(None);
        };
        let Some(sha) = &hold.sha else {
            return Ok(None);
        };
        let commits = commits_since(root, sha, paths)
            .with_context(|| format!("`{module}` holds at {sha}"))?;
        Ok(Some(commits >= hold.reopen_at))
    }

    /// The ledger row that reviews the edge `from → to`, both crate module
    /// paths: the row whose `from` contains the importing module and whose
    /// `to` contains the provider, most specific `to` first.
    pub(super) fn intent_for(&self, from: &str, to: &str) -> Option<(&str, &str)> {
        self.intents
            .iter()
            .filter(|intent| {
                module_is_within(from, &intent.from) && module_is_within(to, &intent.to)
            })
            .max_by_key(|intent| (intent.to.len(), intent.from.len()))
            .map(|intent| (intent.to.as_str(), intent.intent.as_str()))
    }

    fn hold_for(&self, module: &str) -> Option<&Hold> {
        self.holds.iter().find(|hold| hold.module == module)
    }
}

fn resolve(root: &Path, sha: &str) -> Result<String> {
    let ancestry = Command::new("git")
        .args(["merge-base", "--is-ancestor", sha, "HEAD"])
        .current_dir(root)
        .output()
        .context("running git merge-base for the ledger")?;
    if ancestry.status.success() {
        return Ok(sha.to_owned());
    }
    let history = Command::new("git")
        .args([
            "log",
            "--reverse",
            "--format=%h",
            &format!("--abbrev={}", sha.len()),
            &format!("-S{sha}"),
            "HEAD",
            "--",
            LEDGER_FILE,
        ])
        .current_dir(root)
        .output()
        .context("running git log for ledger SHA resolution")?;
    if history.status.success()
        && let Some(hit) = String::from_utf8_lossy(&history.stdout).lines().next()
    {
        return Ok(hit.to_owned());
    }
    match ancestry.status.code() {
        Some(1) => bail!("{sha} is not an ancestor of HEAD"),
        _ => bail!(
            "git merge-base --is-ancestor {sha} HEAD failed: {}",
            String::from_utf8_lossy(&ancestry.stderr).trim()
        ),
    }
}

/// Commits touching the module after its resolved, HEAD-reachable review commit.
fn commits_since(root: &Path, sha: &str, paths: &[&Path]) -> Result<usize> {
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
                sha: Some("abc1234".to_owned()),
                reopen_at: 30,
                line: 10,
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
                .map(|(_, intent)| intent),
            Some("keep")
        );
        assert_eq!(
            ledger
                .intent_for("harness::schedule", "sidebar::refresh::pr")
                .map(|(to, _)| to),
            Some("sidebar::refresh::pr")
        );
        assert_eq!(
            ledger
                .intent_for("harness", "sidebar::refresh::account")
                .map(|(to, _)| to),
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

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(root: &Path) -> String {
        git(root, &["add", "."]);
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "record",
            ],
        );
        git(root, &["rev-parse", "--short=9", "HEAD"])
    }

    fn assert_resolution(shape: &str) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/demo.rs"), "// base\n").unwrap();
        commit(root);
        let from = if shape == "rebase" {
            let branch = git(
                root,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    "HEAD",
                    "-m",
                    "branch",
                ],
            );
            git(root, &["rev-parse", "--short=9", &branch])
        } else {
            "deadbeef0".to_owned()
        };
        let text = format!(
            "## Module verdicts\n\n| module | status | sha | reopen at |\n| --- | --- | --- | --- |\n| `demo` | holds | {from} | 2 |\n"
        );
        let path = root.join(LEDGER_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        if shape == "squash" {
            std::fs::write(root.join("src/demo.rs"), "// reviewed in squash\n").unwrap();
        }
        let to = commit(root);
        if shape == "oldest" {
            std::fs::write(&path, format!("{text}\nLater prose also names {from}.\n")).unwrap();
            commit(root);
        }
        std::fs::write(root.join("src/demo.rs"), "// later\n").unwrap();
        commit(root);
        let ledger = load(root).unwrap().unwrap();
        assert!(ledger.problems.is_empty(), "{:?}", ledger.problems);
        assert_eq!(
            ledger.restamps,
            [Restamp {
                module: "demo".to_owned(),
                from,
                to,
                line: 5
            }]
        );
        assert_eq!(
            ledger
                .reopened(root, "demo", &[Path::new("src/demo.rs")])
                .unwrap(),
            Some(false),
            "only the later module commit counts"
        );
        std::fs::write(root.join("src/demo.rs"), "// later again\n").unwrap();
        commit(root);
        assert_eq!(
            ledger
                .reopened(root, "demo", &[Path::new("src/demo.rs")])
                .unwrap(),
            Some(true)
        );
    }

    #[test]
    fn resolves_rebased_sha_to_the_record_commit() {
        assert_resolution("rebase");
    }

    #[test]
    fn resolves_squashed_sha_without_counting_the_squash() {
        assert_resolution("squash");
    }

    #[test]
    fn resolves_a_committed_sha_without_its_object() {
        assert_resolution("fresh");
    }

    #[test]
    fn resolves_to_the_oldest_occurrence_count_change() {
        assert_resolution("oldest");
    }

    fn rewrite_fixture() -> (tempfile::TempDir, String, Vec<Restamp>) {
        let dir = tempfile::tempdir().unwrap();
        let raw = "Prose deadbeef0\r\n\r\n## Module verdicts\r\n\r\n| module | status | sha | reopen at | note |\r\n| --- | --- | --- | --- | --- |\r\n  | `demo` | holds |  `deadbeef0`  | 2 | deadbeef0 |\r\n| other | holds | deadbeef0 | 3 | keep |\r\n| landed | landed | deadbeef0 | 2 | keep |\r\n\r\nTail without newline".to_owned();
        let path = dir.path().join(LEDGER_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let restamps = [("demo", 7), ("other", 8)]
            .map(|(module, line)| Restamp {
                module: module.to_owned(),
                from: "deadbeef0".to_owned(),
                to: "123456789".to_owned(),
                line,
            })
            .to_vec();
        (dir, raw, restamps)
    }

    #[test]
    fn restamp_changes_only_targeted_sha_cell_bytes() {
        let (dir, raw, restamps) = rewrite_fixture();
        write_restamps(dir.path(), &restamps).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LEDGER_FILE)).unwrap(),
            raw.replace("`deadbeef0`", "`123456789`")
                .replace("other | holds | deadbeef0", "other | holds | 123456789")
        );
    }

    #[test]
    fn restamp_refuses_a_stale_cell_without_writing_any_cells() {
        let (dir, raw, restamps) = rewrite_fixture();
        let changed = raw.replace("other | holds | deadbeef0", "other | holds | abcdef012");
        let path = dir.path().join(LEDGER_FILE);
        std::fs::write(&path, &changed).unwrap();
        let result = write_restamps(dir.path(), &restamps);
        assert!(
            result.is_err(),
            "a stale cell must refuse the entire rewrite"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changed since the survey read it; rerun atlas survey --restamp")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), changed);
    }
}
