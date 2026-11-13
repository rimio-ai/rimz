//! Provider logos on the pixel dashboard: an SVG mark rasterized on a worker,
//! padded into its placeholder box, and kept resident under one image id per
//! visible block.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Write};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use ratatui::style::Color;
use ratatui::text::Line;

use super::{
    ImagePermit, ImageRequest, ImageResidency, LOGO_ID_CAPACITY, RgbaImage, encode_png,
    logo_image_id,
};
use crate::config::ThemeConfig;
use crate::theme::ProviderLogo;

/// Synthetic pixels per cell column; the terminal scales the image to its box.
const CELL_W: u32 = 32;
/// Rows the square mark spans, whatever its box.
const MARK_ROWS: u32 = 4;

/// One rasterized mark: its source, the colour `currentColor` takes, and its
/// square side in synthetic pixels.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct MarkKey {
    source: ProviderLogo,
    rgb: [u8; 3],
    side: u32,
}

impl MarkKey {
    /// The mark drawn `MARK_ROWS` cells tall, or as wide as `cols`, whichever
    /// is smaller.
    fn sized(source: ProviderLogo, rgb: [u8; 3], cell_h: u32, cols: u16) -> Self {
        let side = (MARK_ROWS * cell_h).min(u32::from(cols) * CELL_W);
        Self { source, rgb, side }
    }
}

fn cell_height(cell_aspect: f32) -> u32 {
    (CELL_W as f32 * cell_aspect).round().max(1.0) as u32
}

/// Where a mark sits beside `detail_rows` rows: centred on them at half-row
/// precision. `top` counts rows from the first detail row (negative above it),
/// and the box covers every whole row the mark touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sidebar_pane) struct LogoBox {
    pub(in crate::sidebar_pane) top: isize,
    pub(in crate::sidebar_pane) rows: u16,
    half_row: bool,
}

impl LogoBox {
    pub(in crate::sidebar_pane) fn centred_on(detail_rows: usize) -> Self {
        let start_halves = detail_rows as isize - MARK_ROWS as isize;
        let half_row = start_halves.rem_euclid(2) == 1;
        Self {
            top: start_halves.div_euclid(2),
            rows: MARK_ROWS as u16 + u16::from(half_row),
            half_row,
        }
    }
}

/// One logo image: its mark and the placeholder box it fills. The box is part
/// of the content, so a box change re-places the image.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sidebar_pane) struct LogoImage {
    mark: MarkKey,
    cell_h: u32,
    cols: u16,
    logo_box: LogoBox,
}

impl LogoImage {
    fn rows(&self) -> u16 {
        self.logo_box.rows
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn mark(&self) -> (&ProviderLogo, u32) {
        (&self.mark.source, self.mark.side)
    }

    /// The mark centred in a transparent image of exactly its box's aspect, so
    /// every renderer that fits the image to the box lands the mark alike.
    fn boxed(&self, mark: &RgbaImage) -> RgbaImage {
        let width = u32::from(self.cols) * CELL_W;
        let height = u32::from(self.logo_box.rows) * self.cell_h;
        let mut image = RgbaImage {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        };
        let left = width.saturating_sub(mark.width) / 2;
        let top = u32::from(self.logo_box.half_row) * self.cell_h / 2
            + (MARK_ROWS * self.cell_h).saturating_sub(mark.height) / 2;
        for y in 0..mark.height.min(height.saturating_sub(top)) {
            let row = mark.width.min(width - left) as usize * 4;
            let from = (y * mark.width) as usize * 4;
            let to = (((top + y) * width) + left) as usize * 4;
            image.data[to..to + row].copy_from_slice(&mark.data[from..from + row]);
        }
        image
    }
}

#[derive(Clone, Debug)]
enum MarkState {
    Loading,
    Ready(Arc<RgbaImage>),
    Failed,
}

/// Marks shared by every pane of the process. A file-sourced mark is dropped
/// when the theme changes, so a file rewritten in place reloads then; the
/// change bumps `generation`, which tells every pane to drop its own copies.
#[derive(Default)]
struct SharedMarks {
    theme: Option<ThemeConfig>,
    generation: u64,
    marks: HashMap<MarkKey, MarkState>,
}

fn shared_marks() -> MutexGuard<'static, SharedMarks> {
    static MARKS: LazyLock<Mutex<SharedMarks>> = LazyLock::new(Mutex::default);
    MARKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A mark that has landed in the shared cache: its image, or `None` for a
/// source that failed. A mark still loading, or never requested, is `None`.
fn landed_mark(key: &MarkKey) -> Option<Option<Arc<RgbaImage>>> {
    match shared_marks().marks.get(key)? {
        MarkState::Loading => None,
        MarkState::Ready(image) => Some(Some(Arc::clone(image))),
        MarkState::Failed => Some(None),
    }
}

/// The mark's state, starting its rasterization on a worker when absent.
fn request_mark(key: &MarkKey) -> MarkState {
    let mut shared = shared_marks();
    if let Some(state) = shared.marks.get(key) {
        return state.clone();
    }
    shared.marks.insert(key.clone(), MarkState::Loading);
    let job = MarkJob {
        key: key.clone(),
        generation: shared.generation,
        state: MarkState::Failed,
    };
    // A failed spawn drops the job here, so its guard must be able to lock.
    drop(shared);
    let spawned = std::thread::Builder::new()
        .name("rimz-logo-raster".to_owned())
        .spawn(move || {
            let state = match rasterize_mark(&job.key) {
                Ok(image) => MarkState::Ready(Arc::new(image)),
                Err(err) => {
                    tracing::debug!(error = %err, "provider logo unavailable");
                    MarkState::Failed
                }
            };
            job.finish(state);
        });
    match spawned {
        Ok(_) => MarkState::Loading,
        Err(err) => {
            tracing::debug!(error = %err, "provider logo worker failed to start");
            MarkState::Failed
        }
    }
}

/// One mark on its worker. However the worker ends, a panic or a failed spawn
/// included, the drop takes the mark out of `Loading`: to its result, or to
/// `Failed`. A file's result is dropped when the theme changed while it ran,
/// since the read may predate a rewrite that change is meant to pick up.
struct MarkJob {
    key: MarkKey,
    generation: u64,
    state: MarkState,
}

impl MarkJob {
    fn finish(mut self, state: MarkState) {
        self.state = state;
    }
}

impl Drop for MarkJob {
    fn drop(&mut self) {
        let mut shared = shared_marks();
        if shared.generation != self.generation && matches!(self.key.source, ProviderLogo::File(_))
        {
            return;
        }
        let state = std::mem::replace(&mut self.state, MarkState::Failed);
        shared.marks.insert(self.key.clone(), state);
    }
}

/// Read, parse and draw one mark: worker-only, never under the views lock.
fn rasterize_mark(key: &MarkKey) -> Result<RgbaImage, String> {
    let bytes = match &key.source {
        ProviderLogo::File(path) => crate::config::read_logo_file(path)?,
        ProviderLogo::Shipped(svg) => svg.as_bytes().to_vec(),
    };
    let tree = crate::config::parse_logo_svg(&bytes, key.rgb)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(key.side, key.side)
        .ok_or_else(|| "logo size is empty".to_owned())?;
    let size = tree.size();
    let side = key.side as f32;
    let scale = (side / size.width()).min(side / size.height());
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(
        (side - size.width() * scale) / 2.0,
        (side - size.height() * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(RgbaImage {
        width: key.side,
        height: key.side,
        data: pixmap.take_demultiplied(),
    })
}

/// The frame's logo ids, lent with the meter table. A source that is not ready
/// returns no id, so the block tries its next tier; an id not seen on screen
/// after composition is freed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct LogoPixels {
    id_base: u32,
    cell_h: u32,
    /// The shared cache's generation `marks` was filled under.
    generation: u64,
    /// Ready marks, or `None` for a source that failed.
    marks: BTreeMap<MarkKey, Option<Arc<RgbaImage>>>,
    wanted: BTreeSet<MarkKey>,
    table: BTreeMap<LogoImage, u32>,
}

impl LogoPixels {
    pub(in crate::sidebar_pane) fn new(id_base: u32) -> Self {
        Self {
            id_base,
            cell_h: 0,
            generation: 0,
            marks: BTreeMap::new(),
            wanted: BTreeSet::new(),
            table: BTreeMap::new(),
        }
    }

    /// Settle marks the last frame asked for, and start the ones not yet
    /// requested. Runs in the paint's prepare step, outside the views lock.
    pub(in crate::sidebar_pane) fn prepare(&mut self, theme: &ThemeConfig, cell_aspect: f32) {
        self.cell_h = cell_height(cell_aspect);
        {
            let mut shared = shared_marks();
            if shared.theme.as_ref() != Some(theme) {
                shared.theme = Some(theme.clone());
                shared.generation += 1;
                shared
                    .marks
                    .retain(|key, _| !matches!(key.source, ProviderLogo::File(_)));
            }
            if self.generation != shared.generation {
                self.generation = shared.generation;
                self.marks
                    .retain(|key, _| !matches!(key.source, ProviderLogo::File(_)));
            }
        }
        self.poll();
    }

    fn poll(&mut self) {
        for key in std::mem::take(&mut self.wanted) {
            match request_mark(&key) {
                MarkState::Loading => {
                    self.wanted.insert(key);
                }
                MarkState::Ready(image) => {
                    self.marks.insert(key, Some(image));
                }
                MarkState::Failed => {
                    self.marks.insert(key, None);
                }
            }
        }
    }

    /// A mark is still on its way: the frame keeps ticking until it lands.
    pub(in crate::sidebar_pane) fn loading(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// The image id for `source` drawn in `cols` columns at `logo_box`, or
    /// `None` when its mark is not ready, failed, or every logo id is held. A
    /// mark another pane already landed is taken from the shared cache at once,
    /// so a fresh table draws it in its first frame.
    pub(in crate::sidebar_pane) fn intern(
        &mut self,
        source: ProviderLogo,
        rgb: [u8; 3],
        cols: u16,
        logo_box: LogoBox,
    ) -> Option<u32> {
        let mark = MarkKey::sized(source, rgb, self.cell_h, cols);
        if !self.marks.contains_key(&mark) {
            let Some(landed) = landed_mark(&mark) else {
                self.wanted.insert(mark);
                return None;
            };
            self.marks.insert(mark.clone(), landed);
        }
        if !matches!(self.marks.get(&mark), Some(Some(_))) {
            return None;
        }
        let image = LogoImage {
            mark,
            cell_h: self.cell_h,
            cols,
            logo_box,
        };
        if let Some(index) = self.table.get(&image) {
            return Some(logo_image_id(self.id_base, *index));
        }
        let held = self.table.values().copied().collect::<BTreeSet<_>>();
        let index = (0..LOGO_ID_CAPACITY).find(|index| !held.contains(index))?;
        self.table.insert(image, index);
        Some(logo_image_id(self.id_base, index))
    }

    /// Keep the ids the composed frame shows; the rest are free again.
    pub(in crate::sidebar_pane) fn observe_visible(&mut self, lines: &[Line<'static>]) {
        let visible = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.content.contains('\u{10eeee}'))
            .filter_map(|span| match span.style.fg {
                Some(Color::Rgb(red, green, blue)) => {
                    Some(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue))
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let id_base = self.id_base;
        self.table
            .retain(|_, index| visible.contains(&logo_image_id(id_base, *index)));
    }

    pub(in crate::sidebar_pane) fn visible_images(
        &self,
    ) -> impl Iterator<Item = (u32, &LogoImage, &Arc<RgbaImage>)> {
        self.table.iter().filter_map(|(image, index)| {
            let mark = self.marks.get(&image.mark)?.as_ref()?;
            Some((logo_image_id(self.id_base, *index), image, mark))
        })
    }

    /// The first interned image, seen or not.
    #[cfg(test)]
    fn visible_or_held(&self) -> Option<(u32, &LogoImage, &Arc<RgbaImage>)> {
        let (image, index) = self.table.iter().next()?;
        let mark = self.marks.get(&image.mark)?.as_ref()?;
        Some((logo_image_id(self.id_base, *index), image, mark))
    }

    /// Wait for every requested mark to land or fail.
    #[cfg(test)]
    pub(in crate::sidebar_pane) fn settle(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        self.poll();
        while self.loading() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll();
        }
        !self.loading()
    }
}

/// The logo tier of a frame drawn without image ids (a hidden mirror, or a
/// routed frame without a permit) on a pane that draws logos once it holds a
/// slot: it reports which marks have landed, so the block keeps the rows its
/// logo takes on arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct LogoReserve {
    cell_h: u32,
}

impl LogoReserve {
    pub(in crate::sidebar_pane) fn new(cell_aspect: f32) -> Self {
        Self {
            cell_h: cell_height(cell_aspect),
        }
    }

    /// The mark for `source` in `cols` columns is ready in the shared cache.
    pub(in crate::sidebar_pane) fn landed(
        &self,
        source: ProviderLogo,
        rgb: [u8; 3],
        cols: u16,
    ) -> bool {
        matches!(
            landed_mark(&MarkKey::sized(source, rgb, self.cell_h, cols)),
            Some(Some(_))
        )
    }
}

/// Logo residency for one held slot.
#[derive(Debug)]
pub(in crate::sidebar_pane) struct LogoPainter {
    residency: ImageResidency<u32, LogoImage>,
}

impl LogoPainter {
    pub(in crate::sidebar_pane) fn new(wrap: bool) -> Self {
        Self {
            residency: ImageResidency::new(wrap),
        }
    }

    /// Transmit each visible logo once (and again when its box changes), and
    /// delete the ids the frame no longer shows.
    pub(in crate::sidebar_pane) fn ensure_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        logos: &LogoPixels,
        now_ms: u64,
        permit: &ImagePermit,
    ) -> io::Result<()> {
        let visible = logos
            .visible_images()
            .map(|(image_id, _, _)| image_id)
            .collect::<BTreeSet<_>>();
        self.residency
            .delete_absent(writer, |image_id| visible.contains(image_id), permit)?;
        for (image_id, image, mark) in logos.visible_images() {
            self.residency.ensure(
                writer,
                ImageRequest {
                    key: image_id,
                    image_id,
                    content: image.clone(),
                    now_ms,
                    cols: image.cols,
                    rows: image.rows(),
                },
                || {
                    let boxed = image.boxed(mark);
                    Arc::from(encode_png(boxed.width, boxed.height, &boxed.data))
                },
                permit,
            )?;
        }
        Ok(())
    }

    pub(in crate::sidebar_pane) fn clear<W: Write>(
        &mut self,
        writer: &mut W,
        permit: &ImagePermit,
    ) -> io::Result<()> {
        self.residency.clear(writer, permit)
    }
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;
