//! Best-effort Qwen effective-provider account probe.

use std::collections::BTreeMap;

use crate::agents::account::AccountProbe;

pub(super) fn probe(login_env: &BTreeMap<String, String>) -> AccountProbe {
    match super::selection::resolve(login_env) {
        super::selection::SelectionState::Found(selection) => {
            AccountProbe::Found(selection.account())
        }
        super::selection::SelectionState::LoggedOut => AccountProbe::LoggedOut,
        super::selection::SelectionState::Unavailable => AccountProbe::Unavailable,
    }
}
