use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::super::{Check, SpecErr};
use crate::disk::paths::StatePaths;
use crate::harness::when::{self, CiSource, WhenExpr, WindowReadings};
use crate::store::workflow::{RunRecord, Wait};

#[cfg(test)]
use crate::harness::when::ci_unread;

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
    reading(state, check, record).map(|(object, _)| object)
}

pub(super) fn reading(
    state: &StatePaths,
    check: &Check,
    record: &RunRecord,
) -> Result<(Value, Option<Timestamp>), String> {
    let runtime = crate::RuntimePaths::for_state(state).map_err(|error| error.to_string())?;
    let source = CiSource::read(&runtime);
    let now = Timestamp::now();
    let windows = WindowReadings::new(Some(&runtime), now);
    let expressions = check
        .condition_cases()
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
    let ci_unread = (readings.contains_key("ci") && checked_at.is_some_and(|at| at >= entered_at))
        .then(|| source.ci_fence(&record.scope.path))
        .flatten();
    let reads_forge = expressions
        .iter()
        .flat_map(WhenExpr::terms)
        .any(|term| term.reads_forge());
    let object = answer(
        check,
        &expressions,
        readings,
        checked_at,
        (entered_at, stale_reason),
        ci_unread,
        |key| {
            when::condition::unread_reason(
                source.probe_unread_reason(key, &record.scope.path),
                None,
            )
        },
    );
    Ok((object, checked_at.filter(|_| reads_forge)))
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
    let floor = when::Floor {
        at: entered_at,
        stale: stale_reason,
    };
    let forge_unread = when::forge_unread(
        forge_keys.iter().copied(),
        checked_at,
        floor,
        ci_unread,
        &unread,
    );
    for key in forge_unread.keys() {
        readings.insert(key.clone(), None);
    }
    let cases = check.condition_cases();
    let mut object = json!({"outcome":check.fallback_outcome(), "readings":readings});
    for (index, expression) in expressions.iter().enumerate() {
        match expression.truth(&readings) {
            Some(false) => continue,
            Some(true) => {
                let case = &cases[index];
                object["outcome"] = json!(case.outcome);
                object["case"] = json!(index);
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
                            } else {
                                forge_unread
                                    .get(key)
                                    .cloned()
                                    .unwrap_or_else(|| unread(key))
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
    let condition_cases = check.condition_cases();
    // A hold that survives an unknown answer is drawn from that answer's readings; without an
    // answer, the wait says whether the last one was unknown.
    let selected = wait
        .filter(|wait| {
            wait.is_hold()
                && object
                    .get("outcome")
                    .map_or(wait.unknown.is_none(), |outcome| outcome == "hold")
        })
        .and_then(|wait| wait.case)
        .map(|case| (u64::from(case), CaseMark::Holding))
        .or_else(|| {
            object
                .get("case")
                .and_then(Value::as_u64)
                .map(|case| (case, CaseMark::Winner))
        })
        .filter(|(case, _)| *case < condition_cases.len() as u64);
    if let Some((selected, mark)) = selected {
        return Ok(CaseMarks {
            cases: (0..condition_cases.len() as u64)
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
    for case in condition_cases.iter() {
        let mark = match WhenExpr::parse(&case.when)?.truth(&readings) {
            Some(false) => CaseMark::False,
            Some(true) => CaseMark::Winner,
            None => CaseMark::Unread,
        };
        cases.push(mark);
        if mark != CaseMark::False {
            cases.resize(condition_cases.len(), CaseMark::NotReached);
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
    fn case_marks_read_an_unknown_answer_under_a_surviving_hold() {
        let check: Check = toml::from_str("else='pending'\ncases=[{when=['team.stage=Done'],outcome='done'},{when=['pr=merged'],outcome='merged',for='2m'}]").unwrap();
        let object = json!({"outcome":"unknown","readings":{"team.stage":null,"pr":"merged"}});
        let wait: Wait = serde_json::from_value(json!({"node":"gate","outcome":"hold","case":1,"unknown":"team.stage (no board)","first":"2026-01-01T00:00:00Z","last":"2026-01-01T00:00:01Z","count":2})).unwrap();
        assert_eq!(
            case_marks(&check, &object, Some(&wait)).unwrap(),
            CaseMarks {
                cases: vec![CaseMark::Unread, CaseMark::NotReached],
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
            (
                ["!pr=open", "ci=passed"],
                [Some("none"), None],
                "first",
                Some(0),
            ),
        ] {
            let check: Check = toml::from_str(&format!("else='pending'\n[[cases]]\nwhen=['{}']\noutcome='first'\n[[cases]]\nwhen=['{}']\noutcome='second'\n", conditions[0], conditions[1])).unwrap();
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
            assert!(object.get("key").is_none());
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
