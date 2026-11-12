use std::cell::Cell;
use std::io::Cursor;

use rimz::config::{ThemeMode, ThemeStyle};

use super::*;

fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> Result<()>) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render");
    String::from_utf8(stream.into_inner()).expect("utf8")
}

fn setup(detected: MuxName, ask: bool) -> MuxSetup {
    MuxSetup {
        detected,
        enclosing: None,
        ask,
    }
}

fn questions_with(
    config: &MachineConfig,
    codex: bool,
    mux: MuxSetup,
    art: Option<&str>,
) -> Questions {
    let current = Current::from_config(config, false, codex, mux);
    Questions::from_parts(current, PetArt::Ready(art.map(str::to_owned)))
}

fn questions(config: &MachineConfig, codex: bool, mux: MuxSetup) -> Questions {
    questions_with(config, codex, mux, None)
}

fn walk(questions: &mut Questions, input: &[u8]) -> (bool, String) {
    let total = questions.count();
    let mut finished = false;
    let rendered = strip(|out| {
        finished = questions.walk(1, total, &mut Cursor::new(input.to_vec()), out)?;
        Ok(())
    });
    (finished, rendered)
}

fn ask(questions: &mut Questions, question: Question, input: &[u8]) -> (bool, String) {
    let mut answered = false;
    let rendered = strip(|out| {
        answered = questions.ask(question, "heading", &mut Cursor::new(input.to_vec()), out)?;
        Ok(())
    });
    (answered, rendered)
}

fn review(questions: &mut Questions, hooks: Option<&str>, input: &[u8]) -> (ReviewExit, String) {
    let mut exit = ReviewExit::Quit;
    let rendered = strip(|out| {
        exit = questions.review(hooks, &mut Cursor::new(input.to_vec()), out)?;
        Ok(())
    });
    (exit, rendered)
}

fn hand_changed() -> MachineConfig {
    let mut config = MachineConfig::default();
    config.room.style = RoomStyle::Rimz;
    config.keys.set_keymap("zellij");
    config.mux.default = Some(MuxName::Zellij);
    config.theme.mode = ThemeMode::Truecolor;
    config.theme.glyphs.set = Some("nerd_font".to_owned());
    config.theme.pets.enabled = true;
    config.resume.auto_continue = true;
    config.resume.auto_redeem = true;
    config.harness.idle_compact = IdleCompact::Tokens(250_000);
    config
}

fn change_keys(questions: &Questions) -> Vec<&'static str> {
    questions
        .changes()
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// Every line within the screen width. Input is not echoed here, so a reprompt
/// lands on its prompt's line: each prompt is measured apart from what follows.
fn assert_fits(rendered: &str) {
    for line in rendered.lines().flat_map(|line| line.split_inclusive("] ")) {
        assert!(
            line.chars().count() <= SCREEN_WIDTH,
            "{line:?} passes 80 columns"
        );
    }
}

#[test]
fn enter_at_every_question_and_the_review_writes_nothing() {
    for config in [MachineConfig::default(), hand_changed()] {
        for codex in [false, true] {
            for mux in [setup(MuxName::Tmux, true), setup(MuxName::Zellij, false)] {
                let mut questions = questions(&config, codex, mux);
                let (finished, walked) = walk(&mut questions, &[b'\n'; 12]);
                assert!(finished, "{walked}");
                let (exit, rendered) = review(&mut questions, Some("claude installed"), b"\n");
                assert_eq!(exit, ReviewExit::Write);
                assert_eq!(questions.changes(), Vec::new(), "{walked}{rendered}");
                assert!(
                    rendered.contains("Nothing to write: Enter leaves every setting as it is.")
                );
                assert!(!rendered.contains("was:"));
                assert!(!rendered.contains("rimz reload"));
            }
        }
    }
}

#[test]
fn a_walk_numbers_only_the_questions_it_asks() {
    let mut questions = questions(&MachineConfig::default(), true, setup(MuxName::Tmux, true));
    let total = questions.count() + 1;
    let rendered = strip(|out| {
        questions.walk(2, total, &mut Cursor::new(vec![b'\n'; 12]), out)?;
        Ok(())
    });
    for (number, title) in [
        (2, "Multiplexer"),
        (3, "Room keys"),
        (4, "Color"),
        (5, "Icons"),
        (6, "Pet"),
        (7, "While you're away"),
    ] {
        assert_eq!(
            rendered
                .matches(&format!("{number} of 7 · {title}\n"))
                .count(),
            1
        );
    }

    let unasked = questions_with(
        &MachineConfig::default(),
        true,
        setup(MuxName::Tmux, false),
        None,
    );
    assert_eq!(unasked.count(), 5);
    let mut unasked = unasked;
    let (_, rendered) = walk(&mut unasked, &[b'\n'; 12]);
    assert!(!rendered.contains("Multiplexer"));
    assert!(rendered.contains("1 of 5 · Room keys"));
}

#[test]
fn quit_and_end_of_input_stop_the_walk_and_the_review_unwritten() {
    for input in [&b""[..], b"\n\nq\n", b"\n\nQ\n"] {
        let mut questions = questions(
            &MachineConfig::default(),
            false,
            setup(MuxName::Tmux, false),
        );
        let (finished, _) = walk(&mut questions, input);
        assert!(!finished, "input {input:?}");
    }
    for input in [&b""[..], b"q\n", b"3\nq\n"] {
        let mut questions = questions(
            &MachineConfig::default(),
            false,
            setup(MuxName::Tmux, false),
        );
        let (_, _) = walk(&mut questions, b"2\ny\n\n\n\n");
        assert!(!questions.changes().is_empty());
        let (exit, _) = review(&mut questions, None, input);
        assert_eq!(exit, ReviewExit::Quit, "input {input:?}");
    }
}

#[test]
fn review_reopens_rows_routes_hooks_and_reprompts() {
    let mut questions = questions(
        &MachineConfig::default(),
        false,
        setup(MuxName::Tmux, false),
    );
    let (exit, rendered) = review(&mut questions, Some("claude need an install"), b"1\n");
    assert_eq!(exit, ReviewExit::Hooks);
    assert!(rendered.contains("  1  hooks        claude need an install"));
    assert!(rendered.contains("  2  room keys    yours (tmux keys)"));
    assert!(rendered.contains("  6  away"));

    let (exit, _) = review(&mut questions, None, b"a\n");
    assert_eq!(exit, ReviewExit::WalkAll);

    let (exit, rendered) = review(&mut questions, None, b"9\nx\n3\ny\n\n");
    assert_eq!(exit, ReviewExit::Write);
    assert_eq!(
        rendered
            .matches("  Enter a row number, a, q, or press Enter.")
            .count(),
        2
    );
    assert!(rendered.contains("Use Nerd Font icons?"));
    assert_eq!(
        questions.changes(),
        vec![("theme.glyphs.set", "nerd_font".to_owned())]
    );
    assert!(rendered.contains("  3  icons        Nerd Font   was: plain"));
    assert!(rendered.contains("Enter writes 1 change to ") && rendered.contains("theme.toml.\n"));
}

#[test]
fn review_fits_eighty_columns_and_states_changes_without_colour() {
    let mut config = hand_changed();
    config.notifications.enabled = true;
    config.web.enabled = true;
    assert_eq!(config.web.interface, MachineConfig::default().web.interface);
    let mut questions = questions(&config, true, setup(MuxName::Tmux, true));
    let (_, walked) = walk(&mut questions, b"\n2\nn\nn\nn\n1 2 3\n\n");
    assert_fits(&walked);
    let six = "antigravity, claude, codex, cursor, opencode, pi need an install or refresh";
    let (_, rendered) = review(&mut questions, Some(six), b"\n");
    assert_fits(&rendered);
    assert!(!rendered.contains('\x1b'));
    assert!(
        rendered.contains(
            "  1  hooks        antigravity, claude, codex, cursor, opencode, pi need an\n"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("\n                  install or refresh\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("was: RimZ + Zellij, prefix Ctrl+g"),
        "{rendered}"
    );
    assert!(rendered.contains("  Open rooms switch keys at `rimz reload`."));
    assert!(rendered.contains("  Also on, not asked: alerts when an agent asks or fails\n"));
    assert!(rendered.contains("\n                      web access on loopback\n"));
    assert!(rendered.contains("  Any setting: rimz config set <key> <value>"));
    assert!(
        rendered.contains("Enter writes 7 changes to "),
        "{rendered}"
    );
    assert!(rendered.contains("config.toml and ") && rendered.contains("theme.toml."));
    assert_eq!(
        questions.changes(),
        vec![
            ("keys.keymap", "rimz".to_owned()),
            ("theme.mode", "256".to_owned()),
            ("theme.glyphs.set", "unicode".to_owned()),
            ("theme.pets.enabled", "false".to_owned()),
            ("resume.auto_continue", "false".to_owned()),
            ("resume.auto_redeem", "false".to_owned()),
            ("harness.idle_compact", "off".to_owned()),
        ]
    );
}

#[test]
fn also_on_names_what_the_loaded_config_holds() {
    let mut config = MachineConfig::default();
    config.notifications.enabled = false;
    config.web.enabled = false;
    assert!(also_on(&config).is_empty());
    config.web.enabled = true;
    config.web.interface = "127.0.0.1".to_owned();
    assert_eq!(also_on(&config), ["web access on loopback"]);
}

#[test]
fn each_room_keys_option_writes_exactly_its_keys() {
    let native = MachineConfig::default();
    let mut vim = MachineConfig::default();
    vim.room.style = RoomStyle::Rimz;
    vim.keys.set_keymap("vim");
    for (config, mux, answer, expected) in [
        (&native, MuxName::Tmux, "1", vec![]),
        (&native, MuxName::Tmux, "2", vec![("room.style", "rimz")]),
        (
            &native,
            MuxName::Tmux,
            "3",
            vec![("room.style", "rimz"), ("keys.keymap", "tmux")],
        ),
        (
            &native,
            MuxName::Zellij,
            "3",
            vec![("room.style", "rimz"), ("keys.keymap", "zellij")],
        ),
        (&vim, MuxName::Tmux, "1", vec![("room.style", "native")]),
        (&vim, MuxName::Tmux, "2", vec![("keys.keymap", "rimz")]),
        (&vim, MuxName::Zellij, "3", vec![("keys.keymap", "zellij")]),
    ] {
        let mut questions = questions(config, false, setup(mux, false));
        let (answered, _) = ask(
            &mut questions,
            Question::RoomKeys,
            format!("{answer}\n").as_bytes(),
        );
        assert!(answered);
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(k, v)| (k, v.to_owned()))
            .collect();
        assert_eq!(questions.changes(), expected, "{mux} option {answer}");
    }
}

#[test]
fn a_keys_state_outside_the_options_survives_enter() {
    let mut vim = MachineConfig::default();
    vim.room.style = RoomStyle::Rimz;
    vim.keys.set_keymap("vim");
    let mut zellij_on_tmux = MachineConfig::default();
    zellij_on_tmux.room.style = RoomStyle::Rimz;
    zellij_on_tmux.keys.set_keymap("zellij");
    for (config, name) in [(vim, "vim"), (zellij_on_tmux, "zellij")] {
        let mut questions = questions(&config, false, setup(MuxName::Tmux, false));
        let (_, rendered) = ask(&mut questions, Question::RoomKeys, b"7\n\n");
        assert!(rendered.contains(&format!(
            "  Now: RimZ style, {name} keymap. Enter keeps it."
        )));
        assert!(rendered.contains("Room keys? [keep]"));
        assert!(rendered.contains("  Enter 1, 2 or 3, or press Enter to keep [keep]."));
        assert!(questions.changes().is_empty());
        let (_, rendered) = review(&mut questions, None, b"\n");
        assert!(rendered.contains(&format!("room keys    RimZ style, {name} keymap\n")));
    }
}

#[test]
fn room_keys_text_quotes_the_resolved_chords_per_multiplexer() {
    let facts = KeyFacts::from_config(&MachineConfig::default());
    let tmux = room_keys_options(&facts, MuxName::Tmux, true).join("\n");
    let zellij = room_keys_options(&facts, MuxName::Zellij, true).join("\n");
    assert_ne!(tmux, zellij);
    for text in [&tmux, &zellij] {
        assert_fits(text);
        assert!(text.contains("Alt+j/Alt+k walk agents"));
        assert!(text.contains("Alt+1..9 jump"));
        assert!(text.contains("Alt+p, then: z fullscreen"));
        assert!(text.contains("r resize mode, kept through moves until Esc"));
        assert!(text.contains("your own prefix bindings do not fire in rooms"));
    }
    assert!(tmux.contains("your tmux config and bindings, as they are."));
    assert!(tmux.contains("RimZ + tmux"));
    assert!(tmux.contains("Ctrl+b becomes RimZ's prefix, with tmux-like keys after it"));
    assert!(tmux.contains("Hides the tmux status bar in rooms."));
    assert!(
        tmux.contains("Rooms run on RimZ's own tmux server; sessions outside RimZ are untouched.")
    );
    assert!(!tmux.contains("replace your own Zellij bindings"));
    assert!(zellij.contains("RimZ + Zellij"));
    assert!(zellij.contains("Ctrl+g becomes RimZ's prefix, with Zellij-like keys after it"));
    assert!(zellij.contains("2 and 3 replace your own Zellij bindings in rooms"));
    assert!(!zellij.contains("tmux server"));

    let set_bar = room_keys_options(&facts, MuxName::Tmux, false).join("\n");
    assert!(!set_bar.contains("status bar"));

    let custom: MachineConfig = toml::from_str("[keys]\nprefix = \"Ctrl+a\"\n").expect("config");
    let custom = room_keys_options(&KeyFacts::from_config(&custom), MuxName::Tmux, true).join("\n");
    assert!(custom.contains("Ctrl+a, then: z fullscreen"), "{custom}");
    assert!(custom.contains("Ctrl+a becomes RimZ's prefix"), "{custom}");
    assert!(!custom.contains("Alt+p, then"), "{custom}");

    let gap: MachineConfig =
        toml::from_str("[keys.bind.normal]\n\"Alt+5\" = \"none\"\n").expect("config");
    let gap = room_keys_options(&KeyFacts::from_config(&gap), MuxName::Tmux, true).join("\n");
    assert!(!gap.contains("Alt+1..9"), "{gap}");
    assert!(
        gap.contains("Alt+1/Alt+2/Alt+3/Alt+4/Alt+6/Alt+7/Alt+8/Alt+9 jump"),
        "{gap}"
    );
    assert_fits(&gap);

    let mut arrows = MachineConfig::default();
    arrows.sidebar.focus_key = "Ctrl+Right".to_owned();
    arrows.sidebar.zoom_key = "Ctrl+Left".to_owned();
    let arrows = room_keys_options(&KeyFacts::from_config(&arrows), MuxName::Tmux, true).join("\n");
    assert_fits(&arrows);
    assert!(
        arrows.contains("  Ctrl+Right  sidebar and back\n"),
        "{arrows}"
    );
    assert!(
        arrows.contains("  Ctrl+Left  fullscreen this pane\n"),
        "{arrows}"
    );
}

#[test]
fn away_rows_flip_by_number_and_reprompt_on_unknown_tokens() {
    assert_eq!(parse_rows("1 3", 3), Some(vec![1, 3]));
    assert_eq!(parse_rows("1,3", 3), Some(vec![1, 3]));
    assert_eq!(parse_rows(" 2, 1 ", 3), Some(vec![2, 1]));
    for bad in ["4", "0", "x", "1 x", "1-2", ","] {
        assert_eq!(parse_rows(bad, 3), None, "{bad:?}");
    }

    let mut config = MachineConfig::default();
    config.harness.idle_compact = IdleCompact::Off;
    let mut questions = questions(&config, true, setup(MuxName::Tmux, false));
    let (_, rendered) = ask(&mut questions, Question::Away, b"x\n1 3\n3, 3\n\n");
    assert!(rendered.contains("  Enter row numbers from 1 to 3, or press Enter."));
    assert!(rendered.contains("Numbers flip a row (for example: 1 3). Enter keeps these. [keep]"));
    assert!(rendered.contains("logged (rimz stats --assists)"));
    assert!(rendered.contains("  1  [x] auto-continue"));
    assert_fits(&rendered);
    assert_eq!(
        questions.changes(),
        vec![
            ("resume.auto_continue", "true".to_owned()),
            ("harness.idle_compact", "100k".to_owned()),
        ]
    );
}

#[test]
fn enter_at_away_on_a_fresh_config_leaves_idle_compaction_unwritten() {
    let mut questions = questions(&MachineConfig::default(), true, setup(MuxName::Tmux, false));
    let (_, rendered) = ask(&mut questions, Question::Away, b"\n");
    assert!(rendered.contains("idle compaction"));
    assert!(!change_keys(&questions).contains(&"harness.idle_compact"));
}

#[test]
fn without_codex_the_away_question_never_touches_auto_redeem() {
    let mut config = MachineConfig::default();
    config.resume.auto_redeem = true;
    let mut questions = questions(&config, false, setup(MuxName::Tmux, false));
    let (_, rendered) = ask(&mut questions, Question::Away, b"3\n1 2\n\n");
    assert!(!rendered.contains("auto-redeem"));
    assert!(rendered.contains("  Enter row numbers from 1 to 2, or press Enter."));
    assert!(rendered.contains("Numbers flip a row (for example: 1 2). Enter keeps these."));
    assert!(!rendered.contains("for example: 1 3"));
    assert!(!change_keys(&questions).contains(&"resume.auto_redeem"));
    assert_eq!(questions.current.away.len(), 2);

    let mut with = questions_with(&config, true, setup(MuxName::Tmux, false), None);
    let (_, rendered) = ask(&mut with, Question::Away, b"\n");
    assert!(rendered.contains("spends a Codex reset credit when a limit blocks"));
}

#[test]
fn a_hand_set_idle_floor_prints_and_survives_staying_on() {
    let mut config = MachineConfig::default();
    config.harness.idle_compact = IdleCompact::Tokens(250_000);
    let mut questions = questions(&config, false, setup(MuxName::Tmux, false));
    let (_, rendered) = ask(&mut questions, Question::Away, b"2\n2\n\n");
    assert!(rendered.contains("[x] idle compaction"));
    assert!(rendered.contains("summarizes an idle 250k+ conversation"));
    assert!(questions.changes().is_empty());
    let (_, _) = ask(&mut questions, Question::Away, b"2\n\n");
    assert_eq!(
        questions.changes(),
        vec![("harness.idle_compact", "off".to_owned())]
    );
}

#[test]
fn setup_asks_for_the_multiplexer_only_with_both_and_no_flag() {
    let both = |_| true;
    let only = |only: MuxName| move |mux| mux == only;
    assert!(setup_asks_mux(None, both));
    assert!(!setup_asks_mux(Some(MuxName::Tmux), both));
    assert!(!setup_asks_mux(None, only(MuxName::Tmux)));
    assert!(!setup_asks_mux(None, only(MuxName::Zellij)));
}

#[test]
fn the_first_start_mux_gate_has_one_suppressor_per_input() {
    let base = FirstStartMux {
        first_run: true,
        attended: true,
        flag: None,
        enclosing: None,
    };
    assert!(base.asks(|_| true, || false));
    let cases = [
        (
            "not first run",
            FirstStartMux {
                first_run: false,
                ..base
            },
        ),
        (
            "no terminal",
            FirstStartMux {
                attended: false,
                ..base
            },
        ),
        (
            "a flag",
            FirstStartMux {
                flag: Some(MuxName::Zellij),
                ..base
            },
        ),
        (
            "inside a mux",
            FirstStartMux {
                enclosing: Some(MuxName::Tmux),
                ..base
            },
        ),
    ];
    for (name, gate) in cases {
        let listed = Cell::new(false);
        assert!(!gate.asks(|_| true, || listed.replace(true)), "{name}");
        assert!(
            !listed.get(),
            "{name}: no session listing once the gate is shut"
        );
    }
    assert!(
        !base.asks(|mux| mux == MuxName::Tmux, || false),
        "only tmux installed"
    );
    assert!(
        !base.asks(|mux| mux == MuxName::Zellij, || false),
        "only zellij installed"
    );
    assert!(!base.asks(|_| true, || true), "a live session of this name");
}

#[test]
fn the_mux_bracket_names_its_reason_and_writes_only_a_difference() {
    let inside = MuxChoice::new(Some(MuxName::Zellij), Some(MuxName::Tmux));
    assert_eq!(
        (inside.bracket, inside.reason),
        (MuxName::Zellij, "the one you are inside")
    );
    let stored = MuxChoice::new(None, Some(MuxName::Zellij));
    assert_eq!(
        (stored.bracket, stored.reason),
        (MuxName::Zellij, "your mux.default")
    );
    let shipped = MuxChoice::new(None, None);
    assert_eq!(
        (shipped.bracket, shipped.reason),
        (MuxName::Tmux, "the shipped default")
    );

    let fresh = MachineConfig::default();
    assert!(!mux_answer_differs(&fresh, MuxName::Tmux));
    assert!(mux_answer_differs(&fresh, MuxName::Zellij));
    let mut zellij = MachineConfig::default();
    zellij.mux.default = Some(MuxName::Zellij);
    assert!(!mux_answer_differs(&zellij, MuxName::Zellij));
    assert!(mux_answer_differs(&zellij, MuxName::Tmux));

    let mut first = String::new();
    let answer = {
        let mut out = anstream::StripStream::new(Vec::new());
        let answer = ask_first_start_mux(&fresh, &mut Cursor::new(b"x\n2\n".to_vec()), &mut out)
            .expect("ask");
        first.push_str(&String::from_utf8(out.into_inner()).expect("utf8"));
        answer
    };
    assert_eq!(answer, Some(MuxName::Zellij));
    assert!(first.contains("  1  tmux       ← default: the shipped default"));
    assert!(first.contains("Multiplexer? [1]"));
    assert!(first.contains("  Enter 1 or 2, or press Enter to keep [1]."));
    let mut out = Vec::new();
    assert_eq!(
        ask_first_start_mux(&fresh, &mut Cursor::new(Vec::new()), &mut out).unwrap(),
        None
    );

    // Inside Zellij with nothing stored, Enter keeps Zellij, which differs from
    // the tmux the config resolves to outside a multiplexer, so it is written.
    let mut questions = questions(
        &fresh,
        false,
        MuxSetup {
            detected: MuxName::Zellij,
            enclosing: Some(MuxName::Zellij),
            ask: true,
        },
    );
    let (_, rendered) = ask(&mut questions, Question::Mux, b"\n");
    assert!(rendered.contains("  2  zellij     ← default: the one you are inside"));
    assert_eq!(
        questions.changes(),
        vec![("mux.default", "zellij".to_owned())]
    );
}

#[test]
fn a_review_reached_inside_a_multiplexer_writes_no_unanswered_mux() {
    let inside = MuxSetup {
        detected: MuxName::Zellij,
        enclosing: Some(MuxName::Zellij),
        ask: true,
    };
    for stored in [None, Some(MuxName::Tmux)] {
        let mut config = MachineConfig::default();
        config.mux.default = stored;
        let mut questions = questions(&config, false, inside);
        let (exit, rendered) = review(&mut questions, None, b"\n");
        assert_eq!(exit, ReviewExit::Write);
        assert_eq!(questions.changes(), Vec::new(), "{stored:?}:\n{rendered}");
        assert!(rendered.contains("  1  multiplexer  tmux\n"), "{rendered}");
        assert!(!rendered.contains("was:"), "{rendered}");
        assert!(rendered.contains("Nothing to write: Enter leaves every setting as it is."));
    }
}

#[test]
fn changing_only_the_multiplexer_keeps_a_stored_keymap() {
    let mut config = MachineConfig::default();
    config.room.style = RoomStyle::Rimz;
    config.keys.set_keymap("tmux");
    let mut questions = questions(&config, false, setup(MuxName::Tmux, true));
    let (_, _) = ask(&mut questions, Question::Mux, b"2\n");
    assert_eq!(
        questions.changes(),
        vec![("mux.default", "zellij".to_owned())]
    );
    let (_, rendered) = ask(&mut questions, Question::RoomKeys, b"\n");
    assert!(rendered.contains("Room keys? [keep]"), "{rendered}");
    assert!(rendered.contains("  Now: RimZ style, tmux keymap. Enter keeps it."));
    assert_eq!(
        questions.changes(),
        vec![("mux.default", "zellij".to_owned())]
    );
    let (_, rendered) = review(&mut questions, None, b"\n");
    assert!(
        rendered.contains("room keys    RimZ style, tmux keymap\n"),
        "{rendered}"
    );
    let (_, _) = ask(&mut questions, Question::RoomKeys, b"3\n");
    assert_eq!(
        questions.changes(),
        vec![
            ("mux.default", "zellij".to_owned()),
            ("keys.keymap", "zellij".to_owned()),
        ]
    );
}

#[test]
fn a_reopened_mux_question_keeps_its_reason_beside_the_default() {
    let mut questions = questions(&MachineConfig::default(), false, setup(MuxName::Tmux, true));
    let (_, _) = ask(&mut questions, Question::Mux, b"2\n");
    let (_, rendered) = ask(&mut questions, Question::Mux, b"\n");
    assert!(rendered.contains("  1  tmux       ← default: the shipped default\n"));
    assert!(
        rendered.contains("  2  zellij     ← your answer\n"),
        "{rendered}"
    );
    assert!(rendered.contains("Multiplexer? [2]"), "{rendered}");
    assert_eq!(rendered.matches("← default").count(), 1, "{rendered}");
}

#[test]
fn the_review_says_how_an_open_room_takes_new_keys_per_multiplexer() {
    let mut rimz_style = MachineConfig::default();
    rimz_style.room.style = RoomStyle::Rimz;
    rimz_style.keys.set_keymap("rimz");
    for (mux, answer, note) in [
        (
            MuxName::Tmux,
            "1",
            "  Open rooms switch keys at `rimz reload`.\n",
        ),
        (
            MuxName::Zellij,
            "3",
            "  Open rooms switch keys at `rimz reload`.\n",
        ),
        (
            MuxName::Zellij,
            "1",
            "  Open rooms drop RimZ's keys at `rimz reload`; your own Zellij keys\n  return after a detach and reattach.\n",
        ),
    ] {
        let mut questions = questions(&rimz_style, false, setup(mux, false));
        let (_, _) = ask(
            &mut questions,
            Question::RoomKeys,
            format!("{answer}\n").as_bytes(),
        );
        let (_, rendered) = review(&mut questions, None, b"\n");
        assert!(rendered.contains(note), "{mux} {answer}:\n{rendered}");
        assert_eq!(rendered.matches("rimz reload").count(), 1, "{rendered}");
        assert_fits(&rendered);
    }
}

#[test]
fn the_first_start_defaults_line_names_the_loaded_config() {
    let line =
        strip(|out| write_first_start_defaults(&MachineConfig::default(), MuxName::Tmux, out));
    assert_eq!(
        line,
        "Everything else is on defaults: your tmux keys, auto color, plain glyphs.\n  rimz setup   change any of it\n"
    );
    let line = strip(|out| write_first_start_defaults(&hand_changed(), MuxName::Zellij, out));
    assert!(
        line.contains("RimZ + Zellij keys, truecolor, Nerd Font icons."),
        "{line}"
    );
    let mut vim = MachineConfig::default();
    vim.room.style = RoomStyle::Rimz;
    vim.keys.set_keymap("vim");
    vim.theme.mode = ThemeMode::Indexed;
    vim.theme.glyphs.set = Some("unicode".to_owned());
    let line = strip(|out| write_first_start_defaults(&vim, MuxName::Zellij, out));
    assert!(
        line.contains("RimZ style, vim keymap, 256 colors, Unicode glyphs."),
        "{line}"
    );
    for question in ["?", "[Y/n]", "[y/N]"] {
        assert!(!line.contains(question), "the defaults line asks nothing");
    }
}

#[test]
fn appearance_brackets_fold_terminal_advertisement_and_explicit_theme_choices() {
    let mux = setup(MuxName::Tmux, false);
    let fresh = Current::from_config(&MachineConfig::default(), false, false, mux);
    assert!(!fresh.truecolor && !fresh.nerd_font && !fresh.pet_enabled);
    assert!(Current::from_config(&MachineConfig::default(), true, false, mux).truecolor);

    let mut modern = MachineConfig::default();
    modern.theme.style = Some(ThemeStyle::Modern);
    let current = Current::from_config(&modern, false, false, mux);
    assert!(current.truecolor && current.nerd_font && !current.pet_enabled);

    modern.theme.mode = ThemeMode::Indexed;
    modern.theme.glyphs.set = Some("unicode".to_owned());
    let current = Current::from_config(&modern, true, false, mux);
    assert!(!current.truecolor && !current.nerd_font);

    let mut questions = questions(&MachineConfig::default(), false, mux);
    let (_, rendered) = walk(&mut questions, b"\nmaybe\ny\ny\ny\n\n");
    assert!(rendered.contains("Use truecolor? [y/N]"));
    assert!(rendered.contains("  Enter y or n (q quits)."));
    assert!(
        rendered.contains("Want a pet? It lives in the sidebar and reacts to your fleet. [y/N]")
    );
    assert_eq!(
        questions.changes(),
        vec![
            ("theme.mode", "truecolor".to_owned()),
            ("theme.glyphs.set", "nerd_font".to_owned()),
            ("theme.pets.enabled", "true".to_owned()),
        ]
    );
}

#[test]
fn pet_art_is_injected_only_when_available() {
    let config = MachineConfig::default();
    let mux = setup(MuxName::Tmux, false);
    let mut with_art = questions_with(&config, false, mux, Some("ART\n"));
    let (_, rendered) = ask(&mut with_art, Question::Pet, b"\n");
    let art = rendered.find("ART\n\n").expect("art with a blank line");
    assert!(art < rendered.find("Want a pet?").expect("pet prompt"));

    let mut without = questions(&config, false, mux);
    let (_, rendered) = ask(&mut without, Question::Pet, b"\n");
    assert!(!rendered.contains("ART"));
}

#[test]
fn probe_lines_emit_truecolor_and_aligned_sidebar_glyphs() {
    let gradient = gradient_line();
    let glyphs = glyph_line();
    assert!(gradient.contains("\x1b[38;2;"));
    assert!(gradient.contains('█'));
    assert!(gradient.contains("a full spread of colors"));
    assert!(glyphs.contains(nerd_font_probe_glyphs()[0]));
    assert!(glyphs.contains("eight distinct icons"));
    let glyph_field = glyphs
        .strip_prefix("  ")
        .and_then(|line| line.split("  ←").next())
        .expect("glyph field");
    assert_eq!(glyph_field.chars().count(), GRADIENT_WIDTH);
}

#[test]
fn probe_gradient_steps_smoothly_between_cells() {
    let stops = nerd_font_probe_gradient(GRADIENT_WIDTH);
    assert_eq!(stops.len(), GRADIENT_WIDTH);
    // Interpolated neighbours stay close, which reads as one sweep rather
    // than the hard bands the raw anchor list produced (steps of about 200).
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let delta = (i32::from(a.0) - i32::from(b.0)).abs()
            + (i32::from(a.1) - i32::from(b.1)).abs()
            + (i32::from(a.2) - i32::from(b.2)).abs();
        assert!(delta <= 80, "harsh jump {a:?} -> {b:?} (delta {delta})");
    }
}

#[test]
fn header_uses_title_and_terminal_width_rule_without_box() {
    let rendered = strip(|w| write_header(w));
    assert!(rendered.contains("rimz · first-run setup"));
    assert!(rendered.contains('─'));
    assert!(!rendered.contains('╭'));
    assert_eq!(header_rule(80).chars().count(), 48);
    assert_eq!(header_rule(20).chars().count(), 20);
}
