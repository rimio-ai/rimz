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
pub fn unread_reason(reason: Option<UnreadReason>, now: Timestamp) -> String {
    match reason {
        None | Some(UnreadReason::NotChecked) => "not checked yet".into(),
        Some(UnreadReason::NoForge) => "no supported forge for this checkout".into(),
        Some(UnreadReason::LookupFailing { last_tried }) => {
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
    }
}
