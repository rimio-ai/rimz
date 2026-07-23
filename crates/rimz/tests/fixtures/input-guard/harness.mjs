import assert from "node:assert/strict";
import fs from "node:fs";

const [guardPath] = process.argv.slice(2);
if (!guardPath) throw new Error("usage: node harness.mjs <input_guard.js>");
const source = fs.readFileSync(guardPath, "utf8");
const installInputGuard = new Function(`${source}\nreturn installInputGuard;`)();

let now = 1000;
globalThis.performance = { now: () => now };
globalThis.window = {
  clearTimeout,
  queueMicrotask,
  setTimeout,
};

const sent = [];
const rootListeners = new Map();
const targetListeners = new Map();
const root = {
  addEventListener(type, listener, capture) {
    assert.equal(capture, true);
    rootListeners.set(type, listener);
  },
};
const textarea = {
  value: "",
  blur() {
    dispatch("compositionend");
    dispatch("input");
  },
  focus() {},
};
const dispatch = (type) => {
  let stopped = false;
  const event = {
    target: textarea,
    preventDefault() {},
    stopPropagation() { stopped = true; },
  };
  rootListeners.get(type)?.(event);
  if (!stopped) targetListeners.get(type)?.();
};
let composing = false;
targetListeners.set("compositionstart", () => { composing = true; });
targetListeners.set("compositionend", () => {
  if (composing) sent.push(textarea.value);
  composing = false;
});
targetListeners.set("input", () => {
  if (!composing && textarea.value) sent.push(textarea.value);
});

const keyHandler = installInputGuard({ element: root, textarea }, (data) => {
  sent.push(data);
  return true;
});
const keyEvent = (overrides) => ({
  type: "keydown",
  key: "",
  code: "",
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  prevented: false,
  stopped: false,
  preventDefault() { this.prevented = true; },
  stopPropagation() { this.stopped = true; },
  ...overrides,
});

const altN = keyEvent({ code: "KeyN", key: "Dead", altKey: true });
assert.equal(keyHandler(altN), false);
assert.equal(altN.prevented, true);
assert.equal(altN.stopped, true);
const altNPress = keyEvent({ type: "keypress", code: "KeyN", key: "˜", altKey: true });
if (keyHandler(altNPress) !== false) sent.push(altNPress.key);
assert.equal(altNPress.prevented, true);
assert.equal(altNPress.stopped, true);
assert.deepEqual(sent, ["\u001bn"]);
textarea.value = "˜";
dispatch("compositionstart");
await Promise.resolve();
assert.deepEqual(sent, ["\u001bn"]);
assert.equal(textarea.value, "");

now += 1000;
textarea.value = "å";
dispatch("compositionstart");
dispatch("compositionend");
assert.deepEqual(sent, ["\u001bn", "å"]);

const shiftEnter = keyEvent({ key: "Enter", code: "Enter", shiftKey: true });
assert.equal(keyHandler(shiftEnter), false);
assert.equal(shiftEnter.prevented, true);
assert.equal(shiftEnter.stopped, true);
assert.deepEqual(sent, ["\u001bn", "å", "\u001b[13;2u"]);
