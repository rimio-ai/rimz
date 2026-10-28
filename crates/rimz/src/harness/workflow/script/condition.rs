use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::super::Check;
use crate::disk::paths::StatePaths;
use crate::harness::schedule::{
    self,
    when::{self, CiSource, WhenExpr, WindowReadings},
};
use crate::store::workflow::RunRecord;

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
        |key| schedule::unread_reason(source.probe_unread_reason(key, &record.scope.path), now),
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
