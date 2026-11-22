//! Repeat suppression for a daemon's formatting layer.
//!
//! A place in the code writes one full line per window: a native event's
//! tracing call site, or for a record bridged from the `log` crate (whose
//! records share one call site per level) its original file and line. Its
//! later events inside the window are counted and never formatted; the first
//! event after the window ends its line with ` suppressed=<n>` and opens the
//! next window. A count whose place never logs again is never written. The key
//! set is fixed by the code, so nothing is evicted.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::callsite::Identifier;
use tracing::subscriber::Interest;
use tracing::{Event, Metadata};
use tracing_log::NormalizeEvent;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Filter};

const WINDOW: Duration = Duration::from_secs(60);

thread_local! {
    /// The count the event this thread's filter just admitted carries. The
    /// formatting layer writes that event on the same thread right after.
    static CARRIED: Cell<u64> = const { Cell::new(0) };
}

/// The shared state of one formatting layer's filter and writer.
#[derive(Clone)]
pub struct RepeatSuppression {
    places: Arc<Mutex<HashMap<PlaceKey, Place>>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

#[derive(PartialEq, Eq, Hash)]
enum PlaceKey {
    CallSite(Identifier),
    LogLine(String, Option<u32>),
    LogModule(String, Option<String>),
}

impl PlaceKey {
    fn of(event: &Event<'_>) -> Self {
        let Some(original) = event.normalized_metadata() else {
            return Self::CallSite(event.metadata().callsite());
        };
        match original.file() {
            Some(file) => Self::LogLine(file.to_owned(), original.line()),
            None => Self::LogModule(
                original.target().to_owned(),
                original.module_path().map(str::to_owned),
            ),
        }
    }
}

struct Place {
    opened: Instant,
    suppressed: u64,
}

impl Default for RepeatSuppression {
    fn default() -> Self {
        Self::with_clock(Arc::new(Instant::now))
    }
}

impl RepeatSuppression {
    fn with_clock(clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            places: Arc::default(),
            clock,
        }
    }

    /// The per-layer filter, chained after the level filter so that only the
    /// events it admits are counted.
    pub fn filter<S>(&self) -> impl Filter<S> + Send + Sync + 'static {
        AdmitFirst(self.clone())
    }

    /// Wrap the layer's writer so an admitted line carries its place's count.
    pub fn writer<W>(&self, inner: W) -> impl for<'a> MakeWriter<'a> + Send + Sync + 'static
    where
        W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
    {
        CountingWriter { inner }
    }

    /// Whether `place`'s event is written. An admitted event leaves the count
    /// its line carries in `CARRIED`.
    fn admit(&self, place: PlaceKey) -> bool {
        let now = (self.clock)();
        let mut places = self
            .places
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(seen) = places.get_mut(&place) else {
            places.insert(
                place,
                Place {
                    opened: now,
                    suppressed: 0,
                },
            );
            CARRIED.set(0);
            return true;
        };
        if now.saturating_duration_since(seen.opened) < WINDOW {
            seen.suppressed += 1;
            return false;
        }
        seen.opened = now;
        CARRIED.set(std::mem::take(&mut seen.suppressed));
        true
    }
}

struct AdmitFirst(RepeatSuppression);

impl<S> Filter<S> for AdmitFirst {
    fn enabled(&self, _meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        true
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        if meta.is_event() {
            Interest::sometimes()
        } else {
            Interest::always()
        }
    }

    fn event_enabled(&self, event: &Event<'_>, _cx: &Context<'_, S>) -> bool {
        self.0.admit(PlaceKey::of(event))
    }

    /// Leaves the level hint to the filter this one is chained after.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }
}

struct CountingWriter<W> {
    inner: W,
}

impl<'a, W: MakeWriter<'a>> MakeWriter<'a> for CountingWriter<W> {
    type Writer = Counted<W::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        Counted {
            inner: self.inner.make_writer(),
            suppressed: 0,
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        Counted {
            inner: self.inner.make_writer_for(meta),
            suppressed: CARRIED.take(),
        }
    }
}

/// One formatted event's writer; the formatting layer writes its line whole.
struct Counted<W> {
    inner: W,
    suppressed: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let suppressed = std::mem::take(&mut self.suppressed);
        if suppressed == 0 {
            return self.inner.write(buf);
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(buf);
        let mut counted = line.to_vec();
        writeln!(counted, " suppressed={suppressed}")?;
        self.inner.write_all(&counted)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests;
