import assert from "node:assert/strict";
import fs from "node:fs";

const [layerPath, configJson] = process.argv.slice(2);
if (!layerPath || !configJson) {
  throw new Error("usage: node harness.mjs <pixel_layer.js> <config-json>");
}
const config = JSON.parse(configJson);
const source = fs.readFileSync(layerPath, "utf8");
const installPixelLayer = new Function(
  "RIMZ_PIXEL_PROTOCOL",
  "RIMZ_PIXEL_PLACEHOLDER",
  "RIMZ_PIXEL_DIACRITICS",
  `${source}\nreturn installPixelLayer;`,
)(config.protocol, config.placeholder, config.diacritics);

const almostEqual = (actual, expected, message) => {
  assert.ok(Math.abs(actual - expected) < 1e-9, `${message}: expected ${expected}, got ${actual}`);
};

function createHarness({ cols, rows, width = cols * 10, height = rows * 10 }) {
  const ops = [];
  let pathRects = [];
  const context = {
    imageSmoothingEnabled: true,
    setTransform(...args) { ops.push({ op: "setTransform", args }); },
    clearRect(...args) { ops.push({ op: "clearRect", args }); },
    fillRect(...args) { ops.push({ op: "fillRect", args }); },
    save() { ops.push({ op: "save" }); },
    beginPath() { pathRects = []; ops.push({ op: "beginPath" }); },
    rect(...args) { pathRects.push(args); ops.push({ op: "rect", args }); },
    clip() { ops.push({ op: "clip", rects: pathRects.map((rect) => [...rect]) }); },
    drawImage(image, ...args) { ops.push({ op: "drawImage", args, image: { width: image.width, height: image.height } }); },
    restore() { ops.push({ op: "restore" }); },
  };
  const canvas = {
    width: 300,
    height: 150,
    style: {},
    dataset: {},
    className: "",
    getContext: () => context,
    remove() {},
  };
  const screenRect = { width, height };
  const screen = {
    style: {},
    append(node) { assert.equal(node, canvas); },
    getBoundingClientRect() { return { ...screenRect }; },
  };
  const frameQueue = [];
  const resizeObservers = [];
  const bitmapRequests = [];
  const writes = [];
  const handlers = {};
  const lines = [];
  const blankCell = { getChars: () => "", getFgColor: () => 0 };

  const ensureLine = (row) => {
    while (lines.length <= row) {
      const cells = Array.from({ length: cols }, () => blankCell);
      lines.push({
        length: cols,
        getCellCalls: 0,
        getCell(column) {
          this.getCellCalls++;
          return cells[column];
        },
        translateToString(trimRight) {
          const value = cells.map((cell) => cell.getChars()).join("");
          return trimRight ? value.trimEnd() : value;
        },
        cells,
      });
    }
    return lines[row];
  };
  for (let row = 0; row < rows * 3; row++) ensureLine(row);

  globalThis.window = {
    devicePixelRatio: 1,
    requestAnimationFrame(callback) { frameQueue.push(callback); },
  };
  globalThis.document = { createElement: (tag) => {
    assert.equal(tag, "canvas");
    return canvas;
  } };
  globalThis.getComputedStyle = () => ({ position: "static" });
  globalThis.ResizeObserver = class {
    constructor(callback) { this.callback = callback; resizeObservers.push(this); }
    observe(node) { assert.equal(node, screen); }
  };
  globalThis.createImageBitmap = () => new Promise((resolve, reject) => {
    bitmapRequests.push({ resolve, reject });
  });

  const term = {
    cols,
    rows,
    options: {},
    element: { querySelector: (selector) => selector === ".xterm-screen" ? screen : null },
    buffer: {
      active: {
        viewportY: 0,
        getLine: (row) => lines[row],
        getNullCell: () => ({}),
      },
    },
    write(data, callback) {
      writes.push(data instanceof Uint8Array ? data.slice() : data);
      if (callback) callback();
      return data;
    },
    reset() {},
    onRender(callback) { handlers.render = callback; },
    onScroll(callback) { handlers.scroll = callback; },
    onResize(callback) { handlers.resize = callback; },
    _core: new Proxy({}, { get() { throw new Error("pixel layer accessed private term._core"); } }),
  };
  installPixelLayer(term);

  const writeApc = (control, payload = "") => term.write(`\x1b_G${control};${payload}\x1b\\`);
  const place = (id, placementCols, placementRows) => writeApc(`a=p,U=1,i=${id},c=${placementCols},r=${placementRows}`);
  const transmit = (id) => writeApc(`a=t,f=100,i=${id}`, "eA==");
  const setPlaceholder = (row, col, id, sourceRow, sourceCol) => {
    ensureLine(row).cells[col] = {
      getChars: () => String.fromCodePoint(config.placeholder) + config.diacritics[sourceRow] + config.diacritics[sourceCol],
      getFgColor: () => id,
    };
  };
  const setText = (row, col, text) => {
    ensureLine(row).cells[col] = { getChars: () => text, getFgColor: () => 0 };
  };
  const setPlacementCells = (id, originRow, originCol, placementRows, placementCols) => {
    for (let sourceRow = 0; sourceRow < placementRows; sourceRow++) {
      for (let sourceCol = 0; sourceCol < placementCols; sourceCol++) {
        setPlaceholder(originRow + sourceRow, originCol + sourceCol, id, sourceRow, sourceCol);
      }
    }
  };
  const pumpFrames = () => {
    while (frameQueue.length) frameQueue.shift()(0);
  };
  const settle = async () => {
    await Promise.resolve();
    await Promise.resolve();
  };
  const resolveNext = async (width, bitmapHeight) => {
    const request = bitmapRequests.shift();
    assert.ok(request, "expected a pending image decode");
    request.resolve({ width, height: bitmapHeight, close() {} });
    await settle();
  };
  const rejectNext = async () => {
    const request = bitmapRequests.shift();
    assert.ok(request, "expected a pending image decode");
    request.reject(new Error("decode failed"));
    await settle();
  };
  const takeOps = () => ops.splice(0);
  const takeWrites = () => writes.splice(0);
  const getCellCallCounts = () => lines.map((line) => line.getCellCalls);

  pumpFrames();
  takeOps();
  return {
    term,
    screenRect,
    resizeObservers,
    bitmapRequests,
    handlers,
    place,
    transmit,
    setPlaceholder,
    setPlacementCells,
    setText,
    pumpFrames,
    resolveNext,
    rejectNext,
    takeOps,
    takeWrites,
    getCellCallCounts,
  };
}

const draws = (ops) => ops.filter(({ op }) => op === "drawImage");
const clips = (ops) => ops.filter(({ op }) => op === "clip");
const assertNoFills = (ops) => assert.equal(ops.filter(({ op }) => op === "fillRect").length, 0);

async function placeholderClustersAreConcealedSafely() {
  const harness = createHarness({ cols: 4, rows: 2 });
  const base = String.fromCodePoint(config.placeholder);
  const decoder = new TextDecoder();
  const clusters = [
    base + config.diacritics[0] + config.diacritics[1],
    base + config.diacritics.at(-2) + config.diacritics.at(-1),
  ];

  for (const cluster of clusters) {
    harness.term.write(`A${cluster}B`);
    let writes = harness.takeWrites();
    assert.equal(writes.length, 1);
    assert.equal(
      decoder.decode(writes[0]),
      `A\x1b[8m${cluster}\x1b[28mB`,
      "the placeholder cluster is hidden while neighboring text remains visible",
    );

    const encoded = new TextEncoder().encode(cluster);
    for (let boundary = 1; boundary < encoded.length; boundary++) {
      const split = createHarness({ cols: 4, rows: 2 });
      split.term.write(encoded.subarray(0, boundary));
      assert.equal(split.takeWrites().length, 0, `split ${boundary} was forwarded early`);
      split.term.write(encoded.subarray(boundary));
      writes = split.takeWrites();
      assert.equal(writes.length, 1);
      assert.equal(
        decoder.decode(writes[0]),
        `\x1b[8m${cluster}\x1b[28m`,
        `placeholder suppression survives websocket split ${boundary}`,
      );
    }
  }

  const adjacent = createHarness({ cols: 4, rows: 2 });
  adjacent.term.write(clusters.join(""));
  assert.equal(
    decoder.decode(adjacent.takeWrites()[0]),
    `\x1b[8m${clusters.join("")}\x1b[28m`,
    "one conceal pair covers adjacent placeholder cells",
  );

  const malformed = createHarness({ cols: 4, rows: 2 });
  const literal = `${base}${config.diacritics[0]}\x1b[31mX`;
  malformed.term.write(literal);
  assert.equal(
    decoder.decode(malformed.takeWrites()[0]),
    literal,
    "a non-protocol placeholder cannot conceal or consume following terminal controls",
  );
}

async function petStopsAtPaneBorder() {
  const harness = createHarness({ cols: 20, rows: 11 });
  const id = 0x123456;
  harness.setPlacementCells(id, 1, 4, 9, 15);
  harness.setText(1, 19, "│");
  harness.transmit(id);
  harness.place(id, 15, 9);
  harness.pumpFrames();
  assert.equal(draws(harness.takeOps()).length, 0, "pending pet decode painted");
  await harness.resolveNext(150, 90);
  harness.pumpFrames();
  const ops = harness.takeOps();
  assertNoFills(ops);
  assert.equal(draws(ops).length, 1);
  assert.equal(clips(ops).length, 1);
  assert.equal(clips(ops)[0].rects.length, 135);
  const borderX = 190;
  for (const [x, , width] of clips(ops)[0].rects) assert.ok(x + width <= borderX, "clip crossed pane border");
  const [x, , width] = draws(ops)[0].args;
  assert.ok(x + width <= borderX, "pet draw crossed pane border");
}

async function petPreservesAspect() {
  const harness = createHarness({ cols: 20, rows: 11 });
  const id = 2;
  harness.setPlacementCells(id, 1, 4, 9, 15);
  harness.transmit(id);
  harness.place(id, 15, 9);
  await harness.resolveNext(60, 90);
  harness.pumpFrames();
  const draw = draws(harness.takeOps())[0];
  assert.ok(draw, "aspect-fit pet was not drawn");
  const [x, y, width, height] = draw.args;
  almostEqual(x, 85, "pet centered x");
  almostEqual(y, 10, "pet bottom anchor y");
  almostEqual(width, 60, "pet fitted width");
  almostEqual(height, 90, "pet fitted height");
}

async function decodeLifecycle() {
  const harness = createHarness({ cols: 8, rows: 4 });
  const id = 3;
  harness.setPlacementCells(id, 1, 1, 2, 2);
  harness.transmit(id);
  harness.place(id, 2, 2);
  harness.pumpFrames();
  assert.equal(draws(harness.takeOps()).length, 0, "pending decode painted");
  await harness.rejectNext();
  harness.handlers.render();
  harness.pumpFrames();
  assert.equal(draws(harness.takeOps()).length, 0, "failed decode painted");

  harness.transmit(id);
  assert.equal(harness.bitmapRequests.length, 1, "failed decode left the image id pending");
  harness.pumpFrames();
  assert.equal(draws(harness.takeOps()).length, 0, "retry painted before decode resolved");
  await harness.resolveNext(20, 20);
  harness.pumpFrames();
  assert.equal(draws(harness.takeOps()).length, 1, "late decode did not draw");
}

async function oneRowMeterIsClipped() {
  const harness = createHarness({ cols: 12, rows: 3 });
  const id = 4;
  harness.setPlacementCells(id, 1, 2, 1, 5);
  harness.setText(1, 7, "n");
  harness.transmit(id);
  harness.place(id, 5, 1);
  await harness.resolveNext(17, 8);
  harness.pumpFrames();
  const ops = harness.takeOps();
  assertNoFills(ops);
  assert.deepEqual(draws(ops)[0].args, [20, 10, 50, 10]);
  assert.equal(clips(ops)[0].rects.length, 5);
  for (const [x, , width] of clips(ops)[0].rects) assert.ok(x >= 20 && x + width <= 70, "meter clip touched ordinary text");
}

async function partialPlacementKeepsLogicalOrigin() {
  const harness = createHarness({ cols: 12, rows: 7 });
  const id = 5;
  for (let sourceRow = 1; sourceRow <= 2; sourceRow++) {
    for (let sourceCol = 1; sourceCol <= 3; sourceCol++) {
      harness.setPlaceholder(2 + sourceRow, 3 + sourceCol, id, sourceRow, sourceCol);
    }
  }
  harness.transmit(id);
  harness.place(id, 5, 3);
  await harness.resolveNext(50, 30);
  harness.pumpFrames();
  const ops = harness.takeOps();
  assert.deepEqual(draws(ops)[0].args, [30, 20, 50, 30]);
  assert.equal(clips(ops)[0].rects.length, 6);
  for (const [x, y, width, height] of clips(ops)[0].rects) {
    assert.ok(x >= 40 && x + width <= 70, "partial clip escaped present columns");
    assert.ok(y >= 30 && y + height <= 50, "partial clip escaped present rows");
  }
}

async function resizeAndScrollFollowViewport() {
  const harness = createHarness({ cols: 8, rows: 4 });
  const id = 6;
  harness.setPlacementCells(id, 1, 2, 2, 2);
  harness.transmit(id);
  harness.place(id, 2, 2);
  await harness.resolveNext(20, 20);
  harness.pumpFrames();
  assert.deepEqual(draws(harness.takeOps())[0].args, [20, 10, 20, 20]);

  harness.term.buffer.active.viewportY = 1;
  harness.handlers.scroll();
  harness.pumpFrames();
  let ops = harness.takeOps();
  assert.deepEqual(draws(ops)[0].args, [20, 0, 20, 20]);
  assert.equal(ops.filter(({ op }) => op === "clearRect").length, 1);

  harness.screenRect.width = 160;
  harness.screenRect.height = 80;
  harness.resizeObservers[0].callback();
  harness.pumpFrames();
  ops = harness.takeOps();
  assert.deepEqual(draws(ops)[0].args, [40, 0, 40, 40]);
  assert.ok(ops.filter(({ op }) => op === "clearRect").length >= 2, "ResizeObserver did not clear between frames");

  harness.handlers.resize();
  harness.pumpFrames();
  ops = harness.takeOps();
  assert.equal(draws(ops).length, 1, "onResize did not redraw");
  assert.ok(ops.filter(({ op }) => op === "clearRect").length >= 2, "onResize did not clear between frames");
}

async function repeatedRenderReplacesFrame() {
  const harness = createHarness({ cols: 6, rows: 3 });
  const id = 7;
  harness.setPlacementCells(id, 1, 1, 1, 2);
  harness.transmit(id);
  harness.place(id, 2, 1);
  await harness.resolveNext(20, 10);
  harness.pumpFrames();
  harness.takeOps();
  for (let index = 0; index < 3; index++) {
    harness.setText(1, 1 + index, "");
    harness.setText(1, 2 + index, "");
    harness.setPlacementCells(id, 1, 2 + index, 1, 2);
    harness.handlers.render();
    harness.pumpFrames();
    const ops = harness.takeOps();
    assert.equal(draws(ops).length, 1, `render ${index} accumulated image draws`);
    assert.equal(ops.filter(({ op }) => op === "clearRect").length, 1, `render ${index} did not replace the frame`);
  }
}

async function unchangedSceneDoesNotRepaintCanvas() {
  const harness = createHarness({ cols: 196, rows: 53, width: 3840, height: 2160 });
  const id = 8;
  harness.setPlaceholder(26, 98, id, 0, 0);
  harness.transmit(id);
  harness.place(id, 1, 1);
  await harness.resolveNext(10, 10);
  harness.pumpFrames();
  harness.takeOps();

  harness.handlers.render();
  harness.pumpFrames();
  assert.deepEqual(
    harness.takeOps(),
    [],
    "an unchanged placeholder scene should not touch the full-resolution canvas",
  );

  harness.setText(26, 98, "");
  harness.setPlaceholder(26, 99, id, 0, 0);
  harness.handlers.render();
  harness.pumpFrames();
  const ops = harness.takeOps();
  assert.equal(draws(ops).length, 1, "a moved placeholder should redraw its image");
  assert.equal(ops.filter(({ op }) => op === "clearRect").length, 1, "a moved placeholder should replace the canvas");
}

async function renderEventsCoalesce() {
  const harness = createHarness({ cols: 6, rows: 3 });
  const id = 9;
  harness.setPlacementCells(id, 1, 1, 1, 2);
  harness.transmit(id);
  harness.place(id, 2, 1);
  await harness.resolveNext(20, 10);
  harness.pumpFrames();
  harness.takeOps();
  harness.setText(1, 1, "");
  harness.setText(1, 2, "");
  harness.setPlacementCells(id, 1, 2, 1, 2);

  for (let index = 0; index < 3; index++) {
    harness.handlers.render();
    harness.handlers.scroll();
  }
  assert.equal(harness.takeOps().length, 0, "render events drew before the animation frame");
  harness.pumpFrames();
  const ops = harness.takeOps();
  assert.equal(draws(ops).length, 1, "coalesced frame drew the image more than once");
  assert.equal(ops.filter(({ op }) => op === "clearRect").length, 1, "coalesced frame cleared more than once");
}

async function scanSkipsPlaceholderFreeRows() {
  const harness = createHarness({ cols: 12, rows: 10 });
  const id = 10;
  harness.setPlaceholder(6, 4, id, 0, 0);
  harness.transmit(id);
  harness.place(id, 1, 1);
  await harness.resolveNext(10, 10);
  harness.pumpFrames();

  assert.deepEqual(
    harness.getCellCallCounts().slice(0, 10),
    [0, 0, 0, 0, 0, 0, 12, 0, 0, 0],
    "only rows containing a pixel placeholder should scan cells",
  );
}

const scenarios = [
  placeholderClustersAreConcealedSafely,
  petStopsAtPaneBorder,
  petPreservesAspect,
  decodeLifecycle,
  oneRowMeterIsClipped,
  partialPlacementKeepsLogicalOrigin,
  resizeAndScrollFollowViewport,
  repeatedRenderReplacesFrame,
  unchangedSceneDoesNotRepaintCanvas,
  renderEventsCoalesce,
  scanSkipsPlaceholderFreeRows,
];

for (const scenario of scenarios) {
  try {
    await scenario();
  } catch (error) {
    error.message = `${scenario.name}: ${error.message}`;
    throw error;
  }
}

console.log(`pixel layer harness: ${scenarios.length} scenarios passed`);
