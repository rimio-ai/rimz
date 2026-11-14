use super::super::{LOGO_ID_OFFSET, METER_ID_OFFSET, encode_png};
use super::*;
use ratatui::style::Style;
use ratatui::text::Span;

const BASE: u32 = 0x520000;
const RGB: [u8; 3] = [0x12, 0x34, 0x56];

fn file_logo(dir: &tempfile::TempDir, name: &str, svg: &str) -> ProviderLogo {
    let path = dir.path().join(name);
    std::fs::write(&path, svg).expect("write logo");
    ProviderLogo::File(path.display().to_string())
}

fn shipped(kind: &str) -> ProviderLogo {
    ProviderLogo::Shipped(crate::agents::shipped_logo(kind).expect("a shipped logo"))
}

fn ready(sources: &[(ProviderLogo, [u8; 3])], logo_box: LogoBox) -> LogoPixels {
    let mut logos = LogoPixels::new(BASE);
    logos.prepare(&ThemeConfig::default(), 2.0);
    for (source, rgb) in sources {
        logos.intern(source.clone(), *rgb, 9, logo_box);
    }
    assert!(
        logos.settle(std::time::Duration::from_secs(10)),
        "marks land"
    );
    logos
}

fn placeholder(image_id: u32) -> Line<'static> {
    Line::from(Span::styled(
        super::super::placeholder_cluster(0, 0),
        Style::default().fg(super::super::image_id_color(image_id)),
    ))
}

fn key(source: ProviderLogo, cell_h: u32) -> MarkKey {
    MarkKey::sized(source, RGB, cell_h, 9)
}

fn mark(source: ProviderLogo) -> RgbaImage {
    rasterize_mark(&key(source, 64)).expect("a mark")
}

fn visible(image: &RgbaImage, x: u32, y: u32) -> bool {
    image.pixel(x, y)[3] > 8
}

/// The visible part's first and last column and row, inclusive.
fn visible_bounds(image: &RgbaImage) -> (u32, u32, u32, u32) {
    let xs = (0..image.width)
        .filter(|x| (0..image.height).any(|y| visible(image, *x, y)))
        .collect::<Vec<_>>();
    let ys = (0..image.height)
        .filter(|y| (0..image.width).any(|x| visible(image, x, *y)))
        .collect::<Vec<_>>();
    let (Some(left), Some(right), Some(top), Some(bottom)) =
        (xs.first(), xs.last(), ys.first(), ys.last())
    else {
        panic!("the image paints something visible");
    };
    (*left, *right, *top, *bottom)
}

fn svg(view_box: &str, body: &str) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}">{body}</svg>"#)
}

#[test]
fn the_box_centres_a_three_row_mark_at_half_row_precision() {
    for (detail_rows, top, rows, half_row) in [
        (1, -1, 3, false),
        (2, -1, 4, true),
        (3, 0, 3, false),
        (4, 0, 4, true),
        (5, 1, 3, false),
        (6, 1, 4, true),
    ] {
        assert_eq!(
            LogoBox::centred_on(detail_rows),
            LogoBox {
                top,
                rows,
                half_row
            },
            "{detail_rows} detail rows"
        );
        let mark_top_halves = 2 * top + isize::from(half_row);
        assert_eq!(
            mark_top_halves + 2 * MARK_ROWS as isize,
            2 * detail_rows as isize - mark_top_halves,
            "the mark's middle is the details' middle"
        );
    }
}

#[test]
fn current_color_takes_the_brand_and_other_colours_survive_unmultiplied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = file_logo(
        &dir,
        "two.svg",
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2">
            <rect width="1" height="2" fill="currentColor"/>
            <rect x="1" width="1" height="2" fill="#00ff00" fill-opacity="0.5"/>
        </svg>"##,
    );
    let image = mark(source);
    let (left, right, middle) = (image.width / 4, image.width * 3 / 4, image.height / 2);
    assert_eq!(image.pixel(left, middle), [0x12, 0x34, 0x56, 255]);
    let [red, green, blue, alpha] = image.pixel(right, middle);
    assert_eq!((red, blue), (0, 0));
    assert!(
        (green as i16 - 255).abs() <= 1,
        "demultiplied green: {green}"
    );
    assert!((alpha as i16 - 128).abs() <= 1, "half alpha: {alpha}");
}

#[test]
fn an_untrusted_logo_loads_no_image_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let png = encode_png(1, 1, &[255, 0, 0, 255]);
    let png_path = dir.path().join("red.png");
    std::fs::write(&png_path, &png).expect("write png");
    let data = super::super::base64(&png);
    let svg_path = dir.path().join("red.svg");
    std::fs::write(
        &svg_path,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1" fill="red"/></svg>"#,
    )
    .expect("write svg");
    let source = file_logo(
        &dir,
        "images.svg",
        &format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 2 2">
                <image width="1" height="2" href="{}"/>
                <image x="1" width="1" height="2" xlink:href="data:image/png;base64,{data}"/>
                <image width="2" height="2" href="{}"/>
            </svg>"#,
            png_path.display(),
            svg_path.display()
        ),
    );
    assert_eq!(
        rasterize_mark(&key(source, 64)).err().as_deref(),
        Some("logo paints nothing visible"),
        "no file, data URL, or sibling SVG is drawn"
    );
}

#[test]
fn an_oversized_broken_or_invisible_logo_fails_its_mark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = file_logo(&dir, "big.svg", &" ".repeat(300 * 1024));
    let broken = file_logo(&dir, "broken.svg", "<svg");
    let missing = ProviderLogo::File(dir.path().join("none.svg").display().to_string());
    let blank = file_logo(&dir, "blank.svg", &svg("0 0 1 1", ""));
    let faint = file_logo(
        &dir,
        "faint.svg",
        &svg(
            "0 0 1 1",
            r#"<rect width="1" height="1" fill-opacity="0.02"/>"#,
        ),
    );
    for source in [big, broken, missing, blank, faint] {
        assert!(
            rasterize_mark(&key(source.clone(), 64)).is_err(),
            "{source:?}"
        );
        let mut logos = ready(&[], LogoBox::centred_on(4));
        assert_eq!(
            logos.intern(source.clone(), RGB, 9, LogoBox::centred_on(4)),
            None
        );
        assert!(logos.settle(std::time::Duration::from_secs(10)));
        assert_eq!(
            logos.intern(source, RGB, 9, LogoBox::centred_on(4)),
            None,
            "a failed source keeps returning no id"
        );
    }
}

#[test]
fn a_mark_loading_returns_no_id_until_it_lands() {
    let logo_box = LogoBox::centred_on(3);
    let mut logos = LogoPixels::new(BASE);
    logos.prepare(&ThemeConfig::default(), 2.0);
    assert_eq!(logos.intern(shipped("claude"), RGB, 9, logo_box), None);
    assert!(logos.loading(), "the frame keeps ticking");
    assert!(logos.settle(std::time::Duration::from_secs(10)));
    let image_id = logos
        .intern(shipped("claude"), RGB, 9, logo_box)
        .expect("a ready mark takes an id");
    assert_eq!(image_id, BASE + LOGO_ID_OFFSET);
    assert_eq!(
        logos.intern(shipped("claude"), RGB, 9, logo_box),
        Some(image_id),
        "the same image keeps its id"
    );
    assert!(!logos.loading());
}

#[test]
fn a_mark_whose_drawing_panics_fails_and_the_block_takes_its_next_tier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = file_logo(
        &dir,
        "turbulence.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2">
            <filter id="noise"><feTurbulence baseFrequency="0.1" numOctaves="1" seed="-2147483648"/></filter>
            <rect width="2" height="2" filter="url(#noise)"/>
        </svg>"#,
    );
    let logo_box = LogoBox::centred_on(4);
    let mut logos = ready(&[], logo_box);
    let tiers = [source, shipped("claude")];
    let draw = |logos: &mut LogoPixels| {
        tiers
            .iter()
            .find_map(|tier| logos.intern(tier.clone(), RGB, 9, logo_box))
    };
    draw(&mut logos);
    assert!(
        logos.settle(std::time::Duration::from_secs(10)),
        "the panicked mark leaves loading"
    );
    assert!(!logos.loading(), "the frame stops ticking");
    let image_id = draw(&mut logos).expect("the shipped tier draws");
    let (_, image, _) = logos.visible_or_held().expect("an image");
    assert_eq!(image.source(), &tiers[1], "the next tier: {image_id}");
}

#[test]
fn a_file_mark_finished_after_a_theme_change_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = key(file_logo(&dir, "old.svg", "<svg/>"), 64);
    let shipped_key = MarkKey {
        source: shipped("pi"),
        ..key.clone()
    };
    let generation = shared_marks().generation;
    let jobs = [&key, &shipped_key].map(|key| MarkJob {
        key: key.clone(),
        generation,
        state: MarkState::Failed,
    });
    shared_marks().generation += 1;
    for job in jobs {
        job.finish(MarkState::Ready(Arc::new(mark(shipped("pi")))));
    }
    let shared = shared_marks();
    assert!(
        !shared.marks.contains_key(&key),
        "the file's stale raster is not installed"
    );
    assert!(
        matches!(shared.marks.get(&shipped_key), Some(MarkState::Ready(_))),
        "a shipped mark cannot be stale"
    );
}

#[test]
fn a_fresh_table_draws_a_mark_another_pane_landed_in_its_first_frame() {
    let logo_box = LogoBox::centred_on(4);
    ready(&[(shipped("codex"), RGB)], logo_box);
    let mut fresh = LogoPixels::new(BASE);
    fresh.prepare(&ThemeConfig::default(), 2.0);
    assert!(
        fresh.intern(shipped("codex"), RGB, 9, logo_box).is_some(),
        "the first frame after a slot retake draws the logo"
    );
    assert!(!fresh.loading());
    let reserve = LogoReserve::new(2.0);
    assert!(reserve.landed(shipped("codex"), RGB, 9));
    assert!(!reserve.landed(shipped("grok"), RGB, 9), "never requested");
    assert!(
        !LogoReserve::new(3.0).landed(shipped("codex"), RGB, 9),
        "another cell aspect is another mark"
    );
}

#[test]
fn a_file_rewritten_in_place_reloads_in_every_pane_on_a_theme_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fill = |colour: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1" fill="{colour}"/></svg>"#
        )
    };
    let source = file_logo(&dir, "mine.svg", &fill("#ff0000"));
    let logo_box = LogoBox::centred_on(4);
    let landed = |logos: &mut LogoPixels| {
        logos.intern(source.clone(), RGB, 9, logo_box);
        assert!(logos.settle(std::time::Duration::from_secs(10)));
        logos
            .intern(source.clone(), RGB, 9, logo_box)
            .expect("an id");
        let (_, _, mark) = logos.visible_or_held().expect("an image");
        mark.pixel(mark.width / 2, mark.height / 2)
    };
    let mut panes = [
        ready(&[(source.clone(), RGB)], logo_box),
        ready(&[(source.clone(), RGB)], logo_box),
    ];
    for pane in &mut panes {
        assert_eq!(landed(pane), [255, 0, 0, 255], "the first file lands");
    }

    std::fs::write(dir.path().join("mine.svg"), fill("#00ff00")).expect("rewrite logo");
    let changed = ThemeConfig {
        scheme: Some("changed".to_owned()),
        ..ThemeConfig::default()
    };
    for pane in &mut panes {
        pane.prepare(&changed, 2.0);
    }
    for (index, pane) in panes.iter_mut().enumerate() {
        assert_eq!(
            landed(pane),
            [0, 255, 0, 255],
            "pane {index} draws the rewritten file"
        );
    }
}

#[test]
fn logo_ids_stay_between_the_sprites_and_the_meters_and_free_when_unseen() {
    let logo_box = LogoBox::centred_on(4);
    let sources = (0..=LOGO_ID_CAPACITY)
        .map(|index| (shipped("pi"), [index as u8, 1, 2]))
        .collect::<Vec<_>>();
    let mut logos = ready(&sources, logo_box);
    let ids = sources
        .iter()
        .map(|(source, rgb)| logos.intern(source.clone(), *rgb, 9, logo_box))
        .collect::<Vec<_>>();
    let held = ids.iter().flatten().copied().collect::<Vec<_>>();
    assert_eq!(held.len() as u32, LOGO_ID_CAPACITY, "one id per logo");
    assert_eq!(ids.last(), Some(&None), "past the range a logo gets no id");
    assert!(
        held.iter()
            .all(|id| (BASE + LOGO_ID_OFFSET..BASE + METER_ID_OFFSET).contains(id))
    );
    assert!(super::super::sprite_image_id(BASE, 71) < BASE + LOGO_ID_OFFSET);

    logos.observe_visible(&[placeholder(held[3])]);
    assert_eq!(
        logos
            .visible_images()
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>(),
        [held[3]]
    );
    let (source, rgb) = &sources[LOGO_ID_CAPACITY as usize];
    assert_eq!(
        logos.intern(source.clone(), *rgb, 9, logo_box),
        Some(held[0]),
        "an id no longer seen is free again"
    );
}

#[test]
fn a_logo_transmits_once_moves_with_its_box_and_is_deleted_when_gone() {
    let permit = ImagePermit::for_test();
    let (three, four) = (LogoBox::centred_on(3), LogoBox::centred_on(4));
    let mut logos = ready(&[(shipped("codex"), RGB)], three);
    let mut painter = LogoPainter::new(false);
    let frame = |logos: &mut LogoPixels, painter: &mut LogoPainter, logo_box, now_ms| {
        let image_id = logos.intern(shipped("codex"), RGB, 9, logo_box);
        let lines = image_id.map(placeholder).into_iter().collect::<Vec<_>>();
        logos.observe_visible(&lines);
        let mut out = Vec::new();
        painter
            .ensure_transmitted(&mut out, logos, now_ms, &permit)
            .expect("write");
        (image_id, String::from_utf8_lossy(&out).into_owned())
    };

    let (first, out) = frame(&mut logos, &mut painter, three, 0);
    let first = first.expect("an id");
    assert!(out.contains(&format!("a=t,f=100,i={first}")), "{out}");
    assert!(out.contains(&format!("i={first},p=1,c=9,r=3")), "{out}");
    let (_, out) = frame(&mut logos, &mut painter, three, 100);
    assert!(out.is_empty(), "an unchanged logo is not sent again: {out}");

    let (moved, out) = frame(&mut logos, &mut painter, four, 200);
    let moved = moved.expect("an id");
    assert!(out.contains(&format!("i={moved},p=1,c=9,r=4")), "{out}");
    assert!(
        out.contains(&format!("a=d,d=I,i={first}")) || moved == first,
        "the old box's image goes: {out}"
    );

    let mut out = Vec::new();
    logos.observe_visible(&[]);
    painter
        .ensure_transmitted(&mut out, &logos, 300, &permit)
        .expect("write");
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains(&format!("a=d,d=I,i={moved}")), "{out}");
}

#[test]
fn the_boxed_image_holds_the_box_aspect_with_the_mark_centred() {
    let dir = tempfile::tempdir().expect("tempdir");
    let square = file_logo(
        &dir,
        "square.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>"#,
    );
    let cell_h = 64;
    for (detail_rows, rows, mark_top) in [(3, 3, 0), (4, 4, cell_h / 2)] {
        let logo_box = LogoBox::centred_on(detail_rows);
        let mut logos = ready(&[(square.clone(), RGB)], logo_box);
        logos.intern(square.clone(), RGB, 9, logo_box);
        let (_, image, mark) = logos.visible_or_held().expect("an image");
        let boxed = image.boxed(mark);
        assert_eq!((boxed.width, boxed.height), (9 * CELL_W, rows * cell_h));
        assert_eq!((mark.width, mark.height), (3 * cell_h, 3 * cell_h));
        let painted = |x, y| boxed.pixel(x, y)[3] > 0;
        let rows_painted = (0..boxed.height)
            .filter(|y| painted(boxed.width / 2, *y))
            .collect::<Vec<_>>();
        assert_eq!(rows_painted.first(), Some(&mark_top));
        assert_eq!(rows_painted.len() as u32, 3 * cell_h);
        let left = (0..boxed.width)
            .find(|x| painted(*x, mark_top))
            .expect("painted");
        assert_eq!(
            left,
            (9 * CELL_W - 3 * cell_h) / 2,
            "centred across the column"
        );
    }
}

#[test]
fn a_margin_around_the_drawing_does_not_size_its_mark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rect = |x: f32, y: f32| {
        format!(r#"<rect x="{x}" y="{y}" width="2.1" height="2.1" fill="currentColor"/>"#)
    };
    let tight = file_logo(&dir, "tight.svg", &svg("0 0 2.1 2.1", &rect(0.0, 0.0)));
    let margin = file_logo(&dir, "margin.svg", &svg("0 0 10 10", &rect(3.37, 4.21)));
    for cell_h in [64, 69] {
        let marks = [&tight, &margin]
            .map(|source| rasterize_mark(&key(source.clone(), cell_h)).expect("a mark"));
        for image in &marks {
            assert_eq!((image.width, image.height), (3 * cell_h, 3 * cell_h));
            assert_eq!(
                visible_bounds(image),
                (0, image.width - 1, 0, image.height - 1),
                "the visible part fills the mark"
            );
        }
    }
}

#[test]
fn a_wide_drawing_fills_the_column_and_never_passes_either_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let wide = file_logo(
        &dir,
        "wide.svg",
        &svg(
            "0 0 20 20",
            r#"<rect x="1" y="7" width="13" height="2.6"/>"#,
        ),
    );
    let image = rasterize_mark(&key(wide, 64)).expect("a mark");
    assert_eq!(image.width, 9 * CELL_W);
    assert!(image.height < 3 * 64, "shorter than three rows");
    let shaped = (9 * CELL_W) as f32 * 2.6 / 13.0;
    assert!(
        (image.height as f32 - shaped).abs() <= 1.0,
        "its shape kept: {} against {shaped}",
        image.height
    );

    for (index, (width, height)) in [(7.31, 2.9), (2.9, 7.31), (5.0, 1.666), (3.3, 3.3)]
        .into_iter()
        .enumerate()
    {
        let source = file_logo(
            &dir,
            &format!("shape{index}.svg"),
            &svg(
                "0 0 9 9",
                &format!(r#"<rect x="0.7" y="0.3" width="{width}" height="{height}"/>"#),
            ),
        );
        for cell_h in [37, 61, 64, 69, 75, 97, 128] {
            let image = rasterize_mark(&key(source.clone(), cell_h)).expect("a mark");
            let (cap_w, cap_h) = (9 * CELL_W, 3 * cell_h);
            assert!(
                image.width <= cap_w && image.height <= cap_h,
                "{width}x{height} at {cell_h}: {}x{}",
                image.width,
                image.height
            );
            assert!(
                image.width == cap_w || image.height == cap_h,
                "{width}x{height} at {cell_h} meets a limit: {}x{}",
                image.width,
                image.height
            );
        }
    }
}

#[test]
fn a_drawing_off_centre_in_its_canvas_lands_centred_in_its_box() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tall = file_logo(
        &dir,
        "tall.svg",
        &svg("0 0 10 10", r#"<rect x="1" y="6" width="2" height="3"/>"#),
    );
    let wide = file_logo(
        &dir,
        "wide.svg",
        &svg(
            "0 0 10 10",
            r#"<rect x="0.5" y="0.5" width="9" height="1"/>"#,
        ),
    );
    for cell_h in [64, 69] {
        for source in [&tall, &wide] {
            let mark_key = key(source.clone(), cell_h);
            let mark = rasterize_mark(&mark_key).expect("a mark");
            for detail_rows in 1..=6 {
                let logo_box = LogoBox::centred_on(detail_rows);
                let image = LogoImage {
                    mark: mark_key.clone(),
                    cell_h,
                    cols: 9,
                    logo_box,
                };
                let boxed = image.boxed(&mark);
                let (left, right, top, bottom) = visible_bounds(&boxed);
                let gaps = (left, boxed.width - 1 - right);
                assert!(gaps.0.abs_diff(gaps.1) <= 1, "centred across: {gaps:?}");
                let centre_twice = top + bottom + 1;
                let details_twice = (detail_rows as isize - 2 * logo_box.top) as u32 * cell_h;
                assert!(
                    centre_twice.abs_diff(details_twice) <= 2,
                    "{detail_rows} detail rows at cell height {cell_h}: the visible middle \
                     {centre_twice} against the details' {details_twice}, in half pixels"
                );
            }
        }
    }
}

#[test]
fn every_shipped_logo_meets_one_limit_at_the_default_aspect() {
    let cell_h = cell_height(crate::config::CellAspect::NEUTRAL.ratio());
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("logos");
    let mut names = std::fs::read_dir(&dir)
        .expect("the logos directory")
        .filter_map(|entry| {
            let path = entry.expect("an entry").path();
            let name = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "svg").then_some(name)
        })
        .collect::<Vec<_>>();
    names.sort();
    assert!(!names.is_empty());
    for name in names {
        let image = rasterize_mark(&key(shipped(&name), cell_h)).expect("a mark");
        assert!(
            image.width <= 9 * CELL_W && image.height <= 3 * cell_h,
            "{name}: {}x{}",
            image.width,
            image.height
        );
        assert!(
            image.width == 9 * CELL_W || image.height == 3 * cell_h,
            "{name} meets a limit: {}x{}",
            image.width,
            image.height
        );
    }
}
