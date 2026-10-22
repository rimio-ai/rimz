use std::collections::BTreeMap;

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
    Ok(answer(
        check,
        &expressions,
        readings,
        source.checked_at(&record.scope.path),
        record.entered_at,
        |key| schedule::unread_reason(source.probe_unread_reason(key, &record.scope.path), now),
    ))
}

fn answer(
    check: &Check,
    expressions: &[WhenExpr],
    mut readings: BTreeMap<String, Option<String>>,
    checked_at: Option<Timestamp>,
    entered_at: Timestamp,
    unread: impl Fn(&str) -> String,
) -> Value {
    let forge_keys = expressions
        .iter()
        .flat_map(WhenExpr::terms)
        .filter(|term| term.reads_forge())
        .map(|term| term.key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if !checked_at.is_some_and(|at| at >= entered_at) {
        for key in &forge_keys {
            readings.insert((*key).into(), None);
        }
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
                                "probed before this check was entered".into()
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
                now,
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
    fn forge_floor_is_strict_but_stage_and_windows_have_no_floor() {
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
                    entered,
                    |_| "not checked yet".into(),
                );
                assert_eq!(object["outcome"], outcome);
                assert_eq!(object["readings"]["team.stage"], "Done");
                assert_eq!(object["readings"]["window.5h.left"], "80");
                assert_eq!(object["readings"][key].is_null(), outcome == "unknown");
            }
        }
    }
}
