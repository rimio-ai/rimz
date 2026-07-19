//! Provider-neutral process-plugin discovery, validation, and conformance API.

pub use super::adapters::plugin::{
    LoadedPlugins, PluginCheckReport, PluginDiagnostic, PluginLoadError, ProbeCheckReport,
    ProbeCheckStatus, ProbeDiagnostic, ReplayCheckReport, ReplayFinalState, ReplayRow,
    check_from_root, load_from_root, loaded, plugins_root, valid_kind,
};
