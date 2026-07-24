# Browser drag probe

`drag-probe.mjs` measures the browser client's emitted terminal mouse reports while it drives a fixed-rate diagonal drag across a live RimZ web terminal.

## Setup

Run the probe from the repository root with Node.js 20 or newer. Install Playwright without recording it as a project dependency, then install its Chromium build:

```sh
npm install --no-save --package-lock=false playwright
npx playwright install chromium
```

The target needs a live web room with mouse mode active in the multiplexer or terminal application. Run `yes` in a pane to reproduce the continuous-repaint stress case. Get the page URL and saved credential from `rimz web url`.

## Usage

Pass credentials in the URL:

```sh
node scripts/web-probe/drag-probe.mjs \
  --url 'http://rimz:secret@127.0.0.1:8200/?room=my-room&rimzdebug=1'
```

Or keep them separate:

```sh
node scripts/web-probe/drag-probe.mjs \
  --url 'http://127.0.0.1:8200/?room=my-room&rimzdebug=1' \
  --user rimz \
  --pass secret \
  --duration 4000 \
  --rate 60
```

The report includes generated and emitted counts, the active `ws` or `stream` send channel, emissions per second, inter-coordinate gap percentiles, and the tail of `window.__rimzWeb.decisions` when the URL carries `rimzdebug=1`.

Use `--force-nonstream` to delete `window.WebSocketStream` before the page scripts load and compare the plain WebSocket path. Use `--headed` to watch the run.

This script is a field tool and no repository gate executes it. Verify its assumptions against the current browser client before using its measurements to diagnose a new failure.
