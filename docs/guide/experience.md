# The Rimz experience — first run to fleet

> The product walkthrough, written from the chair of someone meeting Rimz for the first time. [product.md](./product.md) is the five-minute tour; [DESIGN.md](../../DESIGN.md) holds the invariants; this doc is the *felt* experience, phase by phase — what the developer does, sees, feels, and thinks, from the first keystroke to a ten-agent fleet. The frames below are illustrative sketches of the moment; the exact, machine-checked rendering — every glyph, meter, and zone — is the [interface reference](../interface/sidebar.md). Renderer mechanics live in [docs/internals/sidebar.md](../internals/sidebar.md).

The reader of this doc is the primary audience: an engineer who runs Claude Code and Codex agents all day, several at once, and is tired of flipping tabs to find the one that's blocked. They saw Rimz on Hacker News an hour ago. They want to feel the value in under five minutes or they close the tab. Every decision below is in service of that five minutes.

Three experience laws hold the whole walk together. They're the lens for every frame:

1. **Never blank, never lying.** The column always shows the truth about what's running — and when it *can't* (a failed fetch), it says so out loud instead of showing a stale frame.
2. **Notify and route.** The sidebar's whole job is to get you to the pane that needs you — it names who needs you and takes you straight there, and you answer in the agent's own UI where the full context lives. (A script that chose Rimz as its surface is the one item you answer from the sidebar itself.)
3. **The row is the link.** You don't read a pane number and go type it into the mux. You select the row and you're there.

---

## Phase 0 — Discovery and install (before the terminal)

**Does:** Reads the HN post, clicks through to the README, installs.

**Sees:** A landing page that earns the install in three lines — *"One room per project. A sidebar that tells you which agent needs you. Survives detach and reattach from anywhere."* — then a single copy-paste install and a single first command.

```sh
# one of:
brew install rimz
cargo install rimz
curl -fsSL https://rimz.sh/install | sh

cd ~/code/query-engine   # a real, small project they already have
rimz
```

**Feels:** Low-commitment curiosity. They have not read the docs and will not.

**Thinks:** *"Will this mess with my Claude config? How do I back out?"* — the two
questions the next phase must answer before it asks for anything.

> **Design law — the install is one line and the first command is `rimz`.** No init wizard, no config file to write, no account. The binary auto-detects the multiplexer (Zellij or tmux) and the agents (Claude, Codex). Everything Rimz needs, it discovers or asks for in-flow. A tool that needs a tutorial before the first frame has already lost this reader.

---

## Phase 1 — The first keystroke: `rimz` and the consent gate

This is the single most important screen in the product, because it's where trust is won or lost. Running `rimz` the first time on a machine means Rimz wants to add hooks to the agents the reader already has — and modifying their agent config is exactly the thing they were nervous about in Phase 0.

So the first thing they see is not the room. It's a clean, full-terminal consent gate — terminal-native, not a popup — that treats the modification as a security surface and turns their nervousness into trust.

```
  rimz · first run on this machine

  Rimz routes attention across your coding agents into one sidebar.
  To show what an agent is doing, it adds reporting hooks to the
  agents you already have on this machine.

  Detected:
    ● Claude Code     ~/.claude/settings.json
    ● Codex           ~/.codex/config.toml

  What changes — additive, your existing hooks are kept:

    ~/.claude/settings.json
      + SessionStart   → rimz hooks feed --source claude
      + UserPromptSubmit, PreToolUse, PostToolUse, Stop, … (8 total)

  These hooks only *report* events to Rimz. They never answer a prompt
  for you — your agent's own UI stays the answer surface. Reversible any
  time with `rimz hooks uninstall <agent>`.

    [↵] install all             [d] show full diff
    [c] choose per agent        [s] skip — I'll set up later
```

**Does:** Reads it in five seconds. Maybe hits `d` to see the literal diff,
confirms it's additive and boring, hits Enter.

**Feels:** Reassured. The screen answered both Phase-0 questions before they
asked — *additive*, *reversible*, *"never answers for you."*

**Thinks:** *"OK, it's not going to hijack my agents. It just watches. Fine."*

> **Design laws for the consent gate.**
> - **Show the exact diff, framed as additive.** The fear is "it overwrites my hooks." Naming the preserved keys kills that fear in one line. The frame above is illustrative; the authoritative wired set and config shape are in [hooks.md](../internals/hooks.md#hook-install--the-visible-security-step).
> - **State the boundary in the consent itself:** hooks *report*, they don't *answer*. This is the product invariant, surfaced at the exact moment the reader is deciding whether to trust it.
> - **Always offer `skip`.** Declining installs nothing and still drops them into the room — an agent then shows up as a plain process row with no status, and the empty-room hint tells them how to wire it later. Consent is never a wall.
> - **Once per machine, never again.** Hook install is per-machine, per-agent state. Subsequent `rimz` runs go straight to the room. `rimz doctor` reports per-agent install status for anyone who forgets where they're at.
> - **Project config is a *separate*, later gate.** If this repo ever carries a committed `.rimz/config.toml`, trusting it is its own prompt with its own diff (see [trust.md](../internals/trust.md)) — a toy project has none, so the reader never sees it on day one.

---

## Phase 2 — The first frame: the empty room

Consent done, Rimz ensures the session exists and drops the reader in: a working shell pane (focused, pristine — nothing dumped into their scrollback) on the right, and the sidebar pinned left at ~30% width.

The column is never blank. Their shell pane is itself a row. With nothing needing attention, the cockpit's make-up line is omitted (no agents to summarize) and a dim hint points at the *one* next thing to do.

```
 ⌘ query-engine

 ◎ 0
 ¤ 0
 ────────────────────────────────────────────

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ zsh

 no agents yet
 run claude or codex
 in a pane to begin

                  ? for help
```

**Does:** Looks left, reads two words of hint, looks back at the prompt.

**Feels:** Oriented. Nothing is demanding anything. The `⌘ query-engine` line is the project name
they recognize; the `▏main` lane tells them which worktree they're standing in.

**Thinks:** *"Right — it wants me to run my agent in here. Let's see what it
does."*

> **Design laws for the empty room.**
> - **Presence, not emptiness.** Even with nothing running, the shell pane is a row, so the column demonstrates its core idea (one row per pane) before any agent exists.
> - **The hint is the next literal command, and it adapts.** Hooks wired → *"run claude or codex."* Hooks skipped → *"install hooks: rimz hooks install claude."* It clears the instant the first agent or feed item appears.
> - **The hint is for a *healthy* empty room only.** If the refresh loop is degraded, the banner takes over and the hint is suppressed — an empty body under a failed fetch is a *missing* snapshot, not an empty room (see [Phase 8](#phase-8--when-something-is-wrong)).

---

## Phase 3 — The first agent appears by itself

The reader types `claude` in the shell pane and just looks at its input box — hasn't prompted it yet. Within about a second, the pane that read `○ zsh` *becomes* the agent's row. Same row, re-skinned — never a second entry.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 0   ! 0   ○ 1              ✽ 0   ⢿ 0   ✓ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌○ claude · Opus · xhigh
▌  —
▌  ▣ ──────────────────────────────────    0%

                  ? for help
```

**Does:** Nothing. That's the point.

**Feels:** The first small hit of delight. *They did nothing extra* and their
agent showed up in the sidebar, correctly named, with its model and effort. The session-start hook fired, the ledger overlaid identity onto the pane, and the row updated — no config, no flag, no restart.

**Thinks:** *"Oh — it just knows. And it knows it's Opus on xhigh. Nice."*

> **This is the activation moment.** Everything before it was setup; this is the first time the product *does something for them*. The latency budget here is tight: the row must update within a second or two of the session-start hook, or the magic reads as lag. Idle never fills an attention bucket, because an idle agent is not a cue.

---

## Phase 4 — Prompted and working

The reader gives Claude a task. The prompt then the first tool call move the row to `⢿ running`; the task slot fills with the agent's reported task (or the first ~20 chars of the prompt). A *wedged* `running` agent betrays itself by escalating to the static `!` attention state once it falls silent past the stall window, rather than spinning forever.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 0   ! 0   ○ 0              ✽ 0   ⢿ 1   ✓ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⢿ claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%

                  ? for help
```

**Feels:** Calm. The attention buckets hold at `? 0  ! 0` — running is not their cue to do anything.
They go get coffee, or open a second agent.

**Thinks:** *"Green means go, it's working, I don't need to watch it."*

> **Design law — age is the honesty signal.** There is no global "updated 2s ago" stamp anywhere in the product. Freshness is per-row, and fetch health is the degraded banner's job. The resting card stays calm — no age on the compact row; a wedged `running` agent outs itself by escalating to `!`, not by a creeping timestamp. The one place a coarse last-activity age surfaces is the expanded work line, a deep-dive detail you opt into by selecting the row — never Rimz pretending to know more than it does.

---

## Phase 5 — The first question: the moment Rimz earns its place

Claude hits a permission prompt — it wants to run something. A feed item is written to the ledger, the row flips to `? waiting`, rises to the top of its worktree, the cockpit make-up counts it (`? 1`), and a native notification fires.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 1   ! 0   ○ 0              ✽ 0   ⢿ 0   ✓ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%

           ␣ next ?!   ? for help
```

Even if the reader is in another pane or another app, the OS notification reaches them:

```
  ⬤ claude needs you · query-engine
    Permission — fix auth flow
```

**Does:** Selects the row (or clicks the notification, or hits the global triage
key from Phase 6), lands in Claude's pane, reads *the actual prompt* — the real command Claude wants to run — and approves or denies **in Claude's own UI**.

**Feels:** *This is the thing.* They were heads-down in another pane and Rimz
tapped them on the shoulder with exactly the right pane, one keystroke away. They never had to stop and ask "which of these terminals is blocked?"

**Thinks:** *"That's the whole pitch and it just worked. Now show me ten of
these."*

> **Design laws for the attention moment.**
> - **The sidebar notifies and navigates you to the question.** The row says *who* needs you and *what task* — and *is* the jump to that pane. You read and answer the real prompt in the agent's UI, where the full context and the safe defaults already live.
> - **A script's `feed ask` is the one item answerable in place:** it *chose* Rimz as its surface, so its declared options render right on the row.
> - **Notifications are best-effort polish, never truth.** Clicking one focuses the terminal (best-effort) and pre-selects that row, so even if the OS can't focus an exact mux pane, the sidebar already has it highlighted. The ledger, not the notification, is authoritative — a missed notification loses nothing.
> - **Coalesce, then escalate.** Three agents going `waiting` at once is one notification (*"3 agents need you · query-engine"*), not three. An agent that stays `waiting` past a threshold earns one nudge, not a stream.

### With a resolver in front (previewed)

Enrol a resolver later (Phase 10) and this same waiting row shows the chain *working* the item instead of asking you: the glyph becomes a braille spinner and the task slot reads the resolver and its remaining budget. It still counts in the `?` tally — the item is pending, just being handled — and returns to `? waiting` only if the chain comes up empty. The full story is Phase 10.

---

## Phase 6 — The fleet, and the one keystroke that tames it

The reader does exactly what they said they would: spins up four more agents across two worktrees, plus a deploy script paused at a gate. This is the load the product was built for, and it has to stay scannable.

```
 ⌘ query-engine

 ◎ 12           ◇ 76k ↘ 12k ↗ 64k ◍ 12k ◌ 68k
 ¤ 6                                    $4.20
 ────────────────────────────────────────────
 ? 2   ! 1   ○ 1              ✽ 1   ⢿ 1   ✓ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌? claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%
▏✽ claude · Sonnet · high · 200k
▏  add tests
▏  ▣ ━━━━━━────────────────────────────   18%
▏⢿ codex · GPT 5.5 · high
▏  refactor api
▏  ▣ ━━━━━━━━━━━━━━━━━━━━━─────────────   63%

 feature-migration                   +230 -23
 ! claude · Opus · xhigh · 1m
   db migrate
   ▣ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━─────   84%
 ○ codex · GPT 5.5 · low
   —
   ▣ ──────────────────────────────────    0%

 ┄ external ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ? 1
 ? deploy.sh
   Deploy staging?

           ␣ next ?!   ? for help
```

The cockpit make-up is the first thing the eye lands on: `? 2   ! 1` — two waiting, one failed, summed across *every* worktree, counting even rows hidden by a per-worktree cap. Ranking does the triage automatically: the most overdue `waiting`/`failed` rows rise (oldest first); calm agents settle below; each worktree caps its calm tail with a dim `+K more` but *never* hides a `waiting`/`failed` row.

**The power move — never hunt for the blocked pane again.** A single session-scoped keystroke (`␣` / "next ?!" in the footer) focuses the next item that needs attention, in ranking order, *without the reader ever focusing the sidebar*. Twelve agents, one key, straight to the oldest blocked one; press it again for the next.

**Does:** Glances at `? 2  ! 1`, hits the next-attention key twice to clear both
waiting items in their own panes, then jumps to the red `!` to read the failure.

**Feels:** In control of a fleet that would have been five flickering tab-bars a
day ago. The worktree grouping matches their mental model — `main` work here, `feature-migration` work there, the deploy gate down in `external`.

**Thinks:** *"I could run twenty of these. The bottleneck is me, and Rimz just
made me faster at being the bottleneck."*

> **Design laws for the fleet.**
> - **The cockpit make-up is the whole sidebar compressed to one line.** If you read nothing else, `? 2   ! 1` tells you whether to look; a row of zeros means nothing needs you. It never undercounts behind a cap.
> - **Ranking is the triage; you don't sort.** Attention-hungry buckets rise, oldest-first within them; the cap only ever trims the *calm* tail. The sidebar physically cannot bury something that needs you.
> - **A global "focus next attention" key is core, not a nicety.** Seeing the blocked pane and *getting* to it are different actions; the key collapses them so triage cost stays flat as the fleet grows. It's bound only inside the Rimz session, so it never touches the reader's global mux config.
> - **Worktrees are the structure, not tabs.** Groups are keyed on worktree isolation (only same-worktree agents share files); a bold header marks each one, and the worktree you've *selected* reads as one bracketed lane — a thin spine down its full height with a faint dotted seal capping its header, the selected card inside it bolder — so the lane is the only spine ink on screen. The `external` catch-all holds scripts, CI, and panes outside any worktree; it renders as a dim `┄ external ┄` divider and sorts last unless it holds something waiting or failed.
> - **The room scales past one repo.** `rimz start` in `~/code` — or on a headless box with no source control — makes that directory the room: each child repo is a pod with its own branch and churn, the root's own panes sit under a name-only header, and the same cockpit, ranking, and jump triage the whole machine ([the fleet room](./product.md#many-repos-one-room--the-fleet-room)).

### The `?` help overlay — discoverability without a manual

The footer advertises `?`. Pressing it overlays the legend and keys, so the glyph vocabulary is learnable in-place and the reader never has to leave the room to find out what `?` or `!` means.

```
 keys & legend
 ↑/↓ select   1-9 jump   ↵ jump
 ␣ next ?!   ←/→ provider tab
 x dismiss   r reload   ? close
 ⢿ working   ✽ thinking   ? waiting
 ! attention   ○ idle   ✓ done
```

> **Design law — color reinforces, shape carries.** Every status is legible under `NO_COLOR` and to color-blind readers because the *glyph shape* carries the meaning; color is a second, redundant channel. The legend shows both.

---

## Phase 7 — Many tabs, one room

The reader opens a new tab/window and starts a fifth agent there. Every tab is born with its own sidebar pane — but all of them render the *same room-wide snapshot*. The column is identical in every tab.

**Feels:** Coherent. There's no "which tab has the sidebar?" — every tab has the
same one, and selecting any row jumps to that agent's pane *wherever it lives* in the session.

**Thinks:** *"It's one room with one truth, not N independent panels. Good."*

> **Design law — tabs are viewports, worktrees are the subdivision.** Opening a fifth agent changes the roster, not the layout: it joins its worktree group in every tab's sidebar at once. The sidebar's own pane is chrome — it's excluded from the roster and self-closes when the last working pane in its tab exits, so a lone sidebar never lingers.

---

## Phase 8 — Detach, walk away, come back

The reader closes the laptop (the mux's own detach key — Zellij `Ctrl-O d`, tmux `prefix d`). The room keeps running headless on the host; the ledger keeps queuing events while nobody renders. Hours later they `ssh dev-box rimz attach query-engine` from a tablet on the train.

```
$ ssh dev-box rimz attach query-engine
   reconstructing query-engine from ledger…
```

The same reattach has a first-class form: `rimz attach --remote dev-box:query-engine` builds the guarded ssh for them and reattaches itself when the train wifi drops the link — and `rimz attach --remote dev-box:~/code/query-engine` starts the room if it isn't up yet.

The sidebar comes back exactly as they left it — every agent where it was, every question still waiting, ranked identically — plus whatever finished while they were gone, already triaged by the same ranking.

**Feels:** Relief, then trust. The thing they were promised — *"survives detach,
reattach from anywhere"* — is literally true, and the reattach was zero-cost.

**Thinks:** *"I can start a run on the dev box, close everything, and pick it up
on my phone at the airport. That changes how I work."*

> **Design laws for continuity.**
> - **The ledger is truth; the sidebar is a renderer over it.** Detach, sidebar crash, plugin reload, or no client at all never lose feed state. Reattach reconstructs from the ledger, never from screen-scraping.
> - **Reattach has no "loading" lie.** The first usable frame paints from the ledger immediately (a resize/attach is itself a wakeup); the reader never stares at a blank pane waiting for a tick.
> - **Reboot is the host's job, stated plainly.** The ledger survives a reboot; running processes need a host supervisor (systemd, tmux-resurrect, Zellij resurrect). Rimz says so rather than over-promising (see [DESIGN.md → Non-goals](../../DESIGN.md#non-goals)).

---

## Phase 9 — When something is wrong

The product's honesty law gets tested when a fetch fails — the binary moved, the ledger dir vanished mid-write, a snapshot is half-written. The reader must never mistake a *stale* frame for a *current* one.

```
 ⌘ query-engine

 ◎ 1
 ¤ 1
 ────────────────────────────────────────────
 ? 0   ! 0   ○ 0              ✽ 0   ⢿ 1   ✓ 0

▏main ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
▌⢿ claude · Opus · xhigh
▌  fix auth flow
▌  ▣ ━━━━━━━━━━━━━━────────────────────   41%


 ! Sidebar degraded for 8s: snapshot
 failed: ledger not found
```

The loop keeps the last good snapshot for the body but pins a sticky banner to the *bottom* edge — status-bar style, so the body truncates before the banner ever clips — explaining *why* the UI isn't updating and *for how long*. When a fetch finally succeeds the banner doesn't just vanish: it steps down to a dim `⚠ last alert 8s ago: … · x dismiss` notice so a failure that flickered past stays visible, and clears for good when the reader presses `x` (a fresh failure re-arms it). The first-run hint and footer are suppressed while the alert is active — because an empty body under a failed fetch is a missing snapshot, not an empty room.

The same honesty extends to trust and protocol: if this repo carries an untrusted `.rimz/config.toml`, its command-running fields stay inert until the reader runs `rimz trust grant` after reviewing the diff; if a sidebar's protocol version drifts after an upgrade, `rimz doctor` reports the mismatch rather than letting the rail silently stop updating.

**Feels:** Trusting *because* it admits fault. A tool that says "I'm degraded,
here's why, here's for how long" is more trustworthy than one that silently shows old data.

**Thinks:** *"It tells me when it doesn't know. I can rely on the green frames
because the broken ones are labeled."*

> **Design law — surface the failure, never the stale frame as if fresh.** A labeled stale frame is honest; an unlabeled one is a lie. Banners, the trust state, and `rimz doctor` are the three places Rimz tells you what it can't currently vouch for.

---

## Phase 10 — Growing up: resolvers, scripts, CI

By now the reader is hooked on the observe-and-route loop. The product grows with them along three paths they discover when they need them — each is an *addition* to the same feed, never a new mental model.

- **Resolvers (the morning-after upgrade).** Tired of approving `cargo check` for the eighth time, they enrol a resolver once — either one of the two that ship ready-made (`hook_bridge_resolver.py` for routine permissions, `pane_send_resolver.py` for well-known terminal prompts) or a small process of their own wrapping a smarter model: `rimz resolver add opus-policy --order 10 --budget 30s --binary …`. Now routine answers happen ahead of them; the hard ones abstain back to their pane exactly as before. The framing that keeps it safe: in Phases 1–8 they were *already* the answerer — the resolver just slots ahead of them, and **the chain always ends with them.** Deeper chains (Slack, PagerDuty) follow the same shape. Mechanics in [resolvers.md](../internals/resolvers.md).
- **Scripts as citizens.** A deploy or migration script posts to the same sidebar with `rimz event emit` and blocks on `rimz feed ask` — and *because the script chose Rimz as its surface*, its options are answerable straight from the column. No agent involved; same triage, same UX. This is the one case where the sidebar answers, by design.
- **Unattended / CI.** No human at the end of the chain: launch agents with their own bypass flag, or enrol a permissive resolver for a real per-decision audit trail. Detail in [product.md → Unattended runs](./product.md#unattended-runs-in-ci--sandbox).

> **Design law — one feed, three audiences, no new model.** Everything an agent integration does, a shell script does through the same CLI. The reader learns the feed once in Phase 5 and every later capability is the same feed seen from a new angle.

---

## Phase 11 — Leaving cleanly

The reader is done for the day. They detach (the room keeps running) or close their working panes (the sidebar self-closes behind the last one, leaving no orphan). If they decide Rimz isn't for them, `rimz hooks uninstall` removes exactly what the consent gate added — the additive diff in reverse — and their agents are back to untouched.

**Feels:** Respected. Backing out is as clean as opting in, and they were told so
on the very first screen.

**Thinks:** *"Clean in, clean out. I'll keep it."*

> **Design law — every install gesture has a named, equal-and-opposite uninstall, advertised at the moment of install.** A tool you can't cleanly remove is a tool you hesitate to try.

---

## The experience in one screen

| Phase | Reader does | Sees | Feels | The law it proves |
| --- | --- | --- | --- | --- |
| 0 Discovery | installs | 3-line pitch, 1 command | low-commitment | one line in, `rimz` to start |
| 1 Consent | runs `rimz` | additive-diff gate | reassured | report, don't answer; reversible |
| 2 Empty room | looks left | `○ zsh`, hint | oriented | never blank |
| 3 First agent | types `claude` | row re-skins to `○ claude` | delight | it just knows |
| 4 Working | prompts | `⢿ running`, animated head | calm | a wedged agent escalates to `!` |
| 5 Question | gets notified, jumps | `? waiting`, OS notify | *the pitch* | notify & route to the pane |
| 6 Fleet | hits "next ?!" | grouped roster, `? 2  ! 1` | in control | one key tames the fleet |
| 7 Tabs | opens a tab | same room everywhere | coherent | tabs are viewports |
| 8 Detach | closes laptop, ssh back | reconstructed column | relief, trust | ledger is truth |
| 9 Degraded | hits a failure | labeled banner | trust-via-honesty | never a stale frame as fresh |
| 10 Grows up | enrols a resolver | `⠙` chain on the row | leverage | one feed, three audiences |
| 11 Leaves | detaches / uninstalls | clean removal | respected | every install has an uninstall |

The arc: curiosity → reassurance → delight → *the pitch lands* → mastery → trust. If Phase 5 doesn't land inside five minutes, nothing after it matters; every earlier phase exists to get the reader there with their guard down and their agents already on screen.
