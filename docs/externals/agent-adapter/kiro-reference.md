# Kiro CLI protocol reference

> RimZ's verified Kiro behavior is in [kiro.md](../../internals/agents/kiro.md). This document records the upstream surfaces, the Kiro CLI 2.12.1 observations that bound the adapter, and the evidence required before support expands.

This reference targets the early-access v3 engine selected by `kiro-cli chat --v3`, not the older embedded engine. Kiro ships both engines inside a 2.x package, so the package version alone does not identify the hook, permission, or session protocol.

Verification baseline: **2026-07-13**, Kiro CLI **2.12.1**, authenticated stock interactive TUI.

## Upstream sources

| Surface | Official source |
| --- | --- |
| v3 overview and compatibility boundary | <https://kiro.dev/docs/cli/v3/> · <https://kiro.dev/docs/cli/v3/feature-overview/> |
| v3 hooks and trigger catalog | <https://kiro.dev/docs/cli/v3/hooks/> · <https://kiro.dev/docs/hooks/> · <https://kiro.dev/docs/hooks/types/> |
| capability permissions | <https://kiro.dev/docs/cli/v3/permissions/> |
| CLI commands, sessions, and context | <https://kiro.dev/docs/cli/reference/cli-commands/> · <https://kiro.dev/docs/cli/chat/session-management/> · <https://kiro.dev/docs/cli/chat/context/> |
| ACP | <https://kiro.dev/docs/cli/acp/> · <https://agentclientprotocol.com/> |
| authentication and headless operation | <https://kiro.dev/docs/cli/authentication/> · <https://kiro.dev/docs/cli/headless/> |
| models and credits | <https://kiro.dev/docs/cli/models/> · <https://kiro.dev/docs/cli/billing/related-questions/> |
| configuration and `KIRO_HOME` | <https://kiro.dev/docs/cli/chat/configuration/> · <https://kiro.dev/docs/cli/reference/settings/> |

Refresh discovery with the installed executable:

```sh
kiro-cli --version
kiro-cli --help-all
kiro-cli chat --v3 --help
kiro-cli diagnostic --format json
kiro-cli whoami --format json
kiro-cli settings list --all --format json
kiro-cli chat --list-models --format json
```

## Verified stock-v3 surface

Launch a fresh interactive session with:

```sh
kiro-cli chat --v3
```

Use the explicit `chat` subcommand before profile options. Kiro CLI 2.12.1 accepts `--model`, `--effort`, `--resume-id`, and the interactive `/compact` command on this surface. Exact resume is `kiro-cli chat --v3 --resume-id <session-id>`.

The install runs a `kiro-cli` launcher and a `kiro-cli-chat` v3 engine. RimZ recognizes both process names. `kiro-cli-term` is the figterm shell-integration daemon for ordinary shells and does not identify an agent pane.

The authenticated root UI reported an identity shaped as `sess_<uuid>`. After two completed turns, including an approved file write, the only matching file under `${KIRO_HOME:-~/.kiro}/sessions/cli/` was `<session-id>.history`. Its final observed lines contained the submitted prompts and slash commands. The file carried no assistant text, timestamps, model, context usage, credits, tool records, or structured envelope. The capture did not establish append-versus-rewrite timing or resume persistence, so the adapter makes neither claim.

This `.history` file is readline history rather than a provider transcript. Feeding it to `agents history` would create answerless cut turns and imply timing that the source does not contain.

## Negative hook evidence

The v3 documentation describes standalone JSON hook configurations and trigger names such as `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop`. Kiro CLI 2.12.1 did not execute them in the verified stock interactive run.

The attempts covered:

- the existing user `~/.kiro/hooks/rimz.json`;
- an additional user hook file;
- replacement commands in the canonical user file;
- a project `.kiro/hooks` file in the disposable working directory.

None produced a command invocation or stdin payload. The CLI help exposed no hook listing, validation, or diagnostic command that made the configuration reproducible. No native payload keys or event ordering were captured.

RimZ therefore treats lifecycle and hook installation as unsupported for 2.12.1 v3. It retains uninstall-only recognition of a legacy RimZ-owned `rimz.json` so upgrades can remove stale files without touching user-authored configurations.

The older Kiro/Amazon Q engine's embedded lowercase hooks and payload conventions do not fill this gap. They select a different engine and cannot prove a stock-v3 wire.

## ACP and debug recordings

`kiro-cli acp` is a structured JSON-RPC transport that can create or load sessions and stream model and tool updates. Running it makes RimZ the protocol client rather than observing the user's stock TUI, so it is a separate product and security contract.

A paired UUID-only `<uuid>.json` metadata file and `<uuid>.jsonl` event file was observed under `sessions/cli/` for a non-interactive ACP-hosted session. Its metadata said `session_created_reason: "subagent"`; its identity did not match the stock root `sess_<uuid>` shape. RimZ excludes this session class from the stock adapter.

`KIRO_ACP_RECORD_PATH` records internal TUI ACP traffic for debugging. It is opt-in and has no published stability or durability contract, so a stock adapter cannot require or parse it as lifecycle truth.

Manual `/transcript save --json` exports a point-in-time user action rather than a live sidecar. Pane capture remains a rendering and explicit user primitive, not a producer enrichment source.

## Context, usage, and account gaps

Kiro displays context through `/context show` and credits through `/usage`, but 2.12.1 exposes no verified stock-session file, hook payload, statusline, or API response that RimZ can consume continuously. Kiro meters credits rather than raw token dollars; no faithful USD conversion follows from the displayed credit number alone.

`kiro-cli whoami --format json` is the account candidate. The captured signed-out shape is `{"account":null}`; the signed-in schema remains unpublished. RimZ reports no Kiro account spend or realtime dollars.

## Supervised execution boundary

The `chat --v3` parser accepts `--no-interactive`, and Kiro also exposes ACP, but neither is an implicit replacement for a stock interactive lifecycle hook. Before RimZ can support `agents kiro -p`, one transport must prove permission handling, cancellation, exact turn completion, final assistant output, exit status, transcript retention, and session identity under fixture-backed tests.

Until then `rimz agents kiro -p` fails before pane or run-record creation. Ordinary interactive launch and exact resume remain available.

## Re-enable checklist

Re-enable Kiro lifecycle and hook installation only when a pinned v3 release supplies all of the following:

1. A user or project hook configuration that executes reproducibly in a stock interactive session.
2. Redacted stdin fixtures for every installed trigger, with cwd, process attribution, stdout behavior, and event ordering.
3. Stable root identity across start, prompts, tools, successful completion, errors, cancellation, exact resume, and process exit.
4. A test showing installation succeeds on the pinned release and fails fast when its preconditions are absent.

Treat transcript, context, spend, and supervised ACP support as independent additions with their own native evidence. A lifecycle hook capture does not prove any of those surfaces.
