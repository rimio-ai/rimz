//! Private caller-aligned provider capability contracts.
//!
//! Ten traits, bundled into one [`AgentIntegration`] blanket. Every method
//! carries a default, and that default is the single home for "this agent does
//! not do that": an adapter writes an empty `impl` for a capability it has no
//! behavior for, and no dispatch layer restates the gap. What each trait owns
//! and what its defaults mean is
//! [docs/internals/agents/adapter.md](../../../../docs/internals/agents/adapter.md).

use super::*;

#[doc(hidden)]
pub trait CoreCapability: Send + Sync {
    /// The adapter's static identity, branding, capabilities, and
    /// classification tables. Everything `const` about an agent lives here;
    /// the trait methods own everything behavioral.
    fn spec(&self) -> &'static AgentSpec;

    /// Test-only fixtures for registry-wide adapter conformance. Keeping one
    /// record on the adapter avoids a parallel per-agent registry. Its corpus
    /// covers the complete native event surface and may retain several payload
    /// variants for broad hooks such as `PreToolUse`.
    #[cfg(test)]
    fn conformance(&self) -> AdapterConformance {
        AdapterConformance::default()
    }
}

#[doc(hidden)]
pub trait HookCapability: CoreCapability {
    /// Normalize hook-emitter process ownership before workspace or store I/O.
    fn hook_ingress(&self, pid: Option<u32>) -> HookIngressDecision {
        HookIngressDecision::Accept(HookIngressAcceptance::agent(pid))
    }

    /// Decode one native hook payload into every normalized hook output.
    /// An agent with no native hook decoder (Kiro) classifies every event as
    /// unknown, which the shared hook path reads as "nothing to record".
    fn decode_hook(&self, event_name: &str, _payload: &Value) -> Result<HookOutput> {
        Ok(HookOutput::new(ClassifiedHook {
            class: AgentHookClass::Unknown,
            ask_kind: None,
            event_name: event_name.to_owned(),
        }))
    }

    /// Derive provider-store-backed subagent lifecycle observations for parents in this workspace.
    /// Hook ingestion owns rollup deduplication and durable appends; adapters only map local truth.
    fn derive_subagent_observations(&self, _workspace: &Path) -> Vec<AgentLifecycleObservation> {
        Vec::new()
    }

    /// Correlate one identity-bearing hook with one pane-local candidate
    /// parent through provider-owned durable records. The shared hook path
    /// bounds and disambiguates candidates; adapters with no such relation
    /// source abstain.
    fn correlate_subagent(
        &self,
        _input: SubagentCorrelationInput<'_>,
    ) -> Option<SubagentCorrelation> {
        None
    }

    /// Enumerate children proven settled by provider-owned durable records.
    /// The shared hook path adopts a missing stopped child or closes an
    /// existing child bracket, then reconciles exact model/token metadata.
    fn spawned_subagents(&self, _input: SubagentSpawnInput<'_>) -> Vec<SpawnedSubagent> {
        Vec::new()
    }

    /// Canonical options for an ask whose native event does not carry a
    /// structured question list.
    fn ask_options(&self, _kind: AskKind) -> Option<Vec<AskOption>> {
        None
    }

    /// Map validated semantic answers to this agent's native TUI choreography.
    fn answer_plan(
        &self,
        _kind: AskKind,
        _questions: &[AskQuestion],
        _answers: &[AskReply],
    ) -> std::result::Result<Vec<AnswerStep>, AnswerPlanErr> {
        Err(AnswerPlanErr::Unsupported(self.spec().kind))
    }
}

#[doc(hidden)]
pub trait InstallationCapability: CoreCapability {
    /// Provider-owned files whose contents determine sidebar wiring admission
    /// or the default launch model. The list covers every file read by
    /// [`Self::hooks_installed`] and [`Self::default_launch_model`]. Resolving a
    /// path performs no provider-file I/O.
    fn wiring_input_paths(&self) -> Vec<PathBuf> {
        self.managed_integration()
            .map_or_else(Vec::new, |integration| {
                integration.wiring_input_paths(self.spec())
            })
    }

    /// Provider-owned hook and statusline file transaction, when applicable.
    fn managed_integration(&self) -> Option<&'static dyn ManagedIntegration> {
        None
    }

    /// Write or merge the adapter's hook config into the agent's per-user
    /// config file. Defaults to an explicit "not implemented" error until an
    /// adapter owns installation.
    fn install_hooks(&self) -> Result<HookInstallReport> {
        if let Some(integration) = self.managed_integration() {
            return integration.install();
        }
        Err(AgentErr::Install {
            agent: self.spec().kind,
            reason: "install not implemented for this adapter".to_owned(),
        })
    }

    /// Preview the exact per-user config write the installer would make,
    /// without touching disk. Used by the first-run consent gate.
    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        if let Some(integration) = self.managed_integration() {
            return integration.preview();
        }
        Err(AgentErr::Install {
            agent: self.spec().kind,
            reason: "install preview not implemented for this adapter".to_owned(),
        })
    }

    /// Remove the adapter's hook entries from the agent's per-user config
    /// file. Defaults to an explicit "not implemented" error.
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        if let Some(integration) = self.managed_integration() {
            return integration.uninstall();
        }
        Err(AgentErr::Install {
            agent: self.spec().kind,
            reason: "uninstall not implemented for this adapter".to_owned(),
        })
    }

    /// Whether the user's config carries any RimZ-managed hook artifact, including
    /// partial or legacy installs that are not complete enough to be considered
    /// usable by [`Self::hooks_installed`]. No-arg uninstall uses this so
    /// "ensure absent" cleans damaged configs without rewriting untouched ones.
    fn managed_hook_artifacts_present(&self) -> bool {
        self.managed_integration().map_or_else(
            || self.hooks_installed(),
            ManagedIntegration::managed_artifacts_present,
        )
    }

    /// The user's original statusline command this agent currently wraps, if
    /// any. `None` when the agent manages no statusline (Codex), or when no
    /// wrap is configured. The `rimz statusline feed` CLI calls this to find
    /// its pass-through target. Best-effort: a read/parse failure reads as
    /// `None`.
    fn wrapped_status_line_command(&self) -> Option<String> {
        self.managed_integration()
            .and_then(ManagedIntegration::wrapped_status_line_command)
    }

    /// Match the provider's invocation contract when forwarding a wrapped
    /// statusline command. Claude and Qwen evaluate shell text; Cursor uses
    /// direct argv.
    fn status_line_invocation(&self) -> StatusLineInvocation {
        StatusLineInvocation::Shell
    }

    /// The user's original `subagentStatusLine` command this agent currently
    /// wraps, if any — the pass-through target for `rimz statusline feed
    /// --subagent`. `None` when the agent manages no subagent statusline (Codex)
    /// or no wrap is configured. Best-effort: a read/parse failure reads as
    /// `None`.
    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        self.managed_integration()
            .and_then(ManagedIntegration::wrapped_subagent_status_line_command)
    }

    /// Whether this agent's per-user config currently carries RimZ-managed
    /// hooks — i.e. the user ran `rimz hooks install`. Best-effort: a missing
    /// file or any read/parse failure reads as "not installed". An agent only
    /// ever fires `rimz hooks feed` when this holds, so `rimz doctor` surfaces
    /// it — an un-wired agent is invisible, never silently broken.
    fn hooks_installed(&self) -> bool {
        self.managed_integration()
            .is_some_and(ManagedIntegration::installed)
    }

    /// RimZ-installed hook events this agent will silently skip until the
    /// user trusts them in the agent's own UI. Empty for agents without a
    /// trust gate; Codex overrides it from `[hooks.state]` in its config.
    /// RimZ cannot trust on the user's behalf, so `rimz start` and
    /// `rimz doctor` surface the fix ([`hook_trust_fix`]) instead.
    fn untrusted_installed_hooks(&self) -> Vec<String> {
        self.managed_integration()
            .map_or_else(Vec::new, ManagedIntegration::untrusted_installed_hooks)
    }
}

#[doc(hidden)]
pub trait LaunchCapability: CoreCapability {
    /// Whether a command already matched by this adapter's launch descriptors
    /// is an interactive agent process. Providers with service subcommands
    /// override this while ordinary CLIs accept the descriptor match.
    fn is_interactive_process(&self, _command: &str) -> bool {
        true
    }

    /// Model slug to use when `rimz agents` launches without a configured
    /// model, and before a lazy-registering agent reports a real session
    /// model. Defaults to the descriptor's provider fallback; adapters with
    /// user-configured launch defaults override it.
    fn default_launch_model(&self) -> Option<String> {
        self.spec().default_model.map(ToOwned::to_owned)
    }

    /// The agent's configured launch model and reasoning effort, used only as
    /// the lowest-priority card-identity fallback after native payloads and the
    /// launcher-selected preset env.
    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (None, None)
    }

    /// Probe the agent binary's version out-of-band. Producer-only and
    /// display-only: a failure leaves the provider header without a version,
    /// never affecting account truth, cache freshness, or store correctness.
    fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        version::conventional_cli_version(stdout, stderr)
    }

    fn probe_version(&self) -> Option<String> {
        probe_descriptor_version(self.spec(), &|stdout, stderr| {
            self.parse_version(stdout, stderr)
        })
    }

    /// The argv that resumes a prior session of this agent by `session_id`,
    /// launched fresh in `cwd` (the agent's worktree). The launcher seeds a
    /// reborn pane with it so a rebirth restores the conversation idle rather
    /// than coming up empty; the agent's own hooks re-fire on its
    /// `SessionStart` with `source: "resume"`, coalescing back onto the same
    /// `(kind, agent_id)` rollup row and re-stamping the new pane. `None` when
    /// the agent has no resume CLI, so [`crate::harness::resume::plan_resume`] skips it.
    /// Default `None` keeps the contract "implement nothing else" for an agent
    /// that cannot resume yet.
    fn resume_command(&self, _session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        self.spec().launch.resume_command(_session_id)
    }

    /// The argv that launches a fresh interactive session of this agent in the
    /// pane's cwd. `extra_args` are direct agent CLI arguments from the chosen
    /// tab layout; `prompt`, when present, is passed as the agent's positional
    /// startup prompt after a `--` terminator. An agent with no launch CLI
    /// returns `None`.
    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        self.spec().launch.launch_command(extra_args, prompt)
    }

    /// Env vars pinned onto every spawn of this agent — the launch contract
    /// the integration depends on. Applied last at spawn, over any configured
    /// env, so configuration cannot switch the agent into a mode the
    /// integration cannot drive.
    fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    /// Restrict provider-native delegation for a `rimz subagents` child.
    /// The default leaves argv unchanged for providers without a verified
    /// native restriction.
    fn lockdown_subagent_args(&self, _extra_args: &mut Vec<String>) {}

    /// Provider argv that appends launch-scoped text to the system prompt.
    /// The default leaves callers to deliver the text through another channel.
    fn append_system_text_args(&self, _text: &str) -> Option<Vec<String>> {
        None
    }

    /// Restrict provider-native delegation through launch environment for a
    /// `rimz subagents` child. Applied after every configured launch variable;
    /// the default leaves the environment unchanged.
    fn lockdown_subagent_env(&self, _env: &mut BTreeMap<String, String>) {}

    /// Environment a newly-born room exports for direct agent launches. The
    /// mux layer carries this opaque map; provider policy stays in adapters.
    fn room_env(&self, _runtime: &crate::store::RuntimePaths) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

#[doc(hidden)]
pub trait SessionCapability: CoreCapability {
    fn daemon_session_evidence(&self) -> session::DaemonSessionEvidence {
        session::DaemonSessionEvidence::default()
    }

    fn turn_death_needs_pane_confirmation(&self, _error: &AgentTurnError) -> bool {
        false
    }

    fn refine_turn_death_from_frame(&self, _error: &mut AgentTurnError, _frame: &str) {}

    fn infer_turn_death_from_spent_window(
        &self,
        _error: &mut AgentTurnError,
        _capacity: Option<&ProviderCapacity>,
        _now: Timestamp,
    ) {
    }

    #[cfg(feature = "testkit")]
    fn discover_local_sessions_under(
        &self,
        _home: &Path,
        _workspaces: &[&Path],
    ) -> Vec<LocalSessionObservation> {
        Vec::new()
    }

    /// Probe a provider-owned transcript for a resting interruption marker on
    /// one exact session. The store reaper calls this only after proving that
    /// two active roots share one physical agent instance; adapters normalize
    /// provider evidence and perform no writes.
    fn probe_resting_interruption(
        &self,
        _agent_id: &crate::ids::AgentSessionId,
    ) -> Option<Timestamp> {
        None
    }

    /// Discover validated sessions for absolute workspaces from the provider's
    /// machine-local store. The result is pulled display truth; callers bind it
    /// only to currently live panes and never append it to the RimZ event log.
    /// Adapters whose store is shared across workspaces enumerate it once for
    /// the whole batch.
    fn discover_local_sessions(&self, _workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        Vec::new()
    }

    /// Report whether the provider's machine-local store still holds the
    /// conversation that `resume_command` would reopen for one exact session in
    /// `cwd`. Resume planning treats this as the authority on whether a stored
    /// session id is still redeemable, so an implementation answers only where
    /// it resolves the same location the provider's own resume resolves.
    /// `None` keeps planning on its recorded-transcript fallback, for adapters
    /// whose store is absent, shared, or too coarse to prove one id absent.
    fn local_conversation_present(
        &self,
        _session_id: &crate::ids::AgentSessionId,
        _cwd: &Path,
    ) -> Option<bool> {
        None
    }

    /// Parse a provider-native resumed-session command line. Implementations
    /// accept only their actual interactive launcher/engine forms and return a
    /// typed, non-empty session identity.
    fn resumed_session_id_from_cmdline(
        &self,
        _cmdline: &str,
    ) -> Option<crate::ids::AgentSessionId> {
        None
    }
}

#[doc(hidden)]
pub trait TranscriptCapability: CoreCapability {
    /// Parse main-thread transcript JSONL text into normalized conversation
    /// messages, newest last. Adapters own native event shapes and keep
    /// sidechain/subagent replay out of this stream. Defaults to no transcript
    /// surface.
    fn parse_transcript_messages(&self, _lines: &str) -> Vec<transcript::TranscriptMessage> {
        Vec::new()
    }

    /// Read and normalize one complete provider-native transcript source.
    ///
    /// JSONL adapters inherit the text-file implementation. Adapters backed by
    /// a row store override this method, so history callers never need to know
    /// whether a recorded transcript path names text or a database. The typed
    /// session id selects one conversation when a source contains many.
    fn read_transcript_messages(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
    ) -> std::io::Result<Vec<transcript::TranscriptMessage>> {
        std::fs::read_to_string(path).map(|lines| self.parse_transcript_messages(&lines))
    }

    /// Extract newly appended main-thread assistant messages from transcript
    /// JSONL text. The CLI owns the cursor and output transport; adapters own
    /// their native transcript event shapes. Defaults to filtering the
    /// normalized transcript parser.
    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        self.parse_transcript_messages(new_lines)
            .into_iter()
            .filter(|message| message.role == transcript::TranscriptRole::Assistant)
            .map(|message| message.text)
            .collect()
    }

    /// Return the current monotonic end position for a transcript source.
    /// JSONL uses bytes; row-backed adapters can use their highest row id. The
    /// position belongs to the selected session within a shared row store.
    fn transcript_position(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
    ) -> Option<transcript::TranscriptPosition> {
        std::fs::metadata(path)
            .ok()
            .map(|meta| transcript::TranscriptPosition::new(meta.len()))
    }

    /// Read assistant output after `position`, returning the next source-owned
    /// cursor. The default implements torn-write-safe JSONL byte reads.
    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
        position: transcript::TranscriptPosition,
    ) -> Option<transcript::TranscriptPage> {
        let (bytes, next) = read_transcript_lines(path, position.get())?;
        let lines = String::from_utf8_lossy(&bytes);
        Some(transcript::TranscriptPage {
            next: transcript::TranscriptPosition::new(next),
            messages: self.stream_assistant_messages(&lines),
        })
    }
}

#[doc(hidden)]
pub trait ContextCapability: CoreCapability {
    /// Resolve a model's exact context capacity from the shared price book.
    /// Lifecycle ingestion calls this only when the provider payload did not
    /// already report a window, so native signals retain precedence.
    fn context_window_for_model(&self, _model: &str, _prices: &PriceBook) -> Option<u64> {
        None
    }

    /// Translate a raw out-of-band context payload into the normalized
    /// [`AgentContext`]. The transport is the adapter's business: Claude parses
    /// the statusline JSON it is handed on stdin. Returns `None` when the
    /// adapter has no payload-driven rich-context source (Codex — it ingests
    /// out-of-band via the app-server, see [`codex::refresh_app_server_enrichment`], not from
    /// a payload) or the payload is unusable. `source` is the ingest `--source`
    /// tag, stamped onto the record so downstream knows the provenance.
    /// Display-only enrichment — it never reaches the event log or a decision.
    fn observe_context(&self, _source: &str, _payload: &Value) -> Option<ContextObservation> {
        None
    }

    /// Price one provider turn from a native lifecycle payload. The hook
    /// handler owns accumulation and deduplication; adapters only normalize the
    /// turn identity and token-price calculation.
    fn price_turn_locally(
        &self,
        _event_name: &str,
        _payload: &Value,
        _prices: &PriceBook,
    ) -> Option<LocallyPricedTurnCost> {
        None
    }

    /// Price current provider-reported context usage. The statusline command
    /// supplies the local price book and owns persistence; adapters only parse
    /// their native payload and return the normalized cost.
    fn context_cost(&self, _payload: &Value, _prices: &PriceBook) -> Option<AgentCost> {
        None
    }

    /// Harvest per-subagent enrichment from an out-of-band render payload —
    /// Claude's `subagentStatusLine` tasks today. One payload renders many child
    /// rows, so this returns one [`SubagentObservation`] per attributable task
    /// (every task carrying an `agent_id`). Empty when the adapter has no such
    /// transport (Codex) or the payload is unusable. Display-only enrichment,
    /// like [`observe_context`](Self::observe_context) — it never reaches the
    /// event log or a decision.
    fn observe_subagent_context(&self, _payload: &Value) -> Vec<SubagentObservation> {
        Vec::new()
    }

    /// Incrementally price one child's dedicated transcript from provider-native
    /// statusline context. The cursor is display-only enrichment: callers may
    /// persist and resume it, but it never feeds lifecycle or routing decisions.
    /// The price-book fingerprint gates a poisoned cursor's full replay.
    /// `None` means the adapter has no exact per-child source, or that source
    /// could not be read on this tick.
    fn subagent_cost_cursor(
        &self,
        _payload: &Value,
        _child_id: &str,
        _prior: Option<&SubagentUsageCursor>,
        _prices: &PriceBook,
        _book_fingerprint: Option<&str>,
    ) -> Option<SubagentUsageCursor> {
        None
    }

    /// A detached `rimz` helper to spawn after a lifecycle event or producer
    /// tick — the out-of-band enrichment lane. The caller spawns it with fresh,
    /// fully-nulled stdio and never waits, so it adds no latency to the
    /// agent's turn. Display-only enrichment, never correctness. Defaults to
    /// `None` for an agent with no out-of-band refresh.
    fn context_refresh_spawn(
        &self,
        _trigger: RefreshTrigger<'_>,
        _ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        None
    }

    /// Read this provider's out-of-band context source and return the write
    /// intent for one session's sidecar — the body of the detached helper
    /// [`context_refresh_spawn`](Self::context_refresh_spawn) launches.
    /// Read-only: the adapter performs no store I/O, and the caller owns every
    /// write and the sidebar wakeup. Defaults to no intent for an agent whose
    /// enrichment arrives entirely through hooks.
    fn refresh_session_context(
        &self,
        _input: &SessionContextInput<'_>,
    ) -> Option<SessionContextRefresh> {
        None
    }

    /// Fold one rich out-of-band reading onto the stored record, returning
    /// whether anything changed. Field ownership between the provider's rich
    /// channel and its transcript is provider policy, so it lives here; the
    /// caller owns the record lock and the durable write. Only an adapter that
    /// returns [`SessionContextRefresh::observed`] implements this.
    fn merge_session_context(
        &self,
        _record: &mut crate::store::agent_context::AgentContextRecord,
        _observed: &AgentContext,
    ) -> bool {
        false
    }

    /// A cheap, synchronous local enrichment read to run inline after a
    /// progress-proving hook event. This is for bounded file reads that are
    /// lighter than the store write already performed by the hook or cheap
    /// enough for a producer tick; network, subprocess, broker, or app-server
    /// work belongs in [`context_refresh_spawn`](Self::context_refresh_spawn).
    /// The adapter returns mapped fields only and never writes the sidecar
    /// itself.
    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        _ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        None
    }
}

#[doc(hidden)]
pub trait AccountCapability: CoreCapability {
    fn prepare_reset_credit(&self) -> std::result::Result<account::ResetCreditOffer, String> {
        Err(format!(
            "{} does not support reset-credit redemption",
            self.spec().kind
        ))
    }

    /// Probe this provider's account/plan login out-of-band — a `claude auth
    /// status` fork, an auth-file read. Producer-only and best-effort: the
    /// elected sidebar producer single-flights it behind a TTL'd cache, so it
    /// never runs on the per-tick hot path (see [`account`]). Defaults to
    /// [`account::AccountProbe::LoggedOut`] for an agent with no out-of-band
    /// login surface.
    fn probe_account(&self) -> account::AccountProbe {
        account::AccountProbe::LoggedOut
    }

    /// Query this provider's account usage (included rate-limit windows + paid
    /// extra credits) from its selected local credentials. The identity and
    /// normalized [`AccountUsageSnapshot`] return together in one
    /// [`AccountUsageProbe`]. Producer-only and best-effort — the shared
    /// refresh driver single-flights it behind the credits cache and keys the
    /// cache TTL on the returned arm. Defaults to
    /// [`AccountUsageProbe::Unsupported`] for an agent with no account-usage surface.
    fn probe_account_usage(&self) -> AccountUsageProbe {
        AccountUsageProbe::Unsupported
    }

    /// Resolve exact provider-account applicability and identity for a managed launch.
    fn resolve_managed_launch(
        &self,
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
        _model: Option<&str>,
        _argv: &[String],
    ) -> ManagedLaunchState {
        ManagedLaunchState::Unsupported
    }

    /// Probe the provider's own realtime account channel while idle.
    /// Producer-only, best-effort, and read-only: no store writes happen in the
    /// adapter, and the caller owns every cache merge. `RuntimePaths` lets the
    /// adapter locate its local sockets.
    fn probe_realtime_account_usage(
        &self,
        _runtime: &crate::RuntimePaths,
    ) -> Option<AccountUsageSnapshot> {
        None
    }

    /// Dynamic remote-control state from this provider's own settings and
    /// account facts.
    /// Best-effort and read-only: failures return the default "off/unknown"
    /// state. The sidebar uses this only to light a capability-gated flag.
    fn remote_control_status(&self, _account: Option<&AgentAccount>) -> RemoteControlStatus {
        RemoteControlStatus::default()
    }
}

#[doc(hidden)]
pub trait SpendingCapability: CoreCapability {
    /// Conversation/store candidates used to resolve a live session for
    /// transcript UI and session-cost lookup. Historical fleet spending uses
    /// [`spending_sources`](Self::spending_sources) instead.
    fn transcript_files(&self) -> Vec<PathBuf> {
        let mut files = self
            .spending_sources()
            .into_iter()
            .flat_map(|source| source.complete_files())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files
    }

    /// Read the durable identity of one logical transcript or provider store.
    /// Adapters attach every provider-owned companion whose bytes participate
    /// in parsing the primary path; callers use this single boundary for cache
    /// invalidation without discovering companions as duplicate sources.
    fn transcript_stat(&self, path: &Path) -> Option<TranscriptStat> {
        TranscriptStat::from_path(path)
    }

    /// Declarative historical-spend stores consumed by the persistent
    /// [`spending::SpendingWalker`]. An adapter with no historical spend keeps
    /// the empty default even when it exposes transcripts for session lookup.
    fn spending_sources(&self) -> Vec<spending::SpendingSource> {
        Vec::new()
    }

    /// Resolve the local conversation/store that carries a live session's spend.
    /// `prior_path` is the path already published in the context sidecar, so a
    /// steady session pays one stat before falling back to provider discovery.
    /// Providers with one-file-per-session stores usually need no override; stores
    /// whose file name does not contain the session id provide their own mapping.
    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        self.transcript_files().into_iter().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id))
        })
    }

    /// Resolve every file whose bytes carry one session's spend. Providers
    /// that split a session across companion files override this method.
    fn session_spend_transcripts(
        &self,
        session_id: &str,
        prior_path: Option<&Path>,
    ) -> Vec<PathBuf> {
        self.session_transcript(session_id, prior_path)
            .into_iter()
            .collect()
    }

    /// Parse one transcript file into cost entries for the spending pass,
    /// resuming from `resume` when given: read only past its offset, restore
    /// any cross-line state it carries, and return entries the cache appends
    /// to the file's set. `None` parses the whole file cold. An adapter whose
    /// transcripts log dollars reads them verbatim and ignores `prices`; a
    /// token-only adapter (Codex) multiplies its counts through the book.
    /// Read-only and sidebar-safe — spend parsing never writes the store or
    /// blocks on a socket (CI grep on the adapter `spend.rs` files).
    fn parse_spend(
        &self,
        _path: &Path,
        _resume: Option<&spending::SpendCursor>,
        _prices: &PriceBook,
    ) -> spending::SpendParse {
        spending::SpendParse::default()
    }
}

#[doc(hidden)]
pub trait RuntimeControlCapability: CoreCapability {
    fn runtime_control_readiness(
        &self,
        _enabled: bool,
    ) -> runtime_control::RuntimeControlReadiness {
        runtime_control::RuntimeControlReadiness::Disabled
    }

    fn ensure_runtime_control(&self, _enabled: bool) {}

    /// Fill the preconditions this host needs before it can be launched at all —
    /// a recorded first-run answer, a required file — without starting anything.
    /// Readiness gates read the result, so this runs before they judge the host.
    /// Starting a daemon belongs to [`Self::ensure_runtime_control`].
    fn prepare_runtime_control(&self, _enabled: bool) {}

    /// Report whether this host still serves `project_root` from the host's own
    /// durable record. A provider that keeps no such record leaves the default,
    /// so callers see "no evidence" instead of a fabricated verdict.
    fn runtime_control_liveness(
        &self,
        _project_root: &std::path::Path,
    ) -> runtime_control::RuntimeControlLiveness {
        runtime_control::RuntimeControlLiveness::Unknown
    }

    fn reconcile_runtime_control(
        &self,
        _enabled: bool,
    ) -> std::result::Result<(), runtime_control::RuntimeControlError> {
        Ok(())
    }

    fn runtime_control_advisory(&self) -> Option<String> {
        None
    }

    fn runtime_control_wiring_input_path(&self) -> Option<PathBuf> {
        None
    }
}

/// Every capability an agent integration can implement, in one object.
///
/// Each capability trait carries a default for every method, so an adapter
/// implements the traits it has behavior for and leaves the rest empty. The
/// default *is* the "unsupported" answer — it has one home, here, and no
/// dispatch layer restates it.
#[doc(hidden)]
pub trait AgentIntegration:
    CoreCapability
    + HookCapability
    + InstallationCapability
    + LaunchCapability
    + SessionCapability
    + TranscriptCapability
    + ContextCapability
    + AccountCapability
    + SpendingCapability
    + RuntimeControlCapability
{
}

impl<T> AgentIntegration for T where
    T: CoreCapability
        + HookCapability
        + InstallationCapability
        + LaunchCapability
        + SessionCapability
        + TranscriptCapability
        + ContextCapability
        + AccountCapability
        + SpendingCapability
        + RuntimeControlCapability
{
}
