# MiniMax Code adapter

MiniMax Code (`minimax`, alias `mcode`) integrates through native plugin hooks alone. Root-session lifecycle, turns, tool calls, tool approvals, and compaction reach RimZ. Transcripts, context gauges, spend, account probes, and child-agent cards do not. It is the one built-in whose hook runner scrubs the launch environment, so its hooks restore that environment before they resolve any RimZ path ([Hook command and owner environment](#hook-command-and-owner-environment)). The user-facing marks and launch tables live in [agent support](../../reference/agent-support.md), and upstream evidence with its release boundaries lives in [minimax-reference.md](../../externals/agent-adapter/minimax-reference.md).

## Code map

| Anchor | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/minimax/mod.rs::MINIMAX_SPEC` | Identity, process names, coverage, tool classes, and launch argv. |
| `crates/rimz/src/agents/adapters/minimax/mod.rs::MiniMaxAdapter` | Data-directory resolution, interactive-command recognition, the system-text channel, and hook decoding. |
| `crates/rimz/src/agents/adapters/minimax/mod.rs::HOOKS` | The nine installed events, their timeouts, and the conformance corpus. |
| `crates/rimz/src/agents/adapters/minimax/install.rs::MiniMaxIntegration` | Package install, preview, detection, upgrade, and removal. |
| `crates/rimz/src/agents/definition.rs::hook_owner_env` | Owner verification and the restored environment for a non-inheriting hook. |
| `crates/rimz/src/cli/hooks.rs::HooksArgs::restore_owner_env` | The re-exec under the restored environment, reached from `crates/rimz/src/cli/mod.rs::restore_hook_environment` before CLI startup. |

## Data directory

The data directory is the account home, and three environment variables bear on it, each for a different consumer:

| Variable | Declared in | Role |
| --- | --- | --- |
| `MINIMAX_DATA_DIR` | `MiniMaxAdapter::config_home`, `config_home_env_keys` | First choice when trimmed and nonempty. The only key a named RimZ account home sets, so credentials, sessions, configuration, and plugins move together. |
| `MAVIS_DATA_DIR` | `config_home`, `hook_env_keys` | Upstream's legacy variable and the second choice. Listed in `hook_env_keys` so a captured hook frame carries it to the drainer. |
| `HOME` | `config_home` | Fallback `$HOME/.minimax`. Install fails with `$HOME is not set` when neither variable nor `HOME` resolves. |

Upstream's `~/.minimax-<profile>` directories are outside the verified launch surface, and RimZ never selects one. MiniMax declares no skill root (`skills_home` returns `None`) and no host skill switch (`HostSkills::Unsupported`), so a profile `skills` list refuses under scoped isolation and warns and runs unrestricted on the host.

## Native package

`rimz hooks install minimax` owns the directory `<dataDir>/plugins/rimz` whole. It writes three regular files, in this order, so mcode never sees a manifest whose dependencies are missing:

| File | Content |
| --- | --- |
| `icon.png` | An embedded one-pixel PNG. The native manifest requires a real raster icon. |
| `hooks/rimz.json` | One command handler for each of the nine events in [Hook events](#hook-events). |
| `.minimax-plugin/plugin.json` | The manifest: `schemaVersion` 1, name `rimz`, version `1.0.0`, author `RimZ`, category `Other`, the icon and hook paths, a description, and empty `exampleQueries`, `apps`, `mcpServers`, and `skills` arrays. |

The manifest keeps to the pinned source's strict field set. mcode discovers the package as `rimz@local`, enabled by default, so install has no enable step.

Ownership is structural (`install.rs::check_package`), with no marker field. Every operation first refuses a package directory, `hooks/`, or `.minimax-plugin/` that is a symlink or a file. With a manifest present, the package is RimZ's only when the manifest names `rimz` with author `RimZ`. Without one, the directory is RimZ's only when it holds nothing beyond the three file paths and their two directories, which lets an interrupted install resume. A foreign package refuses install, preview, and uninstall with an instruction to move it aside.

| Operation | Behaviour |
| --- | --- |
| Install | Writes each file whose bytes differ, so a repeat install writes nothing. |
| Preview | Shows the two JSON files. The icon has no text diff. |
| Detection (`installed`) | True only when all three paths are regular files with the embedded bytes. Any drift makes `upgrade_available` true while the directory exists. |
| Uninstall | Removes `plugins/rimz` recursively. Sibling plugins and user configuration stay. |

Detection cannot see mcode's own enabled state. That state lives in a private SQLite table, which RimZ neither reads nor writes, and MiniMax implements no `hooks_disabled`. A byte-correct package therefore reports installed while mcode has the plugin disabled, and a reinstall does not change it. The fix is `mcode plugin enable rimz@local`, run with the same data-directory environment.

## Hook command and owner environment

Every handler runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source minimax` with a 10-second timeout, and SessionEnd's is 2 seconds. Both sit below upstream's 15-second event and 3-second teardown budgets. Every reply is byte-empty stdout with exit 0. RimZ sends no permission decision, block, or added context.

mcode runs hooks with an allowlisted environment that drops `RIMZ_*`, `XDG_RUNTIME_DIR`, and the mux pane variables. Restoring only the frame identity would not be enough: the feed would still resolve its RimZ home and runtime sockets outside the room. So MiniMax's `hook_inherits_env` returns `false`, and on Unix the feed restores the owner's whole environment before it reads stdin or any RimZ path:

1. `crates/rimz/src/main.rs` calls `restore_hook_environment` before it installs reporting, which itself reads the RimZ home.
2. `hook_owner_env` looks the source up among the built-ins only, since loading process plugins would resolve the RimZ home too early.
3. It takes the owner from `RIMZ_AGENT_PID`, which the handler stamps from the hook shell's parent. Upstream source places that parent in the TUI process itself.
4. It checks that the owner's `comm` is one of the declared process names (`AgentSpec::runs_as`), reads the owner's environment, then checks `comm` again, so an owner that changed during the read is refused.
5. `HooksArgs::restore_owner_env` re-executes `rimz` with the same arguments under that environment, plus the original `RIMZ_AGENT_PID` and the marker `RIMZ_HOOK_ENV_RESTORED=1`. The marker stops a second restore.

A missing or invalid pid, a `comm` mismatch, an unreadable environment, or a failed exec prints `rimz: cannot restore hook owner environment: ...` on stderr, and the feed continues with the scrubbed environment. The event can then land outside the intended room. Off Unix, no restore happens.

The declared process names are `minimax-code` and the shared runtime name `node`. On Linux, 0.6.12 showed `node-MainThread` during startup and `minimax-code` afterwards. `node-MainThread` is not declared, so a hook that fires while the TUI still carries that name fails owner verification. Whether SessionStart can fire that early is unverified. Pane classification is unaffected: `registry::command_agent_kind_with_comm` decides from the command line, so `node /usr/local/bin/mcode` is MiniMax under either name and `node server.js` is not.

`MiniMaxAdapter::is_interactive_process` treats `mcode` as interactive unless the first positional word, or the word after `--`, is a subcommand: `init`, `exec`, `acp`, `login`, `logout`, `update`, `telemetry`, `provider`, `plugin`, or `help`, or the retired `git`, `changes`, or `projects`. It skips the values of `--model`, `-m`, `--session`, `--resume`, and the four system-prompt flags.

## Hook events

`MiniMaxAdapter::decode_hook` drops every payload that carries `agent_id`, before it reads any session identity. A native child sends its own `mvs_` session id and no parent session id, so accepting its events would register the child as a second root in the parent's pane. Child tool events and child approvals drop with them, and SubagentStart and SubagentStop are not installed. The parent's `task` tool calls are the only sign of child work.

| Native event | Lifecycle signal | Notes |
| --- | --- | --- |
| `SessionStart` | `Registered`, or `CompactionEnded` for source `compact` | Startup, resume, clear, and fork all register. `Registered` sets the card idle. |
| `UserPromptSubmit` | `TurnStarted` | Sets the sanitized prompt and the card's task. |
| `PreToolUse` | `ToolUsed` | Activity only: no tool name, no mutate or edit count. |
| `PostToolUse` | `ToolUsed` | Counts `tool_name`. `bash`, `write`, `edit`, and `task` mutate, and `write` and `edit` edit files. |
| `PermissionRequest` | `AwaitingInput(Permission)` | Installed synchronous. The user answers in the pane, since `rimz answer` is unsupported. |
| `Stop` | `TurnEnded` | Never errored. `last_assistant_message`, when present, is the supervised run's final message. |
| `PreCompact` | `Compacting` | No gauge or summary follows. |
| `PostCompact` | `CompactionEnded` | `trigger` sets the automatic flag. |
| `SessionEnd` | `Ended` | Resume and clear end the old session before the new SessionStart. |

SessionStart can repeat inside one session, and that repeat is not harmless. Upstream reports a plugin activation as source `startup`, so an activation mid-turn re-registers the card, and `lifecycle.rs::map_status` resets it to idle, dropping a compaction or permission wait until the next signal. Whether a package rewrite's hot reload reactivates the plugin is unverified.

A permission wait clears at the next tool or settle signal. Native questions and plan approvals have no verified hook, so they never mark the card waiting. Automatic compaction sends PostCompact, then SessionStart with source `compact`, and the lifecycle fold absorbs the duplicate end.

Beyond the table, every mapped event carries the payload's `model` onto the card, and mux-session death comes from the shared exec wrapper. `registers_lazily` is true, so a MiniMax session can be present without a stamped pane, and the sidebar binds it to its pane by cwd.

Stop does not close every turn. A failed or cancelled turn sends no Stop, so the card stays working until the next prompt or SessionEnd, and a supervised run waits out its deadline. A hook continuation can also resume work after Stop. RimZ reconciles none of this: it reads no private turn tables and maps no background parking or idle notification.

## Launch and resume

| Action | Argv |
| --- | --- |
| Launch with a prompt | `mcode -- <prompt>` |
| Resume a recorded root session | `mcode --session <mvs_id>` |
| Bare provider resume (picker) | `mcode --session` |
| Model | `--model <provider/model[#variant]>` (`-m` is recognised in raw args) |
| Manual compaction | `/compact`, with an optional trailing instruction |

Forking and importing an unrecorded session are unsupported. No effort, fast-mode, interrupt-key, or auto-compaction-window switch is declared.

The `--` separator lets a prompt start with a dash: logged-out 0.6.12 reached the TUI with `mcode -- -probe`, while `mcode -probe` failed. It does not stop Commander's subcommand dispatch, so `mcode -- exec` still runs headless. A prompt equal to one of the subcommand words above has no verified positional form. Authenticated submission, other prompt forms, and native resume eligibility are unverified.

Launch reminders go through `--append-system-prompt` (`SystemTextChannel::TextFlag`). mcode gives the inline flag and `--append-system-prompt-file` one slot, so when raw args already hold the file form, `harness/launch.rs::merge_appended_system_text` reads the file, appends the reminder text, and emits a single inline argument. A profile's `system-prompt-file` maps to `--append-system-prompt-file`: on MiniMax it adds to the native system prompt, where most adapters replace it. The replacing `--system-prompt` and `--system-prompt-file` flags are recognised in raw args and pass through.

Every permission mode's argument vector is empty. The TUI has no `--permission` or `--config` switch, and RimZ does not edit the global `config.yaml` to impose a per-launch mode. The user picks the mode with `permissionMode` in `<dataDir>/config.yaml` (default `auto`), `/permission`, or Alt+M. Plan Mode is separate and has no verified launch flag.

## Context, history, cost, and account

Only the hook's live model reaches the card. The hook's `transcript_path` points to a temporary projection that can be deleted or null, and the durable messages sit in private SQLite under an unverified nested schema. RimZ therefore has no transcript discovery, history parser, context refresh, token gauge, spend parser, account probe, or quota window for MiniMax.

`default_model` is `None`. Upstream's default, `minimax/MiniMax-M3.1-Flash-Preview`, has no RimZ price, and a declared default must pass pricing coverage. Native cost fields are nullable and aggregates zero-filled, which establishes neither dollar semantics nor account isolation, so no dollar figure is inferred.

## Tests and verification boundary

The unit tests in `crates/rimz/src/agents/adapters/minimax/tests.rs` cover the child drop, byte-empty replies, malformed payloads, lifecycle through the real fold, permission open, settle, and close, package ownership, resume, idempotence, and drift, data-directory precedence, and launch argv and process identity. The integration tests in `crates/rimz/tests/integration/hooks/minimax.rs` cover two things. `scrubbed_hook_restores_the_room_pane_and_supervised_run` runs the test binary as a fake `minimax-code` parent that fires the real hook under a scrubbed environment, and proves the event lands in the room, binds the pane, and settles a supervised run. `installed_detection_ignores_the_mux_config_home_pin` proves detection agrees with install under each data-directory variable.

The isolated 0.6.12 live check proved package discovery and the enabled `rimz@local` status. It proved no hook execution, since logged-out mcode fires no hooks. Authenticated turns, the real hook parent, hot reload, child sessions, compaction, and account semantics stay source-backed or unverified, as the [upstream reference](../../externals/agent-adapter/minimax-reference.md#remaining-verification) records.
