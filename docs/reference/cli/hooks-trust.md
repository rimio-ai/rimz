# Hooks and trust

`rimz hooks` writes RimZ's reporting hooks into your coding agents' own config files and takes them out again. `rimz trust` decides whether a repository's `.rimz/config.toml` may run commands on this machine. The threat model behind both is in [security and trust](../../guide/security.md), and the prompts `rimz start` asks about each are tabulated under [prompts at room birth](./getting-started.md#prompts-at-room-birth).

## Agent hooks

```sh
rimz hooks install [--dry-run] [AGENT]
rimz hooks uninstall [AGENT]
```

A hook is an entry in the agent's config that runs `rimz hooks feed` on a lifecycle event, which is how the sidebar learns an agent's state and sees its blocking prompts. Hooks report and never answer: on a blocking ask the hook returns the agent's neutral no-op, and the prompt stays in the agent's own UI ([the hook path](../../internals/agents/adapter.md#the-hook-path)). `rimz hooks feed` is hidden from `--help`, because only installed hooks call it.

### Install hooks

`rimz hooks install` with no `AGENT` installs hooks for every agent that has an installer and whose binary RimZ finds, on `PATH` or in the agent's usual install directory under your home (`~/.opencode/bin`, `~/.kimi-code/bin`, `~/.local/bin`). With `AGENT` it installs that kind whether or not its binary is present. Install writes into the provider home your environment selects (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME`, and the others in the table below); a named [provider account](./accounts.md#add)'s home gets its hooks from `rimz accounts add`.

Rerunning install is safe. RimZ reclaims its own entries and writes the current set again, so a hook block you disturbed by hand is restored.

```console
$ rimz hooks install claude
✓ claude  installed 13 hooks → ~/.claude/settings.json  (updated existing config)

  undo     rimz hooks uninstall
  preview  rimz hooks install --dry-run
All set — your agents appear in the sidebar as they run.
```

The summary after the agent name says what install found:

| Summary | Meaning |
| --- | --- |
| `installed N hooks` | The agent had no RimZ hooks. |
| `refreshed N hooks` | A RimZ-owned Pi or OpenCode file differed from the copy in the running build and was replaced. |
| `hooks up to date (N)` | RimZ hooks were already installed. The row carries no file annotation. |

The annotation is `(new file)` when install created the file and `(updated existing config)` when it edited one. An agent with two files prints the second as an indented `config → PATH` line.

Codex runs a new hook only after you trust it inside Codex. When its installed hooks are still untrusted, install and `rimz start` print `codex hooks are installed but untrusted (…) — codex silently skips them; run /hooks inside codex and trust the RimZ hooks`, and the `HOOKS` row of `rimz doctor` names the same fix.

### What install writes

| Agent | Files | Edit |
| --- | --- | --- |
| `claude` | `${CLAUDE_CONFIG_DIR:-~/.claude}/settings.json` | Merge; wraps the statusline |
| `codex` | `${CODEX_HOME:-~/.codex}/config.toml` | Merge |
| `amp` | `${XDG_CONFIG_HOME:-~/.config}/amp/plugins/rimz.ts` | Whole file |
| `antigravity` | `~/.gemini/config/hooks.json`, `~/.gemini/antigravity-cli/settings.json` | Merge; wraps the statusline |
| `copilot` | `${COPILOT_HOME:-~/.copilot}/hooks/rimz.json`, `${COPILOT_HOME:-~/.copilot}/settings.json` | Whole hook file; wraps the statusline |
| `cursor` | `~/.cursor/hooks.json`, `cli-config.json` in Cursor's config home (see [config homes](../agent-support.md#config-homes-and-skills)); with `XDG_CONFIG_HOME` unset, both `~/.cursor` and `~/.config/cursor`, where Cursor looks inside a tmux room | Merge of both files as one write; wraps the statusline |
| `droid` | `~/.factory/settings.json` | Merge |
| `grok` | `${GROK_HOME:-~/.grok}/hooks/rimz.json` | Merge |
| `kimi` | `${KIMI_CODE_HOME:-~/.kimi-code}/config.toml` | Merge into `[[hooks]]` |
| `kiro` | `~/.kiro/hooks/rimz.json` | Whole file; needs Kiro CLI 2.13.0 or later |
| `opencode` | `${XDG_CONFIG_HOME:-~/.config}/opencode/plugin/rimz.ts` | Whole file |
| `pi` | `~/.pi/agent/extensions/rimz.ts` | Whole file |
| `qwen` | `${QWEN_HOME:-~/.qwen}/settings.json` | Merge; wraps the statusline |

Kiro CLI runs `~/.kiro/hooks/*.json` in every workspace from 2.13.0; below that they fire only in the home directory, so install refuses with `Kiro CLI <found> runs ~/.kiro/hooks only in the home workspace; upgrade to 2.13.0 or later`, and an install with no `AGENT` skips Kiro silently. Kiro's engine resolves `~/.kiro` from the OS home and ignores `KIRO_HOME`, which moves only the launcher's settings. An unmarked hook file at that path whose every hook runs RimZ's feed command is reclaimed rather than refused. [Plugin agents](../agent-plugins.md) have no installer.

A merge adds RimZ's entries and keeps your own hooks and every other value. Rewriting a JSON settings file can reorder its keys, so a dry-run diff may show more lines than the hook entries.

A whole file belongs to RimZ, and its first line carries the `_rimz_managed` marker. Install replaces a marked file with the copy embedded in the running build, and refuses a file at that path without the marker. Pi and OpenCode report `refreshed` when the marked file differs from the build's copy, and `rimz start` offers that refresh in the same prompt as missing hooks; Amp and Copilot are rewritten the same way but report `hooks up to date`.

Where the table says the statusline is wrapped, install puts RimZ's command in place of yours and runs yours inside it, so the sidebar reads live context and your statusline still shows. Uninstall restores the original value. Cursor keeps the displaced `statusLine` value in `~/.rimz/cursor-statusline.json` and deletes that file on uninstall. A Cursor install writes the single `~/.cursor/hooks.json` once per config home it resolves, pairing it each time with that home's `cli-config.json`, and commits each pass on its own: a failed write rolls back the pass it was in, while earlier passes and the statusline record stay. Fix the cause and rerun the install.

### Preview with `--dry-run`

`--dry-run` writes nothing. For each agent it prints the row the `rimz start` consent prompt shows (hook count, path, `new file` or `updates existing config`, and a statusline note when install wraps one), then a unified diff per file, all on stdout:

```console
$ rimz hooks install --dry-run grok
  grok  12 hooks → ~/.grok/hooks/rimz.json  updates existing config
    --- /home/me/.grok/hooks/rimz.json
    +++ /home/me/.grok/hooks/rimz.json
    @@ no changes @@
```

A file install would create diffs from `/dev/null` under an `@@ new file @@` header. RimZ builds every preview before printing any, so when one agent fails (an unmarked whole file, or a Kiro CLI below 2.13.0), the dry run prints only the error and exits 1.

### Uninstall hooks

`rimz hooks uninstall` removes RimZ's entries, deletes the whole files that carry the marker, and restores any statusline it wrapped. An unmarked file at a whole-file path is yours and stays.

With no `AGENT`, uninstall checks every agent's provider home and the home of every account declared in the machine config, and unhooks each one that holds RimZ hooks. It does not need any agent binary to be installed. With `AGENT` it unhooks that kind's provider home only.

```console
$ rimz hooks uninstall claude
✓ claude  removed 13 hooks → ~/.claude/settings.json
```

A named agent with no RimZ hooks prints `codex — no RimZ-managed hooks found`. With no `AGENT` and nothing installed anywhere, uninstall prints `No RimZ-managed hooks are installed; nothing to uninstall.` [`rimz uninstall`](./maintenance.md#uninstall-rimz) runs the same removal as part of removing RimZ.

### Failures

Both commands exit 0 on success and 1 on any of these:

| Situation | Error message |
| --- | --- |
| `install` with no `AGENT` finds no agent | `no supported coding agents detected on PATH (KINDS) - install an agent and rerun, or name one: rimz hooks install <agent>` |
| `AGENT` is not a known kind | ``unknown agent integration `NAME` `` |
| The named kind cannot take RimZ hooks (a plugin agent, or a Kiro CLI below 2.13.0) | `install failed for AGENT: REASON` |
| A whole-file path holds a file without the marker | `refusing to overwrite an unmarked user plugin at PATH; move it aside or remove it to let RimZ manage this file` (`extension` for Pi, `hook file` for Copilot) |
| `uninstall` with no `AGENT` finds the machine's accounts config invalid | `only the providers' own homes were unhooked; fix the accounts config and rerun`, after unhooking those homes |

An install without `AGENT` handles agents one at a time and stops at the first failure; agents already installed keep their hooks.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

A repository's `.rimz/config.toml` can declare agents, profiles, teams, loop tasks, env, and accounts, and each of those can run a command. RimZ hashes every command-running field into the executable surface and applies the project config only while this machine holds a grant for that exact hash. The list of hashed fields is in [security and trust](../../guide/security.md#project-trust). Trust is per project root, as resolved from the current directory or [`--root`](../cli.md#global-flags).

| Verb | What it does |
| --- | --- |
| `status` (the default) | Reads only. Re-hashes the live config and prints the state. |
| `grant` | Pins the current hash and surface on this machine, and clears a declined `rimz start` trust prompt. When it replaces a grant for a different hash, it prints the surface diff. With no project config it writes nothing and prints `no project config`. |
| `revoke` | Deletes the grant, leaving the project `untrusted` (or `no project config`). |

All three exit 0 whatever the state, and 1 when the config or the grant record cannot be read or parsed. `--json` works on each verb.

### Trust states

Every read re-hashes the config, so an edit shows on the next command with no background sweep. The human line and the `--json` `state` spell the states differently:

| Printed | `state` | Meaning |
| --- | --- | --- |
| `no project config` | `no_config` | No `.rimz/config.toml` at the project root; nothing to gate. |
| `untrusted` | `untrusted` | Config present and no grant on this machine. Project config does not apply. |
| `trusted` | `trusted` | The grant matches the live hash. Project config applies. |
| `stale — executable surface changed since last grant` | `stale` | A hashed field changed since the grant. Treated as `untrusted` until you grant again. |

A trust-gated command refused in an `untrusted` or `stale` project names the fix: review with `rimz trust`, then approve with `rimz trust grant`.

### Status output

A `stale` report ends with the surface diff: one line per changed field, `+` added, `-` removed, `~` changed, with the granted value under `-` and the live value under `+`. Array items are numbered, so `tasks[0]` is the first task in name order.

```console
$ rimz trust
trust: stale — executable surface changed since last grant
  workspace id: ws_f89e49906df0621ad2765112
  project root: /home/me/code/query-engine
  config path:  /home/me/code/query-engine/.rimz/config.toml
  record path:  /home/me/.rimz/trust/ws_f89e49906df0621ad2765112/trust.toml
  current hash: sha256:5c1d…
  granted hash: sha256:979e…
  granted at:   2026-09-08T09:21:53.057781717Z
  surface diff:
    ~ tasks[0].check
      - cargo test
      + cargo test && ./deploy.sh
```

The hash and grant lines appear only when they have a value. `--json` prints one object with every key present:

| Field | Value |
| --- | --- |
| `state` | `no_config`, `untrusted`, `trusted`, or `stale` |
| `workspace_id` | The workspace id |
| `project_root`, `config_path`, `record_path` | Absolute paths; `record_path` is the grant file under `~/.rimz/trust/` |
| `current_hash` | `sha256:<hex>` of the live surface, or `null` with no config |
| `granted_hash`, `granted_at` | The grant's hash and RFC 3339 time, or `null` with no grant |
| `surface_diff` | `null`, or an array of `{kind, path, granted, current}` where `kind` is `added`, `removed`, or `changed`, `path` is an array of segments, and `granted` or `current` is omitted when that side has no value |

`surface_diff` is set on `status` for a `stale` project, and on `grant` when the grant replaced one with a different hash.

### Other places RimZ offers a grant

Three other commands ask for a grant or re-pin one:

| Command | What it does about trust |
| --- | --- |
| `rimz start` that creates the room, from a terminal | Offers a grant, default no, when the project has config and no grant. A `stale` project gets no offer. Declining records the current hash, and RimZ asks again only after a hashed field changes. The rows are in [prompts at room birth](./getting-started.md#prompts-at-room-birth). |
| `rimz loop fire` of a project task, from a terminal | When the project is `untrusted` or `stale`, prints the state (and the diff when `stale`) and asks `grant trust and fire?`. Without a terminal it refuses. |
| `rimz loop add --project`, `remove`, and `rename` of a project task | From a `trusted` project or one with no config, re-pins the grant for their own edit without asking. Otherwise asks `grant trust now?` from a terminal, or prints a `rimz trust grant` hint. See [loop](./loop.md). |

How the grant record, the decline record, and launch-time enforcement work is in [project trust internals](../../internals/harness/trust.md).
