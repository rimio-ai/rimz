# Sidebar screenshots

`cargo xtask screenshot` renders sidebar ANSI into a PNG so visual review can happen from the same captured frame.

The renderer uses `freeze` with the checked-in Ghostty TokyoNight config at [ghostty-tokyonight.json](../../xtask/assets/ghostty-tokyonight.json). The command writes PNGs under `target/screenshots/` by default and prints the path when the image lands.

## Bootstrap

Install the local renderer, rasterizer, and font once on the machine that runs the capture:

```sh
mkdir -p ~/.local/bin ~/.local/share/fonts
tmp="$(mktemp -d)"
curl -fsSL https://github.com/charmbracelet/freeze/releases/download/v0.2.2/freeze_0.2.2_Linux_x86_64.tar.gz | tar -xz -C "$tmp"
install -m 0755 "$tmp/freeze_0.2.2_Linux_x86_64/freeze" ~/.local/bin/freeze
curl -fsSL https://github.com/ryanoasis/nerd-fonts/releases/download/v3.4.0/JetBrainsMono.tar.xz | tar -xJ -C ~/.local/share/fonts
fc-cache -f
sudo apt-get install -y librsvg2-bin
freeze --version
rsvg-convert --version
fc-match "JetBrainsMono Nerd Font Mono"
```

The task fails at entry when `freeze`, `rsvg-convert`, or JetBrainsMono Nerd Font Mono is missing and prints these commands.

## Commands

`cargo xtask screenshot list` prints `rimz pane list --json` so a target pane id is easy to copy into a focused capture.

`cargo xtask screenshot live [--lines N] [--output PATH]` finds a live `rimz-sidebar` pane from pane metadata, captures it with `rimz pane capture --ansi`, renders a PNG, and leaves focus untouched.

`cargo xtask screenshot pane <id> [--lines N] [--output PATH]` captures any normalized pane id, for example `zellij:terminal_3` or `tmux:%3`.

`cargo xtask screenshot state <empty|fleet|provider|cockpit|focus|economy|reach> [--width W] [--height H] [--output PATH]` renders deterministic fixture frames through the same headless sidebar renderer used by tests. `cockpit`, `focus`, `economy`, and `reach` are packed gallery states for reviewing fleet breadth, expanded team cards, provider spend with pets, and remote/AFK glyph contrast.

`rimz sidebar gallery` opens one frameless compositor pane in the current room with those packed states side by side, split by thin `│` delimiter rules and no real sidebar dock.

Set `RIMZ_BIN=/path/to/rimz` to capture with a specific binary. Without it, `xtask` runs the current checkout through `cargo run --quiet -p rimz --bin rimz -- ...`.

## Fidelity

Live capture reads the multiplexer grid and re-renders it through `freeze`; content, layout, ANSI colors, and glyphs are the review target. `freeze` writes an SVG and `rsvg-convert` rasterizes it at a fixed ~576px width — the sidebar's reading width at roughly 30% of a 1920px screen — so the installed Nerd Font is used for sidebar symbols and the vectors stay crisp at that size. A handful of glyphs Ghostty paints from its own sprite renderer (the Symbols for Legacy Computing block) carry no outline in the font; the capture remaps those to the nearest glyph the font has so the column stays aligned where Ghostty would have drawn the sprite itself. Client-side terminal details such as subpixel hinting and Ghostty font thickening remain outside this capture path.
