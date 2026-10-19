# Sidebar screenshots

`cargo xtask screenshot` renders sidebar ANSI into a PNG so visual review can happen from the same captured frame.

For clock, stage-flip, click-routing, native-subagent replay, and kitty-graphics capture and Ghostty replay checks in a disposable room, use the [sidebar live check](./sidebar-live-check.md).

The renderer uses `freeze` with the checked-in Ghostty TokyoNight config at [ghostty-tokyonight.json](../../xtask/assets/ghostty-tokyonight.json). The command writes PNGs under `target/screenshots/` by default and prints the path when the image lands.

## Bootstrap

Install the local renderer, rasterizer, and font once on the machine that runs the capture:

```sh
mkdir -p ~/.local/bin ~/.local/share/fonts
sudo apt-get install -y librsvg2-bin unzip
tmp="$(mktemp -d)"
curl -fsSL https://github.com/charmbracelet/freeze/releases/download/v0.2.2/freeze_0.2.2_Linux_x86_64.tar.gz | tar -xz -C "$tmp"
install -m 0755 "$tmp/freeze_0.2.2_Linux_x86_64/freeze" ~/.local/bin/freeze
curl -fsSL -o "$tmp/cascadia.zip" https://github.com/microsoft/cascadia-code/releases/download/v2407.24/CascadiaCode-2407.24.zip
unzip -j -o "$tmp/cascadia.zip" 'ttf/static/CascadiaCodeNF-*.ttf' -d ~/.local/share/fonts
curl -fsSL https://github.com/ryanoasis/nerd-fonts/releases/download/v3.4.0/JetBrainsMono.tar.xz | tar -xJ -C ~/.local/share/fonts
fc-cache -f
freeze --version
rsvg-convert --version
fc-match "Cascadia Code NF"
```

The task fails at entry when `freeze`, `rsvg-convert`, or the required Cascadia Code NF font is missing and prints these commands. JetBrainsMono is the icon fallback.

## Commands

`cargo xtask screenshot list` prints `rimz pane list --json` so a target pane id is easy to copy into a focused capture.

`cargo xtask screenshot live [--lines N] [--output PATH]` captures the `sidebar` target with `rimz pane capture --ansi`, renders a PNG, and leaves focus untouched. The target resolves the caller's own view first, then the focused tab, then any sidebar in the session.

`cargo xtask screenshot pane <id> [--lines N] [--output PATH]` captures any normalized pane id, for example `zellij:terminal_3` or `tmux:%3`.

For either capture command, `--lines N` renders the last `N` lines, reaching into scrollback on both backends. Trailing blank rows do not count; a pane with fewer content lines renders fewer. Without `--lines`, the visible screen is unchanged.

`cargo xtask screenshot scenario <file.scn> [--at 0s] [--width W] [--height H] [--theme-mode auto|truecolor|256] [--theme-scheme NAME] [--output PATH]` plays from zero to the requested time through the real sidebar loop and renders its ANSI frame as a PNG. Without geometry overrides it uses the scenario's dimensions.

Live sidebar state can also be captured as line-oriented terminal output without the mux through `rimz sidebar frame`.

`cargo xtask screenshot frames <dir>` writes a PNG beside each key-frame `.ansi` in a `sidebar play --frames` export, including nested scenario directories.

Set `RIMZ_BIN=/path/to/rimz` to capture with a specific binary. Without it, `xtask` runs the current checkout through `cargo run --quiet -p rimz --bin rimz --features testkit -- ...`.

## Scripted playback

`sidebar play` is hidden and requires a `testkit` build. Checked-in stories live in `crates/rimz/scenarios/`: `pilot.scn` animates delegation, a countdown, an ask held by a layout freeze, its release, and completion. The other files cover the former still scenes, including both clock glyph sets. Cards contain only state their provider can reach on a default configuration.

```sh
cargo run --quiet -p rimz --bin rimz --features testkit -- sidebar play crates/rimz/scenarios/pilot.scn --check
cargo run --quiet -p rimz --bin rimz --features testkit -- sidebar play crates/rimz/scenarios/pilot.scn --frames "$TMPDIR/sidebar-frames"
cargo xtask screenshot frames "$TMPDIR/sidebar-frames"
cargo xtask screenshot scenario crates/rimz/scenarios/pilot.scn --at 17s -o "$TMPDIR/sidebar-ask.png"
```

On a terminal, `sidebar play <file.scn>...` plays inline with one fixed-width column per file. Columns must fit at launch; later resizes clip them. Space pauses, left/right seeks to a beat, `r` restarts, `h` hides the reserved status row, and `q`, Escape or Ctrl-C quits. Other input follows the pointer to the column's native input path. A saved file reloads at its current time; a bad save keeps the last good story playing and shows its parse error, even when the status row is hidden.

For agent review, `--at 17s` prints labelled physical rows, `--at 17s --ansi` emits raw ANSI for one file, and `--at 17s --section @coder#main` selects one card. `#main` selects a group and all its cards; other section names are `cockpit`, `providers`, and `chrome`. Process rows use `<process name>#<group label>`; padding has an empty label. Section filtering happens after full-frame layout. With `--at` or `--frames`, `--height N`, `--width N`, `--theme-mode auto|truecolor|256`, and `--theme-scheme NAME` override rendering without editing the story. `--frames` writes labelled text and `index.jsonl` with timestamps, beat labels, changed sections and key-frame filenames; key frames retain ANSI for colour review. Unchanged beat/deadline key frames remain in the index. Ordinary frames freeze cosmetic animation; `--motion 6s..9s` exports every paint in that window with animation enabled. A pipe without `--at`, `--frames`, or `--check` refuses before terminal output.

### Scenario format

A `.scn` is UTF-8 with one statement per line. An optional header precedes at most one preset, then `agent @handle kind [in #channel|worktree]` declarations (root by default), then non-decreasing timed beats. A `# ` starts a comment or trailing beat label; `#main` is a channel. Header defaults are the file stem, width 54, headless height 34, pets off, and a length three seconds after the last beat. Every beat must be strictly before the length. Durations accept `500ms`, `1.5s`, and `1m30s`.

```text
scenario review width 54 height 34 length 12s pets off glyphs unicode
agent @coder codex in #main
0s @coder runs "implement the parser" # opening
2s @coder spawns @fox claude "review the parser"
4s @fox done "looks good"
5s @coder asks "ship it?"
6s ui expand @coder
8s @coder answered
9s @coder done "shipped"
```

Presets are `empty`, `fleet`, `provider`, `cockpit`, `focus`, `economy`, `reach`, `clocks`, and `clocks-done`; declarations and beats may follow and address their handles. Use `glyphs nerd-font` for the alternate clock icons, or `pets <pet>` for a companion. Other agent verbs are `idle`, `waits <duration> ["reason"]`, `fails "reason"`, `leaves`, and `set context <pct>%|cost $<usd>|tokens <n>|model <name>`. UI beats support `select`, `expand`, `collapse`, or `click @handle`, `key <key>`, and `scroll up|down [n]`. Handle-targeted UI beats address root cards, not spawned children; a child target is refused with its parent handle. Unknown names suggest a correction; undeclared handles and states the provider cannot reach are refused with a line, column and fix.

## Fidelity

Live capture reads the multiplexer grid and re-renders it through `freeze`; content, layout, ANSI colors, and glyphs are the review target. The frame targets Ghostty's 2× Retina geometry with Cascadia Code NF at 13pt and `adjust-cell-height = 2`: 26px glyphs, 32px rows, and 4px padding. A 54×34 frame is 831×1103px, and `rsvg-convert` rasterizes the SVG 1:1 without resizing.

On macOS, 13pt × 2 gives 26 device pixels; Cascadia's typo metrics give round(26 × (1900 + 480)/2048) + 2 = 32px rows, since `adjust-cell-height = 2` adds two device pixels, not four. Default 2pt window padding becomes 4px, while the font's exact advance, 26 × 1200/2048 = 15.234375px, stays fractional here; Ghostty rounds it to 15px, making its frame about 1.6% narrower.

Ghostty draws box-drawing, block, braille, and Legacy Computing glyphs from its sprite renderer to fill the cell, while the font's outlines leave gaps in `│` rules and bar meters. `freeze` places background highlights on its own 1/1.68-em column grid, so a highlight drifts right by up to about 0.8 cell at the right edge of a 54-column frame. `freeze` drops bold and dim styling. CoreText renders heavier than FreeType/librsvg and blends in Display P3. Kitty-graphics pets never appear in these captures. Cascadia 2407.24 lacks six of the sidebar's Nerd Font icons and a few arrows, which fall back to JetBrainsMono NFM (or DejaVu Sans Mono), as Ghostty also falls back. `--theme-mode 256` colors go through freeze's xterm palette, not Ghostty's.
