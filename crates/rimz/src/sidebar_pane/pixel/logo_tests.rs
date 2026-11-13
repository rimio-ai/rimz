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

fn mark(source: ProviderLogo) -> RgbaImage {
    rasterize_mark(&MarkKey {
        source,
        rgb: RGB,
        side: 64,
    })
    .expect("a mark")
}

#[test]
fn the_box_centres_a_four_row_mark_at_half_row_precision() {
    for (detail_rows, top, rows, half_row) in [
        (1, -2, 5, true),
        (2, -1, 4, false),
        (3, -1, 5, true),
        (4, 0, 4, false),
        (5, 0, 5, true),
        (6, 1, 4, false),
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
    assert_eq!(image.pixel(16, 32), [0x12, 0x34, 0x56, 255]);
    let [red, green, blue, alpha] = image.pixel(48, 32);
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
    let image = mark(source);
    assert!(
        image.data.chunks(4).all(|pixel| pixel[3] == 0),
        "no file, data URL, or sibling SVG is drawn"
    );
}

#[test]
fn an_oversized_or_broken_logo_fails_its_mark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = file_logo(&dir, "big.svg", &" ".repeat(300 * 1024));
    let broken = file_logo(&dir, "broken.svg", "<svg");
    let missing = ProviderLogo::File(dir.path().join("none.svg").display().to_string());
    for source in [big, broken, missing] {
        let key = MarkKey {
            source: source.clone(),
            rgb: RGB,
            side: 64,
        };
        assert!(rasterize_mark(&key).is_err(), "{source:?}");
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
    assert_eq!(image.mark().0, &tiers[1], "the next tier: {image_id}");
}

#[test]
fn a_file_mark_finished_after_a_theme_change_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = MarkKey {
        source: file_logo(&dir, "old.svg", "<svg/>"),
        rgb: RGB,
        side: 64,
    };
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
    let (four, five) = (LogoBox::centred_on(4), LogoBox::centred_on(3));
    let mut logos = ready(&[(shipped("codex"), RGB)], four);
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

    let (first, out) = frame(&mut logos, &mut painter, four, 0);
    let first = first.expect("an id");
    assert!(out.contains(&format!("a=t,f=100,i={first}")), "{out}");
    assert!(out.contains(&format!("i={first},p=1,c=9,r=4")), "{out}");
    let (_, out) = frame(&mut logos, &mut painter, four, 100);
    assert!(out.is_empty(), "an unchanged logo is not sent again: {out}");

    let (moved, out) = frame(&mut logos, &mut painter, five, 200);
    let moved = moved.expect("an id");
    assert!(out.contains(&format!("i={moved},p=1,c=9,r=5")), "{out}");
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
    for (detail_rows, rows, mark_top) in [(4, 4, 0), (3, 5, cell_h / 2)] {
        let logo_box = LogoBox::centred_on(detail_rows);
        let mut logos = ready(&[(square.clone(), RGB)], logo_box);
        logos.intern(square.clone(), RGB, 9, logo_box);
        let (_, image, mark) = logos.visible_or_held().expect("an image");
        let boxed = image.boxed(mark);
        assert_eq!((boxed.width, boxed.height), (9 * CELL_W, rows * cell_h));
        assert_eq!((mark.width, mark.height), (4 * cell_h, 4 * cell_h));
        let painted = |x, y| boxed.pixel(x, y)[3] > 0;
        let rows_painted = (0..boxed.height)
            .filter(|y| painted(boxed.width / 2, *y))
            .collect::<Vec<_>>();
        assert_eq!(rows_painted.first(), Some(&mark_top));
        assert_eq!(rows_painted.len() as u32, 4 * cell_h);
        let left = (0..boxed.width)
            .find(|x| painted(*x, mark_top))
            .expect("painted");
        assert_eq!(
            left,
            (9 * CELL_W - 4 * cell_h) / 2,
            "centred across the column"
        );
    }
}
