use super::AgentContextRecord;

impl AgentContextRecord {
    pub(crate) fn apply_fixture(&mut self, mut next: Self, existed: bool) -> bool {
        let observed_cost =
            next.context.cost.is_some() && !next.locally_priced_cost.owns_context_cost;
        if next.context.cost.is_none() {
            next.context.cost.clone_from(&self.context.cost);
        }
        if next.locally_priced_cost.is_empty() {
            next.locally_priced_cost = self.locally_priced_cost.clone();
        }
        if existed {
            next.spend_fold.clone_from(&self.spend_fold);
        }
        if observed_cost {
            next.locally_priced_cost.owns_context_cost = false;
        }
        *self = next;
        true
    }
}
