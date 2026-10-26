# Logins and account homes

A provider home is an account. RimZ resolves a room's account selection into a login ([`ProviderLogin`](../../../crates/rimz/src/agents/login.rs)): a provider kind, an account name, and the environment that points the provider at that account's home. This page owns which login a room and each agent run on, the environment a login builds, and the reconciler that links a named account's home into the provider's own. What RimZ reads under a login (plan, windows, credits) is [providers.md](./providers.md).

A login never changes a process-wide variable. It is an override on an ambient environment map, and every host-side read of a provider home (launch, discovery, install, probes) resolves that map through the adapter's `config_home` (`crates/rimz/src/agents/login.rs` module header). A CI grep keeps the provider home keys out of process-environment reads outside tests ([agents contract](../../../crates/rimz/src/agents/AGENTS.md#layout)).

## Terms

| Term | Means | Code |
| --- | --- | --- |
| login key | `<kind>@<name>`, as in `claude@work` or `codex@default`. Every account probe, account record, and stamp on these pages is keyed by it. A usage reading's claim, lease, and cache entry are keyed by the login plus the credential's scope ([the shared caches](./providers.md#the-shared-caches)) | `LoginKey` |
| `default` | the provider's own home, as the provider resolves it. It carries no path | `ProviderLogin::default_for` |
| named account | a declared `[accounts.<kind>.<name>]` home, selected through the kind's one home variable | `ProviderLogin::named` |
| history mode | `shared` (the default): the account keeps its credentials and links everything else to the provider's own home. `standalone`: it links settings alone and keeps its own sessions | `crates/rimz/src/config/accounts.rs::AccountHistory` |
| history pool | where a login's sessions and spend live: `<kind>@default` for `default` and every shared account, the login's own key for a standalone account or an undeclared name | `LoginCatalog::pool` |
| stamp | the login recorded on an agent row (`AgentState::login`). `None` is `default` | `AgentState::login_key` |
| room default | the login the room launches a kind under now | `RoomLoginSet::default_login` |

Named accounts exist for Claude (`CLAUDE_CONFIG_DIR`), Codex (`CODEX_HOME`), and Pi (`PI_CODING_AGENT_DIR`) only ([`AccountsConfig::named`](../../../crates/rimz/src/config/accounts.rs)). Every other kind has only `default`. An account without a configured `home` lives at `~/.rimz/accounts/<kind>/<name>` (`crates/rimz/src/agents/login.rs::default_named_home`), and nothing in RimZ removes it.

`LoginCatalog::from_config` refuses a declaration named `default`, a relative home, a home containing `,` or ending in `projects`, a home two accounts share, and the provider's own home (`LoginConfigErr`). The read-side catalog (`LoginCatalog::room_view`) drops only the kind whose declaration fails, so the other kinds keep their pools.

## Which login a room uses

[`room_accounts`](../../../crates/rimz/src/agents/login.rs) reads the room's `workspace.json` pins and delegates to `resolve_room_accounts`. Per kind, the first source that names an account wins:

| Order | Source | `LoginSource` |
| --- | --- | --- |
| 1 | the room's pin | `pinned` |
| 2 | the trusted project's `[accounts]` | `project default` |
| 3 | the machine's live `[accounts.use]` | `machine default` |
| 4 | none | `provider default` (`default`) |

A resolution failure stays with the kinds it touches. Only an unreadable `workspace.json` fails the whole resolution. Blocked project trust refuses every kind that would inherit, and an undeclared name or an unreadable machine core refuses only the kinds that need it. A pin to `default` reads neither project nor machine config. `RoomAccounts::account`, `name`, `source`, and `login` answer per kind, and a failed kind never becomes `default`: display readers omit its account and keep its healthy siblings, and `RoomAccounts::names` lists successful kinds only.

The room records explicit pins and nothing else. Birth writes only the pins a launch requested, `rimz accounts use` pins, `--reset` unpins, and `--global` preflights the account before it changes the machine's `[accounts.use]`. An unpinned kind follows the project and machine defaults live, and a change leaves running agents on their stamps. A `workspace.json` that carries the older `logins` map reads its named entries as pins and its `default` entries as inherited ([workspace record](../store.md#the-workspace-record)).

## The default login and exported homes

A pane born on a named account exports that account's home, so a provider child started from it would read the named home as `default`. A catalog-built `default` login (`LoginCatalog::select`, `session_login`, `RoomLoginSet::default_login`, and `in_use`) carries every declared home of its kind, and `ProviderLogin::env` drops an exported home variable that names one of them. Every caller in a named-account pane therefore reads the native home as `default` without opting in. `crates/rimz/src/agents/login.rs::RoomLoginSet::env` applies the same drop to a `default` login built outside the catalog, through the private `LoginCatalog::native_ambient`, which is idempotent on a catalog-built one.

The drop holds at launch too. A default launch pins a dropped home key unset for the provider process (`launch_plan::compile`), and the Claude remote-control host starts under `env -u CLAUDE_CONFIG_DIR` when the login env lacks the key (`crates/rimz/src/agents/adapters/claude/remote_control.rs::host_argv_with_login`). Only exports naming a declared home are dropped: an undeclared export and a named login keep their home selection. A broken declaration leaves the bare `default` on the raw ambient environment. `rimz accounts add` and `rimz doctor` refuse a named account whose home the ambient environment already exports (`ProviderLogin::check_exported_home`), since that export makes every pane read the account as `default`.

A shared account whose adapter names a database-home key (`shared_database_home_env_key`, Codex's `CODEX_SQLITE_HOME`) launches with that key set to the native home, unless the ambient environment sets it (`ProviderLogin::overrides`). A symlink would leave the database's sidecar files in the account home. The Codex app-server, daemon-control, and login-status children receive the key with `CODEX_HOME` (`crates/rimz/src/agents/adapters/codex/mod.rs::forward_login_env`).

## Login sets

[`RoomLoginSet`](../../../crates/rimz/src/agents/login.rs) is the room's logins, resolved once per refresh pass or fold over one ambient snapshot. A kind whose account does not resolve answers `None`, so a caller skips it and never reads another account's state.

| Method | Answers | Read by |
| --- | --- | --- |
| `default_login`, `default_key` | the room default per kind | new launches |
| `current_keys` | `default_key` over every kind | the dashboard ([`PanelScope::Current`](./providers.md#producer-aggregation)) |
| `with_agents`, `in_use`, `keys_in_use` | the room defaults plus the stamps of live root agents (no `ended_at`, provider-native subagents excluded) | producer state, rate-limit fusion, usage reads, auto-redeem |
| `declared` | every login the machine catalog holds, `default` included. None when the account config did not load | [idle accounts](./providers.md#idle-accounts) |
| `pool` | the history pool of a login | spend and the account budget |
| `env` | the environment a login's provider state is read under | every reader of a login |

An unresolvable named stamp is left out of `in_use`, and never becomes a native-home read. A reader that serves one session uses that agent's own stamp, never the room default.

An operation resolves its login once, where it is first checked, and carries it as `LaunchLogin::Pinned` (`crates/rimz/src/store/writer.rs::LaunchLogin`) to every later gate, batch, and connection. Supervised retries, preflight, and tier routing never re-read the room default mid-operation.

The Codex broker is the one shared reader that follows the room default. It re-resolves the default before each request, `initialize` included, respawns its child when the home or credentials move, and stamps every successful response with the home and auth stamp of the child that served it. The consumer accepts a broker response only when that stamp names the canonical home and auth stamp it expects for the login, and discards the attempt otherwise. A broker that is old, or whose child runs another home, falls through to the daemon and the cold spawn, and no reading lands under another login's key. A context refresh for a Codex agent stamped with a login other than the room default bypasses the broker and connects under its own login.

## Agent stamps

The durable stamp is the login of the agent's latest process. Each launch path sets it this way:

| Path | Login |
| --- | --- |
| a fresh launch: peer, team member, loop, interactive | `LaunchLogin::RoomDefault`, resolved as pin-or-live under the launch batch's workspace lock, unless its `SupervisedRunRequest::login` pins one |
| a `rimz subagents` child of its parent's kind | `Pinned` on the parent's stamp. An unstamped parent counts as `default` |
| a `rimz subagents` child of another kind | the room default for its own kind |
| restart, resume, fork, subagent follow-up, lane, team, and crash recovery | the room default, when it shares a history pool with the session's stamp ([`harness::resume::relaunch_login`](../../../crates/rimz/src/harness/resume.rs)) |
| a supervised retry | the preflighted login |
| a provider-native child | its known parent's, for the same provider, at ingress |
| a late hook adopting a provisional launch | the batch stamp |

`session_login` resolves the allocated stamp at exec, so a switch between allocation and exec cannot change the process home. Every relaunch path builds its launch from `relaunch_login`'s answer. On a mismatch, an explicit reopen refuses with the `rimz accounts use` fix (a lane or team resume refuses the whole command), and rebirth skips the session as `different account`. Every relaunch path but crash recovery, and every fresh interactive launch, then runs `ProviderLogin::health` on the login before any write ([fleet.md](../harness/fleet.md#every-path-that-builds-a-launch-layout)).

The wrapper's `agent.attached` carries the process login and moves the stamp to it. A session resumed under another account of its pool is then billed, parked, and preflighted as that account.

A session discovered in a provider home has no durable row until a hook or an attach creates one. Its [local session observation](./instances.md#local-session-observations) carries the account it was discovered under (`ProviderLogin::local_sessions`), and the transient row a snapshot seeds for it takes that account. Restart and the in-use set then read it as the room's account, never as `default`. A prior row the observation binds to keeps its own stamp.

## The account-link reconciler

[`account_links::reconcile`](../../../crates/rimz/src/agents/account_links.rs) brings a named account's home to the link set its history mode asks for. It is the one writer of links between an account home and the provider's own home. `default` has no links.

| Caller | When |
| --- | --- |
| `crates/rimz/src/cli/accounts.rs::add` | `rimz accounts add` |
| `crates/rimz/src/cli/agents_cmd/exec.rs::launch_and_supervise` | the exec wrapper, between login resolution and compile (`launch_plan::prepare_exec`) |
| `crates/rimz/src/harness/headless.rs::prepare` | a headless launch |

`launch_plan::compile` and `rimz agents explain` never write. A launch prints the report's warnings, as `accounts add` does.

### The link set

| Mode | Links |
| --- | --- |
| `shared` | every top-level entry of either home that is not unshared, plus the adapter's `history_home_entries`, created in the native home first when neither home holds them, so a first session never writes history into the account home |
| `standalone` | the adapter's `shared_home_entries` alone |

A name is unshared when it equals an adapter's `private_home_entries` entry or starts with one followed by `.`, when it is `.rimz-aside`, or when it carries `.lock` or `.tmp` as a dotted component past its start (`crates/rimz/src/agents/account_links.rs::is_transient`). No adapter restates the lock and temp rule.

| Kind | `shared_home_entries` | `private_home_entries` | `history_home_entries` |
| --- | --- | --- | --- |
| Claude | `settings.json`, `settings.local.json`, `CLAUDE.md`, `skills/`, `plugins/`, `agents/`, `commands/`, `output-styles/` | `.credentials.json`, `.claude.json`, `.oauth_refresh.lock`, `.last-update-result.json` | `projects/` |
| Codex | `config.toml`, `AGENTS.md`, `rules/` | `auth.json`, `app-server-control`, `app-server-daemon`, `packages`, `models_cache.json` | `sessions/`, `archived_sessions/` |
| Pi | `settings.json`, `models.json`, `mcp.json`, `keybindings.json`, `trust.json`, `AGENTS.md`, `CLAUDE.md`, `SYSTEM.md`, `APPEND_SYSTEM.md`, `extensions/`, `skills/`, `prompts/`, `themes/`, `npm/`, `git/` | `auth.json`, `models-store.json`, `mcp-auth.json`, `oauth.json`, `mcp.log`, `crashes.json`, `pi-debug.log`, `pi-tui-debug.log`, `pi-tui-crash.log` | `sessions/` |

### What happens to each entry

`crates/rimz/src/agents/account_links.rs::classify` decides per planned name, by link text alone:

| Account home holds | Action |
| --- | --- |
| nothing | link |
| a link with the expected text | current, untouched |
| a link with other text that reaches the same existing `(dev, ino)` | replace through `disk::atomic::symlink_atomically`. It is reported in `linked`, and it skips the live-writer gate, set-aside, and skill migration |
| a real entry the native home lacks | move it into the native home, then link. Across filesystems it stays in the account home with a warning |
| any other entry | set it aside under `<home>/.rimz-aside/<UTC timestamp>/`, then link |

Any failed identity lookup takes the set-aside path, including a dangling absolute link. Before a `skills` directory moves or is set aside, RimZ removes its own skill links from it (`skill_links::plan` with no desired links). Before a file is set aside, the adapter may carry provider state across (`adopt_shared_file`): Codex copies `[hooks.state]` entries the native `config.toml` lacks, so hook trust survives.

A link left in the account home at a name the mode no longer shares is stale and removed. A stale link at an unshared name is a defect in either mode: the provider must own that name per home. RimZ removes it without consulting the live-writer gate and reports a `ShareReport::warnings` line, which `accounts add` and a launch both print. The reconciler removes nothing but links.

The reconciler holds one machine-wide lock per kind (`disk::paths::account_lock`), because every shared account of a kind moves entries into the same native home.

### The live-writer gate

A provider appends by path, so moving a directory out from under a running provider loses its writes. Before RimZ sets aside a directory or a link to one, or removes a stale link to one, it asks two questions in order. A converged home, and a change that sets aside only files, ask neither.

1. Does another agent stamped with the account have a pane attached and a recorded process that is not dead (`crates/rimz/src/room/standing.rs::other_live_agents_on`)? A seat of the same batch that has not started and a row a rebirth recovered have no pane. A launch that failed or was killed after its pane was bound stops counting once its process is gone, before the room records its end. A count, or live rooms that cannot be read, refuses (`ShareErr::LiveAgents`, `ShareErr::LiveAgentsUnlink`).
2. With the count at zero, does a provider daemon hold live sessions under the account home (`RuntimeControlCapability::runtime_control_writes_history`)? The answer is a `DaemonSessions`: `Clear` proceeds, and a session count or `Unknown` refuses before any entry changes (`ShareErr::LiveDaemon`, `ShareErr::LiveDaemonUnlink`). A running daemon that holds no session does not block and keeps running. Every kind but Codex answers `Clear`.

Codex answers from its daemon's own records and control socket, and ignores `RIMZ_CODEX_APP_SERVER_SOCK`, which could name another account's daemon ([adapter_codex.md](./adapter_codex.md#remote-control)). A pending restart handoff (`app-server-daemon/loaded-threads.json`) answers `Unknown` even with no daemon running. Both directories it reads are private entries, so the record and the socket belong to the account in both history modes. The loaded-thread set is the evidence, not the room's session rows: a daemon-served row has no pane, a closed remote conversation's row can outlive it, and a row's stamp is the room's default at creation, not the home of the daemon that served it. A thread stays loaded for the unload delay after its last subscriber leaves ([upstream](../../externals/agent-adapter/codex-reference.md#read-methods)), so the gate keeps refusing for that long after a conversation closes. The refusal names two remedies: close the sessions and rerun once the daemon unloads them, or turn `remote_control.codex` off, rerun, and turn it back on, which restarts the daemon under the new mode (`crates/rimz/src/agents/account_links.rs::daemon_writers`).

The gate has four known gaps:

- At startup Codex deletes its restart handoff before serving and restores threads in the background, so the loaded list can be short during restoration.
- The lock serializes reconcilers only. An idle daemon can load a thread between its reply and the rename, and a conversation opened in that window writes into the set-aside directory or behind the removed link.
- Only sessions block. An idle daemon may still write a log through a link the switch removes.
- An idle daemon keeps the environment it started under. `forward_login_env` passes it `CODEX_SQLITE_HOME` only when the login sets it, which a shared login does and a standalone one does not. After a switch to standalone, its next conversation writes rollouts into the account home, and database rows into the native home if the app-server keeps the variable it inherited (provider behaviour, not observed). After a switch to shared, it runs without the variable. A `CODEX_SQLITE_HOME` the user exported reaches the daemon in either mode.

A failure after a set-aside carries the set-aside lines on the error (`ShareErr::AfterSetAside`), since a rerun would no longer report them.

### Link text

A link under `HOME` is relative, from the account home's real location to the configured native home. The reconciler takes the user's home from the ambient map's `HOME` (`crates/rimz/src/agents/account_links.rs::relative_prefix`). A link is relative when three things hold: both `HOME` and the account home canonicalize, the real account home is below the real user home, and the configured native home (absolute, lexically normalized) is below the configured `HOME` (normalized the same way). The text is then `..` per component from the real account home up to the real user home, followed by the configured native home's suffix and the entry name. Otherwise the link is the configured absolute native path. The provider home is never canonicalized for this text, so a stowed native home stays in the link.

The real account home is canonicalized once per reconcile, under the account lock, and that one value serves the link text, the stale scan, and set-aside. An account home that does not canonicalize gets absolute links, and its existing relative text is read from its configured path. The stale scan recognizes the expected text even through a symlinked `HOME`. Other links are resolved from the real account home, lexically normalized, and checked against the configured native home. A relative link set aside is rewritten as the real account home joined with its original text, without normalizing, so the kernel's physical resolution holds (`crates/rimz/src/agents/account_links.rs::set_aside`).

## See also

- [providers.md](./providers.md): the balances read under a login, and the panel per login.
- [spending.md](./spending.md#discovery): spend discovery across history pools.
- [store.md](../store.md#the-workspace-record): the `workspace.json` pins.
- [adapter_codex.md](./adapter_codex.md#remote-control): the Codex daemon the live-writer gate asks.
- [`rimz accounts`](../../reference/cli/accounts.md): the command surface for accounts.
