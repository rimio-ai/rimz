//! Real-browser coverage for RimZ's ttyd web surface.
//!
//! Each test runs a real ttyd daemon, headless Chromium, and an isolated live
//! mux server. Missing browser, ttyd 1.7.5+, or selected mux binaries turn the
//! test into an explicit self-skip so the live tier remains portable.

#![allow(clippy::print_stderr)]

macro_rules! require_web_stack {
    ($mux:literal) => {
        match support::WebStack::resolve($mux) {
            Ok(stack) => stack,
            Err(reason) => {
                eprintln!("{reason}; skipping test");
                return;
            }
        }
    };
}

mod support;

use std::time::{Duration, Instant};

use support::{
    BrowserHandle, LiveWebFixture, LiveZellijWebFixture, assert_no_term, assert_room_attached,
    wait_term_contains,
};

const ATTACH_TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn attach_round_trips_output_and_keystrokes() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    fixture.send_line("printf 'RIMZ_FROM_TMUX\\n'");
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab(&opened.url, &opened.secret);

    wait_term_contains(&tab, "RIMZ_FROM_TMUX", ATTACH_TIMEOUT);
    assert_room_attached(
        &tab,
        &fixture.workspace.session_name,
        &fixture.display_name(),
    );

    tab.type_str("echo RIMZ_FROM_BROWSER")
        .expect("type browser marker");
    tab.press_key("Enter").expect("submit browser marker");
    let capture = fixture.wait_capture_contains("RIMZ_FROM_BROWSER", ATTACH_TIMEOUT);
    assert!(
        capture.contains("RIMZ_FROM_BROWSER"),
        "browser input did not reach tmux:\n{capture}"
    );
}

#[test]
fn wrong_credential_is_refused() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab_allow_error(&opened.url, "definitely-wrong");

    assert_no_term(&tab, Duration::from_secs(5));
    assert_ne!(
        tab_title(&tab),
        format!("{} · RimZ", fixture.display_name()),
        "rejected credential reached the room"
    );
}

#[test]
fn session_manager_lists_rooms_and_attaches() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab(&fixture.base_url(), &opened.secret);

    wait_term_contains(&tab, &fixture.display_name(), ATTACH_TIMEOUT);
    tab.press_key("Enter").expect("attach selected room");
    wait_until_attached(&tab, &fixture.workspace.session_name, ATTACH_TIMEOUT);
    assert_eq!(
        tab_title(&tab),
        format!("{} · RimZ", fixture.display_name())
    );
}

#[test]
fn unknown_room_opens_switcher_with_notice() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab(
        &format!("{}?room=rimz-bogus", fixture.base_url()),
        &opened.secret,
    );

    wait_term_contains(
        &tab,
        "session `rimz-bogus` is not a live RimZ room",
        ATTACH_TIMEOUT,
    );
    wait_term_contains(&tab, &fixture.display_name(), ATTACH_TIMEOUT);
}

#[test]
fn legacy_arg_link_attaches() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    fixture.send_line("printf 'RIMZ_LEGACY_LINK\\n'");
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab(
        &format!(
            "{}?arg={}",
            fixture.base_url(),
            fixture.workspace.session_name
        ),
        &opened.secret,
    );

    wait_term_contains(&tab, "RIMZ_LEGACY_LINK", ATTACH_TIMEOUT);
    wait_until_attached(&tab, &fixture.workspace.session_name, ATTACH_TIMEOUT);
    assert_eq!(
        tab_title(&tab),
        format!("{} · RimZ", fixture.display_name())
    );
}

#[test]
fn share_broadcast_views_without_auth_and_drops_input() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    fixture.send_line("printf 'RIMZ_BROADCAST_OUTPUT\\n'");
    let opened = fixture.share();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.tab(&opened.url);

    wait_term_contains(&tab, "RIMZ_BROADCAST_OUTPUT", ATTACH_TIMEOUT);
    tab.type_str("RIMZ_VIEWER_INPUT")
        .expect("type read-only viewer marker");
    tab.press_key("Enter")
        .expect("submit read-only viewer marker");
    fixture.assert_capture_absent("RIMZ_VIEWER_INPUT", Duration::from_secs(3));

    let second = fixture.add_room("unshared");
    let (_refused, frames) = browser.tab_with_websocket_capture(&format!(
        "{}?room={}",
        fixture.share_base_url(),
        second.session_name
    ));
    frames.wait_contains("this room is not shared", ATTACH_TIMEOUT);
}

#[test]
fn zellij_room_attaches_through_shared_daemon() {
    let stack = require_web_stack!("zellij");
    let fixture = LiveZellijWebFixture::new(&stack);
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let tab = browser.authed_tab(&opened.url, &opened.secret);

    assert_room_attached(
        &tab,
        &fixture.workspace.session_name,
        &fixture.display_name(),
    );
    fixture.send_line("printf 'RIMZ_FROM_ZELLIJ\\n'");
    wait_term_contains(&tab, "RIMZ_FROM_ZELLIJ", ATTACH_TIMEOUT);
}

fn wait_until_attached(tab: &headless_chrome::Tab, session: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let search = support::eval_string(tab, "window.location.search").unwrap_or_default();
        if search.contains(&format!("room={session}")) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "browser did not attach {session}; last search was {search:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn tab_title(tab: &headless_chrome::Tab) -> String {
    support::eval_string(tab, "document.title").unwrap_or_default()
}
