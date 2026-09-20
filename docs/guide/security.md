# Security and Trust

You already run agent CLIs that read your repository, write to it, and run commands as you. RimZ sits beside them at the same privilege: it runs under your user id, with no elevated privileges and no account of its own, and its durable state is a directory of plain files rather than a service. It cannot give an agent a permission you have not already given it.

The design commitments behind everything below are in [DESIGN.md](../../DESIGN.md).

## What RimZ changes on your machine

Four things, plus one file in a room running Copilot. The command that takes each one back is named with it, and the last paragraph of this section takes back the lot.

**Reporting hooks in your agent CLIs' own configs.** To show an agent live in the sidebar, RimZ adds a hook to that agent's own configuration: a line in `~/.claude/settings.json`, a block in Codex's `$CODEX_HOME/config.toml` (`~/.codex/config.toml` by default), and for Copilot, Amp, Pi, and OpenCode, whose extension point is a file rather than a config entry, one RimZ-owned integration file. Run `rimz hooks install --dry-run` for the exact per-agent diff before anything is written. The install is additive: your existing hooks stay, and what RimZ adds only reports events back to RimZ. Answering a prompt stays with you, in the agent's own UI. `rimz hooks uninstall` removes exactly what was added and restores any statusline RimZ wrapped. The whole flow is walked in [set up your machine](./setup.md#install-agent-hooks).

**Skill links in each provider's skill directory.** Unless the agent launches into the [sandbox view](#sandbox-isolation), RimZ links the skills in `~/.rimz/skills/` into that provider's own skill root, so one library reaches every agent. Your own directories and links are never replaced, a name collision is reported at launch, and `rimz uninstall` removes RimZ's links and keeps the library. The mechanics are in [configuration → skills](./configuration.md#skills).

**Files under `~/.rimz/`.** Configuration and durable room state, plain files you can read and edit, with `RIMZ_HOME` relocating all of it. One subtree deserves care: `accounts/` holds the provider homes `rimz accounts add` creates, each a full provider home with its credentials and transcripts. Treat it the way you treat `~/.ssh`, and keep it out of a dotfiles repository you publish. The layout is in [where RimZ keeps its files](./configuration.md#where-rimz-keeps-its-files).

**A permission grant for the Zellij presence plugin.** On Zellij, RimZ seeds a permission grant for the plugin it ships so your first attach is not interrupted by a prompt. Your `config.kdl` stays untouched, and the grant is yours to revoke. [The Zellij presence plugin](#the-zellij-presence-plugin) has it in full.

A room running Copilot gets one more file. RimZ points Copilot's own telemetry exporter at `agent-telemetry/copilot-otel.jsonl` under the room's runtime directory on tmpfs, with message-content capture switched off, and reads the model and token counts for that conversation back out of it. Closing the room removes it, and an exporter you configured yourself is left alone.

`rimz uninstall` removes RimZ from the machine. It prints every root, room, hook, skill-link root, and binary it is about to touch, waits for a `y`, and then reports each removal; `--state` and `--config` add the durable stores and the per-machine config to that list. Two things it never removes, whatever flags you pass: your provider account homes under `~/.rimz/accounts/` and your skill library under `~/.rimz/skills/`. Your credentials are in the first of those, so clearing them is yours to do by hand. The full table of what goes and what stays is in the [maintenance reference](../reference/cli/maintenance.md#uninstall-rimz).

## What can run a command

Inside a workspace, plenty already runs as you: hooks, postinstall scripts, generated binaries, test runners, and the agents themselves. Same-user isolation is no real boundary there, so RimZ does not lean on it. Instead it makes command execution an explicit choice, and only two things in your configuration can make that choice for you: a repository's config, which stays inert until you trust it, and your own notification handlers, which a repository cannot supply.

### Project trust

A cloned repository can ship a `.rimz/config.toml` that declares agents, profiles, teams, loop tasks, hooks, environment variables, and which provider account a fresh room launches into. Each of those can run a command or redirect your credentials, so RimZ keeps the whole file inert until you trust the workspace. On an untrusted clone nothing the file declares can launch; RimZ parses it only far enough to tell you what it is asking for.

A first interactive `rimz start` in an untrusted workspace offers the grant and names what the config declares. The interactive `rimz loop fire` and the project-task edit commands offer it inline when they meet the closed gate, with the field-level diff when a grant has gone stale. Declining leaves the workspace inert and suppresses only the first-start offer; the inline offers keep coming, and the first-start offer returns as soon as a command-running field in `.rimz/config.toml` changes.

`rimz trust grant` pins a single hash over every command-running field in that config. Every later read re-hashes the live file, so drift is caught at each read; `rimz trust status` and `rimz doctor` both re-hash on the spot.

Edits outside RimZ's own project-task commands, a direct file edit by an agent in the room included, make that hash drift. The workspace flips to `stale`, command execution turns back off, and `rimz trust status` prints a field-level diff of what changed before you re-grant. `rimz loop add`, `remove`, and `rename` are the exception: when they make the edit from an already-trusted workspace, or when they create the first config, they re-pin the grant themselves, whether you run them by hand, from a script, or from an agent. From an untrusted or stale workspace they leave the review to you.

`rimz trust revoke` drops the grant. The project config is inert again at the next read; panes already running keep what they launched with until they stop.

Room layout stays out of a repository's reach. A project config carrying a `[layout]` table is refused outright, with the fix to move it to your per-machine config.

The hash covers every field that can cause a process to run:

- `[[agents]]`: `name`, `launch_command`, `env`
- `[profiles.<name>]`: `agent`, `skills`, `mode`, `model`, `effort`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args`
- `[subagents.profiles.<name>]`: the same command-running profile fields, for supervised children
- `[agents.teams.<name>]`: `layout`, `consensus-file`, `append-system-prompt-files`, and each role's profile, signal bindings, and launch fields
- `[tasks.<name>]`: the loop `agent`, `prompt`, `prompt-file`, `check` and `verify` commands, `system-prompt-file`, and the run and schedule options
- `[[hooks]]`: `event`, `command`
- `[env]`: every key and value
- `[accounts]`: which provider account a fresh room launches into

Every name in those tables is hashed too, so renaming a profile drifts the grant. Any field RimZ can execute has to enter this hash, and a unit test guards the projection by hashing one config per field and failing when two of them collide. The mechanics, the grant record, and the diff are in [the trust internals](../internals/harness/trust.md); the command is in [the trust reference](../reference/cli/hooks-trust.md#project-trust).

Per-machine loop schedules are a separate matter. A `check = "<shell>"` line in your own `~/.rimz/loop.toml` is your command, not a repository's. A clone can supply only project `[tasks]`, and those need both a trust grant for the config and a machine-local `rimz loop enable <name>` before they run unattended. Until trust, a same-named machine task remains the runnable definition; after trust, the project definition is visible but disabled here by default.

### Notification handlers

Notification handlers run a command of your choosing when a card needs attention (`[[notifications.handler]]`, or the legacy `[notifications].command`). They live only in your per-machine `~/.rimz/config.toml`, so a clone can never supply one, and they sit outside the trust hash for the same reason. They run under your user id, spawned by the sidebar process, and often carry local push credentials. A handler that acts back on the room should treat pane text and transcripts as untrusted data: match a bounded prompt shape, and stay silent on anything else. Wiring is in [the notifications internals](../internals/sidebar/notifications.md).

### An unattended run

An unattended run changes who is watching, not what can execute. The run still decides permissions the agent's own way: either you keep every native prompt, so each one becomes a waiting card that routes to you, or you pass the agent's own bypass flag. The tradeoffs, and what an out-of-range agent version does to the prompt path, are in [permissions for an unattended run](./loops.md#permissions-for-an-unattended-run).

## Sandbox isolation

An agent pane runs with everything your user account has: your whole home directory, the host's `/tmp` shared with every other process on the machine, and whatever skills the provider discovers for itself. For a narrower view on Linux, `rimz config set agents.isolation sandbox` gives each agent pane a bubblewrap mount view.

Inside the view the agent sees the room's own `/tmp` instead of the host's, and the RimZ skill library merged into its provider's skill root. When the profile carries a [skills list](./configuration.md#skills), only the skills you named stay callable by the model; the rest become skills you invoke by name. RimZ probes bubblewrap before writing the setting, and `rimz start` refuses rather than launching a degraded surface when bubblewrap is missing or unusable.

This is a mount view, not containment. What it does not do:

- The host filesystem stays writable and your credentials stay readable.
- Processes, networking, and IPC are shared with the rest of the machine.
- Overlaid skills are bound read-only at their discovery paths, but their sources on the host are unchanged and may still be reachable by another path.
- Neither mode hides an agent's command line from a same-user `ps`. RimZ passes a composed system prompt by private file for that reason. Pi is the exception: its replacement surface is the `--system-prompt` flag, so a Pi prompt rides the command line, capped at 120 KiB.

Bubblewrap does add one protection: it turns off privilege escalation, so `sudo` and setuid programs cannot elevate inside these panes. Use a host shell for that work.

It also switches one off, deliberately. A provider's own command sandbox would sit on top of the mount view and stop builds from writing caches or reaching a local server the way the pane can, so RimZ disables it: a Codex agent runs with `--sandbox danger-full-access` inside the view. Codex's approval settings keep their values and become the only check on a command, since no write outside the workspace and no network call is now blocked for leaving a sandbox. Host mode keeps Codex's own sandbox.

The `/tmp` the view mounts is one directory for the whole room, so what one agent writes there its siblings can read. It is the room's own directory rather than the host's, and closing the room removes it; `rimz paths` prints where it sits ([reference](../reference/cli/paths.md)).

To stop using the view, run `rimz config set agents.isolation host` and restart the agents that follow machine policy. An agent launched with an explicit `--isolation sandbox` keeps its own view until you replace it: stop it, then [`rimz agents <spec> --resume --isolation host`](../reference/cli/agents.md#resume-a-cohort) keeps its conversation and drops the override for later relaunches and children. Profile `skills` lists can stay where they are, since host mode ignores them.

## The Zellij presence plugin

On Zellij, RimZ loads a small presence plugin into each session so the sidebar learns pane layout by push, and a tab switch lands back on your work instead of the sidebar ([internals](../internals/multiplexers.md#the-zellij-presence-plugin)). Zellij normally prompts before a plugin gets permissions; RimZ seeds that grant ahead of load, keyed to the exact plugin path it materializes, so the first attach is not interrupted. The grant is four permissions:

- **Access Zellij state.** The plugin watches pane and tab shape.
- **Run commands.** It runs the RimZ-owned `rimz sidebar wake`, `rimz sidebar focus`, and `rimz pane zoom` calls, and nothing else.
- **Reconfigure.** It applies the room's mouse options and binds the configured [room keys](./configuration.md#sidebar-rendering), both at runtime, without writing your `config.kdl`.
- **Change application state.** It applies a host-selected pane's mechanical fullscreen toggle; sidebar-aware target selection stays in the host command.

The vendored plugin is reproducibly built from the checked-in source on the repository's pinned Rust toolchain. Its source digest, wasm digest, and producing toolchain are committed beside it; every vendored embed verifies the wasm checksum, and CI rebuilds the source with that toolchain and requires byte-for-byte equality.

The plugin's code, argv, and configuration are all RimZ-owned, never your `config.kdl`, and it ships no pane content anywhere. The grant lives in Zellij's own permission store, where its plugin manager can revoke it. Revoking stops pane discovery, and `rimz doctor` names the fix while discovery is down. It does not stay revoked: the next time RimZ opens or reattaches its session, it seeds the grant again.

## Browser listener

Browser access defaults to a loopback ttyd listener with one machine-wide Basic-Auth credential. That credential authenticates the shared daemon rather than a room, so whoever holds it reaches every live RimZ room on the machine and has a shell as the serving user. Handle it like a machine-wide SSH key.

`[web] auth_header` delegates the public decision to an authenticating reverse proxy, and RimZ's gate stands in front of ttyd. Every request that reaches the gate passes four checks:

1. The source address is loopback, or inside a `[web] trusted_proxies` CIDR. Anything else is dropped without a reply; an empty list leaves only loopback.
2. The named identity header is present exactly once and is not empty. Zero copies, two copies, or an empty value are refused.
3. When `[web] auth_users` is set, the header's trimmed value matches an entry exactly and case-sensitively.
4. Any `Authorization` header the client sent is stripped, and the machine's own Basic credential is injected before the request is forwarded.

The identity provider's application policy remains the primary access control; `auth_users` is defense in depth, using canonical username spellings. On its side, the proxy has to strip client-supplied copies of the identity header, inject its own value after authentication, and remain the only non-loopback source that can reach the gate. Pair the CIDRs with the host firewall, and account for the source address that container or host networking produces. Loopback bypasses only the source-address check: a trusted-header request from loopback still needs the header, and the private ttyd listener still needs Basic Auth. On a multi-user host, host-level user isolation is what keeps another local user from reading the serving user's credential or executing as that user.

The [web guide](./web.md#behind-a-reverse-proxy) configures Traefik and Authentik for this path.

## What leaves your machine

RimZ keeps your work local. Prompts, transcripts, pane text, and credentials stay on the box, and nothing is uploaded on a schedule. These are the calls it makes, each reusing a login you already hold or a host you named:

- **Provider usage.** To fill the cost and budget meters, RimZ reads each provider's usage endpoint with the OAuth login the agent already holds. Usage endpoints stay pinned to each provider's official host; a URL override is refused unless it targets that host or loopback.
- **Pull-request status.** The sidebar's PR marker runs your own `gh` (GitHub) or `tea` (Gitea, Forgejo, Codeberg) against your forge, on your existing login. It reads no RimZ secrets and adds no config field, so it stays off the trust hash.
- **Pets.** Enabling `[theme.pets]` fetches a sprite sheet over HTTPS from the host you name; a built-in pet reaches the public Codex pets CDN, and a local-path pet fetches nothing. `RIMZ_PETS_OFFLINE`, set to any value, makes the process tree cache-only. The request carries the asset URL and nothing else.
- **Release downloads.** `rimz update` asks GitHub for the latest tag, then fetches the archive and its `SHA256SUMS` and verifies the checksum before replacing the binary. It runs only when you run it; nothing checks for a release in the background.
- **Off-box error reporting.** Off by default and opt-in. See [Off-box error reporting](#off-box-error-reporting).

`rimz remote` and browser access connect where you point them. What rides an SSH link is in [remote](./remote.md), and the browser listener's boundary is the section above.

Hook payloads are the most sensitive thing RimZ handles, because they can carry prompts, tool inputs, file paths, command arguments, and errors. They land in RimZ's local state and nothing forwards them off the box, error reporting included.

### Off-box error reporting

Reporting is a non-default Cargo feature: code compiled without `--features sentry` contains no Sentry calls whatever the config says, which is how a stock install is built. Even compiled in, it stays off until you set `[sentry] dsn` (or `RIMZ_SENTRY_DSN`). The DSN is per-machine and never the committed project config, so a clone or a pull cannot redirect a contributor's telemetry.

When on, it sends RimZ's own `warn!` and `error!` events and the provider conditions RimZ observes (rate limits, overload), tagged with low-cardinality facts: the release and build id, the running command, a fault class, and the agent or session id when known. The hostname is stripped, Sentry's default personal-data collection is off, and hook payloads, prompts, and transcripts never ride along. An event's own message text does travel, so a warning that names a file path carries that path; a failed account-usage probe, for instance, reports the request's host and never its path or query. The full payload and the config knobs are in [the diagnostics internals](../internals/diagnostics.md#off-box-error-reporting) and [configuration.md](./configuration.md#off-box-error-reporting).

## Pane text is data, not instructions

`rimz pane capture` returns raw terminal text, exactly as the pane's program drew it. RimZ itself reads a pane in three places and no others: drawing the sidebar, an explicit `pane capture`, and confirming that a Codex turn has ended. None of them feeds the text back to an agent as instructions.

RimZ types into a pane only text you gave it: an answer you send with `rimz answer`, a message you send with `rimz message`, and the continuation line [auto-continue](./loops.md#auto-continue) sends when you switch it on.

If you script an answer through the pane primitives, match a bounded prompt shape and abstain when unsure. Captured text is agent output and can say anything at all, so feeding it into an LLM prompt as if it were a user message is the classic prompt-injection footgun. Keep it as data.

## Reporting a vulnerability

Report privately through GitHub Security Advisories on `github.com/rimio-ai/rimz`, or by email to `security@rimio.ai` if advisories are unavailable. Include the affected versions, the reproduction, the impact, and any mitigation you know of. Maintainers acknowledge reports privately before public issue tracking ([SECURITY.md](../../SECURITY.md)).
