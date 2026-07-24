#!/usr/bin/env node

import { performance } from "node:perf_hooks";

const usage = `Usage:
  node scripts/web-drag-probe.mjs --url <page-url> [options]

Options:
  --user <name>          HTTP Basic Auth username
  --pass <secret>        HTTP Basic Auth password
  --duration <ms>        Drag duration (default: 4000)
  --rate <moves/s>       Generated mouse moves per second (default: 60)
  --force-nonstream      Disable WebSocketStream before the client loads
  --headed               Show the Chromium window
  --help                 Show this help`;

function parseArgs(argv) {
  const options = {
    duration: 4000,
    rate: 60,
    forceNonstream: false,
    headed: false,
  };
  const valueFor = (name, index) => {
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`${name} requires a value`);
    }
    return value;
  };

  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    switch (arg) {
      case "--url":
        options.url = valueFor(arg, index++);
        break;
      case "--user":
        options.user = valueFor(arg, index++);
        break;
      case "--pass":
        options.pass = valueFor(arg, index++);
        break;
      case "--duration":
        options.duration = Number(valueFor(arg, index++));
        break;
      case "--rate":
        options.rate = Number(valueFor(arg, index++));
        break;
      case "--force-nonstream":
        options.forceNonstream = true;
        break;
      case "--headed":
        options.headed = true;
        break;
      case "--help":
        options.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (options.help) return options;
  if (!options.url) throw new Error("--url is required");
  if (!Number.isFinite(options.duration) || options.duration <= 0) {
    throw new Error("--duration must be a positive number");
  }
  if (!Number.isFinite(options.rate) || options.rate <= 0) {
    throw new Error("--rate must be a positive number");
  }
  if ((options.user === undefined) !== (options.pass === undefined)) {
    throw new Error("--user and --pass must be provided together");
  }
  return options;
}

function pageTarget(options) {
  const url = new URL(options.url);
  const inlineUser = url.username ? decodeURIComponent(url.username) : undefined;
  const inlinePass = url.username ? decodeURIComponent(url.password) : undefined;
  const username = options.user ?? inlineUser;
  const password = options.pass ?? inlinePass;
  url.username = "";
  url.password = "";
  return {
    url,
    httpCredentials: username === undefined ? undefined : { username, password },
  };
}

function percentile(sorted, fraction) {
  if (!sorted.length) return null;
  return sorted[Math.ceil(sorted.length * fraction) - 1];
}

function formatMs(value) {
  return value === null ? "n/a" : value.toFixed(1);
}

async function run(options) {
  let chromium;
  try {
    ({ chromium } = await import("playwright"));
  } catch {
    throw new Error("playwright is required; see scripts/web-drag-probe.README.md");
  }

  const target = pageTarget(options);
  const browser = await chromium.launch({
    headless: !options.headed,
    args: [
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
    ],
  });

  try {
    const context = await browser.newContext({
      httpCredentials: target.httpCredentials,
    });
    await context.addInitScript(({ forceNonstream }) => {
      if (forceNonstream) {
        try {
          delete window.WebSocketStream;
        } catch (_) {}
      }

      window.__probeEmitted = [];
      const noteMouseReport = (payload, channel) => {
        let bytes;
        if (payload instanceof ArrayBuffer) {
          bytes = new Uint8Array(payload);
        } else if (ArrayBuffer.isView(payload)) {
          bytes = new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
        } else if (typeof payload === "string") {
          bytes = new TextEncoder().encode(payload);
        }
        if (bytes?.[0] === 0x30 && bytes[1] === 0x1b && bytes[2] === 0x5b) {
          window.__probeEmitted.push({ t: performance.now(), channel });
        }
      };

      const nativeSend = WebSocket.prototype.send;
      WebSocket.prototype.send = function send(payload) {
        noteMouseReport(payload, "ws");
        return nativeSend.call(this, payload);
      };

      const nativeWrite = WritableStreamDefaultWriter.prototype.write;
      WritableStreamDefaultWriter.prototype.write = function write(payload) {
        noteMouseReport(payload, "stream");
        return nativeWrite.call(this, payload);
      };
    }, { forceNonstream: options.forceNonstream });

    const page = await context.newPage();
    await page.goto(target.url.href, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => window.term?.element, null, { timeout: 30_000 });

    const terminal = page.locator(".terminal.xterm").first();
    const box = await terminal.boundingBox();
    if (!box || box.width < 4 || box.height < 4) {
      throw new Error("the terminal element has no usable browser geometry");
    }

    await terminal.focus();
    const inset = Math.min(4, box.width / 4, box.height / 4);
    const start = { x: box.x + inset, y: box.y + inset };
    const end = { x: box.x + box.width - inset, y: box.y + box.height - inset };
    const moves = Math.max(1, Math.round(options.rate * options.duration / 1000));
    const interval = options.duration / moves;
    const pendingMoves = [];

    await page.mouse.move(start.x, start.y);
    await page.mouse.down();
    const startedAt = performance.now();
    for (let index = 1; index <= moves; index++) {
      const fraction = index / moves;
      pendingMoves.push(page.mouse.move(
        start.x + (end.x - start.x) * fraction,
        start.y + (end.y - start.y) * fraction,
      ));
      const remaining = startedAt + index * interval - performance.now();
      if (remaining > 0) await page.waitForTimeout(remaining);
    }
    await Promise.all(pendingMoves);
    await page.mouse.up();
    const elapsed = performance.now() - startedAt;
    await page.waitForTimeout(300);

    const observed = await page.evaluate(() => ({
      emitted: window.__probeEmitted ?? [],
      decisions: window.__rimzWeb?.decisions?.slice(-32) ?? null,
    }));
    const gaps = observed.emitted
      .slice(1)
      .map((entry, index) => entry.t - observed.emitted[index].t)
      .sort((left, right) => left - right);
    const channels = [...new Set(observed.emitted.map(({ channel }) => channel))].join(", ") || "none";
    const emissionSpan = observed.emitted.length > 1
      ? observed.emitted.at(-1).t - observed.emitted[0].t
      : elapsed;
    const emissionSeconds = Math.max(emissionSpan, 1) / 1000;
    const median = gaps.length
      ? gaps.length % 2
        ? gaps[(gaps.length - 1) / 2]
        : (gaps[gaps.length / 2 - 1] + gaps[gaps.length / 2]) / 2
      : null;

    console.log(`generated moves: ${moves}`);
    console.log(`actual drag ms: ${elapsed.toFixed(1)}`);
    console.log(`emitted reports: ${observed.emitted.length}`);
    console.log(`channel: ${channels}`);
    console.log(`emissions/s: ${(observed.emitted.length / emissionSeconds).toFixed(1)}`);
    console.log(
      `inter-coordinate gap ms: median=${formatMs(median)} p90=${formatMs(percentile(gaps, 0.9))} max=${formatMs(gaps.at(-1) ?? null)}`,
    );
    if (target.url.searchParams.get("rimzdebug") === "1") {
      console.log(`flow decisions tail: ${JSON.stringify(observed.decisions)}`);
    }
  } finally {
    await browser.close();
  }
}

let options;
try {
  options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log(usage);
  } else {
    await run(options);
  }
} catch (error) {
  console.error(`web-drag-probe: ${error.message}`);
  process.exitCode = 1;
}
