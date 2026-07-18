//! Zellij presence-plugin materialization, identity, and pipe helpers.
//!
//! The embedded wasm digest and build-stable room configuration travel into
//! each plugin generation and return in topology, letting reload skip the
//! replace-and-retire path only when a fresh writer proves both identities.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use std::{env, fs};

use kdl::{KdlDocument, KdlNode};

use super::backend::RawListedPane;
use super::{
    PRESENCE_BOOT_PIPE, PRESENCE_PIPE_TIMEOUT, PRESENCE_RETIRE_PIPE, PRESENCE_RETIRE_PROOF_TIMEOUT,
    PRESENCE_SHARE_PIPE, PRESENCE_TOPOLOGY_PIPE, TOPOLOGY_CACHE_POLL_STEP, ZellijBackend,
};
use crate::ids::{MuxName, PaneId};
use crate::mux::{MuxErr, Result};
use crate::sidebar::cache::{PresenceDesired, read_pane_topology_cache, write_presence_desired};
use crate::sidebar::timing::unix_now_ms;
use crate::store::{atomic, paths};

const EMBEDDED_PRESENCE_PLUGIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/rimz-presence-zellij.wasm"));
const PRESENCE_PLUGIN_FILE: &str = "rimz-presence-zellij.wasm";
const PRESENCE_PLUGIN_BASE_PERMISSIONS: [&str; 3] =
    ["ReadApplicationState", "RunCommands", "Reconfigure"];
const PRESENCE_PLUGIN_WEB_PERMISSION: &str = "StartWebServer";
static PRESENCE_PLUGIN_BUILD: LazyLock<String> =
    LazyLock::new(|| crate::build_id::of_bytes(EMBEDDED_PRESENCE_PLUGIN));

/// Digest of the embedded wasm generation this host loads.
pub fn presence_plugin_build() -> &'static str {
    &PRESENCE_PLUGIN_BUILD
}

/// Locate the presence-plugin wasm without writing: the
/// `RIMZ_PRESENCE_PLUGIN` override, else an already-materialized embedded
/// plugin under `$XDG_DATA_HOME/rimz/plugins/`, else a development fallback
/// beside the running executable. Owner flows call
/// [`ensure_presence_plugin_artifact`] to create/update the shared artifact.
///
/// Canonical, because Zellij keys the user's one-time permission grant on the
/// exact path string it is handed: one real artifact must read as one string
/// however rimz was invoked (symlinked bin dir, relative exe), or the user is
/// re-prompted per spelling.
pub fn presence_plugin_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RIMZ_PRESENCE_PLUGIN") {
        return PathBuf::from(path)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    if let Some(path) = materialized_presence_plugin_path() {
        return Some(path);
    }
    crate::proc::rimz_exe()
        .parent()?
        .join(PRESENCE_PLUGIN_FILE)
        .canonicalize()
        .ok()
        .filter(|path| path.is_file())
}

/// Materialize the embedded presence-plugin artifact for room-owner flows and
/// return the canonical load path. Generic topology reads use
/// [`presence_plugin_path`] so every worktree build does not rewrite the shared
/// wasm while merely asking for a cache refresh.
pub fn ensure_presence_plugin_artifact() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RIMZ_PRESENCE_PLUGIN") {
        return PathBuf::from(path)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    match materialize_presence_plugin_bytes(EMBEDDED_PRESENCE_PLUGIN, &paths::data_home()) {
        Ok(Some(path)) => path.canonicalize().ok().filter(|path| path.is_file()),
        Ok(None) => None,
        Err(err) => {
            tracing::debug!(error = %err, "materializing embedded presence plugin failed");
            None
        }
    }
    .or_else(|| {
        crate::proc::rimz_exe()
            .parent()?
            .join(PRESENCE_PLUGIN_FILE)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
    })
}

fn materialized_presence_plugin_path() -> Option<PathBuf> {
    materialized_presence_plugin_path_under(&paths::data_home())
        .canonicalize()
        .ok()
        .filter(|path| path.is_file())
}

pub(super) fn materialized_presence_plugin_path_under(data_root: &std::path::Path) -> PathBuf {
    data_root
        .join("rimz")
        .join("plugins")
        .join(PRESENCE_PLUGIN_FILE)
}

pub(super) fn materialize_presence_plugin_bytes(
    bytes: &[u8],
    data_root: &std::path::Path,
) -> std::result::Result<Option<PathBuf>, atomic::AtomicErr> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let path = materialized_presence_plugin_path_under(data_root);
    if fs::read(&path).is_ok_and(|existing| existing == bytes) {
        return Ok(Some(path));
    }
    atomic::write_bytes_atomically(&path, bytes)?;
    Ok(Some(path))
}

/// The `key=value,key=value` configuration the plugin reads at load. The
/// parse is Zellij's — split on `,` then `=` — so a `rimz` path containing
/// either separator cannot be expressed; `rimz_bin` is omitted and the plugin
/// falls back to `rimz` on the host PATH, while an inexpressible plugin URL
/// disables the runtime focus keybind rather than register a mis-targeted pipe.
/// Workspace ids are `ws_` + hex by construction, always expressible.
pub(crate) fn presence_plugin_configuration(opts: &super::super::PresencePluginOptions) -> String {
    let mut configuration = format!("workspace_id={}", opts.workspace_id.as_str());
    let plugin_url = format!("file:{}", opts.wasm.display());
    let focus_destination_expressible = !plugin_url.contains([',', '=']);
    if focus_destination_expressible {
        configuration.push_str(",plugin_url=");
        configuration.push_str(&plugin_url);
    } else {
        tracing::debug!(
            plugin_url,
            "presence plugin URL contains a plugin-configuration separator; the Zellij focus keybind is disabled",
        );
    }
    if opts.session_name.contains([',', '=']) {
        tracing::debug!(
            session = %opts.session_name,
            "session name contains a plugin-configuration separator; command-change shortcut is disabled",
        );
    } else {
        configuration.push_str(",session_name=");
        configuration.push_str(&opts.session_name);
    }
    let bin = opts.rimz_bin.to_string_lossy();
    if bin.contains([',', '=']) {
        tracing::debug!(
            rimz_bin = %bin,
            "rimz path contains a plugin-configuration separator; the plugin resolves `rimz` from PATH instead",
        );
    } else {
        configuration.push_str(",rimz_bin=");
        configuration.push_str(&bin);
    }
    configuration.push_str(",focus_follows_mouse=");
    configuration.push_str(if opts.focus_follows_mouse {
        "true"
    } else {
        "false"
    });
    configuration.push_str(",mouse_click_through=");
    configuration.push_str(if opts.mouse_click_through {
        "true"
    } else {
        "false"
    });
    // The focus chord the plugin binds at load. Grammar validation and the
    // user-facing warning live in `cli::register_focus_key` (it runs for both
    // backends); here we only guard the plugin-config separators and let the
    // plugin's own parser skip anything malformed.
    if let Some(focus_key) = opts.focus_key.as_deref() {
        if !focus_destination_expressible {
            tracing::debug!(
                focus_key,
                "the Zellij focus keybind is disabled because the plugin URL is not expressible",
            );
        } else if focus_key.contains([',', '=']) {
            tracing::debug!(
                focus_key,
                "focus_key contains a plugin-configuration separator; the Zellij focus keybind is disabled",
            );
        } else {
            configuration.push_str(",focus_key=");
            configuration.push_str(focus_key);
        }
    }
    configuration.push_str(",plugin_build=");
    configuration.push_str(presence_plugin_build());
    let config_hash = crate::build_id::of_bytes(configuration.as_bytes());
    configuration.push_str(",plugin_config=");
    configuration.push_str(&config_hash);
    configuration
}

pub(crate) fn presence_plugin_config_hash(configuration: &str) -> Option<&str> {
    configuration
        .rsplit(',')
        .next()?
        .strip_prefix("plugin_config=")
        .filter(|hash| !hash.is_empty())
}

impl ZellijBackend {
    pub(crate) fn live_presence_plugin_ids(&self, session_name: &str) -> Result<Vec<u32>> {
        let mut ids = self
            .raw_listed_panes(session_name, super::super::COMMAND_TIMEOUT)?
            .into_iter()
            .filter(is_presence_plugin_pane)
            .filter_map(|pane| u32::try_from(pane.id).ok())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    pub(super) fn converge_presence_plugin_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        self.converge_presence_plugin_for_with(
            opts,
            PRESENCE_RETIRE_PROOF_TIMEOUT,
            TOPOLOGY_CACHE_POLL_STEP,
        )
    }

    fn converge_presence_plugin_for_with(
        &self,
        opts: &super::super::PresencePluginOptions,
        timeout: Duration,
        poll_step: Duration,
    ) -> Result<()> {
        let floor_ms = unix_now_ms();
        self.ensure_presence_plugin_for(opts)?;
        match self.pipe_to_presence_plugin(opts, PRESENCE_TOPOLOGY_PIPE, "dump") {
            Ok(()) | Err(MuxErr::Timeout { .. }) => {}
            Err(err) => return Err(err),
        }

        self.retire_proven_presence_plugin_for(opts, floor_ms, timeout, poll_step);
        Ok(())
    }

    pub(super) fn ensure_presence_plugin_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        self.seed_presence_permissions(opts);
        let url = format!("file:{}", opts.wasm.display());
        let configuration = presence_plugin_configuration(opts);
        if let Some(config) = presence_plugin_config_hash(&configuration) {
            let desired = PresenceDesired {
                build: presence_plugin_build().to_owned(),
                config: config.to_owned(),
                recorded_at_ms: unix_now_ms(),
            };
            match self.runtime_paths_for_workspace(opts.workspace_id.clone()) {
                Ok(runtime) => {
                    if let Err(err) = write_presence_desired(&runtime, &desired) {
                        tracing::debug!(
                            session = %opts.session_name,
                            error = %err,
                            "recording desired presence-plugin identity failed",
                        );
                    }
                }
                Err(err) => tracing::debug!(
                    session = %opts.session_name,
                    error = %err,
                    "desired presence-plugin identity paths are unavailable",
                ),
            }
        } else {
            tracing::debug!(
                session = %opts.session_name,
                "desired presence-plugin config identity is unavailable",
            );
        }
        if opts.converge {
            // Reload a *running* plugin in place onto the current wasm —
            // `start-or-reload-plugin` converges a pipe-launched instance
            // (verified on 0.44.3: one instance throughout). It needs a
            // connected client; with none the server drops it silently (exit
            // 0 regardless), and the pipe below still ensures a plugin is
            // loaded — the upgrade then lands on the next attached reload.
            self.zellij_action(&opts.session_name)
                .args([
                    "start-or-reload-plugin".to_owned(),
                    url.clone(),
                    "--configuration".to_owned(),
                    configuration.clone(),
                ])
                .run()?;
        }
        match self.pipe_to_presence_plugin(opts, PRESENCE_BOOT_PIPE, "load") {
            Ok(()) => Ok(()),
            // The held-CLI kill: the launch is delivered, the plugin is
            // waiting on the user's one-time permission answer (or the
            // session has no client yet to surface it). Expected, not an
            // error — pokes start once the grant lands.
            Err(MuxErr::Timeout { .. }) => {
                tracing::debug!(
                    session = %opts.session_name,
                    "presence boot pipe held past its deadline (permission pending); launch delivered",
                );
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub(super) fn share_web_session_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        // Load and grant the plugin before sharing. Zellij 0.44.3 can drop
        // share_current_session() when the same pipe also launches the plugin:
        // the call races its permission grant, and cached grants emit no
        // PermissionRequestResult replay.
        self.ensure_presence_plugin_for(opts)?;
        self.pipe_to_presence_plugin(opts, PRESENCE_SHARE_PIPE, "share")
    }

    /// Ask existing presence-plugin instances to publish topology. Readers
    /// broadcast by name and degrade when none runs; only owner flows launch.
    pub(crate) fn dump_topology_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        // Generic readers reach whichever presence-plugin instances already
        // serve the session and degrade if none do; owner flows launch them.
        match self.broadcast_presence_pipe(&opts.session_name, PRESENCE_TOPOLOGY_PIPE, "dump") {
            Ok(()) | Err(MuxErr::Timeout { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub(super) fn retire_proven_presence_plugin_for(
        &self,
        opts: &super::super::PresencePluginOptions,
        floor_ms: u64,
        timeout: Duration,
        poll_step: Duration,
    ) {
        let runtime = match self.runtime_paths_for_workspace(opts.workspace_id.clone()) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::debug!(
                    session = %opts.session_name,
                    error = %err,
                    "presence retire skipped because replacement proof paths are unavailable",
                );
                return;
            }
        };
        let configuration = presence_plugin_configuration(opts);
        let Some(expected_config) = presence_plugin_config_hash(&configuration) else {
            tracing::debug!(
                session = %opts.session_name,
                "presence retire skipped because the desired config identity is unavailable",
            );
            return;
        };
        let Some(writer) = wait_for_presence_replacement(
            &runtime,
            &opts.session_name,
            floor_ms,
            presence_plugin_build(),
            expected_config,
            timeout,
            poll_step,
        ) else {
            tracing::debug!(
                session = %opts.session_name,
                "presence retire skipped because the replacement was not proven live",
            );
            return;
        };
        if let Err(err) = self.broadcast_presence_retire_for(&opts.session_name, &writer) {
            tracing::debug!(
                session = %opts.session_name,
                error = %err,
                "presence retire broadcast failed",
            );
        }
        if let Err(err) = self.sweep_stale_presence_plugins(
            &opts.session_name,
            writer.plugin_id,
            PRESENCE_PIPE_TIMEOUT,
        ) {
            tracing::debug!(
                session = %opts.session_name,
                error = %err,
                "presence force sweep skipped because plugin panes could not be listed",
            );
        }
        if let Err(err) = self.pipe_to_presence_plugin(opts, PRESENCE_BOOT_PIPE, "load") {
            tracing::debug!(
                session = %opts.session_name,
                error = %err,
                "presence boot pipe after retire failed",
            );
        }
    }

    pub(super) fn broadcast_presence_retire_for(
        &self,
        session_name: &str,
        writer: &crate::mux::zellij::pane_topology::TopologyWriter,
    ) -> Result<()> {
        let payload = serde_json::to_string(writer).map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("serializing presence retire generation failed: {err}"),
        })?;
        self.broadcast_presence_pipe(session_name, PRESENCE_RETIRE_PIPE, &payload)
    }

    fn sweep_stale_presence_plugins(
        &self,
        session_name: &str,
        accepted_plugin_id: u32,
        timeout: Duration,
    ) -> Result<()> {
        let listed = self.raw_listed_panes(session_name, timeout)?;
        for pane in listed.into_iter().filter(|pane| {
            pane.id != u64::from(accepted_plugin_id) && is_presence_plugin_pane(pane)
        }) {
            let pane_id = PaneId::from_parts(MuxName::Zellij, format!("plugin_{}", pane.id));
            if let Err(err) = self.close_pane(session_name, &pane_id) {
                tracing::debug!(
                    session = %session_name,
                    pane = %pane_id,
                    error = %err,
                    "closing stale presence-plugin pane failed",
                );
            }
        }
        Ok(())
    }

    fn broadcast_presence_pipe(
        &self,
        session_name: &str,
        pipe_name: &str,
        payload: &str,
    ) -> Result<()> {
        self.cmd()
            .args([
                "--session",
                session_name,
                "pipe",
                "--name",
                pipe_name,
                "--",
                payload,
            ])
            .run_with_timeout(PRESENCE_PIPE_TIMEOUT)
            .map(|_| ())
    }

    fn seed_presence_permissions(&self, opts: &super::super::PresencePluginOptions) {
        let cache_root = self.cache_root.clone().unwrap_or_else(paths::cache_home);
        seed_presence_permissions_in(&cache_root, opts);
    }

    fn pipe_to_presence_plugin(
        &self,
        opts: &super::super::PresencePluginOptions,
        pipe_name: &str,
        payload: &str,
    ) -> Result<()> {
        // `zellij pipe --plugin` launches the plugin if absent — the one load
        // verb that works on a clientless session (`start-or-reload-plugin`
        // refuses without a connected client) — and routes a no-op message to
        // it when running, so the call is idempotent per (url, configuration).
        let url = format!("file:{}", opts.wasm.display());
        let configuration = presence_plugin_configuration(opts);
        self.cmd()
            .args([
                "--session",
                &opts.session_name,
                "pipe",
                "--plugin",
                &url,
                "--plugin-configuration",
                &configuration,
                "--name",
                pipe_name,
                "--",
                payload,
            ])
            .run_with_timeout(PRESENCE_PIPE_TIMEOUT)
            .map(|_| ())
    }
}

fn is_presence_plugin_pane(pane: &RawListedPane) -> bool {
    pane.is_plugin
        && pane
            .title
            .as_deref()
            .is_some_and(|title| title.contains(PRESENCE_PLUGIN_FILE.trim_end_matches(".wasm")))
}

fn wait_for_presence_replacement(
    runtime: &crate::store::RuntimePaths,
    session_name: &str,
    floor_ms: u64,
    expected_build: &str,
    expected_config: &str,
    timeout: Duration,
    poll_step: Duration,
) -> Option<crate::mux::zellij::pane_topology::TopologyWriter> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(writer) = read_pane_topology_cache(runtime, session_name)
            .and_then(|cache| cache.writer)
            .filter(|writer| {
                writer.loaded_at_ms >= floor_ms
                    && writer.build.as_deref() == Some(expected_build)
                    && writer.config.as_deref() == Some(expected_config)
            })
        {
            return Some(writer);
        }
        if Instant::now() >= deadline {
            return None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(poll_step));
    }
}

fn seed_presence_permissions_in(cache_root: &Path, opts: &super::super::PresencePluginOptions) {
    let path = cache_root.join("zellij").join("permissions.kdl");
    // Zellij 0.44.3 keys the permission cache on the plugin path string, not
    // the `file:` URL accepted by `zellij pipe --plugin`; the live integration
    // test seeds this bare path and proves the grant is honored.
    let key = opts.wasm.display().to_string();
    let Some(mut document) = read_presence_permissions_document(&path) else {
        return;
    };
    if !ensure_presence_permissions_document(&mut document, &key, opts.seed_permissions) {
        return;
    }
    document.fmt();
    if let Err(err) = atomic::write_bytes_atomically(&path, document.to_string().as_bytes()) {
        tracing::debug!(
            path = %path.display(),
            error = %err,
            "seeding Zellij presence-plugin permissions failed",
        );
    }
}

fn read_presence_permissions_document(path: &Path) -> Option<KdlDocument> {
    match fs::read_to_string(path) {
        Ok(raw) => match raw.parse() {
            Ok(document) => Some(document),
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "parsing Zellij permission cache failed; rebuilding RimZ presence grant only",
                );
                Some(KdlDocument::new())
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(KdlDocument::new()),
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "reading Zellij permission cache failed",
            );
            None
        }
    }
}

fn ensure_presence_permissions_document(
    document: &mut KdlDocument,
    key: &str,
    include_web: bool,
) -> bool {
    let mut found = false;
    let mut changed = false;
    for node in document
        .nodes_mut()
        .iter_mut()
        .filter(|node| node.name().value() == key)
    {
        found = true;
        changed |= ensure_presence_permissions_node(node, include_web);
    }
    if found {
        return changed;
    }

    let mut node = KdlNode::new(key);
    ensure_presence_permissions_node(&mut node, include_web);
    document.nodes_mut().push(node);
    true
}

fn ensure_presence_permissions_node(node: &mut KdlNode, include_web: bool) -> bool {
    let had_children = node.children().is_some();
    let children = node.ensure_children();
    let mut changed = !had_children;
    for permission in PRESENCE_PLUGIN_BASE_PERMISSIONS
        .into_iter()
        .chain(include_web.then_some(PRESENCE_PLUGIN_WEB_PERMISSION))
    {
        if children.get(permission).is_none() {
            children.nodes_mut().push(KdlNode::new(permission));
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests;
