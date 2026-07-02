//! Resolver allowlist and heartbeat freshness.
//!
//! Resolver trust is a per-machine allowlist — same-UID file access is *not*
//! the trust boundary per `docs/internals/agents/resolvers.md`. Only ids the user has
//! enrolled via [`allowlist::Allowlist`] engage the bridge. Engagement also
//! requires a fresh heartbeat under the workspace runtime dir; see
//! [`freshness`].

pub mod allowlist;
pub mod freshness;
pub mod heartbeat;

pub use allowlist::{Allowlist, AllowlistEntry, AllowlistErr};
pub use freshness::{
    FreshnessErr, RESOLVER_HEARTBEAT_TTL, fresh_enrolled, is_resolver_fresh, restat,
};
