//! Workflow specifications, validation, and schema generation.

mod spec;
mod validate;

pub use spec::{
    Check, Header, HeaderEdges, ScopeKind, SpecErr, Step, StepEdges, WorkflowSpec, load, schema,
};
pub use validate::{Edge, Issue, NodeRow, Validation, validate};

#[cfg(test)]
mod tests;
