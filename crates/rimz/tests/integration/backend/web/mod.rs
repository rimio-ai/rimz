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
/// Smoke-checks that an attached room keeps delivering output for 20 seconds under saturation.
///
/// The writable and broadcast argv unit tests deterministically guard the ping interval; whether
/// Chromium falls far enough behind to expose the old interval is renderer-dependent.
fn continuous_output_keeps_websocket_attached() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let (tab, frames) = browser.tab_with_websocket_capture(&opened.url, &opened.secret);

    frames.wait_url_contains(
        &format!("arg={}", fixture.workspace.session_name),
        ATTACH_TIMEOUT,
    );
    frames.wait_until_sent(ATTACH_TIMEOUT);
    wait_until_attached(&tab, &fixture.workspace.session_name, ATTACH_TIMEOUT);
    let received_before_output = frames.received();
    fixture.send_line("yes RIMZ_CONTINUOUS_OUTPUT");
    fixture.wait_capture_contains("RIMZ_CONTINUOUS_OUTPUT", ATTACH_TIMEOUT);
    let output_deadline = Instant::now() + ATTACH_TIMEOUT;
    while frames.received() == received_before_output {
        assert!(
            Instant::now() < output_deadline,
            "browser received no WebSocket frames from continuous terminal output"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(20));

    let received = frames.received();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        frames.received() > received,
        "browser stopped receiving continuous terminal output"
    );
    assert_eq!(
        frames.closes(),
        0,
        "ttyd closed the browser WebSocket under continuous output"
    );

    fixture.interrupt();
    fixture.send_line("printf '\\nRIMZ_AFTER_CONTINUOUS_OUTPUT\\n'");
    fixture.wait_capture_contains("RIMZ_AFTER_CONTINUOUS_OUTPUT", ATTACH_TIMEOUT);
}

#[test]
fn paced_tmux_border_drag_tracks_before_release_and_lands_at_endpoint() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    fixture.split_horizontally();
    let opened = fixture.open();
    let browser = BrowserHandle::launch(&stack.browser);
    let url = format!("{}&rimzdebug=1", opened.url);
    let tab = browser.authed_tab(&url, &opened.secret);
    wait_until_attached(&tab, &fixture.workspace.session_name, ATTACH_TIMEOUT);
    std::thread::sleep(Duration::from_secs(3));

    let widths = fixture.pane_widths();
    let initial_width = widths[0];
    let divider = widths[0] + 1;
    let expression = format!(
        r#"(()=>{{
const term=window.term;
const send=(code,x,final="M")=>term.input(`\x1b[<${{code}};${{x}};10${{final}}`,true);
send(0,{divider});
let step=0;
window.__rimzDragMotionDone=false;
window.__rimzDragTimer=setInterval(()=>{{
  step++;
  const x={divider}+(step%21)-10;
  send(32,x);
  if(step===300){{
    clearInterval(window.__rimzDragTimer);
    send(32,{divider}+10);
    window.__rimzDragMotionDone=true;
  }}
}},16);
return "started";
}})()"#
    );
    support::eval_string(&tab, &expression).expect("start browser drag");
    let motion_deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let done = support::eval_string(&tab, "String(Boolean(window.__rimzDragMotionDone))")
            .expect("read browser drag progress");
        if done == "true" {
            break;
        }
        assert!(
            Instant::now() < motion_deadline,
            "browser did not finish the paced motion sequence"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let tracking_endpoint = initial_width + 10;
    let tracking_deadline = Instant::now() + Duration::from_secs(2);
    while fixture.pane_widths()[0] != tracking_endpoint {
        assert!(
            Instant::now() < tracking_deadline,
            "tmux stopped tracking before release; expected width {tracking_endpoint}, got {:?}",
            fixture.pane_widths()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let decisions = support::eval_string(
        &tab,
        "JSON.stringify(window.__rimzWeb?.decisions?.map(item=>item.action)??[])",
    )
    .expect("read mouse-flow decisions");
    assert!(
        decisions.contains("\"coalesce\"") && decisions.contains("\"timer-send\""),
        "live drag did not exercise proactive pacing: {decisions}"
    );

    let release_endpoint = initial_width - 8;
    let release = format!(
        r#"(()=>{{
const send=(code,x,final="M")=>window.term.input(`\x1b[<${{code}};${{x}};10${{final}}`,true);
send(32,{divider}-8);
send(0,{divider}-8,"m");
return "released";
}})()"#
    );
    support::eval_string(&tab, &release).expect("release browser drag");
    let release_deadline = Instant::now() + Duration::from_secs(2);
    while fixture.pane_widths()[0] != release_endpoint {
        assert!(
            Instant::now() < release_deadline,
            "tmux did not land at the released endpoint; expected width {release_endpoint}, got {:?}",
            fixture.pane_widths()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
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
}

#[test]
fn zellij_room_attaches_when_daemon_starts_inside_target_room() {
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
