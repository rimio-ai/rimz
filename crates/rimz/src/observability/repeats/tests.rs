use std::sync::atomic::{AtomicUsize, Ordering};

use tracing_subscriber::filter::{EnvFilter, FilterExt, LevelFilter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use super::*;

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Lines {
    fn text(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// Stands in for the Sentry layer beside the formatting layer.
#[derive(Clone, Default)]
struct Beside(Arc<AtomicUsize>);

impl<S: tracing::Subscriber> Layer<S> for Beside {
    fn on_event(&self, _event: &Event<'_>, _cx: Context<'_, S>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Counts how often the formatting layer renders it.
struct Rendered(Arc<AtomicUsize>);

impl std::fmt::Debug for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fetch_add(1, Ordering::SeqCst);
        f.write_str("rendered")
    }
}

struct Harness {
    lines: Lines,
    beside: Beside,
    now: Arc<Mutex<Instant>>,
    subscriber: tracing::Dispatch,
}

fn harness() -> Harness {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = Arc::clone(&now);
    let suppression = RepeatSuppression::with_clock(Arc::new(move || *clock.lock().unwrap()));
    let lines = Lines::default();
    let beside = Beside::default();
    let fmt = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(suppression.writer(lines.clone()))
        .with_filter(EnvFilter::new("warn").and(suppression.filter()));
    let subscriber = tracing_subscriber::registry()
        .with(fmt)
        .with(beside.clone());
    Harness {
        lines,
        beside,
        now,
        subscriber: tracing::Dispatch::new(subscriber),
    }
}

impl Harness {
    fn run(&self, body: impl FnOnce()) {
        tracing::dispatcher::with_default(&self.subscriber, body);
    }

    fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }
}

fn warn_at_one_place(n: u32) {
    tracing::warn!(n, "one place");
}

fn warn_at_another_place(n: u32) {
    tracing::warn!(n, "another place");
}

#[test]
fn one_place_writes_once_per_window_and_the_next_line_carries_the_count() {
    let h = harness();
    h.run(|| {
        for n in 0..4 {
            warn_at_one_place(n);
        }
    });
    h.advance(WINDOW - Duration::from_millis(1));
    h.run(|| warn_at_one_place(4));
    h.advance(Duration::from_millis(1));
    h.run(|| warn_at_one_place(5));
    h.run(|| warn_at_one_place(6));
    h.advance(WINDOW);
    h.run(|| warn_at_one_place(7));

    let lines = h.lines.text();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].ends_with("one place n=0"), "{lines:?}");
    assert!(
        lines[1].ends_with("one place n=5 suppressed=4"),
        "{lines:?}"
    );
    assert!(
        lines[2].ends_with("one place n=7 suppressed=1"),
        "{lines:?}"
    );
}

#[test]
fn two_places_keep_separate_windows() {
    let h = harness();
    h.run(|| {
        warn_at_one_place(0);
        warn_at_another_place(0);
        warn_at_one_place(1);
        warn_at_another_place(1);
    });

    let lines = h.lines.text();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].ends_with("one place n=0"), "{lines:?}");
    assert!(lines[1].ends_with("another place n=0"), "{lines:?}");
}

#[test]
fn a_suppressed_event_is_not_formatted_and_still_reaches_the_layer_beside() {
    let h = harness();
    let rendered = Arc::new(AtomicUsize::new(0));
    h.run(|| {
        for _ in 0..5 {
            tracing::warn!(probe = ?Rendered(Arc::clone(&rendered)), "probed");
        }
    });

    assert_eq!(rendered.load(Ordering::SeqCst), 1);
    assert_eq!(h.beside.0.load(Ordering::SeqCst), 5);
}

#[test]
fn events_below_the_level_filter_are_not_counted() {
    let h = harness();
    h.run(|| {
        for n in 0..3 {
            tracing::info!(n, "quiet");
        }
    });
    h.advance(WINDOW);
    h.run(|| warn_at_one_place(0));

    assert_eq!(h.lines.text().len(), 1);
    assert_eq!(h.beside.0.load(Ordering::SeqCst), 4);
}

fn log_at_one_place(n: u32) {
    log::warn!("one log place n={n}");
}

fn log_at_another_place(n: u32) {
    log::warn!("another log place n={n}");
}

#[test]
fn records_bridged_from_the_log_crate_keep_their_own_places() {
    let _ = tracing_log::LogTracer::init();
    let h = harness();
    h.run(|| {
        log_at_one_place(0);
        log_at_another_place(0);
        log_at_one_place(1);
        log_at_another_place(1);
        log_at_another_place(2);
    });
    h.advance(WINDOW);
    h.run(|| {
        log_at_one_place(3);
        log_at_another_place(3);
    });

    let lines = h.lines.text();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].ends_with("one log place n=0"), "{lines:?}");
    assert!(lines[1].ends_with("another log place n=0"), "{lines:?}");
    assert!(
        lines[2].ends_with("one log place n=3 suppressed=1"),
        "{lines:?}"
    );
    assert!(
        lines[3].ends_with("another log place n=3 suppressed=2"),
        "{lines:?}"
    );
}

#[test]
fn the_level_filter_keeps_its_level_hint() {
    let suppression = RepeatSuppression::default();
    let filter = EnvFilter::new("warn").and(suppression.filter::<Registry>());

    assert_eq!(
        Filter::<Registry>::max_level_hint(&filter),
        Some(LevelFilter::WARN)
    );
}
