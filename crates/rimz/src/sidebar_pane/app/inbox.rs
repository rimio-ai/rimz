//! The pane's inbox: a wheel tick takes every tick queued behind it as one run, and the wakeup that ends the run is held back and returned next.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use ratatui::Terminal;

use super::Result;
use super::backend::PaneBackend;
use super::fetch::FetchDispatcher;
use super::input::Wakeup;
use super::loop_state::LoopState;

pub(super) struct WakeupInbox {
    receiver: Receiver<Wakeup>,
    pending: Option<Wakeup>,
}

impl WakeupInbox {
    pub(super) fn new(receiver: Receiver<Wakeup>) -> Self {
        Self {
            receiver,
            pending: None,
        }
    }

    fn recv_timeout(&mut self, timeout: Duration) -> Wakeup {
        self.pending
            .take()
            .unwrap_or_else(|| self.receiver.recv_timeout(timeout).unwrap_or(Wakeup::Tick))
    }

    fn scroll_run(&mut self, first: Wakeup) -> impl Iterator<Item = Wakeup> + '_ {
        std::iter::once(first).chain(std::iter::from_fn(|| {
            let wakeup = self.receiver.try_recv().ok()?;
            if matches!(wakeup, Wakeup::Scroll { .. }) {
                return Some(wakeup);
            }
            self.pending = Some(wakeup);
            None
        }))
    }

    pub(super) fn dispatch_next(
        &mut self,
        state: &mut LoopState,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
        timeout: Duration,
    ) -> Result<()> {
        let wakeup = self.recv_timeout(timeout);
        if matches!(wakeup, Wakeup::Scroll { .. }) {
            return state.on_inputs(self.scroll_run(wakeup), terminal, fetch);
        }
        state.on_wakeup(fetch, terminal, wakeup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::workspace;
    use crate::sidebar_pane::app::input::KeyAction;
    use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
    use std::sync::mpsc::sync_channel;

    #[test]
    fn every_non_scroll_wakeup_ends_the_run_and_is_returned_next() {
        let event = SidebarEventEnvelope::new(
            workspace(),
            Some("rimz-test".to_owned()),
            1,
            SidebarEvent::BodyFilterChanged,
        );
        for boundary in [
            Wakeup::Snapshot,
            Wakeup::Event(event),
            Wakeup::Tick,
            Wakeup::Resize,
            Wakeup::Key(KeyAction::Down),
            Wakeup::MouseClick { column: 5, row: 10 },
            Wakeup::Press {
                code: ratatui::crossterm::event::KeyCode::Down,
                mods: ratatui::crossterm::event::KeyModifiers::NONE,
            },
        ] {
            let (tx, rx) = sync_channel(64);
            let mut inbox = WakeupInbox::new(rx);
            let scroll = Wakeup::Scroll { down: true };
            tx.send(scroll.clone()).unwrap();
            tx.send(scroll.clone()).unwrap();
            tx.send(boundary.clone()).unwrap();
            tx.send(Wakeup::Scroll { down: false }).unwrap();
            let first = inbox.recv_timeout(Duration::ZERO);
            assert_eq!(
                inbox.scroll_run(first).collect::<Vec<_>>(),
                vec![scroll.clone(), scroll]
            );
            // The boundary was removed from the channel, but must precede the
            // next queued tick, even while the sender remains connected.
            assert_eq!(inbox.pending, Some(boundary.clone()));
            assert_eq!(inbox.recv_timeout(Duration::ZERO), boundary);
            assert_eq!(
                inbox.recv_timeout(Duration::ZERO),
                Wakeup::Scroll { down: false }
            );
        }
    }

    #[test]
    fn an_empty_connected_inbox_finishes_the_run_without_waiting() {
        let (tx, rx) = sync_channel(64);
        let mut inbox = WakeupInbox::new(rx);
        tx.send(Wakeup::Scroll { down: false }).unwrap();
        let first = Wakeup::Scroll { down: true };
        assert_eq!(
            inbox.scroll_run(first.clone()).collect::<Vec<_>>(),
            vec![first, Wakeup::Scroll { down: false }]
        );
        assert!(inbox.pending.is_none());
        tx.send(Wakeup::Tick).unwrap();
        assert_eq!(inbox.recv_timeout(Duration::ZERO), Wakeup::Tick);
    }
}
