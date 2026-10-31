use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::super::{Check, SpecErr};
use crate::disk::paths::StatePaths;
use crate::harness::schedule::{
    self,
    when::{self, CiSource, WhenExpr, WindowReadings},
};
use crate::store::workflow::{RunRecord, Wait};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMark {
    False,
    Winner,
    Holding,
    Unread,
    NotReached,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CaseMarks {
    pub cases: Vec<CaseMark>,
    pub else_mark: CaseMark,
}

pub(super) fn evaluate(
    state: &StatePaths,
    check: &Check,
    record: &RunRecord,
) -> Result<Value, String> {
    let runtime = crate::RuntimePaths::for_state(state).map_err(|error| error.to_string())?;
    let source = CiSource::read(&runtime);
    let now = Timestamp::now();
    let windows = WindowReadings::new(Some(&runtime), now);
    let expressions = check
        .cases
        .iter()
        .map(|case| WhenExpr::parse(&case.when).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut readings = BTreeMap::new();
    for expression in &expressions {
        readings.extend(
            when::evaluate(
                expression,
                &record.scope.path,
                Some(&source),
                check.provider.as_ref(),
                check.account.as_ref(),
                &windows,
            )
            .readings,
        );
    }
    let checked_at = source.checked_at(&record.scope.path);
    let (entered_at, stale_reason) = record.held.as_ref().map_or(
        (record.entered_at, "probed before this check was entered"),
        |held| (held.entered_at, "probed before this session started"),
    );
    let ci_unread = source
        .ci_commits(&record.scope.path)
        .filter(|_| readings.contains_key("ci") && checked_at.is_some_and(|at| at >= entered_at))
        .and_then(|(pr_head, ci_sha)| {
            // Compare ci_sha with the cached PR head, and that head with fresh local HEAD.
            // Read HEAD after the cache. Settlement uses the PR head at checked_at, without
            // re-reading under the lock: a later commit or push is a new fact for the next
            // evaluation here, or after a step returns to this check.
            let local_head = super::super::head(&record.scope.path);
            let on_pr = local_head
                .as_deref()
                .filter(|local| *local != pr_head)
                .and_then(|local| is_ancestor(&record.scope.path, local, pr_head));
            ci_unread(pr_head, ci_sha, local_head.as_deref(), on_pr)
        });
    Ok(answer(
        check,
        &expressions,
        readings,
        checked_at,
        (entered_at, stale_reason),
        ci_unread,
        |key| schedule::unread_reason(source.probe_unread_reason(key, &record.scope.path), None),
    ))
}

fn answer(
    check: &Check,
    expressions: &[WhenExpr],
    mut readings: BTreeMap<String, Option<String>>,
    checked_at: Option<Timestamp>,
    (entered_at, stale_reason): (Timestamp, &str),
    ci_unread: Option<String>,
    unread: impl Fn(&str) -> String,
) -> Value {
    let forge_keys = expressions
        .iter()
        .flat_map(WhenExpr::terms)
        .filter(|term| term.reads_forge())
        .map(|term| term.key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let forge_fresh = checked_at.is_some_and(|at| at >= entered_at);
    let ci_unread = ci_unread.filter(|_| forge_fresh && forge_keys.contains("ci"));
    if !forge_fresh {
        for key in &forge_keys {
            readings.insert((*key).into(), None);
        }
    }
    if ci_unread.is_some() {
        readings.insert("ci".into(), None);
    }
    let mut object = json!({"outcome":check.else_outcome, "readings":readings});
    for (index, expression) in expressions.iter().enumerate() {
        match expression.truth(&readings) {
            Some(false) => continue,
            Some(true) => {
                let case = &check.cases[index];
                object["outcome"] = json!(case.outcome);
                object["case"] = json!(index);
                if let Some(key) = &case.key {
                    object["key"] = json!(key);
                }
            }
            None => {
                object["outcome"] = json!("unknown");
                object["reason"] = json!(
                    readings
                        .iter()
                        .filter(|(_, value)| value.is_none())
                        .map(|(key, _)| {
                            let reason = if !forge_keys.contains(key.as_str()) {
                                "not read".into()
                            } else if checked_at.is_some_and(|at| at < entered_at) {
                                stale_reason.into()
                            } else if key == "ci"
                                && let Some(reason) = &ci_unread
                            {
                                reason.clone()
                            } else {
                                unread(key)
                            };
                            format!("{key} ({reason})")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        break;
    }
    object
}

/// Project a condition answer; pass only the wait belonging to this check.
pub fn case_marks(
    check: &Check,
    object: &Value,
    wait: Option<&Wait>,
) -> Result<CaseMarks, SpecErr> {
    let selected = wait
        .filter(|wait| wait.is_hold())
        .and_then(|wait| wait.case)
        .map(|case| (u64::from(case), CaseMark::Holding))
        .or_else(|| {
            object
                .get("case")
                .and_then(Value::as_u64)
                .map(|case| (case, CaseMark::Winner))
        })
        .filter(|(case, _)| *case < check.cases.len() as u64);
    if let Some((selected, mark)) = selected {
        return Ok(CaseMarks {
            cases: (0..check.cases.len() as u64)
                .map(|index| match index.cmp(&selected) {
                    std::cmp::Ordering::Less => CaseMark::False,
                    std::cmp::Ordering::Equal => mark,
                    std::cmp::Ordering::Greater => CaseMark::NotReached,
                })
                .collect(),
            else_mark: CaseMark::NotReached,
        });
    }
    let readings = object
        .get("readings")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), value.as_str().map(str::to_owned)))
        .collect();
    let mut cases = Vec::new();
    for case in &check.cases {
        let mark = match WhenExpr::parse(&case.when)?.truth(&readings) {
            Some(false) => CaseMark::False,
            Some(true) => CaseMark::Winner,
            None => CaseMark::Unread,
        };
        cases.push(mark);
        if mark != CaseMark::False {
            cases.resize(check.cases.len(), CaseMark::NotReached);
            return Ok(CaseMarks {
                cases,
                else_mark: CaseMark::NotReached,
            });
        }
    }
    Ok(CaseMarks {
        cases,
        else_mark: CaseMark::Winner,
    })
}

/// Recover `answer`'s per-key unread prose without splitting the prose itself on commas.
pub fn unread_reasons(object: &Value) -> BTreeMap<String, String> {
    let Some((readings, mut remaining)) = object
        .get("readings")
        .and_then(Value::as_object)
        .zip(object.get("reason").and_then(Value::as_str))
    else {
        return BTreeMap::new();
    };
    let keys = readings
        .iter()
        .filter(|(_, value)| value.is_null())
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    let mut reasons = BTreeMap::new();
    for (index, key) in keys.iter().enumerate() {
        let Some(body) = remaining.strip_prefix(&format!("{key} (")) else {
            return BTreeMap::new();
        };
        let reason = if let Some(next) = keys.get(index + 1) {
            let Some(end) = body.find(&format!("), {next} (")) else {
                return BTreeMap::new();
            };
            remaining = &body[end + 3..];
            &body[..end]
        } else {
            let Some(reason) = body.strip_suffix(')') else {
                return BTreeMap::new();
            };
            reason
        };
        reasons.insert((*key).to_owned(), reason.to_owned());
    }
    reasons
}

fn ci_unread(
    pr_head: &str,
    ci_sha: Option<&str>,
    local_head: Option<&str>,
    on_pr: Option<bool>,
) -> Option<String> {
    if pr_head.is_empty() {
        return None;
    }
    if let Some(local) = local_head.filter(|local| *local != pr_head)
        && on_pr == Some(false)
    {
        return Some(format!(
            "local HEAD {local:.7} is not on the PR, PR head {pr_head:.7}"
        ));
    }
    let ci_sha = ci_sha.filter(|sha| !sha.is_empty() && *sha != pr_head)?;
    Some(format!("read at {ci_sha:.7}, PR head {pr_head:.7}"))
}

/// `Some(false)` only on a definite exit 1; an unknown object (128), any other exit, a timeout,
/// or a spawn error is `None`.
fn is_ancestor(path: &Path, commit: &str, tip: &str) -> Option<bool> {
    let output = crate::proc::run_bounded_git_output(
        path,
        &["merge-base", "--is-ancestor", "--", commit, tip],
        std::time::Duration::from_secs(2),
    )
    .ok()?;
    if output.timed_out {
        return None;
    }
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn case_marks_paint_only_the_decision_prefix_and_else() {
        use CaseMark::{False, NotReached, Unread, Winner};

        let check: Check = toml::from_str("else='pending'\ncases=[{when=['pr=merged'],outcome='merged'},{when=['ci=passed'],outcome='green'},{when=['team.stage=Done'],outcome='done'}]").unwrap();
        let expressions = check
            .cases
            .iter()
            .map(|case| WhenExpr::parse(&case.when).unwrap())
            .collect::<Vec<_>>();
        let now = "2026-01-01T00:00:00Z".parse().unwrap();
        for (readings, cases, else_mark) in [
            (
                [Some("open"), Some("passed"), Some("Done")],
                vec![False, Winner, NotReached],
                NotReached,
            ),
            (
                [None, Some("passed"), Some("Done")],
                vec![Unread, NotReached, NotReached],
                NotReached,
            ),
            (
                [Some("open"), None, Some("Done")],
                vec![False, Unread, NotReached],
                NotReached,
            ),
            (
                [Some("open"), Some("failed"), Some("Todo")],
                vec![False, False, False],
                Winner,
            ),
            (
                [Some("merged"), None, None],
                vec![Winner, NotReached, NotReached],
                NotReached,
            ),
        ] {
            let object = answer(
                &check,
                &expressions,
                ["pr", "ci", "team.stage"]
                    .into_iter()
                    .zip(readings)
                    .map(|(key, value)| (key.into(), value.map(str::to_owned)))
                    .collect(),
                Some(now),
                (now, "stale"),
                None,
                |_| "not read".into(),
            );
            assert_eq!(
                case_marks(&check, &object, None).unwrap(),
                CaseMarks { cases, else_mark },
                "{object}"
            );
        }
    }

    #[test]
    fn case_marks_use_the_held_case_from_the_record() {
        let check: Check = toml::from_str("else='pending'\ncases=[{when=['pr=merged'],outcome='merged'},{when=['ci=passed'],outcome='green',for='30m'}]").unwrap();
        let object = json!({"outcome":"hold","readings":{"pr":"open","ci":"passed"}});
        let wait: Wait = serde_json::from_value(json!({"node":"gate","outcome":"hold","case":1,"first":"2026-01-01T00:00:00Z","last":"2026-01-01T00:00:01Z","count":2})).unwrap();
        assert_eq!(
            case_marks(&check, &object, Some(&wait)).unwrap(),
            CaseMarks {
                cases: vec![CaseMark::False, CaseMark::Holding],
                else_mark: CaseMark::NotReached
            }
        );
    }

    #[test]
    fn unread_reasons_round_trip_answer_with_commas_and_parentheses() {
        let reasons = [
            (
                "ci",
                "ci=passed",
                "read at aaaaaaa, PR head bbbbbbb (queued)",
            ),
            (
                "pr.queue",
                "pr.queue=queued",
                "no supported forge for this checkout (x), y",
            ),
        ];
        let now = "2026-01-01T00:00:00Z".parse().unwrap();
        for count in [1, 2] {
            let clauses = reasons[..count]
                .iter()
                .map(|(_, clause, _)| *clause)
                .collect::<Vec<_>>();
            let check: Check = toml::from_str(&format!(
                "else='pending'\ncases=[{{when={clauses:?},outcome='ready'}}]"
            ))
            .unwrap();
            let expressions = [WhenExpr::parse(&check.cases[0].when).unwrap()];
            let expected = reasons[..count]
                .iter()
                .map(|(key, _, reason)| (key.to_string(), reason.to_string()))
                .collect::<BTreeMap<_, _>>();
            let object = answer(
                &check,
                &expressions,
                expected.keys().map(|key| (key.clone(), None)).collect(),
                Some(now),
                (now, "stale"),
                None,
                |key| expected[key].clone(),
            );
            assert_eq!(unread_reasons(&object), expected, "{object}");
        }
        assert!(unread_reasons(&json!({"readings":{"ci":"passed"}})).is_empty());
    }

    #[test]
    fn cases_are_ordered_and_unread_is_not_false_even_under_negation() {
        for (conditions, readings, outcome, case) in [
            (
                ["pr=merged", "ci=passed"],
                [Some("merged"), Some("passed")],
                "first",
                Some(0),
            ),
            (
                ["pr=merged", "ci=passed"],
                [Some("merged"), None],
                "first",
                Some(0),
            ),
            (
                ["pr=merged", "ci=passed"],
                [Some("open"), Some("passed")],
                "second",
                Some(1),
            ),
            (
                ["pr=merged", "ci=passed"],
                [Some("open"), Some("failed")],
                "pending",
                None,
            ),
            (
                ["pr=merged", "ci=passed"],
                [None, Some("passed")],
                "unknown",
                None,
            ),
            (
                ["!pr=open", "ci=passed"],
                [None, Some("passed")],
                "unknown",
                None,
            ),
        ] {
            let check: Check = toml::from_str(&format!("else='pending'\n[[cases]]\nwhen=['{}']\noutcome='first'\nkey='fixed'\n[[cases]]\nwhen=['{}']\noutcome='second'\n", conditions[0], conditions[1])).unwrap();
            let expressions: Vec<_> = conditions
                .into_iter()
                .map(|s| WhenExpr::parse(&[s.into()]).unwrap())
                .collect();
            let now = "2026-01-01T00:00:00Z".parse().unwrap();
            let object = answer(
                &check,
                &expressions,
                BTreeMap::from([
                    ("pr".into(), readings[0].map(str::to_owned)),
                    ("ci".into(), readings[1].map(str::to_owned)),
                ]),
                Some(now),
                (now, "probed before this check was entered"),
                None,
                |_| "not read".into(),
            );
            assert_eq!(object["outcome"], outcome);
            assert_eq!(object.get("case"), case.map(|n| json!(n)).as_ref());
            if case == Some(0) {
                assert_eq!(object["key"], "fixed");
            } else {
                assert!(object.get("key").is_none());
            }
            assert_eq!(object.get("reason").is_some(), outcome == "unknown");
        }
    }

    #[test]
    fn forge_floor_includes_entry_but_stage_and_windows_have_no_floor() {
        let entered: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        for (key, value) in [
            ("ci", "passed"),
            ("pr", "merged"),
            ("pr.queue", "queued"),
            ("ci[topic]", "passed"),
            ("pr[topic]", "merged"),
            ("pr.queue[topic]", "queued"),
            ("pr.mergeable", "conflicting"),
            ("pr.review", "required"),
            ("pr.mergeable[topic]", "mergeable"),
            ("pr.review[topic]", "approved"),
        ] {
            let clauses = [
                format!("{key}={value}"),
                "team.stage=Done".into(),
                "window.5h.left>=40".into(),
            ];
            let check: Check = toml::from_str(&format!(
                "else='pending'\n[[cases]]\nwhen={clauses:?}\noutcome='ready'\n"
            ))
            .unwrap();
            let expressions = [WhenExpr::parse(&clauses).unwrap()];
            for (checked, outcome) in [
                (None, "unknown"),
                (
                    Some(entered - jiff::SignedDuration::from_nanos(1)),
                    "unknown",
                ),
                (Some(entered), "ready"),
                (Some(entered + jiff::SignedDuration::from_nanos(1)), "ready"),
            ] {
                let object = answer(
                    &check,
                    &expressions,
                    BTreeMap::from([
                        (key.into(), Some(value.into())),
                        ("team.stage".into(), Some("Done".into())),
                        ("window.5h.left".into(), Some("80".into())),
                    ]),
                    checked,
                    (entered, "probed before this check was entered"),
                    None,
                    |_| "not checked yet".into(),
                );
                assert_eq!(object["outcome"], outcome);
                assert_eq!(object["readings"]["team.stage"], "Done");
                assert_eq!(object["readings"]["window.5h.left"], "80");
                assert_eq!(object["readings"][key].is_null(), outcome == "unknown");
            }
        }
    }

    #[test]
    fn ci_commit_fence_decides_at_the_pr_head_with_ordered_reasons() {
        let entered: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        for (pr_head, ci_sha, local, ancestor, fresh, reason) in [
            (
                "bbbbbbb2",
                Some("bbbbbbb2"),
                Some("bbbbbbb2"),
                None,
                true,
                None,
            ),
            (
                "bbbbbbb2",
                Some("bbbbbbb2"),
                Some("aaaaaaa1"),
                Some(true),
                true,
                None,
            ),
            (
                "bbbbbbb2",
                Some("bbbbbbb2"),
                Some("aaaaaaa1"),
                None,
                true,
                None,
            ),
            (
                "aaaaaaa1",
                Some("aaaaaaa1"),
                Some("bbbbbbb2"),
                Some(false),
                true,
                Some("ci (local HEAD bbbbbbb is not on the PR, PR head aaaaaaa)"),
            ),
            (
                "aaaaaaa1",
                Some("bbbbbbb2"),
                Some("bbbbbbb2"),
                Some(false),
                true,
                Some("ci (local HEAD bbbbbbb is not on the PR, PR head aaaaaaa)"),
            ),
            (
                "bbbbbbb2",
                Some("aaaaaaa1"),
                Some("bbbbbbb2"),
                None,
                true,
                Some("ci (read at aaaaaaa, PR head bbbbbbb)"),
            ),
            (
                "bbbbbbb2",
                Some("aaaaaaa1"),
                Some("ccccccc3"),
                Some(false),
                true,
                Some("ci (local HEAD ccccccc is not on the PR, PR head bbbbbbb)"),
            ),
            (
                "",
                Some("aaaaaaa1"),
                Some("bbbbbbb2"),
                Some(false),
                true,
                None,
            ),
            (
                "bbbbbbb2",
                Some("aaaaaaa1"),
                None,
                None,
                true,
                Some("ci (read at aaaaaaa, PR head bbbbbbb)"),
            ),
            ("bbbbbbb2", Some("bbbbbbb2"), None, None, true, None),
            ("bbbbbbb2", None, Some("bbbbbbb2"), None, true, None),
            ("bbbbbbb2", Some(""), Some("bbbbbbb2"), None, true, None),
            (
                "bbbbbbb2",
                None,
                Some("ccccccc3"),
                Some(false),
                true,
                Some("ci (local HEAD ccccccc is not on the PR, PR head bbbbbbb)"),
            ),
            (
                "bbbbbbb2",
                Some("aaaaaaa1"),
                Some("ccccccc3"),
                Some(false),
                false,
                Some("ci (probed before this check was entered)"),
            ),
            (
                "pr",
                Some("ci"),
                Some("pr"),
                None,
                true,
                Some("ci (read at ci, PR head pr)"),
            ),
        ] {
            for clause in ["ci=failed", "!ci=passed"] {
                let check: Check = toml::from_str(&format!(
                    "else='pending'\n[[cases]]\nwhen=['{clause}']\noutcome='ready'\n"
                ))
                .unwrap();
                let object = answer(
                    &check,
                    &[WhenExpr::parse(&[clause.into()]).unwrap()],
                    BTreeMap::from([("ci".into(), Some("failed".into()))]),
                    Some(if fresh {
                        entered
                    } else {
                        entered - jiff::SignedDuration::from_secs(1)
                    }),
                    (entered, "probed before this check was entered"),
                    ci_unread(pr_head, ci_sha, local, ancestor),
                    |_| "not checked yet".into(),
                );
                assert_eq!(
                    object["outcome"],
                    if reason.is_some() { "unknown" } else { "ready" },
                    "{clause}: {pr_head}, {ci_sha:?}, {local:?}, {ancestor:?}"
                );
                assert_eq!(
                    object["readings"]["ci"],
                    if reason.is_some() {
                        Value::Null
                    } else {
                        json!("failed")
                    }
                );
                assert_eq!(object.get("reason"), reason.map(|s| json!(s)).as_ref());
            }
        }
    }

    #[test]
    fn ci_commit_fence_leaves_other_keys_unchanged() {
        let entered: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        for (key, value) in [
            ("ci[topic]", "passed"),
            ("pr", "open"),
            ("pr.queue", "dequeued"),
            ("pr[topic]", "merged"),
            ("pr.queue[topic]", "queued"),
            ("pr.mergeable", "conflicting"),
            ("pr.review", "required"),
            ("pr.mergeable[topic]", "mergeable"),
            ("pr.review[topic]", "approved"),
        ] {
            let clause = format!("{key}={value}");
            let check: Check = toml::from_str(&format!(
                "else='pending'\n[[cases]]\nwhen=['{clause}']\noutcome='ready'\n"
            ))
            .unwrap();
            let object = answer(
                &check,
                &[WhenExpr::parse(&[clause]).unwrap()],
                BTreeMap::from([(key.into(), Some(value.into()))]),
                Some(entered),
                (entered, "probed before this check was entered"),
                ci_unread("pr", Some("old"), Some("local"), Some(false)),
                |_| "not checked yet".into(),
            );
            assert_eq!(
                object,
                json!({"outcome":"ready", "case":0, "readings":{key:value}})
            );
        }
    }
}
