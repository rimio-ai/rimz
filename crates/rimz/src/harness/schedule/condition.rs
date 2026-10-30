use std::collections::BTreeSet;
use std::path::Path;

use jiff::Timestamp;

use super::when::UnreadReason;

/// Stage names admitted by the project's effective teams, including the terminal stage.
pub fn condition_stages(
    root: &Path,
) -> Result<BTreeSet<String>, crate::config::effective::EffectiveConfigErr> {
    let effective =
        crate::config::effective::load(&crate::config::MachineConfig::load_lenient(), root)?;
    let mut stages = effective
        .teams
        .0
        .values()
        .flat_map(|team| team.pipeline_stages())
        .collect::<BTreeSet<_>>();
    stages.insert(crate::config::DONE_STAGE.to_owned());
    Ok(stages)
}

/// Human-readable explanation shared by loop inspection and workflow condition answers.
/// Pass a read-time timestamp to include probe ages, or `None` for ageless stored text.
pub fn unread_reason(reason: Option<UnreadReason>, now: Option<Timestamp>) -> String {
    match reason {
        None | Some(UnreadReason::NotChecked) => "not checked yet".into(),
        Some(UnreadReason::NoForge) => "no supported forge for this checkout".into(),
        Some(UnreadReason::LookupFailing { last_tried }) => {
            let Some(now) = now else {
                return "forge lookup failing".into();
            };
            let age = i64::try_from(last_tried)
                .ok()
                .and_then(|at| Timestamp::from_millisecond(at).ok())
                .map(|at| {
                    let age = now.duration_since(at);
                    if age.is_negative() {
                        "now".into()
                    } else {
                        format!(
                            "{} ago",
                            crate::utils::time::format_duration_coarse(age.as_secs())
                        )
                    }
                })
                .unwrap_or_else(|| "unknown".into());
            format!("forge lookup failing, last tried {age}")
        }
        Some(UnreadReason::NoPr { branch }) => branch.map_or_else(
            || "no PR for this checkout".into(),
            |branch| format!("no PR for branch {branch}"),
        ),
        Some(UnreadReason::NoCi { branch }) => branch.map_or_else(
            || "no CI for this checkout".into(),
            |branch| format!("no CI for branch {branch}"),
        ),
        Some(UnreadReason::NoOpenPr { branch }) => branch.map_or_else(
            || "no open PR for this checkout".into(),
            |branch| format!("no open PR for branch {branch}"),
        ),
        Some(UnreadReason::PrFactsUnavailable { branch }) => branch.map_or_else(
            || "not reported for this checkout's open PR".into(),
            |branch| format!("not reported for branch {branch}'s open PR"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_reason_keeps_ages_only_at_read_time() {
        let now = "2026-10-30T19:04:00Z".parse::<Timestamp>().unwrap();
        let reason = Some(UnreadReason::LookupFailing {
            last_tried: (now.as_millisecond() - 180_000) as u64,
        });
        assert_eq!(
            unread_reason(reason.clone(), Some(now)),
            "forge lookup failing, last tried 3m ago"
        );
        assert_eq!(unread_reason(reason, None), "forge lookup failing");
    }
}
