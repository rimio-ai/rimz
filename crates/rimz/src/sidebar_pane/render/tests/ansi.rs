use super::*;

#[test]
fn line_ansi_serializer_maps_color_and_row_reset_contract() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 2));
    buffer[(0, 0)]
        .set_symbol("A")
        .set_style(Style::default().fg(Color::Reset));
    buffer[(1, 0)].set_symbol("B").set_style(
        Style::default()
            .fg(Color::Rgb(1, 2, 3))
            .bg(Color::Indexed(4))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    );
    buffer[(0, 1)]
        .set_symbol("C")
        .set_style(Style::default().fg(Color::Indexed(9)).bg(Color::Reset));
    buffer[(1, 1)].set_symbol("D").set_style(
        Style::default()
            .fg(Color::Green)
            .bg(Color::Blue)
            .add_modifier(Modifier::DIM),
    );

    let mut ansi = Vec::new();
    write_buffer_line_ansi(&mut ansi, &buffer).unwrap();
    let ansi = String::from_utf8(ansi).unwrap();

    assert_eq!(
        ansi,
        "\x1b[0;38;2;192;202;245mA\
         \x1b[0;1;4;38;2;1;2;3;48;5;4mB\
         \x1b[0m\n\
         \x1b[0;38;5;9mC\
         \x1b[0;2;32;44mD\
         \x1b[0m\n"
    );
}
#[test]
fn fixed_line_ansi_renderer_emits_one_reset_terminated_line_per_frame_row() {
    let snapshot = snapshot_with(Vec::new());

    let mut ansi = Vec::new();
    render_fixed_line_ansi(&mut ansi, &snapshot, None, 16, 5).unwrap();

    let lines = ansi.iter().filter(|byte| **byte == b'\n').count();
    assert_eq!(lines, 5, "one serialized line per fixed terminal row");
    assert!(
        ansi.ends_with(b"\x1b[0m\n"),
        "the last row reset prevents style bleed into downstream renderers"
    );
}
#[test]
fn no_color_theme_suppresses_color_not_shape_modifiers() {
    let style = Theme::fixed(true).alarm(Modifier::BOLD);

    assert_eq!(style.fg, None);
    assert!(style.add_modifier.contains(Modifier::BOLD));
}
