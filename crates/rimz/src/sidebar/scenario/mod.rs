//! Scripted snapshot sources for contributor sidebar playback.

use std::time::Duration;

use crate::ids::AgentKind;
use crate::store::snapshot::SidebarSnapshot;

mod model;
mod parser;
mod presets;

#[derive(Clone, Copy)]
enum Preset {
    Empty,
    Fleet,
    Provider,
    Cockpit,
    Focus,
    Economy,
    Reach,
    Clocks,
    ClocksDone,
    ClocksNerdFont,
    ClocksDoneNerdFont,
}

impl Preset {
    const NAMED: [(&str, Self); 11] = [
        ("empty", Self::Empty),
        ("fleet", Self::Fleet),
        ("provider", Self::Provider),
        ("cockpit", Self::Cockpit),
        ("focus", Self::Focus),
        ("economy", Self::Economy),
        ("reach", Self::Reach),
        ("clocks", Self::Clocks),
        ("clocks-done", Self::ClocksDone),
        ("clocks-nerd-font", Self::ClocksNerdFont),
        ("clocks-done-nerd-font", Self::ClocksDoneNerdFont),
    ];

    fn named(name: &str) -> Option<Self> {
        Self::NAMED
            .iter()
            .find_map(|(key, preset)| (*key == name).then_some(*preset))
    }
}

pub fn fixture_snapshot(name: &str) -> Result<SidebarSnapshot, ScenarioError> {
    let preset =
        Preset::named(name).ok_or_else(|| ScenarioError::new(1, 1, "unknown fixture preset"))?;
    Ok(presets::snapshot(preset))
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}, column {column}: {fix}")]
pub struct ScenarioError {
    line: usize,
    column: usize,
    fix: String,
}

impl ScenarioError {
    fn new(line: usize, column: usize, fix: impl Into<String>) -> Self {
        Self {
            line,
            column,
            fix: fix.into(),
        }
    }
}

pub(crate) struct Scenario {
    pub(crate) name: String,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) length: Duration,
    pub(crate) beats: Vec<Beat>,
    initial: model::Room,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Playback consumes beat labels in the next task.")
)]
pub(crate) struct Beat {
    pub(crate) at: Duration,
    pub(crate) label: String,
    pub(crate) action: Action,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Playback dispatches UI beats in the next task.")
)]
pub(crate) enum Action {
    Ui(UiAction),
    Agent { handle: String, verb: Verb },
}

pub(crate) enum Verb {
    Idle,
    Runs(String),
    Spawns {
        handle: String,
        kind: AgentKind,
        task: Option<String>,
    },
    Waits {
        duration: Duration,
        what: Option<String>,
    },
    Asks(String),
    Answered,
    Done(Option<String>),
    Fails(String),
    Leaves,
    Set(Field),
}

pub(crate) enum Field {
    Context(u8),
    Cost(f64),
    Tokens(u64),
    Model(String),
}

struct AgentSpec {
    handle: String,
    kind: AgentKind,
    group: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UiAction {
    Select(String),
    Expand(String),
    Collapse(String),
    Click(String),
    Key(String),
    Scroll { up: bool, count: u16 },
}

impl Scenario {
    pub(crate) fn parse(name: &str, source: &str) -> Result<Self, ScenarioError> {
        parser::parse(name, source)
    }

    pub(crate) fn at(&self, at: Duration) -> SidebarSnapshot {
        let mut room = self.initial.clone();
        for beat in self.beats.iter().take_while(|beat| beat.at <= at) {
            if let Action::Agent { handle, verb } = &beat.action {
                room.apply(handle, verb, presets::epoch() + beat.at);
            }
        }
        room.snapshot(presets::epoch() + at)
    }
}

pub fn check(name: &str, source: &str) -> Result<String, ScenarioError> {
    let scenario = Scenario::parse(name, source)?;
    for beat in &scenario.beats {
        scenario.at(beat.at);
    }
    scenario.at(Duration::ZERO);
    Ok(format!(
        "{} beats, {}s",
        scenario.beats.len(),
        scenario.length.as_secs_f64()
    ))
}

#[cfg(test)]
mod tests;
