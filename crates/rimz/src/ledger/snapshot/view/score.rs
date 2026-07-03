//! Fixed-point attention scoring for row, team, and group ordering.

use crate::agents::AgentStatus;

const MILLI: u32 = 1_000;

pub(super) fn age_secs(now: jiff::Timestamp, last_activity: jiff::Timestamp) -> u32 {
    now.duration_since(last_activity)
        .as_secs()
        .max(0)
        .min(i64::from(u32::MAX)) as u32
}

/// Base status weights in fixed-point score units. Waiting and failed sit close
/// enough for heat to interleave an older failure above a fresh ask, while the
/// lowest attention state still starts above the highest calm state.
pub(super) fn status_weight(status: AgentStatus) -> u32 {
    match status {
        AgentStatus::Waiting => 600,
        AgentStatus::Failed => 560,
        AgentStatus::Paused => 400,
        AgentStatus::Success => 300,
        AgentStatus::Running => 200,
        AgentStatus::Idle => 100,
    }
}

pub(super) fn status_weight_opt(status: Option<AgentStatus>) -> u32 {
    status.map_or(0, status_weight)
}

/// Time curve in milli-units:
/// hot attention ramps 1.0→2.0 until the inactive boundary, calm hot work stays
/// flat, warm work decays 1.0→0.0 until archive, and archived work is flat
/// because the archive band already parked it.
pub(super) fn time_factor_milli(
    status: Option<AgentStatus>,
    age_secs: u32,
    inactive_after_secs: u32,
    archive_after_secs: u32,
) -> u32 {
    let inactive_after_secs = inactive_after_secs.max(1);
    let archive_after_secs = archive_after_secs.max(inactive_after_secs.saturating_add(1));
    if age_secs <= inactive_after_secs {
        if status.is_some_and(AgentStatus::is_attention) {
            MILLI
                + ((u64::from(MILLI) * u64::from(age_secs)) / u64::from(inactive_after_secs)) as u32
        } else {
            MILLI
        }
    } else if age_secs <= archive_after_secs {
        let span = archive_after_secs
            .saturating_sub(inactive_after_secs)
            .max(1);
        let remaining = archive_after_secs.saturating_sub(age_secs);
        ((u64::from(MILLI) * u64::from(remaining)) / u64::from(span)) as u32
    } else {
        MILLI
    }
}

pub(super) fn attention_score(
    status: Option<AgentStatus>,
    age_secs: u32,
    inactive_after_secs: u32,
    archive_after_secs: u32,
) -> u32 {
    let weight = status_weight_opt(status);
    let factor = time_factor_milli(status, age_secs, inactive_after_secs, archive_after_secs);
    ((u64::from(weight) * u64::from(factor)) / u64::from(MILLI)) as u32
}

/// Recover the stamped time factor for derived team-state scoring. Integer
/// division can lose several milli-steps on low-weight statuses; that drift stays
/// below the status-weight spacing, and keeps presentation sorting pure over
/// stamped rows.
pub(super) fn recovered_time_factor_milli(status: Option<AgentStatus>, score: u32) -> u32 {
    let weight = status_weight_opt(status);
    if weight == 0 {
        MILLI
    } else {
        ((u64::from(score) * u64::from(MILLI)) / u64::from(weight)) as u32
    }
}

pub(super) fn score_from_weight_and_factor(status: AgentStatus, factor_milli: u32) -> u32 {
    ((u64::from(status_weight(status)) * u64::from(factor_milli)) / u64::from(MILLI)) as u32
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum GitRung {
    Dirty,
    Clean,
    Unknown,
    Merged,
}

pub(super) fn git_rung(clean: Option<bool>, landed: Option<bool>) -> GitRung {
    if clean == Some(false) {
        GitRung::Dirty
    } else if landed == Some(true) {
        GitRung::Merged
    } else if clean == Some(true) {
        GitRung::Clean
    } else {
        GitRung::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INACTIVE: u32 = 3_600;
    const ARCHIVE: u32 = 86_400;

    #[test]
    fn attention_heat_ramps_across_hot_window() {
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Waiting), 0, INACTIVE, ARCHIVE),
            1_000
        );
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Waiting), INACTIVE, INACTIVE, ARCHIVE),
            2_000
        );
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Running), INACTIVE, INACTIVE, ARCHIVE),
            1_000,
            "calm hot work stays flat"
        );
    }

    #[test]
    fn warm_window_decays_to_archive_boundary() {
        assert!(
            time_factor_milli(Some(AgentStatus::Waiting), INACTIVE + 1, INACTIVE, ARCHIVE,) < 1_000,
            "warm band starts decaying immediately after the hot boundary"
        );
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Waiting), ARCHIVE, INACTIVE, ARCHIVE),
            0
        );
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Running), ARCHIVE, INACTIVE, ARCHIVE),
            0
        );
    }

    #[test]
    fn archive_band_is_flat() {
        assert_eq!(
            time_factor_milli(Some(AgentStatus::Waiting), ARCHIVE + 1, INACTIVE, ARCHIVE),
            1_000
        );
        assert_eq!(
            attention_score(Some(AgentStatus::Waiting), ARCHIVE + 1, INACTIVE, ARCHIVE),
            status_weight(AgentStatus::Waiting)
        );
    }

    #[test]
    fn git_rung_orders_dirty_clean_unknown_merged() {
        assert_eq!(git_rung(Some(false), Some(true)), GitRung::Dirty);
        assert_eq!(git_rung(Some(true), Some(false)), GitRung::Clean);
        assert_eq!(git_rung(None, None), GitRung::Unknown);
        assert_eq!(git_rung(Some(true), Some(true)), GitRung::Merged);
        assert!(GitRung::Dirty < GitRung::Clean);
        assert!(GitRung::Clean < GitRung::Unknown);
        assert!(GitRung::Unknown < GitRung::Merged);
    }

    #[test]
    fn weights_keep_attention_above_calm_and_allow_hot_interleave() {
        assert!(status_weight(AgentStatus::Paused) > status_weight(AgentStatus::Success));
        assert!(
            status_weight(AgentStatus::Failed) * 2 > status_weight(AgentStatus::Waiting),
            "an old failure can outrank a fresh ask inside the hot band"
        );
    }
}
