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

`cargo xtask screenshot state <empty|fleet|provider|cockpit|focus|economy|reach> [--width W] [--height H] [--output PATH]` renders deterministic fixture frames through the same headless sidebar renderer used by tests. `cockpit` (fleet breadth), `focus` (expanded team cards and the remote footer), `economy` and `reach` (provider spend; pets appear in the gallery with `--pets`) are packed gallery states for review. A fixture card carries only state its agent kind can reach on a default configuration: a cache clock goes on a Claude or Codex card, since only those adapters declare a prompt-cache lifetime.

Live sidebar state can also be captured as line-oriented terminal output without the mux through `rimz sidebar frame`.

`cargo run --quiet -p rimz --bin rimz --features testkit -- sidebar gallery [--pets]` opens one frameless compositor pane in the current room with those packed states side by side, split by thin `│` delimiter rules and no real sidebar dock; outside any mux session, the same command renders the compositor inline in the current terminal. The contributor-only gallery and fixture verbs require this `testkit` build. The compositor chooses four columns when the terminal is wider than 240 columns and three at or below that width, decided once at launch. `--pets` turns the dashboard companion on in every column.

Set `RIMZ_BIN=/path/to/rimz` to capture with a specific binary. Without it, `xtask` runs the current checkout through `cargo run --quiet -p rimz --bin rimz --features testkit -- ...`.

## Fidelity

Live capture reads the multiplexer grid and re-renders it through `freeze`; content, layout, ANSI colors, and glyphs are the review target. The frame targets Ghostty's 2× Retina geometry with Cascadia Code NF at 13pt and `adjust-cell-height = 2`: 26px glyphs, 32px rows, and 4px padding. A 54×34 frame is 831×1103px, and `rsvg-convert` rasterizes the SVG 1:1 without resizing.

On macOS, 13pt × 2 gives 26 device pixels; Cascadia's typo metrics give round(26 × (1900 + 480)/2048) + 2 = 32px rows, since `adjust-cell-height = 2` adds two device pixels, not four. Default 2pt window padding becomes 4px, while the font's exact advance, 26 × 1200/2048 = 15.234375px, stays fractional here; Ghostty rounds it to 15px, making its frame about 1.6% narrower.

Ghostty draws box-drawing, block, braille, and Legacy Computing glyphs from its sprite renderer to fill the cell, while the font's outlines leave gaps in `│` rules and bar meters. `freeze` places background highlights on its own 1/1.68-em column grid, so a highlight drifts right by up to about 0.8 cell at the right edge of a 54-column frame. `freeze` drops bold and dim styling. CoreText renders heavier than FreeType/librsvg and blends in Display P3. Kitty-graphics pets never appear in these captures. Cascadia 2407.24 lacks six of the sidebar's Nerd Font icons and a few arrows, which fall back to JetBrainsMono NFM (or DejaVu Sans Mono), as Ghostty also falls back. `--theme-mode 256` colors go through freeze's xterm palette, not Ghostty's.
