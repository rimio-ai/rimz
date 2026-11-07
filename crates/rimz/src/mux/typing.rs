//! Per-pane terminal-input timing and non-typing exclusions shared by sends and sidebar pens.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::disk::{atomic, lock::WorkspaceLock, paths::RuntimePaths};
use crate::ids::{MuxClientId, MuxName, PaneId};
use crate::pane::ClientPaneView;

const STEP_MS: u64 = 8_000;
// Allow the plugin's one-second publish floor plus its wake.
const CORROBORATION_GRACE_MS: u64 = 2_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct TypingRecord {
    // The host prunes absent panes once their hold and exclusion have expired.
    panes: HashMap<PaneId, PaneInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
struct PaneInput {
    tty: Option<PathBuf>,
    atime_s: Option<u64>,
    viewers: BTreeSet<MuxClientId>,
    quiet_until_ms: u64,
    input_ms: Option<u64>,
}

pub(crate) struct TtySample {
    atime_s: u64,
    taken_at: Instant,
    taken_at_ms: u64,
}

pub(crate) enum TypingView<'a> {
    Focused(&'a [ClientPaneView]),
    Unreported {
        clients: &'a [ClientPaneView],
        input_ms: &'a BTreeMap<MuxClientId, u64>,
    },
}

impl<'a> TypingView<'a> {
    pub(crate) fn for_mux(
        mux: MuxName,
        clients: &'a [ClientPaneView],
        input_ms: &'a BTreeMap<MuxClientId, u64>,
    ) -> Self {
        match mux {
            MuxName::Tmux => Self::Focused(clients),
            MuxName::Zellij => Self::Unreported { clients, input_ms },
        }
    }
}

pub(crate) enum InputSource<'a> {
    Tty(&'a TypingRecord),
    ClientClocks {
        clients: &'a [ClientPaneView],
        input_ms: &'a BTreeMap<MuxClientId, u64>,
    },
}

impl TypingRecord {
    pub(crate) fn load(runtime: &RuntimePaths) -> Self {
        let path = runtime.typing_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    debug!(%error, path = %path.display(), "typing record unreadable");
                }
                return Self::default();
            }
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            debug!(%error, path = %path.display(), "typing record invalid");
            Self::default()
        })
    }

    pub(crate) fn update<T>(
        runtime: &RuntimePaths,
        change: impl FnOnce(&mut Self) -> T,
    ) -> atomic::Result<T> {
        let path = runtime.typing_lock_path();
        let _lock = WorkspaceLock::acquire_with_timeout(&path, Duration::from_millis(250))
            .map_err(|error| atomic::AtomicErr::Io {
                path,
                source: io::Error::other(error),
            })?;
        let mut record = Self::load(runtime);
        let prior = record.clone();
        let result = change(&mut record);
        if record != prior {
            atomic::write_temp_then_rename_cache(&runtime.typing_path(), &record)?;
        }
        Ok(result)
    }

    pub(crate) fn set_tty(&mut self, pane: &PaneId, tty: PathBuf) {
        let entry = self.panes.entry(pane.clone()).or_default();
        if entry.tty.as_ref() != Some(&tty) {
            *entry = PaneInput {
                tty: Some(tty),
                quiet_until_ms: entry.quiet_until_ms,
                ..Default::default()
            };
        }
    }

    pub(crate) fn sample(&self, pane: &PaneId) -> io::Result<TtySample> {
        let tty = self
            .panes
            .get(pane)
            .and_then(|entry| entry.tty.as_ref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no recorded pane tty"))?;
        let taken_at = Instant::now();
        let taken_at_ms = crate::utils::time::unix_now_ms();
        let atime_s = fs::metadata(tty)?
            .accessed()?
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        Ok(TtySample {
            atime_s,
            taken_at,
            taken_at_ms,
        })
    }

    pub(crate) fn panes(&self) -> impl Iterator<Item = &PaneId> {
        self.panes.keys()
    }

    pub(crate) fn prune(&mut self, listed: &HashSet<PaneId>, hold: Option<Duration>, now_ms: u64) {
        self.panes.retain(|pane, entry| {
            listed.contains(pane)
                || entry.quiet_until_ms > now_ms
                || tty_deadline(entry, hold).is_some_and(|until| until > now_ms)
        });
    }

    pub(crate) fn observe(
        &mut self,
        pane: &PaneId,
        sample: &TtySample,
        view: &TypingView<'_>,
        view_started_at: Option<Instant>,
        now_ms: u64,
    ) -> bool {
        let entry = self.panes.entry(pane.clone()).or_default();
        let clients = match view {
            TypingView::Focused(clients) | TypingView::Unreported { clients, .. } => clients,
        };
        let viewers: BTreeSet<_> = clients
            .iter()
            .filter(|client| client.pane_id == *pane)
            .map(|client| client.client_id.clone())
            .collect();
        if entry.atime_s.is_none() {
            entry.atime_s = Some(sample.atime_s);
            entry.viewers = viewers;
            return false;
        }
        if !viewers.is_subset(&entry.viewers) {
            entry.quiet_until_ms = entry.quiet_until_ms.max(now_ms.saturating_add(STEP_MS));
        }
        entry.viewers = viewers;
        if entry.atime_s >= Some(sample.atime_s) {
            return false;
        }
        if view_started_at.is_none_or(|started| started < sample.taken_at) {
            return true;
        }
        let input_ms = sample.atime_s.saturating_mul(1_000);
        let eligible = !entry.viewers.is_empty() && input_ms >= entry.quiet_until_ms;
        let corroborated = match view {
            TypingView::Focused(_) => true,
            TypingView::Unreported {
                input_ms: clocks, ..
            } => entry
                .viewers
                .iter()
                .filter_map(|client| clocks.get(client))
                .any(|clock| *clock >= input_ms.saturating_sub(1_000)),
        };
        if eligible
            && !corroborated
            && sample.taken_at_ms < input_ms.saturating_add(CORROBORATION_GRACE_MS)
        {
            return true;
        }
        entry.atime_s = Some(sample.atime_s);
        if eligible && corroborated {
            entry.input_ms = Some(input_ms);
        }
        false
    }

    pub(super) fn sent(&mut self, pane: &PaneId, now_ms: u64) {
        let entry = self.panes.entry(pane.clone()).or_default();
        entry.quiet_until_ms = entry.quiet_until_ms.max(now_ms.saturating_add(STEP_MS));
    }
}

fn tty_deadline(entry: &PaneInput, hold: Option<Duration>) -> Option<u64> {
    let hold_ms = u64::try_from(hold?.as_millis()).unwrap_or(u64::MAX);
    Some(entry.input_ms?.saturating_add(hold_ms))
}

impl<'a> InputSource<'a> {
    pub(crate) const fn uses_tty() -> bool {
        cfg!(target_os = "linux")
    }

    pub(crate) fn for_platform(
        record: &'a TypingRecord,
        clients: &'a [ClientPaneView],
        input_ms: &'a BTreeMap<MuxClientId, u64>,
    ) -> Self {
        if Self::uses_tty() {
            Self::Tty(record)
        } else {
            Self::ClientClocks { clients, input_ms }
        }
    }

    pub(crate) fn deadline(&self, pane: &PaneId, hold: Option<Duration>) -> Option<u64> {
        match self {
            Self::Tty(record) => tty_deadline(record.panes.get(pane)?, hold),
            Self::ClientClocks { clients, input_ms } => {
                crate::pane::attended_until(clients, input_ms, pane, hold?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PaneId {
        PaneId::from_parts(MuxName::Tmux, "%1")
    }

    fn clients() -> Vec<ClientPaneView> {
        vec![ClientPaneView {
            client_id: MuxClientId::Tmux("human".into()),
            pane_id: pane(),
        }]
    }

    fn sample(atime_s: u64) -> TtySample {
        TtySample {
            atime_s,
            taken_at: Instant::now(),
            taken_at_ms: atime_s * 1_000,
        }
    }

    fn record() -> TypingRecord {
        TypingRecord {
            panes: HashMap::from([(
                pane(),
                PaneInput {
                    tty: Some("/dev/pts/1".into()),
                    atime_s: Some(10),
                    viewers: BTreeSet::from([clients()[0].client_id.clone()]),
                    ..Default::default()
                },
            )]),
        }
    }

    fn observe(record: &mut TypingRecord, atime_s: u64, clients: &[ClientPaneView], now_ms: u64) {
        let sample = sample(atime_s);
        record.observe(
            &pane(),
            &sample,
            &TypingView::Focused(clients),
            Some(sample.taken_at),
            now_ms,
        );
    }

    #[test]
    fn baseline_records_atime_and_viewers_without_counting_input() {
        let mut record = TypingRecord::default();
        observe(&mut record, 10, &clients(), 10_000);
        assert_eq!(
            record.panes.get(&pane()).map(|entry| (
                entry.atime_s,
                entry.viewers.clone(),
                entry.input_ms
            )),
            Some((
                Some(10),
                BTreeSet::from([clients()[0].client_id.clone()]),
                None
            ))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unfocused_client_does_not_count_an_advance() {
        let mut record = record();
        let mut view = crate::mux::ClientView {
            clients: clients(),
            ..Default::default()
        };
        view.presence
            .unfocused_clients
            .insert(clients()[0].client_id.clone());
        observe(&mut record, 20, &view.typing_clients(), 20_000);
        let entry = &record.panes[&pane()];
        assert_eq!((entry.viewers.is_empty(), entry.input_ms), (true, None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_focus_return_is_an_arrival_without_a_pane_change() {
        let mut record = record();
        let mut view = crate::mux::ClientView {
            clients: clients(),
            ..Default::default()
        };
        view.presence
            .unfocused_clients
            .insert(clients()[0].client_id.clone());
        observe(&mut record, 10, &view.typing_clients(), 10_000);
        view.presence.unfocused_clients.clear();
        observe(&mut record, 20, &view.typing_clients(), 20_000);
        let entry = &record.panes[&pane()];
        assert_eq!((entry.quiet_until_ms, entry.input_ms), (28_000, None));
    }

    #[test]
    fn unreported_focus_needs_a_viewing_clients_clock_at_the_atime_minus_one_second() {
        let clients = clients();
        let actual = [None, Some(18_999), Some(19_000), Some(20_000)].map(|clock| {
            let mut record = record();
            let mut clocks = BTreeMap::from([(MuxClientId::Tmux("elsewhere".into()), 20_000)]);
            if let Some(clock) = clock {
                clocks.insert(clients[0].client_id.clone(), clock);
            }
            let sample = TtySample {
                taken_at_ms: 23_000,
                ..sample(20)
            };
            let pending = record.observe(
                &pane(),
                &sample,
                &TypingView::for_mux(MuxName::Zellij, &clients, &clocks),
                Some(sample.taken_at),
                23_000,
            );
            (pending, record.panes[&pane()].input_ms)
        });
        assert_eq!(
            actual,
            [
                (false, None),
                (false, None),
                (false, Some(20_000)),
                (false, Some(20_000))
            ]
        );
    }

    #[test]
    fn unreported_focus_waits_for_a_late_clock_only_within_the_stat_grace() {
        let clients = clients();
        let actual = [false, true].map(|late_clock| {
            let mut record = record();
            let mut clocks = BTreeMap::from([(clients[0].client_id.clone(), 18_999)]);
            let mut sample = TtySample {
                taken_at_ms: 21_999,
                ..sample(20)
            };
            let pending = record.observe(
                &pane(),
                &sample,
                &TypingView::for_mux(MuxName::Zellij, &clients, &clocks),
                Some(sample.taken_at),
                99_000,
            );
            let deferred = (
                pending,
                record.panes[&pane()].atime_s,
                record.panes[&pane()].input_ms,
            );
            if late_clock {
                clocks.insert(clients[0].client_id.clone(), 19_000);
            } else {
                sample.taken_at_ms = 22_000;
            }
            let pending = record.observe(
                &pane(),
                &sample,
                &TypingView::for_mux(MuxName::Zellij, &clients, &clocks),
                Some(sample.taken_at),
                100_000,
            );
            (
                deferred,
                pending,
                record.panes[&pane()].atime_s,
                record.panes[&pane()].input_ms,
            )
        });
        assert_eq!(
            actual,
            [
                ((true, Some(10), None), false, Some(20), None),
                ((true, Some(10), None), false, Some(20), Some(20_000)),
            ]
        );
    }

    #[test]
    fn viewed_advance_at_or_past_the_exclusion_counts() {
        let actual = [20, 21].map(|atime| {
            let mut record = record();
            record.panes.get_mut(&pane()).unwrap().quiet_until_ms = 20_000;
            observe(&mut record, atime, &clients(), 22_000);
            record.panes[&pane()].input_ms
        });
        assert_eq!(actual, [Some(20_000), Some(21_000)]);
    }

    #[test]
    fn departure_consumes_the_advance_without_counting_focus_out() {
        let mut record = record();
        observe(&mut record, 20, &[], 20_000);
        let entry = &record.panes[&pane()];
        assert_eq!(
            (entry.atime_s, entry.viewers.is_empty(), entry.input_ms),
            (Some(20), true, None)
        );
    }

    #[test]
    fn send_excludes_an_advance_below_the_raised_deadline() {
        let mut record = record();
        record.sent(&pane(), 20_000);
        record.sent(&pane(), 19_000);
        observe(&mut record, 27, &clients(), 27_000);
        let entry = &record.panes[&pane()];
        assert_eq!(
            (entry.atime_s, entry.quiet_until_ms, entry.input_ms),
            (Some(27), 28_000, None)
        );
    }

    #[test]
    fn arrival_excludes_the_same_observations_advance() {
        let mut record = record();
        record.panes.get_mut(&pane()).unwrap().viewers.clear();
        observe(&mut record, 20, &clients(), 20_000);
        let entry = &record.panes[&pane()];
        assert_eq!(
            (entry.atime_s, entry.quiet_until_ms, entry.input_ms),
            (Some(20), 28_000, None)
        );
    }

    #[test]
    fn pre_stat_view_leaves_an_advance_pending_until_a_fresh_read() {
        let mut record = record();
        let sample = sample(20);
        let pending = record.observe(
            &pane(),
            &sample,
            &TypingView::Focused(&clients()),
            Some(sample.taken_at - Duration::from_secs(1)),
            20_000,
        );
        let deferred = (
            pending,
            record.panes[&pane()].atime_s,
            record.panes[&pane()].input_ms,
        );
        let pending = record.observe(
            &pane(),
            &sample,
            &TypingView::Focused(&clients()),
            Some(sample.taken_at),
            20_000,
        );
        assert_eq!(
            (deferred, pending, record.panes[&pane()].input_ms),
            ((true, Some(10), None), false, Some(20_000))
        );
    }

    #[test]
    fn reapplying_a_view_does_not_move_its_arrival_exclusion() {
        let mut record = record();
        record.panes.get_mut(&pane()).unwrap().viewers.clear();
        observe(&mut record, 10, &clients(), 20_000);
        let prior = record.clone();
        observe(&mut record, 10, &clients(), 21_000);
        assert_eq!(
            (record.panes[&pane()].quiet_until_ms, record == prior),
            (28_000, true)
        );
    }

    #[test]
    fn counted_input_outlives_the_viewer_leaving() {
        let mut record = record();
        observe(&mut record, 20, &clients(), 20_000);
        observe(&mut record, 21, &[], 21_000);
        assert_eq!(
            InputSource::Tty(&record).deadline(&pane(), Some(Duration::from_secs(30))),
            Some(50_000)
        );
    }

    #[test]
    fn both_sources_share_the_hold_setting_but_not_the_input_rule() {
        let mut record = record();
        observe(&mut record, 20, &clients(), 20_000);
        let clients = clients();
        let clocks = BTreeMap::from([(clients[0].client_id.clone(), 25_000)]);
        let actual = [
            InputSource::Tty(&record),
            InputSource::ClientClocks {
                clients: &clients,
                input_ms: &clocks,
            },
        ]
        .map(|source| {
            [
                source.deadline(&pane(), Some(Duration::from_secs(30))),
                source.deadline(&pane(), None),
            ]
        });
        assert_eq!(actual, [[Some(50_000), None], [Some(55_000), None]]);
    }
}
