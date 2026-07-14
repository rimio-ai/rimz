# Antigravity adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The pinned upstream surface and live 1.1.2 evidence are in [antigravity-reference.md](../../externals/agent-adapter/antigravity-reference.md).

Antigravity support targets `agy` 1.1.2. RimZ owns stock interactive launch, permission-mode flags, model presets, exact conversation resume, process identity, safe lifecycle hooks, custom-statusline context, validated local-session discovery, transcript history, assistant streaming, and supervised completion.

## Launch, resume, and presence

Fresh sessions run `agy`; a startup prompt uses `agy --prompt-interactive <prompt>`. Profiles map `model` to `--model`; Antigravity publishes no reasoning-effort or system-prompt launch flag, so those profile fields fail before launch. Permission postures map `ask` to the native default, `auto` to `--mode accept-edits`, `plan` to `--mode plan`, and `yolo` to `--dangerously-skip-permissions`.

Exact resume runs `agy --conversation <conversation-id>`, and both split and joined flag forms are recognized on the live process command line. `-c` and `--continue` remain workspace-latest conveniences and do not claim an exact identity. Native `/fork` changes identity inside the current TUI and has no source-ID launch flag, so `rimz agents fork` remains unsupported.

The process matcher recognizes `agy`; it leaves desktop processes named `antigravity` outside the CLI adapter.

A live `agy` pane renders as an identity-less idle Antigravity card before a conversation binds. This preserves its known agent identity and pane routing without borrowing state from a stale or ambiguous provider conversation.

## Local store and transcript

Pulled discovery reads `${RIMZ_ANTIGRAVITY_HOME:-$HOME/.gemini/antigravity-cli}`. The test-only override keeps fixtures isolated; stock use reads the provider path. A transcript is accepted only as a regular file at a direct, non-symlink `brain/<conversation-id>/.system_generated/logs/transcript_full.jsonl` or `transcript.jsonl` path with a safe opaque path component. Hook-bound valid paths win; reconstruction and discovery prefer `transcript_full.jsonl`, fall back to `transcript.jsonl`, and emit one observation per conversation.

An existing hook-bound row authorizes its matching local transcript first when kind, conversation ID, and stamped live pane agree. Exact `--conversation` command-line identity is the next authority. `cache/last_conversations.json` authorizes only otherwise-unregistered fresh-session pairing for the exact absolute pane cwd, so a lagging provider cache cannot hide a native question from a conversation hooks already identified. At most the 512 newest local conversations enter one snapshot discovery pass; ambiguous fresh candidates leave the idle card identity-less.

The 1.1.2 fixtures verify JSONL records with `step_index`, `source`, `type`, `status`, `created_at`, optional `content`, and optional `tool_calls`. RimZ maps only the two visible shapes observed in a stock root conversation:

- `USER_EXPLICIT` / `USER_INPUT` becomes user text and running/reasoning. A value beginning with the exact `<USER_REQUEST>` tag contributes only the body through the matching `</USER_REQUEST>`; trailing metadata/settings blocks stay provider-private, and an unclosed or empty wrapper fails closed. Plain text remains the compatibility fallback.
- `MODEL` / `PLANNER_RESPONSE` becomes assistant text; `status: DONE` settles success unless a typed `ask_question` tool call carries a nonblank question.
- `SYSTEM` conversation-history/checkpoint records, malformed lines, unknown sources, and unknown types stay out of visible history.

Reads preserve physical order, tolerate unknown complete records, and retain a torn final JSONL record until it becomes complete. `transcript_full.jsonl` carries `ask_question.args.questions` as an array; legacy `transcript.jsonl` may JSON-encode that array as a string. A valid question projects waiting/idle with its first nonblank question and record timestamp; a newer question replaces it, and later visible user input or an ordinary completed planner response clears it. This native wait creates no durable RimZ ask, and the Antigravity pane remains the answer surface.

Discovery folds a bounded tail; the first `PreInvocation` reads the same bounded tail to carry the latest completed, visible, sanitized user prompt into the turn, while later lifecycle hooks stay free of transcript reads. Full history and incremental assistant output use the adapter-normalized transcript path.

The pulled records remain a cold-start and history fallback. Installed hooks own live turn state; the statusline owns model, account, and context usage.

## Hooks and rich state

`rimz hooks install antigravity` adds one named `rimz` block to `~/.gemini/config/hooks.json` and wraps `statusLine` in `~/.gemini/antigravity-cli/settings.json`. The preview shows both diffs. Reinstall reclaims RimZ commands by their stable command marker, preserves every user hook, and refuses a user-owned top-level hook named `rimz`. Uninstall removes only RimZ handlers and restores the complete prior `statusLine` value.

The installer wires `PreInvocation`, `PostToolUse`, `PostInvocation`, and `Stop`. The first invocation of an execution opens the turn and establishes an unseen conversation through create-on-miss; later invocation numbers do not reopen the boundary after a tool. Antigravity has no session-only registration event, so the provider-owned local discovery path remains the other registration source. Three disjoint post-tool matchers distinguish documented file edits, `run_command`, and the remaining documented tools without observing pre-tool permission policy. `Stop.terminationReason`, `error`, and `fullyIdle` resolve success, failure, and clean foreground parking on background work. All common payloads carry the conversation ID, first workspace path, transcript path, and model hint.

Hook stdout remains Antigravity's decision channel. `PreInvocation`, `PostInvocation`, and `PostToolUse` receive the documented `{}` no-op. `Stop` receives `{"decision":""}`, which is a non-`continue` decision and therefore allows the stop. RimZ deliberately leaves `PreToolUse` uninstalled and returns no output for a manual feed because its documented decisions (`allow`, `deny`, `ask`, and `force_ask`) all change native permission behavior.

The statusline wrapper forwards the user's prior command and maps the official payload's model ID/display name, CLI version, plan tier, account identity, context limit, percentages, and current input/output/cache composition into `AgentContext`. Live 1.1.2 can publish the selected human label in `model.id`, such as `Gemini 3.5 Flash (Medium)`. A terminal case-insensitive `(Low)`, `(Medium)`, or `(High)` selector qualifier becomes canonical lowercase effort; `(Thinking)` becomes the thinking flag; unknown parenthetical suffixes stay in the display label, and the provider model value remains unchanged. Statusline percentages and window data alone drive the context gauge.

When the model and current usage are priceable, the wrapper prices input, output, cache creation, and cache read as disjoint values into the ordinary live `AgentCost.total_cost_usd` slot and marks it estimated. Canonical IDs use the shared price resolver; a captured selector label with a recognized reasoning qualifier strips that qualifier and tries only normalized exact-table keys. The card and `agents show` render the API-rate value as `≈$`; its point-in-time shape excludes it from room/provider aggregates, budgets, provider history, account spend, and `rimz stats`.

`tool_confirmation_pending` adds a timestamped read-only permission marker: while it is newer than lifecycle activity, the sidebar projects the running card to waiting and routes focus to its pane; a later post-tool/turn hook self-clears a missed false refresh. This marker creates no durable ask and sends no decision to Antigravity. The wrapper emits no display text when no prior command exists and sets `stack_with_default` so Antigravity keeps its built-in line.

With hooks installed, `rimz agents antigravity -p` completes from `Stop` and reads the final visible assistant response from the hook-provided transcript. Permission wait detail, durable asks, artifact waits, structured answers, cumulative session/provider billing, account spend, compaction, identified subagent rows, and remote control remain unsupported. Answer native prompts in the Antigravity pane.
