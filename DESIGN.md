# Design

Rimz turns a project into one durable multiplexer room with a shared feed. The product is narrow on purpose: wrap Zellij or tmux, persist workspace state in a ledger, surface attention in a sidebar, and let humans, scripts, CI, and coding agents participate through one CLI.

> **Invariant.** Rimz routes attention. By default, it does not answer for you.

The agent's own UI stays the answer surface. This is the call that separates Rimz from every other agent multiplexer. Silently auto-approving a tool call hides the only moment a human can stop something destructive. Rimz refuses that move by default. When you want help, you enrol a resolver explicitly; the resolver chain ends with you.

## The three operating paths

Every actionable feed item is created with one `surface`. The surface decides whether anyone is holding the hook open and what answer paths are legal.

| Surface | Source | Hook blocks? | Runtime behaviour |
| --- | --- | --- | --- |
| `native_ui` | Agent hook with no fresh enrolled resolver | no | Hook writes the feed item, wakes sidebars, prints the neutral payload, and exits. The agent's own UI asks the human. |
| `bridge` | Agent hook with a fresh enrolled resolver | yes | Hook writes the feed item, binds a per-request socket, and waits up to the agent's hook cap for a resolver answer. Falls back to native UI on timeout. |
| `script` | A script that called `rimz feed ask` | yes | Script blocks until a human or resolver answers, or until the script's own timeout fires. No agent involved. |

These three paths are the product contract. There is no fourth path; Rimz core never adds a hidden auto-approve. Unattended auto-approve is `native_ui` with the agent's own bypass flag, or `bridge` with a permissive resolver — both visible in the ledger.

## Commitments

Each commitment is a decision a reader might challenge. The reason is on the same line.

- **One repo, one room.** A project repo maps to one workspace, one multiplexer session, one ledger, one sidebar. Worktrees of the repo group inside it. A repo with five branches and ten agents stays scannable as one room with internal subdivisions.
- **The feed is the shared surface.** Agents, scripts, CI shims, and humans publish or resolve through `rimz event ...` and `rimz feed ...`. One concept to learn, three audiences to serve.
- **The ledger owns durability.** Detach, sidebar reload, sidebar crash, or no-client mode never lose feed state. The sidebar is a renderer over the ledger; correctness lives one layer down.
- **One feed, many renderers.** The `rimz sidebar snapshot` JSON is the shared view-model; every sidebar is a projection of it. The default renderer is the native binary in a pane, identical on Zellij and tmux and across detach/reattach. Zellij users may opt in to a docked plugin rail for nicer placement. Renderers are interchangeable presentation; none owns state and none gates correctness.
- **Interactive attach is opportunistic.** `rimz` enters the selected mux only when stdin/stdout are TTYs and the caller is not already inside that mux. Non-interactive callers get a printed attach command; explicit flags override the default.
- **Observe and route by default.** Without an enrolled resolver, Rimz never answers an agent prompt. It tells the human which pane needs attention and gets out of the way.
- **Resolvers are explicit and per-machine.** A resolver engages the bridge only if it is on the local allowlist *and* it is heartbeating freshly. Same-UID file access is not the trust boundary.
- **No transcript correctness.** Pane contents and transcripts may enrich display only. They never decide permissions, state transitions, or correctness. Core code never scrapes a pane.
- **No core auto-type.** `pane capture` and `pane send` are public primitives for humans and resolvers. Rimz core does not type into panes on anyone's behalf.
- **Both multiplexers are first-class.** Zellij and tmux run the same ledger, bridge, CLI, and sidebar model. Core behaviour cannot depend on a Zellij-only pipe or a tmux-only feature. The optional Zellij plugin rail is an enhancement only; tmux reaches the same surface through the native pane.
- **Headless works.** Hooks, the bridge, and `rimz feed ask` work with no sidebar and no attached client. The sidebar is a UI; the workspace runs without one.

## Sidebar shape

The sidebar is a **worktree-keyed presence and attention map**, not a feed reader. Three laws govern it:

- **Worktree-first.** A worktree is total isolation — only same-worktree agents collaborate — so the sidebar groups by worktree before anything else.
- **Notify, don't answer.** The sidebar routes you to the pane that needs you; it never reproduces the agent's question. You read and answer in the agent's own UI. (A script's `feed ask`, which chose Rimz as its surface, is the exception.)
- **Presence and attention, never history.** It shows what is *running now* — every pane's foreground process, with agents enriched from the ledger — and what *needs you* (waiting/failed agents, pending items). Nothing resolved or historical; that lives in `rimz feed list`.

**Presence is a live view; the ledger is truth.** Row presence is read live from the multiplexer's pane list — a pane running `zsh` is a row, and it becomes the agent's row when that pane runs an agent. The ledger stays the source of attention and of every durable fact; the pane list never decides correctness. A hook-driven agent that exits is gone the moment its pane reverts to a shell or closes — liveness is the live process, not a status the ledger has to retract.

Agent status is a five-value rollup the agent owns (`running`/`waiting`/`idle`/`success`/`failed`); Rimz observes it and paints one glyph + color per *displayed* state. The glyph carries the state by **shape** (so it survives `NO_COLOR`); color reinforces it. Two symbols carry every attention state — `?` *needs your answer*, `!` *needs a look* — and only genuinely-active states animate. A pane running a non-agent process (a bare shell, an editor) renders as a dim process row with no status glyph and never counts as attention.

| state              | glyph | animation              | color         | attention |
|--------------------|-------|------------------------|---------------|-----------|
| `waiting`          | `?`   | —                      | yellow→red    | yes       |
| `failed`           | `!`   | —                      | yellow→red    | yes       |
| stalled (≥10 min)  | `!`   | —                      | yellow→red    | yes       |
| `running` working  | `⢿`   | spin `⣾⣽⣻⢿⡿⣟⣯⣷`        | Claude clay   | no        |
| `running` thinking | `✽`   | sparkle `· ✢ ✳ ✶ ✻ ✽`  | Claude clay   | no        |
| resolver answering | `⠋`   | braille spin           | yellow        | yes       |
| `idle`             | `○`   | —                      | green dim     | no        |
| `success`          | `✓`   | —                      | green dim     | no        |

`waiting` is a pending human ask folded onto the agent's row; "thinking" is `running` in read-only plan mode; "stalled" is a `running` agent silent past Claude Code's ~10-minute operation timeout, escalated to `!` so a wedged agent becomes actionable rather than a frozen spinner. Both "thinking" and "stalled" are display projections — the rollup keeps the true `running` status. The two attention glyphs (`?`/`!`) rest bold **yellow** and redden to bold **red** once a row sits unanswered past the neglect window (`[sidebar] attention_redden_secs`, default 30 minutes) — a fresh ask reads calm-urgent, a long-ignored one heats up. The two calm states share a quiet green: a hollow `○` idle and a `✓` success.

Agent permission postures (observed from the agent, not set by Rimz) are one sticky reading of the agent's permission slider, rendered as capability tokens; `default` and `unknown` are omitted, and the rest carry a *permission-heat* gradient — `plan` calm blue, `auto` amber, `yolo` bold red (the security surface, loud even when other tokens dim). `plan` is the read-only slider position: it shows as a blue pill whenever the agent is not running, and surfaces as the "thinking" state above while it is. `interactive` folds into `default`.

```text
default   plan   auto   yolo   unknown
```

Live enrichments use one grammar. Each agent row carries one context-window meter (`▣`); todo progress renders as dots, tokens as a glyph set (`◇` total · `↘` input · `↗` output · `◌` cached), and worktree diff stats as paired numeric tokens. The **5-hour and 7-day budgets are account-scoped, not session-scoped** — every session of a provider shares one account — so they leave the rows entirely for a pinned **per-provider dashboard** at the bottom of the sidebar: one block per provider — active or merely logged in, so your accounts and budgets show even between turns (a brand emblem, the plan and version, aggregate spend/tokens, and the two budgets as draining "mana" bars; an unmetered API-key account shows an `∞` bar instead). Enrichments enrich display only and never drive a decision. A running agent's leading cell animates continuously on a wall-clock tick — a braille spinner while working, a sparkle while thinking, both in Claude clay — so motion tracks live work; silence no longer freezes the spinner but escalates to `!` once the agent crosses the stall window. Color is a garnish layer over the same glyph grammar, and `NO_COLOR` keeps every shape readable.

Renderer details — the borderless frame and selection lane, the card anatomy, the meter grammar and configurable row density, the fixed cockpit, the per-provider dashboard, attention ranking, and the jump interaction — live in [docs/internals/sidebar.md](./docs/internals/sidebar.md).

## Non-goals

- Not a cloud control plane or cross-workspace orchestrator.
- Not agent-required — scripts and humans are first-class.
- Ships no resolver as product. The protocol is the contract; resolver code is user code.
- Does not own process resurrection across host restart. The ledger survives a reboot; running sessions need tmux-resurrect, Zellij resurrect, systemd, or another host supervisor.
