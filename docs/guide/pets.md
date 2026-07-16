# Pets

A pet is a small animated companion on the sidebar's [provider dashboard](./sidebar.md#the-provider-dashboard): a pixel-art sprite that lives beside the budget bars, runs while the agents run, and waves when one waits on you. The pet mirrors the selected card's state, so the bottom panel carries the room's motion while the agent cards above stay steady. Two lines turn one on:

```sh
rimz config set theme.pets.enabled true
rimz config set theme.pets.pet rocky      # rimz list-pets previews every choice
```

<p align="center">
  <img src="../rimz-pets.png" alt="rimz list-pets in Ghostty: the eight built-in pets rendered as crisp pixels" width="100%">
  <br/><sub><code>rimz list-pets</code> in Ghostty: the built-in pets as crisp pixels.</sub>
</p>

Everything on this page is the `[theme.pets]` table in `~/.config/rimz/theme.toml`; the rest of the sidebar's appearance — palettes, glyphs, animations — is the [theming guide](./theme.md). Enabling pets widens the automatic sidebar default to 30%; an explicit `theme.display.width_percent` still wins. A pet is pure display: it changes what the dashboard paints, never what an agent can do.

```toml
[theme.pets]
enabled = true
pet = "rocky"
glyphs = "auto"
# cell_aspect = 2.5
voice = true
```

| key | does |
| --- | --- |
| `enabled` | turns the dashboard pet on (off by default) |
| `pet` | which pet: a built-in id, a [petdex](#petdex) pet, an HTTPS URL, or a local sheet path |
| `glyphs` | render tier: `auto`, `pixel`, or `sextant` |
| `cell_aspect` | optional terminal cell height/width ratio for sextant aspect correction |
| `voice` | one-line captions on pet-action changes, drawn from each state's pool |

## What the pet does

The pet acts out the selected card, one steady animation per state: it waves when an agent waits on your answer, paces while one reasons, runs while the work moves, has a rough patch when a turn fails, and rests when everything is caught up. Any state change lands with a small jump first, so motion in the corner of your eye means the room changed. A one-line caption under the sprite names the moment (`someone needs you`, `thinking it through`, `all caught up`), drawn from a pool of a hundred-plus lines per state so the pet rarely repeats itself; `voice = false` keeps the animation and hides the captions.

## Choosing a pet

`rimz list-pets` previews every choice as a sprite grid, straight in the terminal: the built-ins first, then any [petdex](#petdex) pets installed locally, each labeled with the id you give `theme.pets.pet`. The built-in ids are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`.

### Petdex

[petdex.dev](https://petdex.dev/) is the community pet gallery: thousands of open-source pixel pets, browsable by collection and previewable in every animation state. RimZ reads the petdex install format directly, so a pet from the gallery is two commands away:

```sh
npx petdex install wall-e                # installs under ~/.codex/pets/wall-e/
rimz config set theme.pets.pet wall-e    # a bare name selects the installed pet
```

A petdex install is a directory under `~/.codex/pets/<name>/` holding a `pet.json` manifest beside its WebP or PNG sprite sheet; RimZ reads the manifest and loads the sheet it names. `pet = "~/.codex/pets/wall-e/"` reads the same directory by path, and any directory laid out this way works, wherever it lives.

### Your own sheet

The same `pet` key takes bring-your-own art:

- `pet = "https://example.com/my-pet.webp"` fetches an HTTPS WebP or PNG sheet once and caches it. Plain `http://` is refused.
- `pet = "~/art/my-pet.png"` reads a local WebP or PNG sheet; a directory path is read as a [petdex](#petdex) install.

Every source uses the petdex sheet geometry — a `1536x1872` image holding an `8x9` grid of `192x208` frames — with each row an animation the pet plays. The row-by-row catalog, cache layout, and decode pipeline live in the [pets internals](../internals/sidebar/pets.md#assets).

## Crisp pixels and cell art

Two tiers render the same sheet. **Pixel** draws the sprite through the kitty graphics protocol, crisp at native resolution: it needs a Ghostty or kitty terminal ([installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional) sets one up), and inside tmux also tmux 3.6+ with `allow-passthrough on` (or `all`). **Sextant** draws the sprite as cell art, each terminal cell split into a 2x3 pixel grid, and works everywhere: Zellij, SSH, and any terminal that shows color.

<p align="center">
  <img src="../rimz-pets-sexant.png" alt="rimz list-pets in Alacritty: the same built-in pets rendered as sextant cell art" width="100%">
  <br/><sub>The same pets as sextant cell art, here in Alacritty; Zellij rooms render this tier too.</sub>
</p>

`glyphs` picks the tier:

| `glyphs` | renders |
| --- | --- |
| `auto` (default) | pixels when the terminal and mux qualify, otherwise sextant cell art |
| `pixel` | opts past the terminal-name allowlist for newer kitty-compatible terminals, while hard gates such as tmux passthrough still apply |
| `sextant` | the most portable cell art, on every backend |

Sextant pets fit their source proportions inside the fixed dashboard footprint. RimZ probes the terminal's cell pixel dimensions when the pty exposes them; Zellij reports no pixel dimensions, so set the ratio manually when its font makes pets look tall or wide: `rimz config set theme.pets.cell_aspect 2.5`. Explicit config wins over the probe, and a neutral `13/6` fallback preserves the previous rendering when neither fact is available.

On macOS, terminal graphics updates can make AppKit re-evaluate the pointer shape while pixel pets animate; RimZ transmits each sprite image once to minimize that traffic, and `glyphs = "sextant"` switches to the flicker-free cell-art path when you want it fully gone.

## Offline and privacy

Built-in and URL sheets fetch once over HTTPS into the per-machine cache; `RIMZ_PETS_OFFLINE=1` serves the cache only. Petdex and local sheets read from disk and make no network request.

Pets run no commands. Asset loading sends only the configured asset request; prompts, transcripts, pane text, workspace paths, and provider credentials stay local. The pet's place in the threat model is one bullet in [security.md](./security.md#what-leaves-your-machine).

Sheet geometry, the cache layout, and the pixel gates live in the [pets internals](../internals/sidebar/pets.md).
