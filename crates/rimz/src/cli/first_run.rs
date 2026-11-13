//! Shared setup questions for `rimz setup` and the first `rimz` start: the
//! multiplexer, room keys, appearance probes and the while-you're-away rows, the
//! review screen that writes their answers, and the first-start defaults line.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::agents::AgentStatus;
use rimz::config::keys::{KeyChord, KeyMode, RoomKeymap};
use rimz::config::{
    CellAspect, ColorDepth, ConfigEditor, IdleCompact, MachineConfig, PetsConfig, RoomStyle,
    ThemeMode,
};
use rimz::ids::MuxName;
use rimz::sidebar_pane::pets::{self, PetRenderTier};
use rimz::theme::{nerd_font_probe_glyphs, nerd_font_probe_gradient};
use serde_json::Value;

use super::list_pets::{LiveGraphicsPacer, write_pet_row, write_pixel_pet_row_with_pacer};
use super::render;

const HEADER_RULE_WIDTH: usize = 48;
const SCREEN_WIDTH: usize = 80;
const AWAY_LABEL_WIDTH: usize = 16;
const KEYS_LABEL_WIDTH: usize = 14;
const REVIEW_LABEL_WIDTH: usize = 12;
const IDLE_FLOOR_DEFAULT: &str = "100k";

/// One hands-off behaviour the away question offers: each spends something of
/// the user's (a session's context, a turn, a reset credit) on its own.
#[derive(Debug)]
struct AwayRow {
    key: &'static str,
    label: &'static str,
    detail: fn(&MachineConfig) -> String,
    on_value: &'static str,
    off_value: &'static str,
    is_on: fn(&MachineConfig) -> bool,
    needs_codex: bool,
}

// Rows are compared by their config key: the table holds each key once.
impl PartialEq for AwayRow {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for AwayRow {}

static AWAY_ROWS: [AwayRow; 3] = [
    AwayRow {
        key: "resume.auto_continue",
        label: "auto-continue",
        detail: |_| "types \"continue\" once a rate limit or API error clears".to_owned(),
        on_value: "true",
        off_value: "false",
        is_on: |config| config.resume.auto_continue,
        needs_codex: false,
    },
    AwayRow {
        key: "resume.auto_redeem",
        label: "auto-redeem",
        detail: |config| {
            format!(
                "spends a Codex reset credit when a limit blocks {}+",
                config.resume.auto_redeem_min_gain
            )
        },
        on_value: "true",
        off_value: "false",
        is_on: |config| config.resume.auto_redeem,
        needs_codex: true,
    },
    AwayRow {
        key: "harness.idle_compact",
        label: "idle compaction",
        detail: |config| {
            let floor = match config.harness.idle_compact {
                IdleCompact::Off => IDLE_FLOOR_DEFAULT.to_owned(),
                floor => floor.to_string(),
            };
            format!("summarizes an idle {floor}+ conversation; detail is gone")
        },
        on_value: IDLE_FLOOR_DEFAULT,
        off_value: "off",
        is_on: |config| config.harness.idle_compact != IdleCompact::Off,
        needs_codex: false,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct OfferedRow {
    row: &'static AwayRow,
    on: bool,
    detail: String,
}

/// The three room-key answers, in option order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoomKeys {
    /// `room.style = "native"`: the multiplexer's own keys plus two RimZ chords.
    Yours,
    /// `room.style = "rimz"` with the `rimz` keymap.
    Rimz,
    /// `room.style = "rimz"` with the keymap named after the room's multiplexer.
    RimzMux,
}

impl RoomKeys {
    const ALL: [Self; 3] = [Self::Yours, Self::Rimz, Self::RimzMux];

    fn number(self) -> usize {
        Self::ALL
            .iter()
            .position(|keys| *keys == self)
            .unwrap_or_default()
            + 1
    }
}

/// The chords the room-keys text quotes, as the loaded config resolves them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyFacts {
    focus: Option<KeyChord>,
    zoom: Option<KeyChord>,
    rimz: StyleFacts,
    tmux_prefix: Option<KeyChord>,
    zellij_prefix: Option<KeyChord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StyleFacts {
    next: Option<KeyChord>,
    prev: Option<KeyChord>,
    dashboard: Option<KeyChord>,
    terminals: Vec<Option<KeyChord>>,
    needs_you: Option<KeyChord>,
    prefix: Option<KeyChord>,
    zoom: Option<KeyChord>,
    resize: Option<KeyChord>,
}

impl KeyFacts {
    fn from_config(config: &MachineConfig) -> Self {
        let native = keymap_as(config, RoomStyle::Native, None);
        let rimz = keymap_as(config, RoomStyle::Rimz, Some("rimz"));
        let prefix_of = |name| {
            keymap_as(config, RoomStyle::Rimz, Some(name))
                .style
                .map(|style| style.prefix)
        };
        Self {
            focus: chord_for(&native, KeyMode::Normal, "sidebar.focus"),
            zoom: chord_for(&native, KeyMode::Normal, "pane.zoom"),
            rimz: StyleFacts {
                next: chord_for(&rimz, KeyMode::Normal, "card.next"),
                prev: chord_for(&rimz, KeyMode::Normal, "card.prev"),
                dashboard: chord_for(&rimz, KeyMode::Normal, "tab.dashboard"),
                terminals: (1..=9)
                    .map(|n| chord_for(&rimz, KeyMode::Normal, &format!("tab.terminal.{n}")))
                    .collect(),
                needs_you: chord_for(&rimz, KeyMode::Normal, "filter.needs_you"),
                prefix: rimz.style.map(|style| style.prefix),
                zoom: chord_for(&rimz, KeyMode::Prefix, "pane.zoom"),
                resize: chord_for(&rimz, KeyMode::Prefix, "mode.resize"),
            },
            tmux_prefix: prefix_of("tmux"),
            zellij_prefix: prefix_of("zellij"),
        }
    }

    fn mux_prefix(&self, mux: MuxName) -> Option<KeyChord> {
        match mux {
            MuxName::Tmux => self.tmux_prefix,
            MuxName::Zellij => self.zellij_prefix,
        }
    }
}

/// The keymap this config would resolve under `style` and, for rimz style, the
/// named bundled keymap, keeping its prefix override and bindings.
fn keymap_as(config: &MachineConfig, style: RoomStyle, keymap: Option<&str>) -> RoomKeymap {
    let mut config = config.clone();
    config.room.style = style;
    if let Some(keymap) = keymap {
        config.keys.set_keymap(keymap);
    }
    config.room_keymap()
}

fn chord_for(keymap: &RoomKeymap, mode: KeyMode, action: &str) -> Option<KeyChord> {
    keymap
        .modes
        .get(&mode)?
        .iter()
        .find(|(_, bound)| bound.to_string() == action)
        .map(|(chord, _)| *chord)
}

/// The multiplexer question's bracket and the reason printed beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MuxChoice {
    bracket: MuxName,
    reason: &'static str,
    /// What the config resolves to outside a multiplexer with both installed.
    resolved: MuxName,
}

impl MuxChoice {
    fn new(enclosing: Option<MuxName>, stored: Option<MuxName>) -> Self {
        let resolved = stored.unwrap_or(MuxName::Tmux);
        let (bracket, reason) = match (enclosing, stored) {
            (Some(mux), _) => (mux, "the one you are inside"),
            (None, Some(mux)) => (mux, "your mux.default"),
            (None, None) => (MuxName::Tmux, "the shipped default"),
        };
        Self {
            bracket,
            reason,
            resolved,
        }
    }
}

/// Whether `rimz setup` asks for the multiplexer: both are installed and no
/// `--mux`, `--tmux` or `--zellij` flag decided.
pub(crate) fn setup_asks_mux(flag: Option<MuxName>, installed: impl Fn(MuxName) -> bool) -> bool {
    flag.is_none() && installed(MuxName::Tmux) && installed(MuxName::Zellij)
}

/// The inputs that decide whether a first plain `rimz` asks for the multiplexer
/// before it picks one. The question is the one startup prompt ahead of room
/// admission, since its answer decides which backend admits the birth.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FirstStartMux {
    pub(crate) first_run: bool,
    pub(crate) attended: bool,
    pub(crate) flag: Option<MuxName>,
    pub(crate) enclosing: Option<MuxName>,
}

impl FirstStartMux {
    /// `live_session` is asked last, since it lists both backends' sessions: a
    /// live session of this name already decides the multiplexer.
    pub(crate) fn asks(
        self,
        installed: impl Fn(MuxName) -> bool,
        live_session: impl FnOnce() -> bool,
    ) -> bool {
        self.first_run
            && self.attended
            && self.enclosing.is_none()
            && setup_asks_mux(self.flag, installed)
            && !live_session()
    }
}

/// What the loaded config holds for every question, and the facts its text quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Current {
    mux: Option<MuxChoice>,
    detected_mux: MuxName,
    style: RoomStyle,
    keymap: Value,
    status_bar_unset: bool,
    keys: KeyFacts,
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
    away: Vec<OfferedRow>,
    also_on: Vec<String>,
}

impl Current {
    fn from_config(
        config: &MachineConfig,
        truecolor_advertised: bool,
        codex_detected: bool,
        mux: MuxSetup,
    ) -> Self {
        Self {
            mux: mux
                .ask
                .then(|| MuxChoice::new(mux.enclosing, config.mux.default)),
            detected_mux: mux.detected,
            style: config.room.style,
            keymap: config.keys.keymap().clone(),
            status_bar_unset: config.mux.status_bar.is_none(),
            keys: KeyFacts::from_config(config),
            truecolor: config
                .theme
                .effective_theme_mode()
                .depth(truecolor_advertised)
                == ColorDepth::Truecolor,
            nerd_font: config.theme.glyph_set_source() == Some("nerd_font"),
            pet_enabled: config.theme.pets.enabled,
            away: AWAY_ROWS
                .iter()
                .filter(|row| codex_detected || !row.needs_codex)
                .map(|row| OfferedRow {
                    row,
                    on: (row.is_on)(config),
                    detail: (row.detail)(config),
                })
                .collect(),
            also_on: also_on(config),
        }
    }

    /// The stored keys as one of the three options, or `None` for a state the
    /// options do not cover (`vim`, the other multiplexer's keymap, an unknown
    /// or non-string keymap under rimz style).
    fn keys_option(&self, mux: MuxName) -> Option<RoomKeys> {
        if self.style == RoomStyle::Native {
            return Some(RoomKeys::Yours);
        }
        match self.keymap.as_str() {
            Some("rimz") => Some(RoomKeys::Rimz),
            Some(name) if name == mux.as_str() => Some(RoomKeys::RimzMux),
            _ => None,
        }
    }

    fn stored_keys_text(&self) -> String {
        match self.keymap.as_str() {
            Some(name) => format!("RimZ style, {name} keymap"),
            None => "RimZ style, an unreadable keys.keymap".to_owned(),
        }
    }
}

/// Settings that are on and that setup never asks about, from the loaded config.
fn also_on(config: &MachineConfig) -> Vec<String> {
    let mut items = Vec::new();
    let notifications = &config.notifications;
    if notifications.enabled {
        let asks = notifications.triggers_status(AgentStatus::Waiting);
        let fails = notifications.triggers_status(AgentStatus::Failed);
        match (asks, fails) {
            (true, true) => items.push("alerts when an agent asks or fails".to_owned()),
            (true, false) => items.push("alerts when an agent asks".to_owned()),
            (false, true) => items.push("alerts when an agent fails".to_owned()),
            (false, false) => {}
        }
    }
    if config.web.enabled {
        let interface = config.web.interface.as_str();
        let loopback = interface == "localhost"
            || interface
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        items.push(if loopback {
            "web access on loopback".to_owned()
        } else {
            format!("web access on {interface}")
        });
    }
    items
}

/// Which multiplexer the questions speak of, and whether to ask for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MuxSetup {
    pub(crate) detected: MuxName,
    pub(crate) enclosing: Option<MuxName>,
    pub(crate) ask: bool,
}

/// What the user answered in this run. A question left unanswered keeps what
/// the loaded config resolves to, so it writes nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answers {
    /// `None` until the multiplexer question is answered.
    mux: Option<MuxName>,
    /// `None` until a room-keys option is picked.
    keys: Option<RoomKeys>,
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
    /// One state per offered row, in `Current::away` order.
    away: Vec<bool>,
}

impl Answers {
    fn kept(current: &Current) -> Self {
        Self {
            mux: None,
            keys: None,
            truecolor: current.truecolor,
            nerd_font: current.nerd_font,
            pet_enabled: current.pet_enabled,
            away: current.away.iter().map(|offered| offered.on).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Question {
    Mux,
    RoomKeys,
    Color,
    Icons,
    Pet,
    Away,
}

impl Question {
    fn title(self) -> &'static str {
        match self {
            Self::Mux => "Multiplexer",
            Self::RoomKeys => "Room keys",
            Self::Color => "Color",
            Self::Icons => "Icons",
            Self::Pet => "Pet",
            Self::Away => "While you're away",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mux => "multiplexer",
            Self::RoomKeys => "room keys",
            Self::Color => "color",
            Self::Icons => "icons",
            Self::Pet => "pet",
            Self::Away => "away",
        }
    }
}

enum PetArt {
    Pending(std::thread::JoinHandle<Option<String>>),
    Ready(Option<String>),
}

impl PetArt {
    fn get(&mut self) -> Option<&str> {
        *self = match std::mem::replace(self, Self::Ready(None)) {
            Self::Pending(handle) => Self::Ready(handle.join().ok().flatten()),
            ready @ Self::Ready(_) => ready,
        };
        match self {
            Self::Ready(art) => art.as_deref(),
            Self::Pending(_) => None,
        }
    }
}

/// How the review screen was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewExit {
    /// Enter: write the answers.
    Write,
    /// `q` or end of input: write nothing.
    Quit,
    /// The hooks row's number: the caller asks the hooks question.
    Hooks,
    /// `a`: the caller walks every question again.
    WalkAll,
}

/// One run of the setting questions: what the config holds and the answers so far.
pub(crate) struct Questions {
    current: Current,
    answers: Answers,
    pet_art: PetArt,
}

impl Questions {
    pub(crate) fn new(config: &MachineConfig, mux: MuxSetup) -> Self {
        let pets_config = config.theme.pets.clone();
        let pet_art = PetArt::Pending(std::thread::spawn(move || build_pet_art(pets_config)));
        let current = Current::from_config(
            config,
            rimz::tui::truecolor(),
            rimz::harness::auto_redeem::provider_located(),
            mux,
        );
        Self::from_parts(current, pet_art)
    }

    fn from_parts(current: Current, pet_art: PetArt) -> Self {
        Self {
            answers: Answers::kept(&current),
            current,
            pet_art,
        }
    }

    fn questions(&self) -> Vec<Question> {
        let mut questions = Vec::new();
        if self.current.mux.is_some() {
            questions.push(Question::Mux);
        }
        questions.extend([
            Question::RoomKeys,
            Question::Color,
            Question::Icons,
            Question::Pet,
            Question::Away,
        ]);
        questions
    }

    /// How many questions a full walk asks.
    pub(crate) fn count(&self) -> usize {
        self.questions().len()
    }

    /// The multiplexer answer, or what the config resolves to while unanswered.
    fn mux(&self) -> MuxName {
        let current = &self.current;
        self.answers
            .mux
            .unwrap_or_else(|| current.mux.map_or(current.detected_mux, |mux| mux.resolved))
    }

    /// The room-keys answer, or the stored keys under the current multiplexer
    /// answer while unanswered; `None` is a stored state no option covers.
    fn keys(&self) -> Option<RoomKeys> {
        self.answers
            .keys
            .or_else(|| self.current.keys_option(self.mux()))
    }

    /// Ask every question in order, numbered from `first` of `total`. Returns
    /// `false` when the walk was left by `q` or end of input.
    pub(crate) fn walk(
        &mut self,
        first: usize,
        total: usize,
        input: &mut dyn BufRead,
        out: &mut dyn Write,
    ) -> Result<bool> {
        for (index, question) in self.questions().into_iter().enumerate() {
            let heading = format!("{} of {total} · {}", first + index, question.title());
            if !self.ask(question, &heading, input, out)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn ask(
        &mut self,
        question: Question,
        heading: &str,
        input: &mut dyn BufRead,
        out: &mut dyn Write,
    ) -> Result<bool> {
        writeln!(out)?;
        writeln!(out, "{}", render::paint(render::palette::header(), heading))?;
        writeln!(out)?;
        let answered = match question {
            Question::Mux => self.ask_mux(input, out)?,
            Question::RoomKeys => self.ask_room_keys(input, out)?,
            Question::Color => self.ask_color(input, out)?,
            Question::Icons => self.ask_icons(input, out)?,
            Question::Pet => self.ask_pet(input, out)?,
            Question::Away => self.ask_away(input, out)?,
        };
        Ok(answered)
    }

    fn ask_mux(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        let Some(choice) = self.current.mux else {
            return Ok(true);
        };
        let bracket = self.answers.mux.unwrap_or(choice.bracket);
        let Some(mux) = prompt_mux(bracket, choice, input, out)? else {
            return Ok(false);
        };
        self.answers.mux = Some(mux);
        Ok(true)
    }

    fn ask_room_keys(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        let mux = self.mux();
        writeln!(out, "Which keys move you between panes and agents?")?;
        writeln!(out)?;
        for line in room_keys_options(&self.current.keys, mux, self.current.status_bar_unset) {
            writeln!(out, "{line}")?;
        }
        writeln!(out)?;
        if self.keys().is_none() {
            writeln!(
                out,
                "  Now: {}. Enter keeps it.",
                self.current.stored_keys_text()
            )?;
        }
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::muted(),
                "  The vim keymap: rimz config set keys.keymap vim"
            )
        )?;
        writeln!(out)?;
        let bracket = self
            .keys()
            .map_or_else(|| "keep".to_owned(), |keys| keys.number().to_string());
        loop {
            write_prompt(out, "Room keys?", &bracket)?;
            let Some(answer) = read_answer(input, out)? else {
                return Ok(false);
            };
            if answer.is_empty() {
                return Ok(true);
            }
            match answer.parse::<usize>() {
                Ok(number @ 1..=3) => {
                    self.answers.keys = Some(RoomKeys::ALL[number - 1]);
                    return Ok(true);
                }
                _ => writeln!(
                    out,
                    "  Enter 1, 2 or 3, or press Enter to keep [{bracket}]."
                )?,
            }
        }
    }

    fn ask_color(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        writeln!(out, "{}", gradient_line())?;
        writeln!(out)?;
        write_muted(
            out,
            &[
                "  Y if the bar above is one smooth sweep; N if it breaks into flat",
                "  bands — RimZ then falls back to 256 colors.",
            ],
        )?;
        writeln!(out)?;
        let Some(truecolor) = prompt_bool("Use truecolor?", self.answers.truecolor, input, out)?
        else {
            return Ok(false);
        };
        self.answers.truecolor = truecolor;
        Ok(true)
    }

    fn ask_icons(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        writeln!(out, "{}", glyph_line())?;
        writeln!(out)?;
        write_muted(
            out,
            &[
                "  Y if you see eight distinct icons; N for boxes or ? marks — RimZ",
                "  then falls back to plain text glyphs. (Needs a Nerd Font.)",
            ],
        )?;
        writeln!(out)?;
        let Some(nerd_font) =
            prompt_bool("Use Nerd Font icons?", self.answers.nerd_font, input, out)?
        else {
            return Ok(false);
        };
        self.answers.nerd_font = nerd_font;
        Ok(true)
    }

    fn ask_pet(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        if let Some(art) = self.pet_art.get() {
            out.write_all(art.as_bytes())?;
            writeln!(out)?;
        }
        let Some(pet_enabled) = prompt_bool(
            "Want a pet? It lives in the sidebar and reacts to your fleet.",
            self.answers.pet_enabled,
            input,
            out,
        )?
        else {
            return Ok(false);
        };
        self.answers.pet_enabled = pet_enabled;
        Ok(true)
    }

    fn ask_away(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
        writeln!(
            out,
            "Each of these spends something of yours and is logged (rimz stats --assists)."
        )?;
        let count = self.current.away.len();
        let example = if count < 3 { "1 2" } else { "1 3" };
        let ask = format!("Numbers flip a row (for example: {example}). Enter keeps these.");
        loop {
            writeln!(out)?;
            for (index, offered) in self.current.away.iter().enumerate() {
                writeln!(
                    out,
                    "{}",
                    away_row_line(index + 1, offered, self.answers.away[index])
                )?;
            }
            writeln!(out)?;
            write_prompt(out, &ask, "keep")?;
            let Some(answer) = read_answer(input, out)? else {
                return Ok(false);
            };
            if answer.is_empty() {
                return Ok(true);
            }
            match parse_rows(&answer, count) {
                Some(rows) => {
                    for row in rows {
                        self.answers.away[row - 1] ^= true;
                    }
                }
                None => writeln!(
                    out,
                    "  Enter row numbers from 1 to {count}, or press Enter."
                )?,
            }
        }
    }

    /// Show the review and read one answer; a number of a setting question
    /// reopens it in place. `hooks` is the hooks row's text, numbered first.
    pub(crate) fn review(
        &mut self,
        hooks: Option<&str>,
        input: &mut dyn BufRead,
        out: &mut dyn Write,
    ) -> Result<ReviewExit> {
        loop {
            self.write_review(hooks, out)?;
            write_prompt(out, "Review?", "write")?;
            let Some(answer) = read_answer(input, out)? else {
                return Ok(ReviewExit::Quit);
            };
            if answer.is_empty() {
                return Ok(ReviewExit::Write);
            }
            if answer.eq_ignore_ascii_case("a") {
                return Ok(ReviewExit::WalkAll);
            }
            let offset = usize::from(hooks.is_some());
            let questions = self.questions();
            match answer.parse::<usize>() {
                Ok(1) if hooks.is_some() => return Ok(ReviewExit::Hooks),
                Ok(number) if (offset + 1..=offset + questions.len()).contains(&number) => {
                    let question = questions[number - offset - 1];
                    if !self.ask(question, question.title(), input, out)? {
                        return Ok(ReviewExit::Quit);
                    }
                }
                _ => writeln!(out, "  Enter a row number, a, q, or press Enter.")?,
            }
        }
    }

    fn write_review(&self, hooks: Option<&str>, out: &mut dyn Write) -> Result<()> {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::header(),
                "Review · your settings are written when you press Enter"
            )
        )?;
        writeln!(out)?;
        let mut rows = Vec::new();
        if let Some(hooks) = hooks {
            rows.push(("hooks", hooks.to_owned(), None));
        }
        for question in self.questions() {
            let (now, was) = self.review_values(question);
            let was = (now != was).then_some(was);
            rows.push((question.label(), now, was));
        }
        for (index, (label, now, was)) in rows.into_iter().enumerate() {
            for line in review_row_lines(index + 1, label, &now, was.as_deref()) {
                writeln!(out, "{line}")?;
            }
        }
        if self.keys_change() {
            write_muted(out, &self.reload_note())?;
        }
        writeln!(out)?;
        if !self.current.also_on.is_empty() {
            let first = "  Also on, not asked: ";
            for line in wrap_items(first, first.len(), &self.current.also_on) {
                writeln!(out, "{line}")?;
            }
        }
        writeln!(out, "  Any setting: rimz config set <key> <value>")?;
        writeln!(out)?;
        let changes = self.changes();
        if changes.is_empty() {
            writeln!(
                out,
                "Nothing to write: Enter leaves every setting as it is."
            )?;
        } else {
            let files = changed_files(&changes);
            writeln!(
                out,
                "Enter writes {} to {}.",
                count_noun(changes.len(), "change"),
                files.join(" and ")
            )?;
        }
        write_muted(
            out,
            &["A number reopens that question · a asks them all · q quits, settings unchanged."],
        )?;
        Ok(())
    }

    /// The answer and the stored value, as the review prints them.
    fn review_values(&self, question: Question) -> (String, String) {
        let current = &self.current;
        let answers = &self.answers;
        let on_off = |on: bool| if on { "on" } else { "off" }.to_owned();
        match question {
            Question::Mux => {
                let stored = current.mux.map_or(current.detected_mux, |mux| mux.resolved);
                (self.mux().to_string(), stored.to_string())
            }
            Question::RoomKeys => {
                let stored = current.keys_option(self.mux());
                (self.keys_text(self.keys()), self.keys_text(stored))
            }
            Question::Color => (color_text(answers.truecolor), color_text(current.truecolor)),
            Question::Icons => (icons_text(answers.nerd_font), icons_text(current.nerd_font)),
            Question::Pet => (on_off(answers.pet_enabled), on_off(current.pet_enabled)),
            Question::Away => {
                let stored: Vec<bool> = current.away.iter().map(|offered| offered.on).collect();
                (self.away_text(&answers.away), self.away_text(&stored))
            }
        }
    }

    fn keys_text(&self, keys: Option<RoomKeys>) -> String {
        let mux = self.mux();
        let facts = &self.current.keys;
        match keys {
            None => self.current.stored_keys_text(),
            Some(RoomKeys::Yours) => format!("yours ({} keys)", mux_label(mux)),
            Some(RoomKeys::Rimz) => with_prefix("RimZ".to_owned(), facts.rimz.prefix),
            Some(RoomKeys::RimzMux) => {
                with_prefix(format!("RimZ + {}", mux_label(mux)), facts.mux_prefix(mux))
            }
        }
    }

    fn away_text(&self, states: &[bool]) -> String {
        let on: Vec<&str> = self
            .current
            .away
            .iter()
            .zip(states)
            .filter(|(_, on)| **on)
            .map(|(offered, _)| offered.row.label)
            .collect();
        if on.is_empty() {
            "all off".to_owned()
        } else {
            format!("{} on", on.join(" · "))
        }
    }

    fn keys_change(&self) -> bool {
        self.changes()
            .iter()
            .any(|(key, _)| *key == "room.style" || *key == "keys.keymap")
    }

    /// How an open room picks up changed keys: `rimz reload` re-applies the
    /// keymap on both backends, but on Zellij only a fresh attach restores the
    /// user's own bindings once RimZ style is left.
    fn reload_note(&self) -> Vec<&'static str> {
        let leaves_rimz_style =
            self.current.style == RoomStyle::Rimz && self.keys() == Some(RoomKeys::Yours);
        if self.mux() == MuxName::Zellij && leaves_rimz_style {
            vec![
                "  Open rooms drop RimZ's keys at `rimz reload`; your own Zellij keys",
                "  return after a detach and reattach.",
            ]
        } else {
            vec!["  Open rooms switch keys at `rimz reload`."]
        }
    }

    /// Every setting whose answer differs from the loaded config, as the key and
    /// the value `rimz config set` takes.
    fn changes(&self) -> Vec<(&'static str, String)> {
        let current = &self.current;
        let answers = &self.answers;
        let mut changes = Vec::new();
        if let (Some(choice), Some(mux)) = (current.mux, answers.mux)
            && mux != choice.resolved
        {
            changes.push(("mux.default", mux.as_str().to_owned()));
        }
        let rimz_keymap = match self.keys() {
            Some(RoomKeys::Yours) => {
                if current.style != RoomStyle::Native {
                    changes.push(("room.style", "native".to_owned()));
                }
                None
            }
            Some(RoomKeys::Rimz) => Some("rimz"),
            Some(RoomKeys::RimzMux) => Some(self.mux().as_str()),
            None => None,
        };
        if let Some(keymap) = rimz_keymap {
            if current.style != RoomStyle::Rimz {
                changes.push(("room.style", "rimz".to_owned()));
            }
            if current.keymap.as_str() != Some(keymap) {
                changes.push(("keys.keymap", keymap.to_owned()));
            }
        }
        if answers.truecolor != current.truecolor {
            let mode = if answers.truecolor {
                "truecolor"
            } else {
                "256"
            };
            changes.push(("theme.mode", mode.to_owned()));
        }
        if answers.nerd_font != current.nerd_font {
            let set = if answers.nerd_font {
                "nerd_font"
            } else {
                "unicode"
            };
            changes.push(("theme.glyphs.set", set.to_owned()));
        }
        if answers.pet_enabled != current.pet_enabled {
            changes.push(("theme.pets.enabled", answers.pet_enabled.to_string()));
        }
        for (offered, &on) in current.away.iter().zip(&answers.away) {
            if on != offered.on {
                let row = offered.row;
                let value = if on { row.on_value } else { row.off_value };
                changes.push((row.key, value.to_owned()));
            }
        }
        changes
    }

    /// Write every changed answer and report how many settings were written.
    pub(crate) fn apply(&self, out: &mut dyn Write) -> Result<usize> {
        let changes = self.changes();
        if changes.is_empty() {
            writeln!(out, "✓ nothing to write; your settings are unchanged")?;
            return Ok(0);
        }
        let editor = ConfigEditor::machine();
        for (key, value) in &changes {
            editor.set(key, value)?;
        }
        writeln!(
            out,
            "{} wrote {}",
            render::paint(render::palette::good().bold(), "✓"),
            count_noun(changes.len(), "setting")
        )?;
        Ok(changes.len())
    }
}

fn count_noun(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn changed_files(changes: &[(&'static str, String)]) -> Vec<String> {
    let mut files = Vec::new();
    for (key, _) in changes {
        let path = if key.starts_with("theme.") {
            MachineConfig::theme_path()
        } else {
            MachineConfig::config_path()
        };
        let path = render::home_relative_path(&path);
        if !files.contains(&path) {
            files.push(path);
        }
    }
    files
}

fn with_prefix(name: String, prefix: Option<KeyChord>) -> String {
    match prefix {
        Some(prefix) => format!("{name}, prefix {prefix}"),
        None => name,
    }
}

fn color_text(truecolor: bool) -> String {
    if truecolor { "truecolor" } else { "256 colors" }.to_owned()
}

fn icons_text(nerd_font: bool) -> String {
    if nerd_font { "Nerd Font" } else { "plain" }.to_owned()
}

fn mux_label(mux: MuxName) -> &'static str {
    match mux {
        MuxName::Tmux => "tmux",
        MuxName::Zellij => "Zellij",
    }
}

/// One review row, with the stored value beside it when the answer differs, or
/// on its own line when the pair would pass the screen width.
fn review_row_lines(number: usize, label: &str, now: &str, was: Option<&str>) -> Vec<String> {
    let indent = " ".repeat(5 + REVIEW_LABEL_WIDTH + 1);
    let mut lines = vec![format!("  {number}  {label:<REVIEW_LABEL_WIDTH$} ")];
    for (index, part) in render::wrap_words(now, SCREEN_WIDTH - indent.len())
        .into_iter()
        .enumerate()
    {
        match index {
            0 => lines[0].push_str(&part),
            _ => lines.push(format!("{indent}{part}")),
        }
    }
    let Some(was) = was else {
        return lines;
    };
    let was = format!("was: {was}");
    if let [row] = lines.as_slice() {
        let joined = format!("{row}   {was}");
        if joined.chars().count() <= SCREEN_WIDTH {
            return vec![joined];
        }
    }
    lines.push(format!("{indent}{was}"));
    lines
}

/// Wrap `items` joined by ` · ` under a hanging indent, within the screen width.
fn wrap_items(first: &str, indent: usize, items: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = first.to_owned();
    let mut fresh = true;
    for item in items {
        let candidate = if fresh {
            format!("{line}{item}")
        } else {
            format!("{line} · {item}")
        };
        if !fresh && candidate.chars().count() > SCREEN_WIDTH {
            lines.push(line);
            line = format!("{}{item}", " ".repeat(indent));
        } else {
            line = candidate;
        }
        fresh = false;
    }
    lines.push(line);
    lines
        .into_iter()
        .flat_map(|line| break_at_slashes(line, indent))
        .collect()
}

/// Breaks a line past the screen after its last `/` that fits, continuing
/// under the indent, so a long chord list keeps every chord on screen.
fn break_at_slashes(mut line: String, indent: usize) -> Vec<String> {
    let mut lines = Vec::new();
    while line.chars().count() > SCREEN_WIDTH {
        let fits: String = line.chars().take(SCREEN_WIDTH).collect();
        let Some(cut) = fits.rfind('/').filter(|&cut| cut >= indent) else {
            break;
        };
        let rest = format!("{}{}", " ".repeat(indent), &line[cut + 1..]);
        line.truncate(cut + 1);
        lines.push(line);
        line = rest;
    }
    lines.push(line);
    lines
}

fn chord(chord: Option<KeyChord>) -> String {
    chord.map_or_else(|| "(unbound)".to_owned(), |chord| chord.to_string())
}

/// The bound terminal chords in terminal order, a run of consecutive digits
/// under one modifier collapsed to its ends (`Alt+1..9`).
fn terminals_text(terminals: &[Option<KeyChord>]) -> String {
    let mut runs: Vec<(KeyChord, KeyChord)> = Vec::new();
    for next in terminals.iter().flatten().copied() {
        match runs.last_mut() {
            Some((_, last)) if next_digit(*last, next) => *last = next,
            _ => runs.push((next, next)),
        }
    }
    if runs.is_empty() {
        return chord(None);
    }
    let texts: Vec<String> = runs
        .iter()
        .map(|&(first, last)| match last {
            KeyChord::Alt(digit) | KeyChord::Ctrl(digit) if first != last => {
                format!("{first}..{digit}")
            }
            _ => first.to_string(),
        })
        .collect();
    texts.join("/")
}

fn next_digit(previous: KeyChord, next: KeyChord) -> bool {
    let step =
        |a: char, b: char| a.is_ascii_digit() && b.is_ascii_digit() && b as u32 == a as u32 + 1;
    match (previous, next) {
        (KeyChord::Alt(a), KeyChord::Alt(b)) | (KeyChord::Ctrl(a), KeyChord::Ctrl(b)) => step(a, b),
        _ => false,
    }
}

/// The three room-key options, each chord as the loaded config resolves it.
fn room_keys_options(facts: &KeyFacts, mux: MuxName, status_bar_unset: bool) -> Vec<String> {
    let indent = 5 + KEYS_LABEL_WIDTH;
    let pad = " ".repeat(indent);
    let head = |number: usize, label: &str| format!("  {number}  {label:<KEYS_LABEL_WIDTH$}");
    let name = mux_label(mux);
    let mut lines = Vec::new();

    let adds: Vec<String> = [
        facts.focus.map(|key| format!("{key}  sidebar and back")),
        facts.zoom.map(|key| format!("{key}  fullscreen this pane")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let yours = if adds.is_empty() {
        format!("your {name} config and bindings, as they are.")
    } else {
        format!("your {name} config and bindings, as they are. RimZ adds:")
    };
    lines.push(format!("{}{yours}", head(1, "Yours")));
    let joined = format!("{pad}  {}", adds.join("     "));
    if !adds.is_empty() && joined.chars().count() <= SCREEN_WIDTH {
        lines.push(joined);
    } else {
        lines.extend(adds.iter().map(|add| format!("{pad}  {add}")));
    }

    let rimz = &facts.rimz;
    lines.extend(wrap_items(
        &head(2, "RimZ"),
        indent,
        &[
            format!("{}/{} walk agents", chord(rimz.next), chord(rimz.prev)),
            format!("{} dashboard", chord(rimz.dashboard)),
            format!("{} terminals", terminals_text(&rimz.terminals)),
            format!("{} who needs you", chord(rimz.needs_you)),
        ],
    ));
    lines.extend(wrap_items(
        &pad,
        indent,
        &[
            format!(
                "{}, then: {} fullscreen",
                chord(rimz.prefix),
                chord(rimz.zoom)
            ),
            format!(
                "{} resize mode, kept through moves until Esc",
                chord(rimz.resize)
            ),
        ],
    ));
    if status_bar_unset {
        lines.push(format!("{pad}Hides the {name} status bar in rooms."));
    }

    lines.extend(wrap_items(
        &head(3, &format!("RimZ + {name}")),
        indent,
        &[
            "the same agent keys".to_owned(),
            format!(
                "{} becomes RimZ's prefix, with {name}-like keys after it",
                chord(facts.mux_prefix(mux))
            ),
            "your own prefix bindings do not fire in rooms".to_owned(),
        ],
    ));
    lines.push(String::new());
    match mux {
        MuxName::Tmux => lines.push(
            "  Rooms run on RimZ's own tmux server; sessions outside RimZ are untouched.".into(),
        ),
        MuxName::Zellij => lines.extend([
            "  2 and 3 replace your own Zellij bindings in rooms; going back to yours".into(),
            "  needs a detach and reattach.".into(),
        ]),
    }
    lines
}

fn away_row_line(number: usize, offered: &OfferedRow, on: bool) -> String {
    let mark = if on { "[x]" } else { "[ ]" };
    format!(
        "  {number}  {mark} {:<AWAY_LABEL_WIDTH$} {}",
        offered.row.label,
        render::paint(render::palette::muted(), &offered.detail),
    )
}

/// Row numbers separated by spaces or commas, each within `1..=count`.
fn parse_rows(answer: &str, count: usize) -> Option<Vec<usize>> {
    let rows: Vec<usize> = answer
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .parse::<usize>()
                .ok()
                .filter(|row| (1..=count).contains(row))
        })
        .collect::<Option<_>>()?;
    (!rows.is_empty()).then_some(rows)
}

/// `bracket` is what Enter keeps; the reason prints beside the default it
/// explains, and an answer that moved off that default is marked as yours.
fn prompt_mux(
    bracket: MuxName,
    choice: MuxChoice,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<MuxName>> {
    const ORDER: [MuxName; 2] = [MuxName::Tmux, MuxName::Zellij];
    writeln!(
        out,
        "Both tmux and Zellij are installed. A room keeps the one it was born in."
    )?;
    writeln!(out)?;
    for (index, mux) in ORDER.iter().enumerate() {
        let note = if *mux == choice.bracket {
            format!("   ← default: {}", choice.reason)
        } else if *mux == bracket {
            "   ← your answer".to_owned()
        } else {
            String::new()
        };
        let row = format!("  {}  {:<8}{note}", index + 1, mux.as_str());
        writeln!(out, "{}", row.trim_end())?;
    }
    writeln!(out)?;
    let default = ORDER
        .iter()
        .position(|mux| *mux == bracket)
        .unwrap_or_default()
        + 1;
    loop {
        write_prompt(out, "Multiplexer?", &default.to_string())?;
        let Some(answer) = read_answer(input, out)? else {
            return Ok(None);
        };
        if answer.is_empty() {
            return Ok(Some(bracket));
        }
        let picked = match answer.to_ascii_lowercase().as_str() {
            "1" | "tmux" => Some(MuxName::Tmux),
            "2" | "zellij" => Some(MuxName::Zellij),
            _ => None,
        };
        match picked {
            Some(mux) => return Ok(Some(mux)),
            None => writeln!(out, "  Enter 1 or 2, or press Enter to keep [{default}].")?,
        }
    }
}

/// The multiplexer question a first plain `rimz` asks before it picks one, with
/// no review behind it. `None` when it was left by `q` or end of input.
pub(crate) fn ask_first_start_mux(
    config: &MachineConfig,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<MuxName>> {
    let choice = MuxChoice::new(None, config.mux.default);
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "Multiplexer")
    )?;
    writeln!(out)?;
    prompt_mux(choice.bracket, choice, input, out)
}

/// Whether `answer` needs a `mux.default` write: it differs from what the
/// config resolves to outside a multiplexer with both installed.
pub(crate) fn mux_answer_differs(config: &MachineConfig, answer: MuxName) -> bool {
    MuxChoice::new(None, config.mux.default).resolved != answer
}

/// The line a first plain `rimz` prints after hooks, naming the room keys,
/// color and glyphs the loaded config holds.
pub(crate) fn write_first_start_defaults(
    config: &MachineConfig,
    mux: MuxName,
    out: &mut dyn Write,
) -> Result<()> {
    let keys = Current::from_config(
        config,
        false,
        false,
        MuxSetup {
            detected: mux,
            enclosing: None,
            ask: false,
        },
    );
    let keys = match keys.keys_option(mux) {
        Some(RoomKeys::Yours) => format!("your {} keys", mux_label(mux)),
        Some(RoomKeys::Rimz) => "RimZ keys".to_owned(),
        Some(RoomKeys::RimzMux) => format!("RimZ + {} keys", mux_label(mux)),
        None => keys.stored_keys_text(),
    };
    let color = match config.theme.effective_theme_mode() {
        ThemeMode::Auto => "auto color",
        ThemeMode::Truecolor => "truecolor",
        ThemeMode::Indexed => "256 colors",
    };
    let glyphs = match config.theme.glyph_set_source() {
        None => "plain glyphs".to_owned(),
        Some("nerd_font") => "Nerd Font icons".to_owned(),
        Some("unicode") => "Unicode glyphs".to_owned(),
        Some(set) => format!("{set} glyphs"),
    };
    writeln!(
        out,
        "Everything else is on defaults: {keys}, {color}, {glyphs}."
    )?;
    writeln!(
        out,
        "  {}   change any of it",
        render::paint(render::palette::accent(), "rimz setup")
    )?;
    Ok(())
}

pub(crate) fn write_header(out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "rimz · first-run setup")
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::faint(),
            &header_rule(render::terminal_columns(80))
        )
    )?;
    Ok(())
}

fn header_rule(term_cols: usize) -> String {
    "─".repeat(term_cols.min(HEADER_RULE_WIDTH))
}

const GRADIENT_WIDTH: usize = 36;

fn gradient_line() -> String {
    let gradient = nerd_font_probe_gradient(GRADIENT_WIDTH)
        .into_iter()
        .map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m█"))
        .chain(std::iter::once(String::from("\x1b[0m")))
        .collect::<String>();
    format!("  {gradient}  ← a full spread of colors")
}

fn glyph_line() -> String {
    let glyphs = nerd_font_probe_glyphs().join("    ");
    format!("  {glyphs}  ← eight distinct icons")
}

fn build_pet_art(pets_config: PetsConfig) -> Option<String> {
    let (caps, wrap_pixels) = rimz::sidebar_pane::detect_pixel_render_env();
    let tier = pets::resolve_render_tier(pets_config.glyphs, caps);
    let mut buf = Vec::new();
    match tier {
        PetRenderTier::Pixel => {
            let preview = pets::load_pixel_preview(&pets_config.pet)?;
            preview.frame.as_ref().ok()?;
            write_pixel_pet_row_with_pacer(
                &mut buf,
                &[(1, preview)],
                wrap_pixels,
                None::<&mut LiveGraphicsPacer>,
            )
            .ok()?;
        }
        PetRenderTier::Cell => {
            let aspect = pets_config
                .cell_aspect
                .or_else(pets::probe_cell_aspect)
                .unwrap_or(CellAspect::NEUTRAL);
            let slot = pets::dashboard_pet_size(tier);
            let preview = pets::load_cell_preview(&pets_config.pet, slot, aspect)?;
            preview.grid.as_ref().ok()?;
            write_pet_row(&mut buf, &[preview], slot).ok()?;
        }
    }
    String::from_utf8(buf).ok()
}

fn write_muted(out: &mut dyn Write, lines: &[&str]) -> Result<()> {
    for line in lines {
        writeln!(out, "{}", render::paint(render::palette::muted(), line))?;
    }
    Ok(())
}

fn write_prompt(out: &mut dyn Write, prompt: &str, bracket: &str) -> Result<()> {
    write!(
        out,
        "{prompt} {} ",
        render::paint(render::palette::accent().bold(), &format!("[{bracket}]"))
    )?;
    out.flush()?;
    Ok(())
}

/// One trimmed answer line; `None` on end of input or `q`, which leave the walk.
fn read_answer(input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Option<String>> {
    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        writeln!(out)?;
        return Ok(None);
    }
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("q") {
        return Ok(None);
    }
    Ok(Some(answer.to_owned()))
}

fn prompt_bool(
    prompt: &str,
    default_yes: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<bool>> {
    let suffix = if default_yes { "Y/n" } else { "y/N" };
    loop {
        write_prompt(out, prompt, suffix)?;
        let Some(answer) = read_answer(input, out)? else {
            return Ok(None);
        };
        if answer.is_empty() {
            return Ok(Some(default_yes));
        }
        if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
            return Ok(Some(true));
        }
        if answer.eq_ignore_ascii_case("n") || answer.eq_ignore_ascii_case("no") {
            return Ok(Some(false));
        }
        writeln!(out, "  Enter y or n (q quits).")?;
    }
}

#[cfg(test)]
mod tests;
