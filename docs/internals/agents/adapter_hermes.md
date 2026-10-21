# Hermes adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. The raw upstream protocol is in [hermes-reference.md](../../externals/agent-adapter/hermes-reference.md), pinned to Hermes Agent v0.22.2.

Hermes support targets the classic CLI (`hermes chat --cli`) of Hermes Agent 0.22.2 or later, integrated through shell hooks in the default profile's `config.yaml`. RimZ owns launch, exact resume, process identity, the managed hook entries, and the context gauge. It reads no Hermes session database, so history, cost, and account panels are absent by design rather than pending.

Only the registry reaches the concrete adapter: [`HermesAdapter`](../../../crates/rimz/src/agents/adapters/hermes/mod.rs) and its private [`install`](../../../crates/rimz/src/agents/adapters/hermes/install.rs) module stay behind the provider-neutral capabilities.

## Launch and resume

Fresh sessions run `hermes chat --cli`. The classic UI is the surface the reference checks for native prompts and hooks; `--tui` runs a separate Ink process whose process ownership is unvalidated, so a user-launched `hermes --tui` is outside the claimed coverage.

The launch prompt renders as one `--query=<text>` token (`PromptStyle::JoinedFlag`). argparse rejects `-q -text` as a missing value but accepts the joined form, so a prompt beginning with `-` reaches Hermes intact. Exact resume runs `hermes chat --cli --resume <id>`. There is no picker and no fork: Hermes's `/branch` is untraced.

Profiles map `model` to `--model` and `effort` to `--reasoning`. Effort is a `StaticPresetMatcher::ValueFlag`, so `render_preset` refuses any level outside `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, and `ultra` instead of launching a Hermes that would reject it.

Yolo adds `--yolo`. Ask and Plan add nothing: Hermes's `/plan` is a prompt convention with no enforced policy, so `hermes-plan` launches plain Hermes, as `codex-plan`, `opencode-plan`, and `grok-plan` do. Hermes has no automatic-approval tier between ask and yolo, so `hermes-auto` is not registered.

Manual compaction sends `/compress`, with any focus text trailing the command.

There is no launch-reminder channel. `HERMES_EPHEMERAL_SYSTEM_PROMPT` is Hermes's only system-text input, and a nonempty value replaces the user's `display.personality` and `agent.system_prompt` rather than adding to them. Supervised children get the reminder appended to their prompt instead.

`process_names` is `["hermes"]`. The launcher execs `python3`, and Hermes renames its own process to `hermes` with `prctl(PR_SET_NAME)` before it spawns a hook, so the ancestor walk from a hook child stops at the right PID. Claiming `python3` would claim every Python process on the machine.

## Install and consent

`rimz hooks install hermes` merges one entry per installed event into the top-level `hooks` key of `$HERMES_HOME/config.yaml`, default `~/.hermes/config.yaml`. Each entry is `{command: "rimz hooks feed --source hermes", timeout: 10}`. The command string is identical for every event and constant across RimZ versions: Hermes keys consent on the exact `(event, command)` pair, so changing it would re-prompt every user. Ownership is the substring `hooks feed --source hermes`, so reinstall reclaims old entries.

The edit is text surgery around a structured parse. `serde-saphyr` parses the file, and its event parser locates the byte range of the top-level `hooks` value:

- Bytes outside that range stay byte-identical. Inside it, RimZ renders the merged value as a JSON flow mapping, which is valid YAML and preserves every value the parse accepted.
- When the merged value equals what the file already holds, the file keeps its bytes. Hermes's own config writes re-emit `hooks` in their own layout, and an unchanged reinstall must not churn it.
- User entries and the reserved `output_spill` and `outbound` subsections keep their values. Uninstall removes only RimZ entries, drops event keys it emptied, and drops `hooks` only when RimZ's entries were all it held.
- A file that does not parse, a root that is not a block mapping, a second document, or an anchor, alias, or tag inside `hooks` refuses with a by-hand fix and writes nothing.
- A missing file is created holding only the `hooks` block.

Install and preview refuse when `hermes --version` reports a release older than 0.22.2, where the human-input hooks do not exist; an unreadable version installs. The refusal is the managed integration's `install_blocker`, so a bare `rimz hooks install` skips Hermes and continues with the other agents.

RimZ never writes `shell-hooks-allowlist.json` and never passes `--accept-hooks` or `HERMES_ACCEPT_HOOKS`: either would also accept any other unseen hook the user configured. Hermes prints one `Allow this hook to run? [y/N]` prompt per unseen pair at interactive startup, before session-start hooks or a seeded prompt, and skips unapproved hooks silently when stdin is not a terminal. Answering `y` persists approval; answering `N` skips that hook for the process, without an in-session approval path. `untrusted_installed_hooks` reads the allowlist and reports each RimZ event whose exact command has no approval; a missing or malformed allowlist reports every installed event. `InstallationCapability::hook_trust_choice` opts into watched launch notices with `answer y to each RimZ hook prompt`: watched launches proceed, unattended starts refuse short, and rebirth skips the agent with a user-run fix ([launch checks](../../reference/cli/agents.md#the-account-check-at-launch)). Headless operations (supervised runs, loops, message replies) keep refusing with `launch hermes in a terminal and answer y to each RimZ hook prompt`, the adapter's `hook_trust_fix`.

`hermes hooks doctor` flags every hook named by a bare command, RimZ's included, as `script missing or not executable`: its check needs a file path, while the runtime spawn resolves `rimz` on `PATH`.

## Hooks and lifecycle

Hermes runs each hook with `shell=False`, the payload on stdin, and a fresh process group. Every payload carries `hook_event_name`, `session_id`, `cwd`, `profile`, and an `extra` object with the event's own fields. Neutral output is empty stdout on every event.

| Native event | `LifecycleSignal` | Notes |
| --- | --- | --- |
| `on_session_start` | `Registered` | `SessionSource::Startup`. Fires at the first turn, not at launch. |
| `on_session_reset` | `Registered` | `SessionSource::Clear` with the new `session_id`, the way Claude's `/clear` rebinds; the card is not ended. |
| `pre_llm_call` | `TurnStarted` | `extra.user_message` becomes the sanitized task and prompt. |
| `post_tool_call` | `ToolUsed { mutates, edits }` | `write_file` and `patch` edit; `terminal` mutates. Carries a constant native key (below). |
| `on_session_end` | `TurnEnded` | A turn end despite its name. `extra.failed` marks it errored; an interrupted turn ends as a success. |
| `post_api_request` | none | The context gauge (below). |
| `on_human_input_request` | `AwaitingInput` | Keyed on `extra.request_id`. `approval` and `sudo` are permissions; `clarify` and any other kind are questions. |
| `on_human_input_resolved` | `AskResolved` | Same key, closing that wait. Ignored outside `waiting`. |
| `subagent_start` | `SubagentStarted` | Child keyed on `extra.child_session_id`, parent on the top-level `session_id` (Hermes fills it from `parent_session_id`); `extra.child_goal` is the task. |
| `subagent_stop` | `SubagentStopped` | Same child key. `child_status` of `failed`, `error`, or `timeout` is errored. |

Three choices hold the ask bracket together:

- A keyless `ToolUsed` clears any keyed wait. `post_tool_call` therefore carries the constant key `hermes:post_tool_call`, which matches no request id, so a sibling tool finishing during an open prompt leaves the wait open.
- Hermes cannot forget a request the way Pi's extension does when a turn settles, so a late `on_human_input_resolved` can arrive after `on_session_end`. A `ToolUsed` there would reconcile the settled row back to running, so the close is its own signal, [`LifecycleSignal::AskResolved`](../../../crates/rimz/src/agents/lifecycle.rs), which `step` ignores on every status but `waiting`.
- The human-input brackets stay lifecycle-classified rather than actionable asks: Hermes exposes no structured answer route, and the user answers in the pane.

`on_session_finalize` is not installed. Its reason is `session_boundary` both on exit and on `/new`, and mapping it to `Ended` would mark a running card failed. `ended` is derived from the `rimz exec` wrapper, as for Codex and Amp.

Not installed either: `pre_tool_call` (blocking-capable, and `post_tool_call` carries what RimZ reads), `pre_approval_request` and `post_approval_response` (a second correlation shape for the wait the human-input bracket already covers), and the request, auxiliary, stream, and verify hooks (each another consent prompt and another spawn per request for nothing RimZ reads).

## Context

`post_api_request` stamps an observed context per request: `extra.usage.prompt_tokens` is the occupancy, `extra.context_length` the window, and `extra.model` the model. A missing field is left out, so the sparse merge keeps the last known value, and each request replaces the occupancy. The frame also carries the model's output (`response`, `assistant_message`); the decoder reads none of it, and the ingress frame is truncated after drain like Claude's `PostToolUse`.

## Account, cost, and history

Hermes reports tokens but no billed dollars, and its usage can be subscription-included or of unknown cost status. Pricing tokens through the price book would invent bills, so realtime cost, spend, and `rimz stats` dollars are unsupported. History and account are unsupported because the reference has no validated read-only query over Hermes's `state.db`.

Named logins are refused. `HERMES_HOME` is declared as the config-home key so the scoped pins it, but `supports_named_logins` is false: Hermes falls back to the global root's credentials, so a separate home does not isolate an account.

## Known gaps

Run `rimz coverage` for the current matrix. The gaps worth a reason:

- **Default profile only.** Install targets `$HERMES_HOME/config.yaml`; profiles under `<root>/profiles/<name>` (`hermes -p NAME`) get no hooks.
- **Uncovered prompt panels.** The memory `write_approval` prompt and the secret, vault, and confirmation panels emit no human-input bracket, so those waits never reach the card.
- **Rotating compression.** With the non-default `compression.in_place: false`, compression changes `session_id` with no event. The card follows the newest payload's `session_id`.
- **Subagent timing.** `subagent_stop` fires when Hermes processes the child's result, not at the child's last token.
- **Consent prompts.** The first interactive launch after install asks ten y/N questions, one per installed event.
- **Skills.** `--skills` preloads rather than restricts, so a profile `skills` list refuses under scoped isolation and warns on the host.

## Live check

A live check runs Hermes without anyone's login, so every home it reads is a scratch one:

- **Scratch homes.** Set `HOME`, `HERMES_HOME` (`$HOME/.hermes`), `RIMZ_HOME`, and a short `XDG_RUNTIME_DIR` (`/tmp/<8 chars>`) to directories under one scratch root, and unset `ZELLIJ`, `ZELLIJ_SOCKET_DIR`, `ZELLIJ_SESSION_NAME`, and `TMUX`. Never point any of them at `~/.hermes`: its `auth.json`, `.env`, `state.db`, and `config.yaml` are the user's.
- **A stub model server.** Real turns need a model, and a local OpenAI-compatible server stands in for one with no credentials. It answers `GET /v1/models` with one model and its `context_length`, and answers `POST /v1/chat/completions` from a scripted list of replies, one per request. A reply is plain content, or a tool call (`terminal` for a permission ask, `clarify` for a question). Each reply carries `usage`, which is what the context gauge reads. The scratch `config.yaml` points the `model` block at it: `provider: custom`, `base_url: http://127.0.0.1:<port>/v1`, any `api_key`, `streaming: false`.
- **The installs link.** Hermes resolves its dependency state under `$HERMES_HOME/installs`, and an empty scratch home cannot start. Link `$HERMES_HOME/installs` to `~/.hermes/installs`.
- **What the link touches.** It is the one path from the scratch home into real state. A launch touches `~/.hermes/installs/<key>/pm-runtime` and takes the update lock at `~/.hermes/hermes-agent/.git/hermes-update.lock`, which a real Hermes update also takes. Afterwards, confirm the real `hermes --version` still runs.

Install with `rimz hooks install hermes` under these homes, then answer the consent prompts in the first interactive `hermes chat --cli` by hand: RimZ never grants consent itself, and neither should the check. Stop the stub server by its PID.
