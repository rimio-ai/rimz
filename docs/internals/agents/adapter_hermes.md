# Hermes adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. The raw upstream protocol is in [hermes-reference.md](../../externals/agent-adapter/hermes-reference.md), pinned to Hermes Agent v0.22.2.

RimZ supports the classic CLI of Hermes Agent 0.22.2 or later (`hermes chat --cli`) through shell hooks in the default profile's `config.yaml`. RimZ owns the launch argv, exact resume, process identity, the managed hook entries, and the context gauge. It reads no Hermes session database, so the adapter has no transcript, history, cost, or account source: every one of those capability traits is an empty `impl`.

The adapter lives in [`crates/rimz/src/agents/adapters/hermes/`](../../../crates/rimz/src/agents/adapters/hermes/mod.rs):

| File | Owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/agents/adapters/hermes/mod.rs) | the spec (`HERMES_SPEC`), the hook catalog (`HERMES_HOOKS`), hook decoding (`HermesAdapter::decode_hook`), the hook-trust strings, the version floor, and the config home |
| [`install.rs`](../../../crates/rimz/src/agents/adapters/hermes/install.rs) | the `config.yaml` edit, install, preview, uninstall, detection, and the consent mirror (`HermesManagedIntegration`) |
| `tests.rs`, `install/tests.rs` | decoding and install tests |

## Launch and resume

`HERMES_SPEC.launch` renders every argv. The classic UI is the surface the upstream reference checks for native prompts and hooks. `hermes --tui` runs a separate Ink process whose process identity is unvalidated ([reference](../../externals/agent-adapter/hermes-reference.md#processes-and-transports)), so a user-launched TUI session is outside the claimed coverage.

| Launch input | Argv |
| --- | --- |
| fresh session | `hermes chat --cli` |
| launch prompt | one joined `--query=<text>` token (`PromptStyle::JoinedFlag`) |
| exact resume | `hermes chat --cli --resume <id>` |
| profile `model` | `--model <model>` |
| profile `effort` | `--reasoning <level>`, refused outside `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` |
| yolo | `--yolo` |
| ask, plan | nothing |
| manual compaction | `/compress`, with any focus text after the command (`CompactInstruction::Trailing`) |

The prompt is one joined token because argparse can read a separate value that starts with `-` as an option ([reference](../../externals/agent-adapter/hermes-reference.md#launch-prompts-and-resume)). The joined form delivers such a prompt intact.

Effort is a `StaticPresetMatcher::ValueFlag`, so `render_preset` refuses a level outside the list before it launches a Hermes that would reject it.

Ask and plan add no flag. Hermes's `/plan` is a prompt convention with no enforced read-only policy ([reference](../../externals/agent-adapter/hermes-reference.md#planning-and-added-system-text)), so `hermes-plan` launches plain Hermes. Hermes has no automatic-approval tier between ask and yolo, so `hermes-auto` is not registered. There is no resume picker and no fork, since Hermes's `/branch` is untraced.

Hermes launches carry no RimZ text outside the user's prompt:

- No launch reminder. The adapter keeps the default `LaunchCapability::append_system_text_channel` of `None`, so `harness/launch.rs` renders the reminder but adds it to no argv, environment variable, or prompt. `HERMES_EPHEMERAL_SYSTEM_PROMPT` is Hermes's only system-text input, and a nonempty value replaces the user's `display.personality` and `agent.system_prompt` ([reference](../../externals/agent-adapter/hermes-reference.md#planning-and-added-system-text)).
- No system-prompt replacement. The spec has no `SystemPromptFile` matcher, so a profile's system-prompt fields fail with `PromptComposeErr::Unsupported` (`crates/rimz/src/harness/prompt_compose.rs::plan_system_prompt`).
- No headless command. `LaunchSpec.headless` is unset, so a workflow judge or a loop agent check on Hermes fails with `AgentProcessCompileErr::NoHeadless`.

Every interactive launch checks the binary against `install::MIN_VERSION` (0.22.2) after the shell rc resolves the program (`crates/rimz/src/agents/version.rs::check_launch_version_floor`, called from `crates/rimz/src/launcher/mod.rs::check_cell` and `crates/rimz/src/cli/agents_cmd/exec.rs::launch_and_supervise`). An unreadable version passes.

`process_names` is `["hermes"]`. The shell launcher execs Python, and Hermes renames its process to `hermes` with `prctl(PR_SET_NAME)`, so the ancestor walk from a hook child finds Hermes by that name. The reference has not verified that the rename happens before the first hook spawns ([reference](../../externals/agent-adapter/hermes-reference.md#processes-and-transports)). Claiming `python3` instead would claim every Python process on the machine.

## Install

### The config path

`HermesAdapter::config_home` resolves the home from the login environment. It uses `HERMES_HOME` when that value is absolute, is not the filesystem root, and is neither `HOME` nor an ancestor of it. Otherwise it uses `$HOME/.hermes`. Install edits `<home>/config.yaml` and fails when `HOME` is unset. Only the default profile gets hooks, since named profiles live under `<home>/profiles/<name>` ([reference](../../externals/agent-adapter/hermes-reference.md#home-profiles-authentication-and-skills)).

### The managed entries

`rimz hooks install hermes` adds one entry per `HERMES_HOOKS` event to the top-level `hooks` key. Each entry is `{"command": "rimz hooks feed --source hermes", "timeout": 10}`. The command string is the same for every event and stays fixed across RimZ versions, because Hermes keys consent on the exact `(event, command)` pair: a changed string would prompt every user again.

Ownership is the substring `hooks feed --source hermes` (`crates/rimz/src/agents/adapters/hermes/install.rs::owned`), so a reinstall first removes every owned entry, then appends the canonical one. Two predicates read the result:

- `installed_at` requires every catalog event to hold the exact canonical object. An owned entry with another timeout or an extra field reads as not installed.
- `managed_artifacts_present` is true when any owned entry remains, so uninstall finds a partial install.

### The YAML edit

The edit is text surgery around a structured parse. `serde-saphyr` parses the whole file for its values, and its event parser (`Config::parse`) finds the `hooks` stanza: from the start of the `hooks` key's line to the end of the value's last line. `Config::render` replaces that stanza and leaves every byte outside it unchanged.

- The new stanza is `hooks:` at the root mapping's indent, followed by the merged value as an indented JSON flow mapping. JSON is valid YAML and keeps every value the parse accepted.
- A rewrite loses what the stanza held beyond its values: comments inside it, a comment on its first or last line, and the quoting of the `hooks` key.
- When the merged value equals what the file holds, the file keeps its bytes (`preview_at`). Hermes's own config writes emit `hooks` in their own layout, and an unchanged reinstall must not rewrite it.
- User entries and the reserved `output_spill` and `outbound` subsections keep their values. Uninstall removes only owned entries, drops each event key it emptied, and drops the stanza when nothing is left in it.
- A file with no `hooks` key gets the stanza appended. A missing, empty, or comment-only file is created or extended the same way.

The edit refuses, writes nothing, and names the by-hand fix (`config_refusal`) for any of these:

- a file that does not parse
- a second YAML document
- a root that is not a block mapping
- a `hooks` value that is not a mapping, or an event value that is neither a list nor null
- an anchor, alias, or tag inside `hooks`, or a `hooks` key with an anchor or tag, or one that spans lines

### The install blocker

`HermesManagedIntegration::install_blocker` is any error `preview` returns: a Hermes older than 0.22.2 (where the human-input hooks do not exist), an unset `HOME`, or a YAML refusal. An unreadable `hermes --version` does not block. A detected install (`rimz start`, `rimz setup`, bare `rimz hooks install`) skips a blocked Hermes with only a debug log, while `rimz hooks install hermes` reports the error ([adapter.md](./adapter.md#hook-install)).

### Hook consent

Hermes runs a shell hook only after the user approves its `(event, command)` pair at an interactive `Allow this hook to run? [y/N]` prompt. On non-terminal stdin it skips an unapproved hook without a prompt. Approvals persist in `$HERMES_HOME/shell-hooks-allowlist.json` ([reference](../../externals/agent-adapter/hermes-reference.md#shell-hook-wire)). RimZ never writes that file and never passes `--accept-hooks` or `HERMES_ACCEPT_HOOKS`, since either would also accept any other unseen hook the user configured. The first interactive launch after install therefore asks one question per installed event, ten in all.

`untrusted_installed_hooks` mirrors the check (`crates/rimz/src/agents/adapters/hermes/install.rs::untrusted_at`). It reads the allowlist's `approvals` array and reports each catalog event whose owned entry has no approval with the same event and command. A missing or malformed allowlist reports every installed event. An unreadable `config.yaml` reports none.

The adapter supplies two strings to the shared hook-trust policy. `hook_trust_choice` (`answer y to each RimZ hook prompt`) opts Hermes into watched launches that proceed with a notice. `hook_trust_fix` (`launch hermes in a terminal and answer y to each RimZ hook prompt`) is the fix that `rimz doctor`, the `rimz start` notice, and the hook preflights print, so supervised runs, loops, and message replies refuse with it (`crates/rimz/src/agents/mod.rs::hook_trust_fix`). Unattended starts and rebirth name a user-run command instead (`crates/rimz/src/config/definitions/mod.rs::hook_trust_fix`). The policy by launch class is the one [adapter_codex.md](./adapter_codex.md#hook-trust) tabulates, and [launch checks](../../reference/cli/agents.md#the-account-check-at-launch) define the classes.

## Hooks and lifecycle

Hermes sends each hook one JSON object on stdin and runs the command with `shell=False` ([reference](../../externals/agent-adapter/hermes-reference.md#shell-hook-wire)). Every payload carries `hook_event_name`, `session_id`, `cwd`, `profile`, and an `extra` object with the event's own fields. `decode_hook` routes every event by the top-level `session_id` and reads the event fields from `extra`. Neutral output is empty stdout on every event.

| Native event | `LifecycleSignal` | Notes |
| --- | --- | --- |
| `on_session_start` | `Registered` | `SessionSource::Startup` |
| `on_session_reset` | `Registered` | `SessionSource::Clear`, with the new `session_id` |
| `pre_llm_call` | `TurnStarted` | `extra.user_message` becomes the sanitized task and prompt |
| `post_tool_call` | `ToolUsed { mutates, edits }` | `write_file` and `patch` edit, and `terminal` mutates. Carries the constant native key `hermes:post_tool_call` |
| `on_session_end` | `TurnEnded` | a turn end despite its name. `extra.failed` marks it errored, so an interrupted turn ends as a success |
| `post_api_request` | none | the context gauge ([Context](#context)) |
| `on_human_input_request` | `AwaitingInput` | keyed on `extra.request_id`. `approval` and `sudo` are permissions, and `clarify` and any other kind are questions |
| `on_human_input_resolved` | `AskResolved` | keyed on `extra.request_id` |
| `subagent_start` | `SubagentStarted` | child `extra.child_session_id`, parent the top-level `session_id`. `extra.child_goal` is the task |
| `subagent_stop` | `SubagentStopped` | same child key. `extra.child_status` of `failed`, `error`, or `timeout` is errored |

### The ask bracket

The human-input bracket is the only native-prompt signal, and three choices keep it intact:

- A keyless `ToolUsed` clears any keyed wait. `post_tool_call` therefore carries the constant key `hermes:post_tool_call`, which matches no request id, so a sibling tool that finishes during an open prompt leaves the wait open.
- Hermes emits the resolution in a `finally` block ([reference](../../externals/agent-adapter/hermes-reference.md#native-prompts-and-permissions)), so it can arrive after `on_session_end` has settled the row. A `ToolUsed` there would move the settled row back to running. The close is therefore its own signal, [`LifecycleSignal::AskResolved`](../../../crates/rimz/src/agents/lifecycle.rs), which the state machine ignores outside `waiting` and when its key names a different open ask (`crates/rimz/src/agents/lifecycle.rs::stale_ask_close`).
- The brackets stay lifecycle signals and never become actionable asks. Hermes has no structured answer route, so the user answers in the pane.

### Sessions

Hermes declares `registers_lazily = false` and `local_session_discovery = Unsupported`, so with hooks installed its pane shows the idle row until its first hook arrives ([instances.md](./instances.md#before-a-session-binds)). Hermes fires `on_session_start` after it builds the system prompt, and a resume can continue without firing it again ([reference](../../externals/agent-adapter/hermes-reference.md#event-families)).

`on_session_reset` (`/new`) produces the same `Registered` signal as a start, under the new `session_id`. It ends nothing: the old root stays, and [same-pane ownership](./instances.md#same-pane-ownership) decides which root owns the card.

`ended` is derived from the `rimz exec` wrapper, as for Codex and Amp. `on_session_finalize` is not installed: its reason is `session_boundary` both on exit and on `/new`, and mapping it to `Ended` would end a card that is still running.

### Subagents

`subagent_start` and `subagent_stop` resolve their pair through `crates/rimz/src/agents/identity.rs::resolve_subagent_identity`. A blank child or parent, or a child equal to its parent, logs an error and quarantines the event, which then attaches no observation. The stop fires when Hermes processes the child's result, which can be later than the child's last token ([reference](../../externals/agent-adapter/hermes-reference.md#native-subagents)).

### Events not installed

| Event | Reason |
| --- | --- |
| `on_session_finalize` | cannot tell exit from `/new` ([Sessions](#sessions)) |
| `pre_tool_call` | blocking-capable, and `post_tool_call` carries what RimZ reads |
| `pre_approval_request`, `post_approval_response` | a second correlation shape for the wait the human-input bracket covers |
| request, auxiliary, stream, and verify hooks | each costs another consent prompt and a spawn per request, for nothing RimZ reads |

## Context

`post_api_request` writes an observed context for the request's session and no lifecycle signal. `extra.usage.prompt_tokens` is the occupancy, `extra.context_length` the window, and `extra.model` the model. A missing field is left out, so the sparse merge keeps the last known value, and each request replaces the occupancy.

The same payload carries the model's output in `response` and `assistant_message`. The decoder reads neither, but the raw frame holds them in the [hook ingress log](../store.md#hook-ingress) until the drain truncates it.

## Account, cost, and history

Hermes reports tokens with no billed dollars, and its usage can be subscription-included or of unknown cost status ([reference](../../externals/agent-adapter/hermes-reference.md#context-usage-and-cost)). Pricing those tokens through the price book would invent bills, so realtime cost, spend, and `rimz stats` dollars are unsupported. History and account are unsupported because the reference has no validated read-only query over Hermes's `state.db`.

Named logins are refused. `HERMES_HOME` is the declared config-home key, so the scoped view pins it, but `supports_named_logins` is false: Hermes can fall back to the global root's credentials, so a separate home does not isolate an account ([reference](../../externals/agent-adapter/hermes-reference.md#home-profiles-authentication-and-skills)).

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid. These gaps have a reason worth recording:

- Only the default profile is wired. Install targets `<home>/config.yaml`, and a `hermes -p NAME` session runs no RimZ hook.
- The memory `write_approval` prompt emits no human-input bracket, and the reference has not checked the secret, vault, and confirmation panels. Those waits never reach the card.
- Rotating compression strands the old root. With the non-default `compression.in_place: false`, compression switches `session_id` with no event. A rotation inside a turn leaves the old root holding its open turn while the turn end lands on the new id. No supersession rule links the two ([instances.md](./instances.md#supersession)), and `KeepPrimary` keeps the older open root as the card's owner.
- A profile `skills` list refuses under scoped isolation and warns on the host, because `--skills` preloads rather than restricts (`HostSkills::Unsupported`). Profile tool rules warn and are dropped (`ToolRules::Unsupported`).
- The process-name match rests on the unverified ordering of Hermes's rename and its first hook ([Launch and resume](#launch-and-resume)).

## Live check

A live check runs Hermes without anyone's login, so every home it reads is a scratch one. These steps have not been rerun against the current tree.

1. Set `HOME`, `HERMES_HOME` (`$HOME/.hermes`), `RIMZ_HOME`, and a short `XDG_RUNTIME_DIR` (`/tmp/<8 chars>`) to directories under one scratch root. Unset `ZELLIJ`, `ZELLIJ_SOCKET_DIR`, `ZELLIJ_SESSION_NAME`, and `TMUX`. Never point any of them at `~/.hermes`: its `auth.json`, `.env`, `state.db`, and `config.yaml` are the user's.
2. Run a stub model server. A local OpenAI-compatible server answers `GET /v1/models` with one model and its `context_length`, and answers `POST /v1/chat/completions` from a scripted list of replies, one per request. A reply is plain content or a tool call (`terminal` for a permission ask, `clarify` for a question), and each carries `usage`, which the context gauge reads.
3. Point the scratch `config.yaml`'s `model` block at the stub: `provider: custom`, `base_url: http://127.0.0.1:<port>/v1`, any `api_key`, and `streaming: false`.
4. Link `$HERMES_HOME/installs` to `~/.hermes/installs`. Hermes resolves its dependency state there, and an empty scratch home cannot start. This link is the one path into real state: a launch touches `~/.hermes/installs/<key>/pm-runtime` and takes the update lock at `~/.hermes/hermes-agent/.git/hermes-update.lock`, which a real Hermes update also takes.
5. Run `rimz hooks install hermes` under these homes.
6. Answer the consent prompts by hand in the first interactive `hermes chat --cli`. RimZ never grants consent itself, and the check must not either.
7. Stop the stub server by its PID, then confirm the real `hermes --version` still runs.
