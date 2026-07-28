import assert from "node:assert/strict";
import fs from "node:fs";

const [mouseFlowPath, wsUrlPath] = process.argv.slice(2);
if (!mouseFlowPath || !wsUrlPath) {
  throw new Error("usage: node harness.mjs <mouse_flow.js> <ws_url.js>");
}
const mouseFlowSource = fs.readFileSync(mouseFlowPath, "utf8");
const wsUrlSource = fs.readFileSync(wsUrlPath, "utf8");
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function createHarness({ search = "?room=room-a" } = {}) {
  let now = 0;
  let nextTimer = 1;
  const timers = new Map();
  const wire = [];

  class FakeWebSocket extends EventTarget {
    static OPEN = 1;

    constructor(url, protocols) {
      super();
      this.url = url;
      this.protocols = protocols;
      this.readyState = FakeWebSocket.OPEN;
    }

    send(data) {
      wire.push(data);
    }
  }

  globalThis.window = {
    WebSocket: FakeWebSocket,
    location: new URL(search, "https://rimz.test/"),
    performance: {
      now: () => now,
    },
    setTimeout(callback, delay = 0) {
      const id = nextTimer++;
      timers.set(id, { callback, due: now + delay });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  };
  const api = new Function(
    `${mouseFlowSource}\n${wsUrlSource}\nreturn {MOTION_INTERVAL_MS,MOUSE_NONE,MOUSE_BOUNDARY,MOUSE_MOTION,mouseFlow,mouseReportKind,sendWithMouseFlow,resetMouseFlow,installRoomWebSocketUrl};`,
  )();
  const advance = (duration) => {
    const target = now + duration;
    while (true) {
      const ready = [...timers.entries()]
        .filter(([, timer]) => timer.due <= target)
        .sort((left, right) => left[1].due - right[1].due || left[0] - right[0]);
      if (!ready.length) break;
      const [id, timer] = ready[0];
      timers.delete(id);
      now = timer.due;
      timer.callback();
    }
    now = target;
  };
  const nextDelay = () => {
    const due = Math.min(...[...timers.values()].map((timer) => timer.due));
    return Number.isFinite(due) ? due - now : null;
  };
  return { ...api, advance, nextDelay, timers, wire };
}

const sgrMouse = (code, column, row, final = "M") => (
  encoder.encode(`0\x1b[<${code};${column};${row}${final}`)
);
const x10Mouse = (code, column, row) => (
  Uint8Array.of(0x30, 0x1b, 0x5b, 0x4d, code + 32, column + 32, row + 32)
);
const input = (value) => encoder.encode(`0${value}`);
const messages = (sent) => sent.map((payload) => decoder.decode(payload));

function idleMotionSendsImmediately() {
  const harness = createHarness();
  const sent = [];
  const motion = sgrMouse(32, 2, 1);
  harness.sendWithMouseFlow((data) => sent.push(data), motion);

  assert.equal(sent.length, 1);
  assert.equal(sent[0], motion, "the leading edge must preserve the original frame");
  assert.equal(harness.mouseFlow.timer, 0);
  assert.equal(harness.mouseFlow.pending, null);
  assert.equal(harness.mouseFlow.lastSentAt, 0);
}

function slowDragPassesThroughUnchanged() {
  const harness = createHarness();
  const sent = [];
  const motion = [
    sgrMouse(32, 2, 1),
    sgrMouse(32, 3, 1),
    x10Mouse(32, 4, 1),
  ];
  for (const report of motion) {
    harness.sendWithMouseFlow((data) => sent.push(data), report);
    harness.advance(harness.MOTION_INTERVAL_MS + 1);
  }

  assert.equal(sent.length, motion.length);
  for (let index = 0; index < motion.length; index++) {
    assert.equal(sent[index], motion[index], "slow motion must preserve the original frame");
  }
  assert.equal(harness.timers.size, 0);
}

function fastDragCoalescesAtABoundedRate() {
  const harness = createHarness();
  const sent = [];
  for (let column = 1; column <= 8; column++) {
    harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, column, 1));
  }

  assert.deepEqual(messages(sent), ["0\x1b[<32;1;1M"]);
  assert.equal(harness.nextDelay(), harness.MOTION_INTERVAL_MS);
  harness.advance(harness.MOTION_INTERVAL_MS - 1);
  assert.deepEqual(messages(sent), ["0\x1b[<32;1;1M"]);
  harness.advance(1);
  assert.deepEqual(messages(sent), ["0\x1b[<32;1;1M", "0\x1b[<32;8;1M"]);

  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 9, 1));
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 10, 1));
  assert.equal(harness.nextDelay(), harness.MOTION_INTERVAL_MS);
  harness.advance(harness.MOTION_INTERVAL_MS);
  assert.deepEqual(
    messages(sent),
    ["0\x1b[<32;1;1M", "0\x1b[<32;8;1M", "0\x1b[<32;10;1M"],
  );
}

function partialIntervalUsesTheRemainingSlice() {
  const harness = createHarness();
  const sent = [];
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 1, 1));
  harness.advance(30);
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 2, 1));

  assert.equal(harness.nextDelay(), 20);
  harness.advance(19);
  assert.deepEqual(messages(sent), ["0\x1b[<32;1;1M"]);
  harness.advance(1);
  assert.deepEqual(messages(sent), ["0\x1b[<32;1;1M", "0\x1b[<32;2;1M"]);
}

function boundaryFlushesPendingMotionBeforeRelease() {
  const harness = createHarness();
  const sent = [];
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 4, 1));
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 8, 1));
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(0, 8, 1, "m"));

  assert.deepEqual(
    messages(sent),
    ["0\x1b[<32;4;1M", "0\x1b[<32;8;1M", "0\x1b[<0;8;1m"],
  );
  assert.equal(harness.mouseFlow.timer, 0);
  assert.equal(harness.timers.size, 0);
  assert.equal(harness.mouseFlow.lastSentAt, null);
}

function otherInputDoesNotOvertakePendingMotion() {
  const harness = createHarness();
  const sent = [];
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 4, 1));
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 8, 1));
  harness.sendWithMouseFlow((data) => sent.push(data), input("x"));

  assert.deepEqual(
    messages(sent),
    ["0\x1b[<32;4;1M", "0\x1b[<32;8;1M", "0x"],
  );
  assert.equal(harness.timers.size, 0);
}

function classifierRecognizesSupportedMouseFrames() {
  const harness = createHarness();
  assert.equal(harness.mouseReportKind(sgrMouse(32, 2, 1)), harness.MOUSE_MOTION);
  assert.equal(harness.mouseReportKind(sgrMouse(0, 2, 1)), harness.MOUSE_BOUNDARY);
  assert.equal(harness.mouseReportKind(sgrMouse(0, 2, 1, "m")), harness.MOUSE_BOUNDARY);
  assert.equal(harness.mouseReportKind(x10Mouse(32, 2, 1)), harness.MOUSE_MOTION);
  assert.equal(harness.mouseReportKind(x10Mouse(0, 2, 1)), harness.MOUSE_BOUNDARY);
  assert.equal(harness.mouseReportKind(input("x")), harness.MOUSE_NONE);
  assert.equal(harness.mouseReportKind("0\x1b[<32;2;1M"), harness.MOUSE_NONE);

  const tight = sgrMouse(32, 9, 3);
  const allocation = new Uint8Array(tight.length * 3 + 5);
  allocation.set(tight, 4);
  const subarray = allocation.subarray(4, 4 + tight.length);
  assert.notEqual(subarray.byteLength, subarray.buffer.byteLength);
  assert.equal(harness.mouseReportKind(subarray), harness.MOUSE_MOTION);
}

function socketLifecycleResetsQueuedMotion() {
  const harness = createHarness({ search: "?room=room-a&rimzdebug=1" });
  harness.installRoomWebSocketUrl();
  const socket = new window.WebSocket("wss://rimz.test/ws", ["tty"]);
  socket.send(sgrMouse(32, 2, 1));
  socket.send(sgrMouse(32, 7, 1));
  assert.ok(harness.mouseFlow.pending);
  assert.ok(harness.mouseFlow.timer);

  socket.dispatchEvent(new Event("close"));
  assert.equal(harness.mouseFlow.pending, null);
  assert.equal(harness.mouseFlow.timer, 0);
  assert.equal(harness.mouseFlow.lastSentAt, null);
  harness.advance(harness.MOTION_INTERVAL_MS);
  assert.deepEqual(messages(harness.wire), ["0\x1b[<32;2;1M"]);
  assert.equal(window.__rimzWeb.flow, harness.mouseFlow);
  assert.ok(window.__rimzWeb.decisions.length > 0);

  harness.sendWithMouseFlow(() => {}, sgrMouse(32, 8, 1));
  harness.sendWithMouseFlow(() => {}, sgrMouse(32, 9, 1));
  assert.ok(harness.mouseFlow.pending);
  new window.WebSocket("wss://rimz.test/ws", ["tty"]);
  assert.equal(harness.mouseFlow.pending, null);
  assert.equal(harness.mouseFlow.timer, 0);
  assert.equal(harness.mouseFlow.lastSentAt, null);
}

function releasePreservesTheFinalDragCoordinate() {
  const harness = createHarness();
  const sent = [];
  const send = (data) => sent.push(data);
  harness.sendWithMouseFlow(send, sgrMouse(0, 1, 1));
  harness.sendWithMouseFlow(send, sgrMouse(32, 4, 1));
  harness.sendWithMouseFlow(send, sgrMouse(32, 12, 1));
  harness.sendWithMouseFlow(send, sgrMouse(0, 12, 1, "m"));

  assert.deepEqual(
    messages(sent),
    [
      "0\x1b[<0;1;1M",
      "0\x1b[<32;4;1M",
      "0\x1b[<32;12;1M",
      "0\x1b[<0;12;1m",
    ],
  );
}

const scenarios = [
  idleMotionSendsImmediately,
  slowDragPassesThroughUnchanged,
  fastDragCoalescesAtABoundedRate,
  partialIntervalUsesTheRemainingSlice,
  boundaryFlushesPendingMotionBeforeRelease,
  otherInputDoesNotOvertakePendingMotion,
  classifierRecognizesSupportedMouseFrames,
  socketLifecycleResetsQueuedMotion,
  releasePreservesTheFinalDragCoordinate,
];

for (const scenario of scenarios) {
  try {
    scenario();
  } catch (error) {
    error.message = `${scenario.name}: ${error.message}`;
    throw error;
  }
}

console.log(`mouse-flow harness: ${scenarios.length} scenarios passed`);
