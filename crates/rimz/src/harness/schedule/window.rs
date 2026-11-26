//! The add-time window check: whether a window condition can read its provider's windows
//! before the run, for both `loop add` paths and `rimz watch`.

use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;

use crate::agents::{
    ManagedLaunchState, ProviderCapacity, RateLimitWindow, WindowSpan, find_definition,
};
use crate::disk::paths::RuntimePaths;
use crate::ids::AgentKind;

/// Why `rimz loop add` refuses a window trigger; each display carries its fix.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowRefusal {
    #[error("a window trigger reads a provider's windows; name it with --provider <kind>")]
    NoProvider,
    #[error(
        "{kind} selects an exact managed account at launch, so its windows cannot be read before the run; window triggers do not support {kind}"
    )]
    ManagedAccount { kind: String },
    #[error("cannot resolve the room's {kind} account; run `rimz accounts list`")]
    NoAccount { kind: String },
    #[error(
        "no current {kind} {span} window reading; open the room's sidebar or run `rimz accounts --refresh`"
    )]
    NoReading { kind: String, span: WindowSpan },
    #[error("{kind} has no {span} window")]
    NoWindow { kind: String, span: WindowSpan },
}

/// A provider's window of one span as `rimz loop add` reads it.
#[derive(Clone, Debug, PartialEq)]
struct WindowAtAdd {
    pub kind: AgentKind,
    pub span: WindowSpan,
    pub window: RateLimitWindow,
}

/// The provider a task's `window.*` terms read, once add can read each window
/// they name; `None` when the clauses read no window or do not parse.
pub fn window_condition_provider(
    when: &[String],
    kind: Option<&str>,
    login: Option<&crate::ids::LoginKey>,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<Option<AgentKind>, WindowRefusal> {
    let Ok(expr) = crate::harness::when::WhenExpr::parse(when) else {
        return Ok(None);
    };
    let spans = expr
        .window_spans()
        .collect::<std::collections::BTreeSet<_>>();
    let mut provider = None;
    for span in spans {
        provider = Some(window_at_add(kind, span, login, root, runtime, now)?.kind);
    }
    Ok(provider)
}

/// The stored reading of `kind`'s `span` window on `login`, the key of the
/// task's [`task_login`](crate::harness::task_login::task_login), refused in
/// the order the fixes apply: provider, account selection, account, reading,
/// window.
fn window_at_add(
    kind: Option<&str>,
    span: WindowSpan,
    login: Option<&crate::ids::LoginKey>,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<WindowAtAdd, WindowRefusal> {
    let kind = kind.ok_or(WindowRefusal::NoProvider)?;
    let owned = || kind.to_owned();
    if !matches!(
        unresolved_managed_state(root, kind),
        ManagedLaunchState::Unsupported
    ) {
        return Err(WindowRefusal::ManagedAccount { kind: owned() });
    }
    let key = login.ok_or_else(|| WindowRefusal::NoAccount { kind: owned() })?;
    // A login's model refusals read without its usage entry; with no windows there is no reading.
    let capacity = ProviderCapacity::read(runtime, key)
        .filter(|capacity| !capacity.windows.is_empty())
        .ok_or_else(|| WindowRefusal::NoReading {
            kind: owned(),
            span,
        })?;
    // Projecting to the earliest instant leaves the stored reading as cached:
    // add refuses an expired window instead of reading it as refilled.
    let window = capacity
        .window_of_span(span, Timestamp::MIN)
        .ok_or_else(|| WindowRefusal::NoWindow {
            kind: owned(),
            span,
        })?;
    if !window.lifted && window.resets_at.is_some_and(|reset| reset <= now) {
        return Err(WindowRefusal::NoReading {
            kind: owned(),
            span,
        });
    }
    Ok(WindowAtAdd {
        kind: AgentKind::new_unchecked(kind),
        span,
        window,
    })
}

fn unresolved_managed_state(root: &Path, kind: &str) -> ManagedLaunchState {
    let Some(adapter) = find_definition(kind) else {
        return ManagedLaunchState::Unsupported;
    };
    let state = adapter.resolve_managed_launch(root, &std::collections::BTreeMap::new(), None, &[]);
    if matches!(state, ManagedLaunchState::Unsupported) {
        state
    } else {
        ManagedLaunchState::Unresolved
    }
}

#[cfg(test)]
mod tests;
