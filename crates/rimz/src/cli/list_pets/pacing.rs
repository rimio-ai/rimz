//! TTY barrier adapter for kitty graphics preview pacing.

pub(crate) struct LiveGraphicsPacer {
    inner: rimz::sidebar_pane::LiveGraphicsPacer,
    active: bool,
}

impl LiveGraphicsPacer {
    pub(crate) fn open() -> Option<Self> {
        rimz::sidebar_pane::LiveGraphicsPacer::open().map(|inner| Self {
            inner,
            active: true,
        })
    }
}

pub(crate) trait PixelPacer {
    fn active(&self) -> bool;
    fn wait_for_barrier(&mut self);
}

impl PixelPacer for LiveGraphicsPacer {
    fn active(&self) -> bool {
        self.active
    }

    fn wait_for_barrier(&mut self) {
        self.active = self.inner.wait_for_barrier();
    }
}
