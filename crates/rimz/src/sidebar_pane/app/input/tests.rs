use super::*;

fn default_keymap() -> NavKeymap {
    NavKeymap::from_config(&crate::config::SidebarKeys::default())
}

#[test]
fn typing_resolves_before_keymap_and_command_table() {
    let keymap = NavKeymap::from_config(&crate::config::SidebarKeys {
        wider: "ctrl+b".to_owned(),
        ..Default::default()
    });
    for ch in "njqr?A1".chars() {
        assert_eq!(
            resolve_key(
                &keymap,
                InputMode::Typing,
                KeyCode::Char(ch),
                KeyModifiers::NONE
            ),
            KeyAction::QueryChar(ch)
        );
    }
    assert_eq!(
        resolve_key(
            &keymap,
            InputMode::Typing,
            KeyCode::Char('b'),
            KeyModifiers::CONTROL
        ),
        KeyAction::Other
    );
    assert_eq!(
        resolve_key(
            &keymap,
            InputMode::Typing,
            KeyCode::Char('B'),
            KeyModifiers::SHIFT
        ),
        KeyAction::QueryChar('B')
    );
    for (code, mods, action) in [
        (KeyCode::Up, KeyModifiers::NONE, KeyAction::Up),
        (KeyCode::Down, KeyModifiers::NONE, KeyAction::Down),
        (KeyCode::Char('n'), KeyModifiers::CONTROL, KeyAction::Down),
        (KeyCode::Char('p'), KeyModifiers::CONTROL, KeyAction::Up),
        (KeyCode::Esc, KeyModifiers::NONE, KeyAction::CancelSearch),
        (KeyCode::Backspace, KeyModifiers::NONE, KeyAction::Backspace),
        (KeyCode::Enter, KeyModifiers::NONE, KeyAction::Enter),
        (KeyCode::Left, KeyModifiers::NONE, KeyAction::Other),
        (KeyCode::Up, KeyModifiers::ALT, KeyAction::Other),
        (KeyCode::Char('y'), KeyModifiers::CONTROL, KeyAction::Other),
    ] {
        assert_eq!(resolve_key(&keymap, InputMode::Typing, code, mods), action);
    }
}

fn encode_default(code: KeyCode) -> Option<String> {
    encode_key(code, KeyModifiers::NONE)
}

fn resolve_wakeup(bytes: &[u8]) -> Wakeup {
    match decode_wakeup(bytes) {
        Wakeup::Press { code, mods } => Wakeup::Key(resolve_key(
            &default_keymap(),
            InputMode::Normal,
            code,
            mods,
        )),
        wakeup => wakeup,
    }
}

#[test]
fn raw_presses_round_trip_without_resolving_actions() {
    for (code, mods, word) in [
        (KeyCode::Char('j'), KeyModifiers::NONE, "press:-:char:j"),
        (KeyCode::Char(':'), KeyModifiers::NONE, "press:-:char::"),
        (KeyCode::Char('+'), KeyModifiers::SHIFT, "press:-:char:+"),
        (KeyCode::Char('/'), KeyModifiers::NONE, "press:-:char:/"),
        (KeyCode::Char(' '), KeyModifiers::NONE, "press:-:char: "),
        (KeyCode::Char('n'), KeyModifiers::CONTROL, "press:c:char:n"),
        (KeyCode::Esc, KeyModifiers::ALT, "press:a:esc"),
        (
            KeyCode::Backspace,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            "press:ca:backspace",
        ),
        (KeyCode::Up, KeyModifiers::NONE, "press:-:up"),
        (KeyCode::Down, KeyModifiers::NONE, "press:-:down"),
        (KeyCode::Left, KeyModifiers::NONE, "press:-:left"),
        (KeyCode::Right, KeyModifiers::NONE, "press:-:right"),
        (KeyCode::Home, KeyModifiers::NONE, "press:-:home"),
        (KeyCode::End, KeyModifiers::NONE, "press:-:end"),
        (KeyCode::PageUp, KeyModifiers::NONE, "press:-:pageup"),
        (KeyCode::PageDown, KeyModifiers::NONE, "press:-:pagedown"),
        (KeyCode::Enter, KeyModifiers::NONE, "press:-:enter"),
        (KeyCode::Tab, KeyModifiers::NONE, "press:-:tab"),
        (KeyCode::Delete, KeyModifiers::NONE, "press:-:delete"),
        (KeyCode::Char('é'), KeyModifiers::NONE, "press:-:char:é"),
    ] {
        assert_eq!(encode_key(code, mods).as_deref(), Some(word));
        assert_eq!(
            decode_wakeup(word.as_bytes()),
            Wakeup::Press {
                code,
                mods: mods & (KeyModifiers::CONTROL | KeyModifiers::ALT),
            }
        );
    }
    assert_eq!(encode_key(KeyCode::F(1), KeyModifiers::NONE), None);
}

#[test]
fn malformed_press_words_decode_as_ticks() {
    for word in [
        "press:",
        "press:-",
        "press:x:up",
        "press:ac:up",
        "press:-:char:",
        "press:-:char:ab",
        "press:-:up:extra",
        "press:-:unknown",
        "key:up",
    ] {
        assert_eq!(decode_wakeup(word.as_bytes()), Wakeup::Tick, "{word}");
    }
}

#[test]
#[cfg(feature = "testkit")]
fn injected_click_decodes_as_mouse_press() {
    let wire = encode_click(4, 7);
    let press = encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 7).unwrap();
    assert_eq!(wire, press);
    assert_eq!(
        decode_wakeup(wire.as_bytes()),
        Wakeup::MouseClick { column: 4, row: 7 }
    );
}

#[test]
fn mouse_events_encode_clicks_and_scrolls() {
    let encoded = encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 7)
        .expect("left button down is encoded");
    assert_eq!(
        decode_wakeup(encoded.as_bytes()),
        Wakeup::MouseClick { column: 4, row: 7 }
    );
    // The release must NOT also encode a click: one physical click is one
    // selection event, so the card's compact→full expansion between press
    // and release can't relocate the highlight.
    assert_eq!(
        encode_mouse(MouseEventKind::Up(MouseButton::Left), 4, 7),
        None
    );
    // The wheel scrolls the viewport, never the selection: it must round-
    // trip to the scroll wakeup, not an arrow key.
    assert_eq!(
        decode_wakeup(
            encode_mouse(MouseEventKind::ScrollUp, 4, 7)
                .unwrap()
                .as_bytes()
        ),
        Wakeup::Scroll { down: false }
    );
    assert_eq!(
        decode_wakeup(
            encode_mouse(MouseEventKind::ScrollDown, 4, 7)
                .unwrap()
                .as_bytes()
        ),
        Wakeup::Scroll { down: true }
    );
}

#[test]
fn r_key_triggers_a_reload() {
    // Pressing `r` re-execs the renderer in place unless the help overlay
    // consumes it first; external reloads keep the shared control word.
    let encoded = encode_default(KeyCode::Char('r')).expect("r is bound");
    assert_eq!(
        resolve_wakeup(encoded.as_bytes()),
        Wakeup::Key(KeyAction::Reload)
    );
    assert_eq!(
        resolve_wakeup(RELOAD_CONTROL_WORD.as_bytes()),
        Wakeup::Reload
    );
}

#[test]
fn sidebar_event_envelope_decodes_to_event() {
    let envelope = SidebarEventEnvelope::new(
        crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        Some("rimz-test".to_owned()),
        42,
        crate::wakeup::events::SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
    );
    let encoded = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(decode_wakeup(&encoded), Wakeup::Event(envelope));
    assert_eq!(decode_wakeup(b"{}"), Wakeup::Tick);
}

#[test]
fn agent_session_boundary_event_requests_fresh_panes() {
    let start = crate::wakeup::events::SidebarEvent::StoreDelta {
        event_method: Some("agent.lifecycle".to_owned()),
        agent_signal: Some(crate::agents::LifecycleSignal::Registered.tag().to_owned()),
    };
    assert!(start.requests_producer_verification());

    let status = crate::wakeup::events::SidebarEvent::StoreDelta {
        event_method: Some("agent.lifecycle".to_owned()),
        agent_signal: Some(
            crate::agents::LifecycleSignal::TurnStarted { turn_id: None }
                .tag()
                .to_owned(),
        ),
    };
    assert!(!status.requests_producer_verification());
}

#[test]
fn keys_round_trip_through_the_wire() {
    // Every raw keypress resolves to the action the serve loop dispatches: vim row/focus keys, the J/K worktree jumps,
    // and the full filter key set. (The `r` reload keypress is covered by
    // `r_key_triggers_a_reload`; the literal reload word is checked below.)
    let cases = [
        // vim row and focus keys
        (
            "j → down",
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Down),
        ),
        (
            "↓ → down",
            KeyCode::Down,
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Down),
        ),
        (
            "k → up",
            KeyCode::Char('k'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Up),
        ),
        (
            "↑ → up",
            KeyCode::Up,
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Up),
        ),
        (
            "l → enter",
            KeyCode::Char('l'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Enter),
        ),
        (
            "a → narrower",
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::WidthNarrower),
        ),
        (
            "d → wider",
            KeyCode::Char('d'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::WidthWider),
        ),
        // worktree-jump keys
        (
            "J → worktree down",
            KeyCode::Char('J'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::WorktreeDown),
        ),
        (
            "K → worktree up",
            KeyCode::Char('K'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::WorktreeUp),
        ),
        // top/bottom jumps
        (
            "g → top",
            KeyCode::Char('g'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Top),
        ),
        (
            "G → bottom",
            KeyCode::Char('G'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::Bottom),
        ),
        (
            "Ctrl+b → page up",
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            Wakeup::Key(KeyAction::PageUp),
        ),
        (
            "PageUp → page up",
            KeyCode::PageUp,
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::PageUp),
        ),
        (
            "Ctrl+f → page down",
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
            Wakeup::Key(KeyAction::PageDown),
        ),
        (
            "PageDown → page down",
            KeyCode::PageDown,
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::PageDown),
        ),
        (
            "H → screen top",
            KeyCode::Char('H'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::ScreenTop),
        ),
        (
            "L → screen bottom",
            KeyCode::Char('L'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::ScreenBottom),
        ),
        // inbox triage: n and Space step forward, N steps back
        (
            "n → inbox next",
            KeyCode::Char('n'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::InboxNext),
        ),
        (
            "space → inbox next",
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::InboxNext),
        ),
        (
            "N → inbox prev",
            KeyCode::Char('N'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::InboxPrev),
        ),
        // read-state hygiene without jumping
        (
            "m → toggle read",
            KeyCode::Char('m'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::MarkToggle),
        ),
        (
            "M → mark all read",
            KeyCode::Char('M'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::MarkAllRead),
        ),
        // filter keys
        (
            "A → all",
            KeyCode::Char('A'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::Filter(None)),
        ),
        (
            "u → unread",
            KeyCode::Char('u'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Unread))),
        ),
        (
            "q → waiting",
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Waiting,
            )))),
        ),
        (
            "! → failed",
            KeyCode::Char('!'),
            KeyModifiers::SHIFT,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Failed,
            )))),
        ),
        (
            "e → failed",
            KeyCode::Char('e'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Failed,
            )))),
        ),
        (
            "o → idle",
            KeyCode::Char('o'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Idle,
            )))),
        ),
        (
            "p → paused",
            KeyCode::Char('p'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Paused,
            )))),
        ),
        (
            "w → running",
            KeyCode::Char('w'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Running,
            )))),
        ),
        (
            "s → success",
            KeyCode::Char('s'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Success,
            )))),
        ),
        (
            "z → sleeping",
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            Wakeup::Key(KeyAction::Filter(Some(BodyFilter::Status(
                AgentStatus::Sleeping,
            )))),
        ),
    ];
    for (label, key, mods, wakeup) in cases {
        let encoded = encode_key(key, mods).expect("key is encoded");
        assert_eq!(resolve_wakeup(encoded.as_bytes()), wakeup, "{label}");
    }
    assert_eq!(
        resolve_key(
            &default_keymap(),
            InputMode::Normal,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL
        ),
        KeyAction::Other,
        "modified fixed keys do not fall back to bare actions"
    );
    // The literal reload control word also decodes to a reload on its own.
    assert_eq!(resolve_wakeup(b"reload"), Wakeup::Reload);
}

#[test]
fn control_words_never_start_with_brace() {
    // The leading-brace discriminator (store delta vs control/input) holds
    // only while no control or input wire word can begin with `{`.
    let mut words = vec![
        "resize".to_owned(),
        RELOAD_CONTROL_WORD.to_owned(),
        SUPERVISOR_HANDOFF_CONTROL_WORD.to_owned(),
        "scroll:up".to_owned(),
        "scroll:down".to_owned(),
        String::from_utf8(SNAPSHOT_WAKEUP.to_vec()).unwrap(),
    ];
    for (code, mods) in [
        (KeyCode::Up, KeyModifiers::NONE),
        (KeyCode::Down, KeyModifiers::NONE),
        (KeyCode::Char('b'), KeyModifiers::CONTROL),
        (KeyCode::PageUp, KeyModifiers::NONE),
        (KeyCode::Char('f'), KeyModifiers::CONTROL),
        (KeyCode::PageDown, KeyModifiers::NONE),
        (KeyCode::Char('H'), KeyModifiers::SHIFT),
        (KeyCode::Char('L'), KeyModifiers::SHIFT),
        (KeyCode::Left, KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::NONE),
        (KeyCode::Enter, KeyModifiers::NONE),
        (KeyCode::Char('j'), KeyModifiers::NONE),
        (KeyCode::Char('k'), KeyModifiers::NONE),
        (KeyCode::Char('J'), KeyModifiers::SHIFT),
        (KeyCode::Char('K'), KeyModifiers::SHIFT),
        (KeyCode::Char('g'), KeyModifiers::NONE),
        (KeyCode::Char('G'), KeyModifiers::SHIFT),
        (KeyCode::Char('l'), KeyModifiers::NONE),
        (KeyCode::Char('n'), KeyModifiers::NONE),
        (KeyCode::Char('N'), KeyModifiers::SHIFT),
        (KeyCode::Char(' '), KeyModifiers::NONE),
        (KeyCode::Char('m'), KeyModifiers::NONE),
        (KeyCode::Char('M'), KeyModifiers::SHIFT),
        (KeyCode::Char('?'), KeyModifiers::SHIFT),
        (KeyCode::Char('A'), KeyModifiers::SHIFT),
        (KeyCode::Char('q'), KeyModifiers::NONE),
        (KeyCode::Char('!'), KeyModifiers::SHIFT),
        (KeyCode::Char('e'), KeyModifiers::NONE),
        (KeyCode::Char('o'), KeyModifiers::NONE),
        (KeyCode::Char('p'), KeyModifiers::NONE),
        (KeyCode::Char('w'), KeyModifiers::NONE),
        (KeyCode::Char('s'), KeyModifiers::NONE),
        (KeyCode::Char('z'), KeyModifiers::NONE),
        (KeyCode::Char('x'), KeyModifiers::NONE),
        (KeyCode::Char('r'), KeyModifiers::NONE),
        (KeyCode::Char('1'), KeyModifiers::NONE),
        (KeyCode::Esc, KeyModifiers::NONE),
        (KeyCode::Char('y'), KeyModifiers::NONE),
        (KeyCode::Char('{'), KeyModifiers::NONE),
    ] {
        if let Some(w) = encode_key(code, mods) {
            words.push(w);
        }
    }
    words.push(encode_mouse(MouseEventKind::Down(MouseButton::Left), 1, 2).unwrap());
    for word in words {
        assert_ne!(
            word.as_bytes().first(),
            Some(&b'{'),
            "{word:?} must not collide with the store-delta discriminator"
        );
    }
}

#[test]
fn digit_keys_round_trip_one_through_nine() {
    for c in '1'..='9' {
        let encoded = encode_key(KeyCode::Char(c), KeyModifiers::NONE).expect("digit is encoded");
        let n = c.to_digit(10).unwrap() as u8;
        assert_eq!(
            resolve_wakeup(encoded.as_bytes()),
            Wakeup::Key(KeyAction::Digit(n))
        );
    }
    // '0' and out-of-range digit wire strings are not selectable rows.
    assert_eq!(
        resolve_wakeup(
            encode_key(KeyCode::Char('0'), KeyModifiers::NONE)
                .expect("unbound keys close help")
                .as_bytes()
        ),
        Wakeup::Key(KeyAction::Other)
    );
    assert_eq!(resolve_wakeup(b"key:digit:0"), Wakeup::Tick);
}

#[test]
fn unbound_keys_round_trip_as_other() {
    for code in [KeyCode::Esc, KeyCode::Char('y')] {
        let encoded = encode_key(code, KeyModifiers::NONE).expect("unbound key is encoded");
        assert_eq!(
            resolve_wakeup(encoded.as_bytes()),
            Wakeup::Key(KeyAction::Other)
        );
    }
}
