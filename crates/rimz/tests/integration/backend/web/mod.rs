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

use headless_chrome::protocol::cdp::Input;
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
    const MOTION_DURATION: Duration = Duration::from_millis(4_800);

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
    let motion_duration_ms = MOTION_DURATION.as_millis();
    let expression = format!(
        r#"(()=>{{
const term=window.term;
const send=(code,x,final="M")=>term.input(`\x1b[<${{code}};${{x}};10${{final}}`,true);
send(0,{divider});
let step=0;
const startedAt=performance.now();
window.__rimzDragMotionDone=false;
window.__rimzDragMotionSteps=step;
window.__rimzDragTimer=setInterval(()=>{{
  step++;
  window.__rimzDragMotionSteps=step;
  const x={divider}+(step%21)-10;
  send(32,x);
  if(performance.now()-startedAt>={motion_duration_ms}){{
    clearInterval(window.__rimzDragTimer);
    send(32,{divider}+10);
    window.__rimzDragMotionDone=true;
  }}
}},16);
return "started";
}})()"#
    );
    support::eval_string(&tab, &expression).expect("start browser drag");
    let motion_deadline = Instant::now() + MOTION_DURATION + Duration::from_secs(5);
    loop {
        let done = support::eval_string(&tab, "String(Boolean(window.__rimzDragMotionDone))")
            .expect("read browser drag progress");
        if done == "true" {
            break;
        }
        if Instant::now() >= motion_deadline {
            let steps = support::eval_string(&tab, "String(window.__rimzDragMotionSteps)")
                .unwrap_or_else(|err| format!("unavailable ({err})"));
            panic!("browser did not finish the paced motion sequence; callbacks={steps}");
        }
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
fn tmux_mouse_mode_churn_keeps_held_drag_tracking() {
    let stack = require_web_stack!("tmux");
    let fixture = LiveWebFixture::new(&stack);
    fixture.enable_mouse();
    fixture.send_line(
        "for i in $(seq 1 140); do printf 'RIMZ-%03d-XXXXXXXXXXXXXXXXXXXXXXXX\\n' \"$i\"; done",
    );
    fixture.wait_capture_contains("RIMZ-140-", ATTACH_TIMEOUT);
    fixture.prepare_mouse_mode_churn();
    let opened = fixture.open();
    let browser = BrowserHandle::launch_with_size(&stack.browser, (1280, 1100));
    let url = format!("{}&rimzdebug=1", opened.url);
    let (tab, frames) = browser.tab_with_websocket_capture(&url, &opened.secret);
    wait_until_attached(&tab, &fixture.workspace.session_name, ATTACH_TIMEOUT);
    std::thread::sleep(Duration::from_secs(3));

    support::eval_string(
        &tab,
        r#"(()=>{try{
const term=window.term;
term.options.fontSize=6;
const host=term.element.parentElement;
host.style.width="240px";
host.style.height="990px";
for(let index=0;index<4;index++){
  term.fit();
  host.style.width=`${host.getBoundingClientRect().width*64/term.cols}px`;
  host.style.height=`${host.getBoundingClientRect().height*120/term.rows}px`;
}
term.fit();
const rect=term.element.querySelector(".xterm-screen").getBoundingClientRect();
const cell=term._core._renderService.dimensions.css.cell;
window.__probeRect={x:rect.x,y:rect.y,cellWidth:cell.width,cellHeight:cell.height,cols:term.cols,rows:term.rows};
return JSON.stringify(window.__probeRect);
}catch(error){return String(error&&error.stack||error)}})()"#,
    )
    .expect("configure churn regression terminal");
    std::thread::sleep(Duration::from_secs(1));

    let pane_geometry = fixture.target_pane_geometry();
    let values = pane_geometry
        .split(',')
        .map(|value| value.parse::<u16>().expect("numeric pane geometry"))
        .collect::<Vec<_>>();
    let [pane_left, pane_top, pane_width, pane_height] = values.as_slice() else {
        panic!("unexpected target pane geometry: {pane_geometry}");
    };
    assert!(
        *pane_width >= 10 && *pane_height > 110,
        "target pane is not usable for the drag: {pane_geometry}"
    );
    let raw_x = pane_left + (pane_width / 2).max(1);
    let start_raw_y = pane_top + 5;
    let end_raw_y = pane_top + 110;
    let pointer = support::eval_string(
        &tab,
        &format!(
            r#"(()=>{{const r=window.__probeRect;return JSON.stringify({{
x:r.x+({raw_x}+.5)*r.cellWidth,
startY:r.y+({start_raw_y}+.5)*r.cellHeight,
endY:r.y+({end_raw_y}+.5)*r.cellHeight
}})}})()"#
        ),
    )
    .expect("read churn drag coordinates");
    let pointer: serde_json::Value = serde_json::from_str(&pointer).expect("pointer JSON");
    let x = pointer["x"].as_f64().expect("x");
    let start_y = pointer["startY"].as_f64().expect("start y");
    let end_y = pointer["endY"].as_f64().expect("end y");

    fixture.begin_copy_selection();
    dispatch_mouse(
        &tab,
        Input::DispatchMouseEventTypeOption::MouseMoved,
        x,
        start_y,
        None,
        None,
    );
    dispatch_mouse(
        &tab,
        Input::DispatchMouseEventTypeOption::MousePressed,
        x,
        start_y,
        Some(Input::MouseButton::Left),
        Some(1),
    );
    let down_deadline = Instant::now() + Duration::from_secs(2);
    while !frames.sent_payloads().iter().any(|payload| {
        payload
            .windows(b"\x1b[<0;".len())
            .any(|part| part == b"\x1b[<0;")
    }) {
        assert!(
            Instant::now() < down_deadline,
            "physical mousedown produced no tmux mouse press report"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let selection_deadline = Instant::now() + Duration::from_secs(2);
    while fixture.target_copy_cursor_y().abs_diff(start_raw_y) > 2 {
        assert!(
            Instant::now() < selection_deadline,
            "physical mousedown missed the target copy-mode pane: geometry={}, state={}",
            fixture.target_pane_geometry(),
            fixture.target_copy_state(),
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fixture.start_mouse_mode_churn();
    let received_before_drag = frames.received();
    let mut samples = Vec::new();
    const COPY_CURSOR_TOLERANCE: u16 = 12;
    for step in 1..=180 {
        let phase = step % 120;
        let sweep = if phase <= 60 { phase } else { 120 - phase };
        let jitter = match step % 7 {
            0 => -2.0,
            1 => 2.0,
            _ => 0.0,
        };
        let row_fraction = (f64::from(sweep) / 60.0 + jitter / 105.0).clamp(0.0, 1.0);
        let y = start_y + (end_y - start_y) * row_fraction;
        dispatch_mouse(
            &tab,
            Input::DispatchMouseEventTypeOption::MouseMoved,
            x,
            y,
            Some(Input::MouseButton::Left),
            Some(1),
        );
        std::thread::sleep(Duration::from_millis(16));
        if step % 30 == 0 {
            let expected_y = start_raw_y + (105.0 * row_fraction).round() as u16;
            let sample_deadline = Instant::now() + Duration::from_millis(500);
            let cursor_y = loop {
                let cursor_y = fixture.target_copy_cursor_y();
                if cursor_y.abs_diff(expected_y) <= COPY_CURSOR_TOLERANCE {
                    break cursor_y;
                }
                assert!(
                    Instant::now() < sample_deadline,
                    "tmux copy cursor stopped tracking the held drag: expected={expected_y}, cursor={cursor_y}, samples={samples:?}"
                );
                dispatch_mouse(
                    &tab,
                    Input::DispatchMouseEventTypeOption::MouseMoved,
                    x,
                    y,
                    Some(Input::MouseButton::Left),
                    Some(1),
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let state = fixture.target_copy_state();
            samples.push((step, expected_y, cursor_y, state));
        }
    }
    let received_after_drag = frames.received();
    fixture.stop_mouse_mode_churn();
    std::thread::sleep(Duration::from_millis(100));
    dispatch_mouse(
        &tab,
        Input::DispatchMouseEventTypeOption::MouseReleased,
        x,
        end_y,
        Some(Input::MouseButton::Left),
        None,
    );
    let release_deadline = Instant::now() + Duration::from_secs(2);
    while !frames
        .sent_payloads()
        .iter()
        .any(|payload| payload.starts_with(b"0\x1b[<0;") && payload.last() == Some(&b'm'))
    {
        assert!(
            Instant::now() < release_deadline,
            "browser release produced no tmux mouse report"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fixture.cancel_target_copy_mode();

    let sent_payloads = frames.sent_payloads();
    let mouse_reports = sent_payloads
        .iter()
        .filter(|payload| {
            payload
                .windows(b"\x1b[<32;".len())
                .any(|part| part == b"\x1b[<32;")
        })
        .count();
    let press_reports = sent_payloads
        .iter()
        .filter(|payload| payload.starts_with(b"0\x1b[<0;") && payload.last() == Some(&b'M'))
        .count();
    let release_reports = sent_payloads
        .iter()
        .filter(|payload| payload.starts_with(b"0\x1b[<0;") && payload.last() == Some(&b'm'))
        .count();
    let decisions = support::eval_string(&tab, "JSON.stringify(window.__rimzWeb?.decisions??[])")
        .expect("read churn drag decisions");
    assert!(
        frames.saw_mouse_mode_disable(),
        "churn pane did not make tmux emit the expected ?1002l mode reset"
    );
    assert!(
        received_after_drag > received_before_drag,
        "terminal output stopped during the churn drag"
    );
    assert!(
        mouse_reports >= 50,
        "held drag stopped producing mouse frames across tmux mode churn: reports={mouse_reports}, samples={samples:?}"
    );
    assert_eq!(
        press_reports, 1,
        "synthetic re-arm press leaked to tmux: {decisions}"
    );
    assert_eq!(
        release_reports, 1,
        "held drag release was lost across tmux mode churn: {decisions}"
    );
    assert!(
        decisions.contains("\"action\":\"rearm\"")
            && decisions.contains("\"action\":\"swallow-press\""),
        "churn drag did not exercise the re-arm shim: {decisions}"
    );
}

fn dispatch_mouse(
    tab: &headless_chrome::Tab,
    event_type: Input::DispatchMouseEventTypeOption,
    x: f64,
    y: f64,
    button: Option<Input::MouseButton>,
    buttons: Option<u32>,
) {
    tab.call_method(Input::DispatchMouseEvent {
        Type: event_type,
        x,
        y,
        modifiers: None,
        timestamp: None,
        button,
        buttons,
        click_count: Some(1),
        force: None,
        tangential_pressure: None,
        tilt_x: None,
        tilt_y: None,
        twist: None,
        delta_x: None,
        delta_y: None,
        pointer_Type: Some(Input::DispatchMouseEventPointer_TypeOption::Mouse),
    })
    .expect("dispatch browser mouse event");
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
