//! Workflow specifications, validation, and durable script-run transitions.

mod engine;
mod script;
mod spec;
mod state;
mod validate;

pub use engine::{WorkflowErr, advance, cancel, create, resume, wake};
pub use spec::{
    Check, Header, HeaderEdges, ScopeKind, SpecErr, Step, StepEdges, WorkflowSpec, load, schema,
};
pub use validate::{Edge, Issue, NodeRow, Validation, validate};

#[cfg(test)]
mod tests;
