import assert from "node:assert/strict";
import fs from "node:fs";

const [flowControlPath] = process.argv.slice(2);
if (!flowControlPath) {
  throw new Error("usage: node harness.mjs <flow_control.js>");
}
const source = fs.readFileSync(flowControlPath, "utf8");
const encoder = new TextEncoder();
const decoder = new TextDecoder();

// This idealized harness completes write callbacks before the render that paints them; real xterm interleaves writes and renders.
function createHarness({
  cols = 196,
  rows = 53,
  search = "?room=room-a",
  sendFailures = 0,
} = {}) {
  const sent = [];
  const timers = new Map();
  const writeCallbacks = [];
  let nextTimer = 1;
  let now = 0;
  let render;
  let remainingSendFailures = sendFailures;

  class NativeWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url, protocols) {
      super();
      this.url = url;
      this.protocols = protocols;
      this.readyState = NativeWebSocket.OPEN;
    }

    send(data) {
      if (remainingSendFailures > 0) {
        remainingSendFailures--;
        throw new Error("simulated send failure");
      }
      const bytes = data instanceof ArrayBuffer
        ? new Uint8Array(data)
        : ArrayBuffer.isView(data)
          ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
          : data;
      sent.push(bytes instanceof Uint8Array ? bytes.slice() : bytes);
    }
  }

  globalThis.window = {
    WebSocket: NativeWebSocket,
    WebSocketStream: undefined,
    location: new URL(search, "https://rimz.test/"),
    setTimeout(callback, delay = 0) {
      const id = nextTimer++;
      timers.set(id, { callback, due: now + delay });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  };

  const { flow, installWebSocketGate, installBacklogMeter } = new Function(
    `${source}\nreturn {flow,installWebSocketGate,installBacklogMeter};`,
  )();
  installWebSocketGate();

  const term = {
    cols,
    rows,
    write(_data, callback) {
      writeCallbacks.push(callback);
    },
    onRender(callback) {
      render = callback;
    },
    onResize() {},
  };
  installBacklogMeter(term);

  const socket = new window.WebSocket("wss://rimz.test/ws", ["tty"]);
  const messages = () => sent.map((payload) => (
    payload instanceof Uint8Array ? decoder.decode(payload) : payload
  ));
  const completeWrite = () => {
    const callback = writeCallbacks.shift();
    assert.ok(callback, "expected a pending terminal write");
    callback();
  };
  const paintOutput = () => {
    term.write(encoder.encode("terminal output"));
    completeWrite();
    assert.ok(render, "flow control did not observe xterm renders");
    render();
  };
  const renderWithoutOutput = () => {
    assert.ok(render, "flow control did not observe xterm renders");
    render();
  };
  const advance = (duration) => {
    const target = now + duration;
    while (true) {
      const entries = [...timers.entries()]
        .filter(([, timer]) => timer.due <= target)
        .sort((left, right) => left[1].due - right[1].due || left[0] - right[0]);
      if (!entries.length) break;
      const [id, timer] = entries[0];
      timers.delete(id);
      now = timer.due;
      timer.callback();
    }
    now = target;
  };

  return {
    flow,
    socket,
    messages,
    paintOutput,
    renderWithoutOutput,
    advance,
  };
}

const sgrMouse = (code, column, row, final = "M") => (
  encoder.encode(`0\x1b[<${code};${column};${row}${final}`)
);
const defaultMouse = (code, column, row) => (
  Uint8Array.of(0x30, 0x1b, 0x5b, 0x4d, code + 32, column + 32, row + 32)
);
const input = (value) => encoder.encode(`0${value}`);

function continuousDragKeepsOnlyTheLatestCoordinate() {
  const harness = createHarness();
  harness.socket.send(sgrMouse(0, 1, 1));
  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 3, 1));
  harness.socket.send(sgrMouse(32, 4, 1));

  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<0;1;1M", "0\x1b[<32;2;1M"],
    "one drag coordinate should be in flight while later coordinates coalesce",
  );

  harness.paintOutput();
  assert.equal(
    harness.messages().length,
    2,
    "the first render band should not release the next full-grid drag coordinate",
  );
  harness.advance(40);
  assert.equal(harness.messages().length, 2, "full-grid drag pacing should span more than one browser frame");
  harness.advance(1);
  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<0;1;1M", "0\x1b[<32;2;1M", "0\x1b[<32;4;1M"],
    "the next paint should send only the latest queued coordinate",
  );

  harness.socket.send(sgrMouse(32, 5, 1));
  harness.renderWithoutOutput();
  assert.equal(
    harness.messages().length,
    3,
    "an unrelated render should not release a drag coordinate before output parses",
  );

  harness.paintOutput();
  harness.advance(41);
  assert.equal(harness.messages().at(-1), "0\x1b[<32;5;1M");
}

function releaseFlushesTheFinalCoordinateInOrder() {
  const harness = createHarness();
  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 8, 1));
  harness.socket.send(sgrMouse(0, 8, 1, "m"));

  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<32;2;1M", "0\x1b[<32;8;1M", "0\x1b[<0;8;1m"],
    "mouse release should follow the newest drag coordinate without waiting for paint",
  );

  harness.socket.send(sgrMouse(32, 9, 1));
  assert.equal(
    harness.messages().at(-1),
    "0\x1b[<32;9;1M",
    "release should clear the previous drag flight",
  );
}

function keyboardInputDoesNotOvertakeQueuedMotion() {
  const harness = createHarness();
  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 7, 1));
  harness.socket.send(input("x"));

  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<32;2;1M", "0\x1b[<32;7;1M", "0x"],
    "later terminal input should preserve its order behind queued mouse motion",
  );
}

function defaultMouseEncodingAlsoCoalesces() {
  const harness = createHarness();
  harness.socket.send(defaultMouse(32, 1, 1));
  harness.socket.send(defaultMouse(32, 2, 1));
  assert.equal(harness.messages().length, 1, "default-encoded drag motion should queue");
  harness.paintOutput();
  harness.advance(41);
  assert.deepEqual(
    harness.messages().map((message) => [...encoder.encode(message)]),
    [
      [...defaultMouse(32, 1, 1)],
      [...defaultMouse(32, 2, 1)],
    ],
  );
}

function stalledPaintStillMakesBoundedProgress() {
  const harness = createHarness();
  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 7, 1));
  harness.socket.send(sgrMouse(32, 9, 1));
  assert.equal(harness.messages().length, 1);

  harness.advance(249);
  assert.equal(harness.messages().length, 1, "pacing alone should not release motion without a painted response");
  harness.advance(1);
  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<32;2;1M", "0\x1b[<32;9;1M"],
    "a missing paint should eventually send the newest coordinate",
  );
}

function continuousOutputBandsDoNotStallTheDrag() {
  const harness = createHarness({ cols: 40, rows: 20 });
  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 7, 1));

  for (let elapsed = 0; elapsed < 16; elapsed += 4) {
    harness.paintOutput();
    harness.advance(4);
  }
  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<32;2;1M", "0\x1b[<32;7;1M"],
    "continuous output should release the queued coordinate at the 16 ms pace deadline",
  );
}

function throwingSendStillMakesBoundedProgress() {
  const harness = createHarness({ sendFailures: 1 });
  assert.throws(
    () => harness.socket.send(sgrMouse(32, 2, 1)),
    /simulated send failure/,
  );
  harness.socket.send(sgrMouse(32, 7, 1));

  harness.advance(249);
  assert.deepEqual(harness.messages(), []);
  harness.advance(1);
  assert.deepEqual(
    harness.messages(),
    ["0\x1b[<32;7;1M"],
    "a throwing send should not strand the newest coordinate",
  );
}

function debugHookExposesGateStateOnlyWhenFlagged() {
  createHarness();
  assert.equal(window.__rimzWeb, undefined, "the debug hook should be absent without the flag");

  const harness = createHarness({ search: "?room=room-a&rimzdebug=1" });
  const debug = window.__rimzWeb;
  assert.equal(debug.flow, harness.flow, "the debug hook should expose the live gate state");

  harness.socket.send(sgrMouse(32, 2, 1));
  harness.socket.send(sgrMouse(32, 7, 1));
  assert.equal(debug.flow.mouse.pending.data, harness.flow.mouse.pending.data);
  harness.advance(250);
  harness.socket.send(sgrMouse(32, 9, 1));
  harness.socket.send(sgrMouse(0, 9, 1, "m"));

  assert.deepEqual(
    debug.decisions.map(({ action }) => action),
    ["sent", "queued", "stall-release", "sent", "queued", "boundary-flush"],
    "the hook should record each mouse send decision in order",
  );
  assert.equal(debug.flow.mouse.inFlight, false, "the exposed flow state should stay live");
}

const scenarios = [
  continuousDragKeepsOnlyTheLatestCoordinate,
  releaseFlushesTheFinalCoordinateInOrder,
  keyboardInputDoesNotOvertakeQueuedMotion,
  defaultMouseEncodingAlsoCoalesces,
  stalledPaintStillMakesBoundedProgress,
  continuousOutputBandsDoNotStallTheDrag,
  throwingSendStillMakesBoundedProgress,
  debugHookExposesGateStateOnlyWhenFlagged,
];

for (const scenario of scenarios) {
  try {
    scenario();
  } catch (error) {
    error.message = `${scenario.name}: ${error.message}`;
    throw error;
  }
}

console.log(`flow-control harness: ${scenarios.length} scenarios passed`);
