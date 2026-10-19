//! Line grammar with token spans and typed semantic admission.

use std::collections::BTreeSet;
use std::time::Duration;

use winnow::ascii::digit1;
use winnow::combinator::{alt, delimited, opt, preceded, repeat};
use winnow::token::{any, none_of, take_while};
use winnow::{ModalResult, Parser};

use super::{
    Action, AgentSpec, Beat, Field, Preset, Scenario, ScenarioError, UiAction, Verb, model::Room,
    presets::epoch,
};
use crate::agents::{ConcernCoverage, IntegrationConcern, find_definition, known_kinds};
use crate::config::ThemeStyle;
use crate::ids::AgentKind;
use crate::utils::suggest::nearest_name;

struct Token {
    text: String,
    column: usize,
    quoted: bool,
}

struct Statement {
    tokens: Vec<Token>,
    text: String,
    label: Option<String>,
    line: usize,
    end: usize,
    next: usize,
}

impl Statement {
    fn lex(line: usize, source: &str) -> Result<Self, ScenarioError> {
        let mut input = source;
        let mut tokens = Vec::new();
        let mut label = None;
        let mut statement_end = source.len();
        loop {
            input = input.trim_start();
            if input.is_empty() {
                break;
            }
            let offset = source.len() - input.len();
            if input
                .strip_prefix('#')
                .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
            {
                label = Some(input[1..].trim().to_owned()).filter(|label| !label.is_empty());
                statement_end = offset;
                break;
            }
            let column = source[..offset].chars().count() + 1;
            let quoted = input.starts_with('"');
            let raw: ModalResult<&str> = if quoted {
                delimited(
                    '"',
                    repeat::<_, _, (), _, _>(
                        0..,
                        alt((preceded('\\', any).void(), none_of(['"', '\\']).void())),
                    ),
                    '"',
                )
                .take()
                .parse_next(&mut input)
            } else {
                take_while(1.., |ch: char| !ch.is_whitespace() && ch != '"').parse_next(&mut input)
            };
            let raw = raw.map_err(|_| {
                ScenarioError::new(line, column, "expected a complete quoted string")
            })?;
            let text = if quoted {
                serde_json::from_str(raw).map_err(|_| {
                    ScenarioError::new(line, column, "use a JSON-escaped quoted string")
                })?
            } else {
                raw.to_owned()
            };
            tokens.push(Token {
                text,
                column,
                quoted,
            });
            if !input.is_empty() && !input.starts_with(char::is_whitespace) {
                return Err(ScenarioError::new(
                    line,
                    source[..source.len() - input.len()].chars().count() + 1,
                    "separate arguments with a space",
                ));
            }
        }
        Ok(Self {
            tokens,
            text: source[..statement_end].trim().to_owned(),
            label,
            line,
            end: source.chars().count() + 1,
            next: 0,
        })
    }

    fn take(&mut self, expected: &str) -> Result<&Token, ScenarioError> {
        let index = self.next;
        self.next += 1;
        self.tokens
            .get(index)
            .ok_or_else(|| ScenarioError::new(self.line, self.end, format!("expected {expected}")))
    }

    fn quoted(&mut self, expected: &str) -> Result<String, ScenarioError> {
        let line = self.line;
        let token = self.take(expected)?;
        if !token.quoted {
            return Err(ScenarioError::new(
                line,
                token.column,
                format!("expected quoted {expected}"),
            ));
        }
        Ok(token.text.clone())
    }

    fn optional_quote(&mut self, expected: &str) -> Result<Option<String>, ScenarioError> {
        if self.next == self.tokens.len() {
            return Ok(None);
        }
        self.quoted(expected).map(Some)
    }

    fn finish(&self) -> Result<(), ScenarioError> {
        if let Some(token) = self.tokens.get(self.next) {
            return Err(ScenarioError::new(
                self.line,
                token.column,
                "unexpected argument",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Start,
    Header,
    Preset,
    Agents,
    Beats,
}

pub(super) fn parse(name: &str, source: &str) -> Result<Scenario, ScenarioError> {
    let mut scenario = Scenario {
        name: name.to_owned(),
        width: 54,
        height: 34,
        length: Duration::ZERO,
        beats: Vec::new(),
        initial: Room::new(Preset::Empty),
    };
    let mut known = scenario.initial.clone();
    let mut phase = Phase::Start;
    let mut length = None;
    let mut pet = None;
    let mut style = None;
    for (index, source) in source.lines().enumerate() {
        let line = index + 1;
        let mut statement = Statement::lex(line, source)?;
        if statement.tokens.is_empty() {
            continue;
        }
        let first = statement.take("statement")?.text.clone();
        match first.as_str() {
            "scenario" => {
                if !matches!(phase, Phase::Start) {
                    return Err(ScenarioError::new(line, 1, "scenario header must be first"));
                }
                scenario.name = statement.take("scenario name")?.text.clone();
                if !safe_name(&scenario.name) {
                    return Err(ScenarioError::new(
                        line,
                        10,
                        "use a scenario name without path separators or control characters",
                    ));
                }
                let mut settings = BTreeSet::new();
                while statement.next < statement.tokens.len() {
                    let setting = statement.take("setting")?;
                    let column = setting.column;
                    let setting = setting.text.clone();
                    if !settings.insert(setting.clone()) {
                        return Err(ScenarioError::new(line, column, "setting is repeated"));
                    }
                    match setting.as_str() {
                        "width" => scenario.width = positive(&mut statement, "width")?,
                        "height" => scenario.height = positive(&mut statement, "height")?,
                        "length" => length = Some(duration(statement.take("duration")?, line)?),
                        "pets" => {
                            let value = statement.take("pet or off")?.text.clone();
                            pet = (value != "off").then_some(value);
                        }
                        "glyphs" => {
                            let value = statement.take("nerd-font or unicode")?;
                            style = Some(match value.text.as_str() {
                                "nerd-font" => ThemeStyle::Modern,
                                "unicode" => ThemeStyle::Default,
                                _ => {
                                    return Err(unknown(
                                        line,
                                        value,
                                        "glyph set",
                                        ["nerd-font", "unicode"],
                                    ));
                                }
                            });
                        }
                        _ => {
                            return Err(unknown_at(
                                line,
                                column,
                                &setting,
                                "setting",
                                ["width", "height", "length", "pets", "glyphs"],
                            ));
                        }
                    }
                }
                phase = Phase::Header;
            }
            "preset" => {
                if !matches!(phase, Phase::Start | Phase::Header) {
                    return Err(ScenarioError::new(
                        line,
                        1,
                        "preset must precede agents and beats",
                    ));
                }
                let token = statement.take("preset name")?;
                let preset = Preset::named(&token.text).ok_or_else(|| {
                    unknown(line, token, "preset", Preset::NAMED.map(|(name, _)| name))
                })?;
                scenario.initial = Room::new(preset);
                known = scenario.initial.clone();
                phase = Phase::Preset;
            }
            "agent" => {
                if matches!(phase, Phase::Beats) {
                    return Err(ScenarioError::new(
                        line,
                        1,
                        "agent declarations must precede beats",
                    ));
                }
                let token = statement.take("@handle")?;
                let handle = fresh_handle(token, line, &known)?;
                let kind = kind(statement.take("agent kind")?, line)?;
                let group = if statement.next < statement.tokens.len() {
                    let token = statement.take("in")?;
                    if token.text != "in" {
                        return Err(ScenarioError::new(
                            line,
                            token.column,
                            "expected in <group>",
                        ));
                    }
                    let group = statement.take("group")?;
                    let valid = group
                        .text
                        .strip_prefix('#')
                        .map_or_else(|| safe_name(&group.text), crate::channel::valid_name);
                    if !valid {
                        return Err(ScenarioError::new(
                            line,
                            group.column,
                            "use a channel or worktree name, not a path",
                        ));
                    }
                    group.text.clone()
                } else {
                    "main".to_owned()
                };
                scenario.initial.declare(
                    &AgentSpec {
                        handle,
                        kind,
                        group,
                    },
                    None,
                    epoch(),
                );
                known = scenario.initial.clone();
                phase = Phase::Agents;
            }
            _ => {
                let at = duration(&statement.tokens[0], line)?;
                if scenario.beats.last().is_some_and(|beat| beat.at > at) {
                    return Err(ScenarioError::new(line, 1, "beat times must not decrease"));
                }
                let target = statement.take("@handle or ui")?;
                let target_column = target.column;
                let target = target.text.clone();
                let action = if target == "ui" {
                    Action::Ui(ui(&mut statement, &known)?)
                } else {
                    let handle = live_handle_at(&target, target_column, line, &known)?;
                    let verb = verb(&mut statement, &known, &known.agents[&handle].kind, at)?;
                    Action::Agent { handle, verb }
                };
                statement.finish()?;
                if let Action::Agent { handle, verb } = &action {
                    known.apply(handle, verb, epoch() + at);
                }
                scenario.beats.push(Beat {
                    at,
                    label: statement
                        .label
                        .clone()
                        .unwrap_or_else(|| statement.text.clone()),
                    action,
                });
                phase = Phase::Beats;
            }
        }
        statement.finish()?;
    }
    let last = scenario.beats.last().map_or(Duration::ZERO, |beat| beat.at);
    if !safe_name(&scenario.name) {
        return Err(ScenarioError::new(
            1,
            1,
            "use a scenario name without path separators or control characters",
        ));
    }
    scenario.length = length.unwrap_or_else(|| last + Duration::from_secs(3));
    if scenario.length.is_zero()
        || scenario.length <= last
        || epoch().checked_add(scenario.length).is_err()
    {
        return Err(ScenarioError::new(
            1,
            1,
            "length must be positive and include every beat within the timestamp range",
        ));
    }
    scenario.initial.snapshot.theme.pets.enabled = pet.is_some();
    if let Some(pet) = pet {
        scenario.initial.snapshot.theme.pets.pet = pet;
    }
    if let Some(style) = style {
        scenario.initial.snapshot.theme.style = Some(style);
    }
    Ok(scenario)
}

fn positive(statement: &mut Statement, setting: &str) -> Result<u16, ScenarioError> {
    let line = statement.line;
    let token = statement.take("positive cell count")?;
    token
        .text
        .parse::<u16>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            ScenarioError::new(line, token.column, format!("{setting} must be 1..65535"))
        })
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

fn unknown<'a>(
    line: usize,
    token: &Token,
    category: &str,
    names: impl IntoIterator<Item = &'a str>,
) -> ScenarioError {
    unknown_at(line, token.column, &token.text, category, names)
}

fn unknown_at<'a>(
    line: usize,
    column: usize,
    text: &str,
    category: &str,
    names: impl IntoIterator<Item = &'a str>,
) -> ScenarioError {
    let suggestion = nearest_name(text, names)
        .map_or_else(String::new, |name| format!(", did you mean {name:?}?"));
    ScenarioError::new(
        line,
        column,
        format!("unknown {category} {text:?}{suggestion}"),
    )
}

fn handle(token: &Token, line: usize) -> Result<String, ScenarioError> {
    token
        .text
        .strip_prefix('@')
        .filter(|name| {
            !name.is_empty()
                && name.chars().all(|ch| {
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_')
                })
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ScenarioError::new(
                line,
                token.column,
                "expected @handle using lowercase letters, digits, - or _",
            )
        })
}

fn fresh_handle(token: &Token, line: usize, room: &Room) -> Result<String, ScenarioError> {
    let handle = handle(token, line)?;
    if room.agents.contains_key(&handle) {
        return Err(ScenarioError::new(
            line,
            token.column,
            format!("handle @{handle} is already declared"),
        ));
    }
    Ok(handle)
}

fn live_handle_at(
    text: &str,
    column: usize,
    line: usize,
    room: &Room,
) -> Result<String, ScenarioError> {
    let handle = handle(
        &Token {
            text: text.to_owned(),
            column,
            quoted: false,
        },
        line,
    )?;
    if !room.agents.contains_key(&handle) {
        return Err(ScenarioError::new(
            line,
            column,
            format!("unknown handle @{handle}; declare it with agent or spawns first"),
        ));
    }
    Ok(handle)
}

fn kind(token: &Token, line: usize) -> Result<AgentKind, ScenarioError> {
    find_definition(&token.text)
        .map(|definition| AgentKind::new_unchecked(definition.spec().kind.to_owned()))
        .ok_or_else(|| unknown(line, token, "kind", known_kinds()))
}

fn reachable(
    line: usize,
    column: usize,
    label: &str,
    kind: &AgentKind,
    concern: IntegrationConcern,
) -> Result<(), ScenarioError> {
    if find_definition(kind.as_str()).is_some_and(|definition| {
        matches!(
            definition.concern_coverage(concern),
            ConcernCoverage::Unsupported { .. }
        )
    }) {
        return Err(ScenarioError::new(
            line,
            column,
            format!("{label} is not reachable for kind {kind}"),
        ));
    }
    Ok(())
}

fn verb(
    statement: &mut Statement,
    room: &Room,
    kind: &AgentKind,
    at: Duration,
) -> Result<Verb, ScenarioError> {
    let line = statement.line;
    let token = statement.take("verb")?;
    let column = token.column;
    let verb = token.text.clone();
    Ok(match verb.as_str() {
        "idle" => Verb::Idle,
        "runs" => Verb::Runs(statement.quoted("task")?),
        "spawns" => {
            reachable(line, column, "spawns", kind, IntegrationConcern::Subagents)?;
            let handle = fresh_handle(statement.take("@child")?, line, room)?;
            let kind = self::kind(statement.take("child kind")?, line)?;
            let task = statement.optional_quote("task")?;
            Verb::Spawns { handle, kind, task }
        }
        "waits" => {
            let token = statement.take("duration")?;
            let duration = duration(token, line)?;
            if (epoch() + at).checked_add(duration).is_err() {
                return Err(ScenarioError::new(
                    line,
                    token.column,
                    "wait deadline exceeds the timestamp range",
                ));
            }
            Verb::Waits {
                duration,
                what: statement.optional_quote("wait description")?,
            }
        }
        "asks" => {
            reachable(line, column, "asks", kind, IntegrationConcern::UserQuestion)?;
            Verb::Asks(statement.quoted("question")?)
        }
        "answered" => {
            reachable(line, column, "answered", kind, IntegrationConcern::Answer)?;
            Verb::Answered
        }
        "done" => Verb::Done(statement.optional_quote("summary")?),
        "fails" => Verb::Fails(statement.quoted("reason")?),
        "leaves" => Verb::Leaves,
        "set" => Verb::Set(field(statement, kind)?),
        _ => {
            return Err(unknown_at(
                line,
                column,
                &verb,
                "verb",
                [
                    "idle", "runs", "spawns", "waits", "asks", "answered", "done", "fails",
                    "leaves", "set",
                ],
            ));
        }
    })
}

fn field(statement: &mut Statement, kind: &AgentKind) -> Result<Field, ScenarioError> {
    let line = statement.line;
    let field = statement.take("field")?;
    let column = field.column;
    let field = field.text.clone();
    let concern = match field.as_str() {
        "context" | "tokens" => IntegrationConcern::ContextUsage,
        "cost" => IntegrationConcern::RealtimeCost,
        "model" => IntegrationConcern::RichContext,
        _ => {
            return Err(unknown_at(
                line,
                column,
                &field,
                "field",
                ["context", "cost", "tokens", "model"],
            ));
        }
    };
    reachable(line, column, &field, kind, concern)?;
    let value = statement.take("value")?;
    let error = |fix| ScenarioError::new(line, value.column, fix);
    Ok(match field.as_str() {
        "context" => Field::Context(
            value
                .text
                .strip_suffix('%')
                .and_then(|text| text.parse::<u8>().ok())
                .filter(|pct| *pct <= 100)
                .ok_or_else(|| error("context must be 0%..100%"))?,
        ),
        "tokens" => Field::Tokens(
            value
                .text
                .parse()
                .map_err(|_| error("tokens must be a non-negative integer"))?,
        ),
        "cost" => Field::Cost(
            value
                .text
                .strip_prefix('$')
                .and_then(|text| text.parse::<f64>().ok())
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
                .ok_or_else(|| error("cost must be a finite non-negative dollar amount"))?,
        ),
        _ => Field::Model(value.text.clone()),
    })
}

fn ui(statement: &mut Statement, room: &Room) -> Result<UiAction, ScenarioError> {
    let line = statement.line;
    let token = statement.take("UI verb")?;
    let column = token.column;
    let verb = token.text.clone();
    Ok(match verb.as_str() {
        "select" | "expand" | "collapse" | "click" => {
            let token = statement.take("@handle")?;
            let handle = live_handle_at(&token.text, token.column, line, room)?;
            if let Some(parent) = &room.agents[&handle].parent {
                return Err(ScenarioError::new(
                    line,
                    token.column,
                    format!("@{handle} is a child of @{parent}; use ui {verb} @{parent} instead"),
                ));
            }
            match verb.as_str() {
                "select" => UiAction::Select(handle),
                "expand" => UiAction::Expand(handle),
                "collapse" => UiAction::Collapse(handle),
                _ => UiAction::Click(handle),
            }
        }
        "key" => UiAction::Key(statement.take("key")?.text.clone()),
        "scroll" => {
            let token = statement.take("up or down")?;
            let up = match token.text.as_str() {
                "up" => true,
                "down" => false,
                _ => return Err(ScenarioError::new(line, token.column, "scroll up or down")),
            };
            let count = if statement.next < statement.tokens.len() {
                positive(statement, "scroll count")?
            } else {
                1
            };
            UiAction::Scroll { up, count }
        }
        _ => {
            return Err(unknown_at(
                line,
                column,
                &verb,
                "UI verb",
                ["select", "expand", "collapse", "click", "key", "scroll"],
            ));
        }
    })
}

fn duration_part<'a>(input: &mut &'a str) -> ModalResult<(&'a str, Option<&'a str>, &'a str)> {
    (digit1, opt(preceded('.', digit1)), alt(("ms", "s", "m"))).parse_next(input)
}

pub fn parse_duration(raw: &str) -> Result<Duration, ScenarioError> {
    duration(
        &Token {
            text: raw.to_owned(),
            column: 1,
            quoted: false,
        },
        1,
    )
}

fn duration(token: &Token, line: usize) -> Result<Duration, ScenarioError> {
    let invalid = || {
        ScenarioError::new(
            line,
            token.column,
            "use a duration such as 500ms, 1.5s or 1m30s within the timestamp range",
        )
    };
    let parts: Vec<_> = repeat(1.., duration_part)
        .parse(&token.text)
        .map_err(|_| invalid())?;
    let mut nanos = 0u128;
    for (whole, fraction, unit) in parts {
        let scale = match unit {
            "ms" => 1_000_000u128,
            "s" => 1_000_000_000,
            _ => 60_000_000_000,
        };
        let whole = whole
            .parse::<u128>()
            .ok()
            .and_then(|value| value.checked_mul(scale))
            .ok_or_else(invalid)?;
        let fraction = if let Some(fraction) = fraction {
            let denominator = 10u128
                .checked_pow(fraction.len() as u32)
                .ok_or_else(invalid)?;
            let numerator = fraction
                .parse::<u128>()
                .ok()
                .and_then(|value| value.checked_mul(scale))
                .ok_or_else(invalid)?;
            if numerator % denominator != 0 {
                return Err(invalid());
            }
            numerator / denominator
        } else {
            0
        };
        nanos = nanos
            .checked_add(whole)
            .and_then(|value| value.checked_add(fraction))
            .ok_or_else(invalid)?;
    }
    let seconds = u64::try_from(nanos / 1_000_000_000).map_err(|_| invalid())?;
    let duration = Duration::new(seconds, (nanos % 1_000_000_000) as u32);
    epoch().checked_add(duration).map_err(|_| invalid())?;
    Ok(duration)
}
