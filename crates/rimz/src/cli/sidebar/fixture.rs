//! Compatibility shim until scenarios replace the fixture commands.

use super::{Result, SidebarFixtureState, SidebarSnapshot};
use clap::ValueEnum;

pub(super) fn sidebar_fixture_snapshot(state: SidebarFixtureState) -> Result<SidebarSnapshot> {
    // Every derived enum variant has a possible value.
    let value = state
        .to_possible_value()
        .expect("fixture enum value is named");
    Ok(rimz::sidebar::scenario::fixture_snapshot(value.get_name())?)
}
