# MiniMax Code adapter

MiniMax Code (`minimax`, alias `mcode`) is a hooks-only integration. Root session, turn, tool, approval, and compaction events reach RimZ; transcripts, context gauges, spend, account probes, and native child cards do not. The shipped marks and launch tables live in [agent support](../../reference/agent-support.md); upstream evidence and release boundaries live in [minimax-reference.md](../../externals/agent-adapter/minimax-reference.md).

## Code map

| Anchor | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/minimax/mod.rs::MINIMAX_SPEC` | Identity, coverage, tool classes, and launch argv. |
| `crates/rimz/src/agents/adapters/minimax/mod.rs::MiniMaxAdapter` | Data-directory resolution, interactive-process recognition, and native hook decoding. |
| `crates/rimz/src/agents/adapters/minimax/install.rs::MiniMaxIntegration` | Install, preview, detection, byte-drift upgrade, and whole-package removal. |
| `crates/rimz/src/agents/definition.rs::hook_owner_env` | Verified owner-environment recovery for scrubbed hook runners. |

## Home and installation

The data directory is the first trimmed, nonempty `MINIMAX_DATA_DIR`, then `MAVIS_DATA_DIR`, then `$HOME/.minimax`. Named RimZ account homes use `MINIMAX_DATA_DIR`, relocating credentials, sessions, configuration, and plugins together. Setting an explicit data-directory variable also disables mcode's Git auto-configuration. Native profile directories such as `~/.minimax-<profile>` are outside the verified launch surface. No skill root or host skill switch is declared; scoped profile skill lists are refused, and host lists warn and run unrestricted.

`rimz hooks install minimax` owns `<dataDir>/plugins/rimz` with three real files:

- `icon.png`: an embedded raster icon required by the native manifest.
- `hooks/rimz.json`: the nine native event handlers below.
- `.minimax-plugin/plugin.json`: the `rimz` manifest, published last after its dependencies.

The manifest uses schema version 1, version `1.0.0`, author `RimZ`, category `Other`, the icon and hook paths, a description, and empty `exampleQueries`, `apps`, `mcpServers`, and `skills` arrays. It stays within the pinned source's strict field set even though installed 0.6.12 accepts some unknown fields. The native package is discovered as `rimz@local`, enabled by default, without an enable step. Upstream watches local packages for changes; partial writes retain the last-good loaded package.

Install rejects symlinked package directories and a manifest not bearing name `rimz` and author `RimZ`. Without a manifest, install and uninstall refuse directories containing anything beyond RimZ's own file and directory paths; an interrupted install containing only those paths can resume. A second identical install writes nothing. `installed()` requires all three regular files to match the embedded bytes; drift offers an upgrade. Preview shows the two JSON files, not binary icon bytes. Uninstall removes only the owned `plugins/rimz` directory, leaving sibling plugins and user configuration intact.

RimZ does not read the private SQLite disabled-plugin table or implement `hooks_disabled` for MiniMax. Byte-correct files can therefore report installed while mcode has disabled the plugin. Run `mcode plugin enable rimz@local` in the same data-directory environment to restore it; reinstalling does not change that private setting.

## Hook process and replies

Every handler runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source minimax`. Timeouts are 10 seconds except SessionEnd's 2 seconds, below the upstream 15-second event and 3-second teardown budgets. Replies are byte-empty stdout and exit 0; RimZ emits no permission decision, blocking response, or deadline-context reply.

The native hook runner strips `RIMZ_*`, `XDG_RUNTIME_DIR`, and mux pane variables. Recovering only frame identity would still resolve the feed's home and runtime sockets outside the room. MiniMax declares `hook_inherits_env = false`: on Unix, the feed re-executes once under the verified owner's whole environment before reading stdin or resolving RimZ paths. The owner is `RIMZ_AGENT_PID`, stamped from the hook shell's parent, the in-process TUI according to upstream source. Its process name is checked before and after reading its environment. The invocation retains its owner pid and adds a restoration marker. Other, inheriting adapters do not re-execute.

An unverifiable owner, unreadable environment, or failed exec emits a diagnostic and continues with the scrubbed environment; the hook can then land outside the intended room. Declared process names are `minimax-code` and the shared launcher `node`. Linux process measurements on 0.6.12 saw the generic `node-MainThread` during startup and `minimax-code` afterward; the generic startup name is not declared, since it would classify unrelated Node commands as MiniMax. Startup detection uses the `mcode` command line instead. Command recognition excludes noninteractive subcommands, including `exec` and `acp`.

## Hooks and lifecycle

`crates/rimz/src/agents/adapters/minimax/mod.rs::MiniMaxAdapter::decode_hook` drops every payload carrying `agent_id` before extracting session identity. Native children supply their own `mvs_` session but no parent session id. Accepting those events would register a child as a root in the parent's pane. SubagentStart and SubagentStop are not installed; child tool events and approvals are dropped too. The parent's `task` tool activity is the only child-work indication.

| Native event | Lifecycle signal | Boundary |
| --- | --- | --- |
| `SessionStart` | `Registered`, or `CompactionEnded` for source `compact` | Repeated plugin activation is idempotent; startup, resume, clear, and fork use registration. |
| `UserPromptSubmit` | `TurnStarted` | Carries the sanitized prompt and task. |
| `PreToolUse` | `ToolUsed` | Activity only, without counting a completed tool or edit. |
| `PostToolUse` | `ToolUsed` | Counts the tool; `bash`, `write`, `edit`, and `task` mutate, while `write` and `edit` edit files. |
| `PermissionRequest` | `AwaitingInput(Permission)` | Root approvals only; answer in the pane, not through `rimz answer`. |
| `Stop` | `TurnEnded` | Successful boundary; optional `last_assistant_message` supplies supervised output. |
| `PreCompact` | `Compacting` | No context gauge or summary projection. |
| `PostCompact` | `CompactionEnded` | `trigger` distinguishes automatic compaction. |
| `SessionEnd` | `Ended` | Resume and clear close the old session before the new start. |

The next tool or settle signal clears a permission wait. Native questions and plan approvals have no verified hook. Automatic compaction sends PostCompact then SessionStart(`compact`); duplicate compaction ends are harmless under the lifecycle fold. Mux death is derived from the shared exec wrapper, not a native hook.

Stop is upstream's close attempt, not a guaranteed final result: hook continuations can resume work. Failed and cancelled turns skip Stop entirely, leaving the card working until another prompt or SessionEnd; supervised runs can wait out their deadline. RimZ does not reconcile private turn-ingress tables, background parking, or idle notifications.

## Launch and resume

RimZ launches `mcode -- <prompt>` and resumes recorded root sessions with `mcode --session <mvs_id>`. Bare provider resume uses `mcode --session`; importing unrecorded sessions and forking are unsupported. The model is passed with `--model <provider/model[#variant]>`; no effort, fast-mode, interrupt-key, or native auto-compaction-window switch is declared. Manual compact uses `/compact` with an optional trailing instruction.

Launch reminders use `--append-system-prompt`. Its inline and `--append-system-prompt-file` forms share one slot, so the launcher reads file contents before merging reminders and emits one inline argument. Typed system-prompt replacement is unsupported; native replacement flags remain available through raw args.

All permission-mode argument vectors are empty. The TUI has no `--permission` or `--config` switch, and RimZ never edits global `config.yaml` to impose a per-launch mode. Select `permissionMode` in `<dataDir>/config.yaml` (default `auto`), use `/permission`, or press Alt+M. Plan Mode is separate and has no verified launch flag.

The separator admits a leading-dash prompt: logged-out 0.6.12 reached the TUI with `mcode -- -probe`, while raw `mcode -probe` failed. It does not escape Commander subcommands: `exec` still dispatches headless after `--`. Prompts equal to `init`, `exec`, `acp`, `login`, `logout`, `update`, `telemetry`, `provider`, `plugin`, or `help`, and retired `git`, `changes`, or `projects`, have no verified working positional form. Authenticated submission, other boundary forms, and native resume eligibility remain unverified.

## Context, history, cost, and account

Only the hook's live model is projected. Hook `transcript_path` points to a temporary projection that can be deleted or null, not a durable history source. Canonical messages require private SQLite lookup and an unverified nested schema. There is no transcript discovery, history parser, context refresh, token gauge, spend parser, account probe, or quota window.

RimZ declares no default model: upstream's `minimax/MiniMax-M3.1-Flash-Preview` has no RimZ price, and a declared default must pass pricing coverage. Nullable native costs and zero-filled aggregates do not establish dollar semantics or account isolation. No dollars are inferred.

## Verification boundary

Adapter tests cover child drops, silent replies, malformed payloads, lifecycle through the real fold, and permission open/settle/close. Integration fixtures prove native package ownership, idempotence and drift, data-directory precedence, and scrubbed-environment hooks landing in the room, binding the pane, and settling a supervised run.

The isolated 0.6.12 live check proved package discovery and `rimz@local` enabled status, not native hook execution. Logged-out mcode fires no hooks. Authenticated turns, actual hook-parent identity, hot reload, child sessions, compaction, and account semantics remain source-backed or unverified as recorded in the [upstream reference](../../externals/agent-adapter/minimax-reference.md#remaining-verification).
