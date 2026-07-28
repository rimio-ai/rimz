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
  const protocolListeners = [];

  class FakeTarget {
    constructor() {
      this.listeners = new Map();
      this.dispatched = [];
    }

    addEventListener(type, listener) {
      if (!this.listeners.has(type)) this.listeners.set(type, new Set());
      this.listeners.get(type).add(listener);
    }

    removeEventListener(type, listener) {
      this.listeners.get(type)?.delete(listener);
    }

    dispatchEvent(event) {
      if (event.target === undefined) event.target = this;
      this.dispatched.push(event);
      for (const listener of this.listeners.get(event.type) ?? []) {
        listener.call(this, event);
      }
      return !event.defaultPrevented;
    }

    contains(target) {
      return target === this;
    }
  }

  class FakeMouseEvent {
    constructor(type, init = {}) {
      this.type = type;
      this.bubbles = Boolean(init.bubbles);
      this.cancelable = Boolean(init.cancelable);
      this.view = init.view;
      this.button = init.button ?? 0;
      this.buttons = init.buttons ?? 0;
      this.clientX = init.clientX ?? 0;
      this.clientY = init.clientY ?? 0;
      this.shiftKey = Boolean(init.shiftKey);
      this.altKey = Boolean(init.altKey);
      this.ctrlKey = Boolean(init.ctrlKey);
      this.metaKey = Boolean(init.metaKey);
      this.defaultPrevented = false;
    }

    preventDefault() {
      if (this.cancelable) this.defaultPrevented = true;
    }
  }

  const ownerDocument = new FakeTarget();
  const element = new FakeTarget();
  const dispatchElementEvent = element.dispatchEvent.bind(element);
  element.ownerDocument = ownerDocument;
  element.dispatchEvent = (event) => {
    if (event.target === undefined) event.target = element;
    ownerDocument.dispatchEvent(event);
    return dispatchElementEvent(event);
  };
  const coreMouseService = {
    onProtocolChange(listener) {
      protocolListeners.push(listener);
      return { dispose() {} };
    },
  };
  const term = {
    element,
    _core: { coreMouseService },
  };

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
    MouseEvent: FakeMouseEvent,
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
    `${mouseFlowSource}\n${wsUrlSource}\nreturn {MOTION_INTERVAL_MS,MOUSE_NONE,MOUSE_BOUNDARY,MOUSE_MOTION,mouseFlow,mouseReportKind,sendWithMouseFlow,resetMouseFlow,installMouseDragRearm,installRoomWebSocketUrl};`,
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
  const emitProtocol = (events) => {
    for (const listener of protocolListeners) listener(events);
  };
  return {
    ...api,
    advance,
    coreMouseService,
    element,
    emitProtocol,
    nextDelay,
    ownerDocument,
    term,
    timers,
    wire,
  };
}

const sgrMouse = (code, column, row, final = "M") => (
  encoder.encode(`0\x1b[<${code};${column};${row}${final}`)
);
const x10Mouse = (code, column, row) => (
  Uint8Array.of(0x30, 0x1b, 0x5b, 0x4d, code + 32, column + 32, row + 32)
);
const input = (value) => encoder.encode(`0${value}`);
const messages = (sent) => sent.map((payload) => decoder.decode(payload));

function installXtermDragModel(harness, sent) {
  const drag = (event) => {
    harness.sendWithMouseFlow(
      (data) => sent.push(data),
      sgrMouse(32, Math.max(Math.round(event.clientX), 1), Math.max(Math.round(event.clientY), 1)),
    );
  };
  const down = (event) => {
    harness.sendWithMouseFlow(
      (data) => sent.push(data),
      sgrMouse(0, Math.max(Math.round(event.clientX), 1), Math.max(Math.round(event.clientY), 1)),
    );
    harness.ownerDocument.addEventListener("mousemove", drag);
  };
  harness.element.addEventListener("mousedown", down);
  harness.coreMouseService.onProtocolChange((events) => {
    if (!(events & 4)) harness.ownerDocument.removeEventListener("mousemove", drag);
  });
}

function churnWhileHeldRearmsOnceAndSwallowsPress() {
  const harness = createHarness();
  const sent = [];
  installXtermDragModel(harness, sent);
  harness.installMouseDragRearm(harness.term);

  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
  }));
  assert.deepEqual(messages(sent), ["0\x1b[<0;3;4M"]);

  harness.emitProtocol(0);
  harness.emitProtocol(2);
  harness.emitProtocol(6);
  harness.emitProtocol(6);
  harness.advance(0);
  harness.ownerDocument.dispatchEvent(new window.MouseEvent("mousemove", {
    buttons: 1,
    clientX: 8,
    clientY: 9,
  }));

  const downs = harness.element.dispatched.filter((event) => event.type === "mousedown");
  assert.equal(downs.length, 2, "one physical press plus one synthetic re-arm");
  assert.deepEqual(
    messages(sent),
    ["0\x1b[<0;3;4M", "0\x1b[<32;8;9M"],
    "the synthetic press must be swallowed and the restored drag sent once",
  );
}

function churnWithoutHeldButtonDoesNotRearm() {
  const harness = createHarness();
  harness.installMouseDragRearm(harness.term);
  harness.emitProtocol(0);
  harness.emitProtocol(6);
  harness.advance(0);

  assert.equal(harness.element.dispatched.length, 0);
  assert.equal(harness.timers.size, 0);
}

function protocolBurstCoalescesToOneRearm() {
  const harness = createHarness();
  const sent = [];
  harness.installMouseDragRearm(harness.term);
  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
  }));

  harness.emitProtocol(0);
  harness.emitProtocol(2);
  harness.emitProtocol(6);
  harness.emitProtocol(0);
  harness.emitProtocol(6);
  assert.equal(harness.timers.size, 1);
  harness.advance(0);

  const downs = harness.element.dispatched.filter((event) => event.type === "mousedown");
  assert.equal(downs.length, 2, "one protocol burst must schedule one synthetic press");
  assert.equal(harness.mouseFlow.suppressPress, false);
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(0, 3, 4, "m"));
  assert.deepEqual(messages(sent), ["0\x1b[<0;3;4m"], "a later real boundary must not be swallowed");
}

function enableWithoutDisableDoesNotRearm() {
  const harness = createHarness();
  harness.installMouseDragRearm(harness.term);
  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
  }));

  harness.emitProtocol(6);
  harness.advance(0);

  const downs = harness.element.dispatched.filter((event) => event.type === "mousedown");
  assert.equal(downs.length, 1, "an initial reporting enable must not re-arm");
}

function burstEndingDisabledDoesNotRearm() {
  const harness = createHarness();
  harness.installMouseDragRearm(harness.term);
  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
  }));

  harness.emitProtocol(0);
  harness.emitProtocol(6);
  harness.emitProtocol(0);
  harness.advance(0);

  const downs = harness.element.dispatched.filter((event) => event.type === "mousedown");
  assert.equal(downs.length, 1, "a protocol burst ending disabled must not re-arm");
}

function rearmPreservesModifiersAndIgnoresOutsidePresses() {
  const harness = createHarness();
  harness.installMouseDragRearm(harness.term);
  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
    shiftKey: true,
    altKey: true,
    ctrlKey: true,
    metaKey: true,
  }));
  harness.emitProtocol(0);
  harness.emitProtocol(6);
  harness.advance(0);

  const synthetic = harness.element.dispatched.at(-1);
  assert.equal(synthetic.shiftKey, true);
  assert.equal(synthetic.altKey, true);
  assert.equal(synthetic.ctrlKey, true);
  assert.equal(synthetic.metaKey, true);

  harness.ownerDocument.dispatchEvent(new window.MouseEvent("mouseup", {
    buttons: 0,
    clientX: 3,
    clientY: 4,
  }));
  harness.ownerDocument.dispatchEvent(new window.MouseEvent("mousedown", {
    button: 0,
    buttons: 1,
    clientX: 8,
    clientY: 9,
  }));
  harness.emitProtocol(0);
  harness.emitProtocol(6);
  harness.advance(0);

  const downs = harness.element.dispatched.filter((event) => event.type === "mousedown");
  assert.equal(downs.length, 2, "a primary press outside the terminal must not arm the shim");
}

function swallowedRearmPressPreservesPacingCadence() {
  const harness = createHarness();
  const sent = [];
  harness.installMouseDragRearm(harness.term);
  harness.element.dispatchEvent(new window.MouseEvent("mousedown", {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientX: 3,
    clientY: 4,
  }));
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 3, 4));
  harness.element.addEventListener("mousedown", () => {
    harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(0, 3, 4));
  });

  harness.emitProtocol(0);
  harness.emitProtocol(6);
  harness.advance(0);
  assert.deepEqual(messages(sent), ["0\x1b[<32;3;4M"]);
  assert.equal(harness.mouseFlow.lastSentAt, 0);

  harness.advance(30);
  harness.sendWithMouseFlow((data) => sent.push(data), sgrMouse(32, 4, 4));
  assert.equal(harness.nextDelay(), 20, "re-arm must not reset the motion interval");
  harness.advance(20);
  assert.deepEqual(messages(sent), ["0\x1b[<32;3;4M", "0\x1b[<32;4;4M"]);
}

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
  churnWhileHeldRearmsOnceAndSwallowsPress,
  churnWithoutHeldButtonDoesNotRearm,
  protocolBurstCoalescesToOneRearm,
  enableWithoutDisableDoesNotRearm,
  burstEndingDisabledDoesNotRearm,
  rearmPreservesModifiersAndIgnoresOutsidePresses,
  swallowedRearmPressPreservesPacingCadence,
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
