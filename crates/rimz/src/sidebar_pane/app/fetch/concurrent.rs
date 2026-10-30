//! Concurrent attachment fixture, using the production inbox and serve-loop decision point.

use super::*;
use crate::sidebar_pane::app::backend::PaneBackend;
use crate::sidebar_pane::app::inbox::WakeupInbox;
use crate::testkit::host_pass::{self, Counter};
use std::sync::Barrier;

#[derive(Default)]
pub struct FanoutThread {
    pub views_locks: u64,
    pub record_loads: u64,
    pub events_takes: u64,
    pub backstops: u64,
    pub publication_wakes: u64,
    pub paints: usize,
    pub cpu_us: u64,
}

pub struct ConcurrentFanout {
    pub publications: usize,
    pub hidden: Vec<FanoutThread>,
    pub watched: FanoutThread,
    pub publisher_views_locks: u64,
    pub total_views_locks: u64,
    pub generations: u64,
    pub paint_latency: Vec<Duration>,
    pub paint_lateness: Vec<Duration>,
    pub reveal_latency: Duration,
}

#[derive(Clone, Default)]
struct Paints(Arc<Mutex<Vec<Instant>>>);

impl std::io::Write for Paints {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().expect("paint stamps").push(Instant::now());
        Ok(())
    }
}

fn thread_cpu_us() -> u64 {
    #[cfg(target_os = "linux")]
    {
        use nix::sys::resource::{UsageWho, getrusage};
        use nix::sys::time::TimeValLike;
        let usage = getrusage(UsageWho::RUSAGE_THREAD).expect("thread CPU");
        (usage.user_time().num_microseconds() + usage.system_time().num_microseconds()) as u64
    }
    #[cfg(not(target_os = "linux"))]
    0
}

fn sample(paints: &Paints) -> FanoutThread {
    let counts = host_pass::thread_counters();
    FanoutThread {
        views_locks: counts.get(Counter::ViewsLock),
        record_loads: counts.get(Counter::ViewsRecord),
        events_takes: counts.get(Counter::EventsTake),
        backstops: counts.get(Counter::TimeoutIdle),
        publication_wakes: counts.get(Counter::WakePublication),
        paints: paints.0.lock().expect("paint stamps").len(),
        cpu_us: thread_cpu_us(),
    }
}

impl FanoutThread {
    fn delta(self, before: Self) -> Self {
        Self {
            views_locks: self.views_locks - before.views_locks,
            record_loads: self.record_loads - before.record_loads,
            events_takes: self.events_takes - before.events_takes,
            backstops: self.backstops - before.backstops,
            publication_wakes: self.publication_wakes - before.publication_wakes,
            paints: self.paints - before.paints,
            cpu_us: self.cpu_us - before.cpu_us,
        }
    }
}

impl IdleCycleFixture {
    /// Run real attachment threads and one publisher. Warm through an idle backstop, measure
    /// content publications and a nudge, then reveal a hidden pane outside the counting window.
    pub fn run_concurrent(mut self, publications: usize) -> ConcurrentFanout {
        assert!(publications >= 5);
        assert!(self.renderers.is_empty());
        self.run_cycle();
        let receivers = std::mem::take(&mut self.receivers);
        let threads = receivers.len();
        assert!(threads >= 2);
        let barrier = Arc::new(Barrier::new(threads + 1));
        let publication = Arc::new(Mutex::new((0, Instant::now())));
        self.sink.publication = Some(publication.clone());
        let (ack, acknowledgments) = std::sync::mpsc::channel();
        let (finished, results) = std::sync::mpsc::channel();
        let (backstop, backstops) = std::sync::mpsc::channel();
        let views = self.sink.host_data.lock().unwrap().views.clone();
        let subscribers = self.sink.cycle.clone();
        let viewed = self
            .sink
            .host_data
            .lock()
            .unwrap()
            .inputs
            .as_ref()
            .unwrap()
            .frame
            .viewed_panes
            .clone();
        let IdleCycleFixture {
            worker,
            sink,
            state,
            ..
        } = self;
        std::thread::scope(|scope| {
            let mut paints = Vec::new();
            let mut watched = None;
            for (index, (subscriber, receiver)) in
                subscribers.into_iter().zip(receivers).enumerate()
            {
                let watching = viewed.contains(&subscriber.own_pane);
                if watching {
                    assert!(watched.replace(index).is_none(), "exactly one watched pane");
                }
                let reveal = subscriber.own_pane.raw() == "sidebar_1";
                let config = worker.config.clone();
                let runtime = worker.runtime.clone();
                let views = views.clone();
                let output = Paints::default();
                paints.push(reveal);
                let barrier = barrier.clone();
                let publication = publication.clone();
                let ack = ack.clone();
                let finished = finished.clone();
                let backstop = backstop.clone();
                scope.spawn(move || {
                    let mut renderer = fixture_renderer(
                        config,
                        runtime,
                        &subscriber,
                        views,
                        super::super::clock::LoopClock::fixture_wall(),
                    );
                    renderer.on_snapshot();
                    assert_eq!(renderer.bench_frame_timing(false), watching);
                    let mut backend = PaneBackend::ambient(
                        std::fs::File::options()
                            .write(true)
                            .open("/dev/null")
                            .unwrap()
                            .into(),
                        || {
                            Ok(ratatui::backend::WindowSize {
                                columns_rows: ratatui::layout::Size::new(40, 60),
                                pixels: ratatui::layout::Size::new(400, 1200),
                            })
                        },
                    )
                    .expect("fixture terminal");
                    backend.play_output(output.clone());
                    let mut terminal = ratatui::Terminal::new(backend).expect("fixture terminal");
                    let mut inbox = WakeupInbox::new(receiver);
                    let counts = host_pass::thread_counters();
                    while renderer.bench_painted().is_none() {
                        renderer
                            .step(&mut inbox, &mut terminal)
                            .expect("warm first frame");
                    }
                    let warm = counts.get(Counter::TimeoutIdle);
                    while if watching {
                        counts.get(Counter::WakePublication) == 0
                    } else {
                        counts.get(Counter::TimeoutIdle) == warm
                    } {
                        renderer
                            .step(&mut inbox, &mut terminal)
                            .expect("warm attachment");
                    }
                    let before = sample(&output);
                    let mut acknowledged = before.publication_wakes;
                    let events = counts.get(Counter::WakeRoomEvent);
                    barrier.wait();
                    barrier.wait();
                    let mut idle_reported = watching;
                    if watching {
                        backstop.send(index).unwrap();
                    }
                    loop {
                        renderer
                            .step(&mut inbox, &mut terminal)
                            .expect("attachment pass");
                        let wakes = counts.get(Counter::WakePublication);
                        if !idle_reported && counts.get(Counter::TimeoutIdle) > before.backstops {
                            backstop.send(index).unwrap();
                            idle_reported = true;
                        }
                        if wakes > acknowledged {
                            let paint = watching
                                .then(|| painted_publication(&renderer, &publication))
                                .flatten();
                            if !watching || paint.is_some() {
                                ack.send((index, paint)).expect("publisher waiting");
                                acknowledged = wakes;
                            }
                        }
                        if counts.get(Counter::WakeRoomEvent) != events {
                            finished
                                .send((index, sample(&output).delta(before)))
                                .unwrap();
                            break;
                        }
                    }
                    let before_reveal = publication.lock().unwrap().0;
                    barrier.wait();
                    if !(watching || reveal) {
                        ack.send((index, None)).unwrap();
                        return;
                    }
                    loop {
                        renderer
                            .step(&mut inbox, &mut terminal)
                            .expect("reveal pass");
                        let publication_revision = publication.lock().unwrap().0;
                        if publication_revision > before_reveal
                            && let Some(paint) = painted_publication(&renderer, &publication)
                        {
                            ack.send((index, Some(paint))).unwrap();
                            break;
                        }
                    }
                });
            }
            let watched = watched.expect("one watched pane");
            scope
                .spawn(move || {
                    let mut fixture = IdleCycleFixture {
                        worker,
                        sink,
                        state,
                        cursors: Vec::new(),
                        renderers: Vec::new(),
                        receivers: Vec::new(),
                    };
                    let own = host_pass::thread_counters();
                    let before = own.get(Counter::ViewsLock);
                    barrier.wait();
                    let total = host_pass::TOTAL.get(Counter::ViewsLock);
                    let generation = views.generation();
                    barrier.wait();
                    let mut latency = Vec::new();
                    let mut lateness = Vec::new();
                    for _ in 0..publications {
                        fixture.run_content_cycle();
                        std::thread::sleep(FOLD_FLOOR);
                        let painted = wait_for_paints(&acknowledgments, threads);
                        let (visible, late) = painted[&watched].expect("watched paint");
                        latency.push(visible);
                        lateness.push(late);
                    }
                    wait_for_passes(&backstops, threads);
                    let event = SidebarEventEnvelope::new(
                        fixture.state.workspace_id.clone(),
                        None,
                        crate::utils::time::unix_now_ms(),
                        SidebarEvent::PanesChanged,
                    );
                    for subscriber in &fixture.sink.cycle {
                        subscriber.wake.send(Wakeup::Event(event.clone())).unwrap();
                    }
                    let mut hidden = Vec::new();
                    let mut watched_counts = None;
                    for _ in 0..threads {
                        let (index, counts) = results
                            .recv_timeout(Duration::from_secs(15))
                            .expect("nudge pass");
                        if index == watched {
                            watched_counts = Some(counts);
                        } else {
                            hidden.push(counts);
                        }
                    }
                    let total_views_locks = host_pass::TOTAL.get(Counter::ViewsLock) - total;
                    let generations = views.generation() - generation;
                    let publisher_views_locks = own.get(Counter::ViewsLock) - before;
                    {
                        let mut data = fixture.sink.host_data.lock().unwrap();
                        let frame = Arc::make_mut(&mut data.inputs.as_mut().unwrap().frame);
                        let pane = PaneId::from_parts(crate::MuxName::Zellij, "sidebar_1");
                        frame.viewed_panes.push(pane.clone());
                        frame.client_views.push(crate::pane::ClientPaneView {
                            client_id: crate::ids::MuxClientId::Zellij(2),
                            pane_id: pane,
                        });
                    }
                    barrier.wait();
                    fixture.run_content_cycle();
                    let painted = wait_for_paints(&acknowledgments, threads);
                    let reveal = paints.iter().position(|reveal| *reveal).unwrap();
                    let (reveal_latency, _) = painted[&reveal].expect("revealed paint");
                    ConcurrentFanout {
                        publications,
                        hidden,
                        watched: watched_counts.unwrap(),
                        publisher_views_locks,
                        total_views_locks,
                        generations,
                        paint_latency: latency,
                        paint_lateness: lateness,
                        reveal_latency,
                    }
                })
                .join()
                .expect("publisher")
        })
    }
}

fn painted_publication(
    renderer: &super::super::loop_state::LoopState,
    publication: &Mutex<(u64, Instant)>,
) -> Option<(Duration, Duration)> {
    let (revision, stamp) = *publication.lock().unwrap();
    let (painted_revision, painted_at, deadline) = renderer.bench_painted()?;
    if painted_revision < revision {
        return None;
    }
    let latency = painted_at.checked_duration_since(stamp)?;
    let lateness = painted_at.duration_since(deadline.map_or(stamp, |due| due.max(stamp)));
    Some((latency, lateness))
}

fn wait_for_paints(
    acknowledgments: &Receiver<(usize, Option<(Duration, Duration)>)>,
    threads: usize,
) -> BTreeMap<usize, Option<(Duration, Duration)>> {
    let mut paints = BTreeMap::new();
    for _ in 0..threads {
        let (index, paint) = acknowledgments
            .recv_timeout(Duration::from_secs(15))
            .expect("publication paint pass");
        assert!(paints.insert(index, paint).is_none());
    }
    paints
}

fn wait_for_passes(acknowledgments: &Receiver<usize>, threads: usize) {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..threads {
        assert!(
            seen.insert(
                acknowledgments
                    .recv_timeout(Duration::from_secs(15))
                    .expect("publication pass")
            )
        );
    }
}
