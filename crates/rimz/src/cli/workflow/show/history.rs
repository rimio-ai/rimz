use super::*;

pub(super) use workflow_view::history::{fold_note, tail, value};

#[cfg(test)]
use workflow_view::history::{children_out, collapsed_history, digests_out, wait_row_counts};

impl View<'_> {
    pub(super) fn history(&self, out: &mut impl Write) -> Result<()> {
        workflow_view::history::render(
            self.record,
            self.spec,
            self.journal,
            self.now,
            render::prose::Prose::for_stdout(),
            out,
        )
    }
}

#[cfg(test)]
mod tests;
