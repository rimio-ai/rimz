//! Bounded input and room lanes; queued wheel ticks share one paint and retain the wakeup that ends their run.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use ratatui::Terminal;

use super::Result;
use super::backend::PaneBackend;
use super::input::Wakeup;
use super::loop_state::LoopState;
#[cfg(any(test, feature = "testkit"))]
use crate::testkit::host_pass::{self, Counter};

pub(super) struct WakeupInbox {
    receiver: Receiver<Wakeup>,
    pending: Option<Wakeup>,
    input: Option<Receiver<Wakeup>>,
    pending_input: Option<Wakeup>,
    #[cfg(any(test, feature = "testkit"))]
    close_requested: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(any(test, feature = "testkit"))]
    received_source: Counter,
}

impl WakeupInbox {
    pub(super) fn with_input(mut self, input: Receiver<Wakeup>) -> Self {
        self.input = Some(input);
        self
    }

    pub(super) fn new(receiver: Receiver<Wakeup>) -> Self {
        Self {
            receiver,
            pending: None,
            input: None,
            pending_input: None,
            #[cfg(any(test, feature = "testkit"))]
            close_requested: None,
            #[cfg(any(test, feature = "testkit"))]
            received_source: Counter::TimeoutIdle,
        }
    }

    fn recv_timeout(&mut self, timeout: Duration) -> Wakeup {
        if let Some(wakeup) = self.pending.take() {
            #[cfg(any(test, feature = "testkit"))]
            {
                self.received_source = wake_source(&wakeup);
            }
            return wakeup;
        }
        let received = self.receiver.recv_timeout(timeout);
        #[cfg(any(test, feature = "testkit"))]
        {
            self.received_source = match &received {
                Ok(wakeup) => wake_source(wakeup),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => host_pass::timeout_source(),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Counter::WakeDisconnected,
            };
        }
        received.unwrap_or(Wakeup::Tick)
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(super) fn track_close(&mut self, requested: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.close_requested = Some(requested);
    }

    #[cfg(any(test, feature = "testkit"))]
    fn closing(&self) -> bool {
        self.close_requested
            .as_ref()
            .is_some_and(|requested| requested.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub(super) fn wait(&mut self, timeout: Duration) {
        if !self.has_input() {
            self.pending = Some(self.recv_timeout(timeout));
        }
        #[cfg(any(test, feature = "testkit"))]
        host_pass::count(if self.closing() {
            Counter::WakeClose
        } else if self.has_input() {
            Counter::WakeInput
        } else {
            self.received_source
        });
    }

    fn scroll_run(&mut self, first: Wakeup) -> impl Iterator<Item = Wakeup> + '_ {
        Self::scroll_from(first, &self.receiver, &mut self.pending)
    }

    fn scroll_from<'a>(
        first: Wakeup,
        receiver: &'a Receiver<Wakeup>,
        pending: &'a mut Option<Wakeup>,
    ) -> impl Iterator<Item = Wakeup> + 'a {
        std::iter::once(first).chain(std::iter::from_fn(move || {
            let wakeup = receiver.try_recv().ok()?;
            if matches!(wakeup, Wakeup::Scroll { .. }) {
                return Some(wakeup);
            }
            *pending = Some(wakeup);
            None
        }))
    }

    pub(super) fn has_input(&mut self) -> bool {
        if self.pending_input.is_none() {
            self.pending_input = self.input.as_ref().and_then(|input| input.try_recv().ok());
        }
        self.pending_input.is_some()
    }

    pub(super) fn dispatch_input(
        &mut self,
        state: &mut LoopState,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        self.has_input();
        let Some(wakeup) = self.pending_input.take() else {
            return Ok(());
        };
        if matches!(wakeup, Wakeup::Scroll { .. })
            && let Some(input) = &self.input
        {
            return state.on_inputs(
                Self::scroll_from(wakeup, input, &mut self.pending_input),
                terminal,
            );
        }
        state.on_wakeup(terminal, wakeup)
    }

    pub(super) fn dispatch_next(
        &mut self,
        state: &mut LoopState,
        terminal: &mut Terminal<PaneBackend>,
        timeout: Duration,
    ) -> Result<()> {
        if self.has_input() {
            return self.dispatch_input(state, terminal);
        }
        let mut wakeup = self.recv_timeout(timeout);
        // Wake hints must not delay already queued work.
        while matches!(wakeup, Wakeup::Tick) {
            let Ok(next) = self.receiver.try_recv() else {
                break;
            };
            wakeup = next;
        }
        if matches!(wakeup, Wakeup::Scroll { .. }) {
            return state.on_inputs(self.scroll_run(wakeup), terminal);
        }
        state.on_wakeup(terminal, wakeup)
    }
}

#[cfg(any(test, feature = "testkit"))]
fn wake_source(wakeup: &Wakeup) -> Counter {
    match wakeup {
        Wakeup::Snapshot => Counter::WakePublication,
        Wakeup::Event(_) => Counter::WakeRoomEvent,
        Wakeup::Tick => Counter::WakeHint,
        Wakeup::FocusFailed { .. } => Counter::WakeFocus,
        _ => Counter::WakeInput,
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
    fn wake_counters_distinguish_receives_timeouts_input_and_close_without_recounting() {
        let counts = host_pass::thread_counters();
        counts.reset();
        let (tx, rx) = sync_channel(64);
        let (input_tx, input_rx) = sync_channel(64);
        let mut inbox = WakeupInbox::new(rx).with_input(input_rx);
        let event =
            SidebarEventEnvelope::new(workspace(), None, 1, SidebarEvent::BodyFilterChanged);
        for (wakeup, source) in [
            (Wakeup::Snapshot, Counter::WakePublication),
            (Wakeup::Event(event), Counter::WakeRoomEvent),
            (Wakeup::Tick, Counter::WakeHint),
            (Wakeup::Resize, Counter::WakeInput),
            (
                Wakeup::FocusFailed {
                    pane: crate::PaneId::from_parts(crate::MuxName::Zellij, "1"),
                    error: "gone".into(),
                },
                Counter::WakeFocus,
            ),
        ] {
            let before = counts.get(source);
            tx.send(wakeup.clone()).unwrap();
            inbox.wait(Duration::ZERO);
            assert_eq!(counts.get(source), before + 1);
            assert_eq!(inbox.recv_timeout(Duration::ZERO), wakeup);
            assert_eq!(
                counts.get(source),
                before + 1,
                "pending is not a second wake"
            );
        }
        for source in [
            Counter::TimeoutIdle,
            Counter::TimeoutFrame,
            Counter::TimeoutWatchdog,
            Counter::TimeoutFreeze,
            Counter::TimeoutFocusDwell,
            Counter::TimeoutTabRead,
            Counter::TimeoutWidth,
            Counter::TimeoutCaps,
            Counter::TimeoutGate,
        ] {
            host_pass::set_timeout_source(source);
            inbox.wait(Duration::ZERO);
            assert_eq!(inbox.recv_timeout(Duration::ZERO), Wakeup::Tick);
            assert_eq!(counts.get(source), 1);
        }
        let inputs = counts.get(Counter::WakeInput);
        assert!(!inbox.has_input());
        assert_eq!(counts.get(Counter::WakeInput), inputs);
        input_tx.send(Wakeup::Resize).unwrap();
        tx.send(Wakeup::Tick).unwrap();
        inbox.wait(Duration::ZERO);
        assert!(inbox.has_input());
        assert!(inbox.has_input());
        assert_eq!(counts.get(Counter::WakeInput), inputs + 1);
        assert_eq!(
            counts.get(Counter::WakeHint),
            1,
            "input's hint is not a second wake"
        );
        inbox.pending_input = None;
        inbox.recv_timeout(Duration::ZERO);
        let closing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        inbox.track_close(closing.clone());
        tx.send(Wakeup::Tick).unwrap();
        inbox.wait(Duration::ZERO);
        inbox.recv_timeout(Duration::ZERO);
        assert_eq!(counts.get(Counter::WakeClose), 1);
        closing.store(false, std::sync::atomic::Ordering::SeqCst);
        drop(tx);
        inbox.wait(Duration::ZERO);
        assert_eq!(counts.get(Counter::WakeDisconnected), 1);
    }

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
