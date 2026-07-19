//! Best-effort Qwen effective-provider account probe.

use crate::agents::account::AccountProbe;

pub(crate) fn probe() -> AccountProbe {
    match super::selection::resolve() {
        super::selection::SelectionState::Found(selection) => {
            AccountProbe::Found(selection.account())
        }
        super::selection::SelectionState::LoggedOut => AccountProbe::LoggedOut,
        super::selection::SelectionState::Unavailable => AccountProbe::Unavailable,
    }
}
