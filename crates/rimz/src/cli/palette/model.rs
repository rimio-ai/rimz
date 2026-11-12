//! Published candidates, fuzzy ranking, and the palette's selection state.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::PathBuf;

use rimz::address::{
    AddressContext, ChannelOrigin, addressable_agents, agent_handle, resolve_many,
};
use rimz::agents::{AgentState, single_line_description, spec_by_kind};
use rimz::ids::PaneId;
use rimz::store::snapshot::{
    ProcessState, SidebarRow, SidebarSnapshot, SidebarSubAgent, SidebarWorktreeGroup,
};

use super::Mode;
use super::launch::{LaunchFlow, LaunchSetup, Place, Step, launch_args, places};

#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) id: String,
    pub(super) pane: PaneId,
    pub(super) target: String,
    pub(super) agent: bool,
    pub(super) compact: bool,
    pub(super) label: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) status: String,
    pub(super) description: String,
    pub(super) worktree: String,
    pub(super) branch: String,
    pub(super) pr: Option<u64>,
    pub(super) group_key: String,
    /// The checkout the card's agent runs in, which a launch from its pane starts beside.
    pub(super) checkout: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Effect {
    Act,
    Show,
    Launch,
}

#[derive(Clone, Debug)]
pub(super) struct Action {
    pub(super) args: Vec<String>,
    pub(super) effect: Effect,
}

impl Action {
    pub(super) fn command_line(&self) -> String {
        let mut line = "rimz".to_owned();
        for word in &self.args {
            line.push(' ');
            line.push_str(&shell_word(word));
        }
        line
    }
}

/// Addresses and pane ids stay as a user types them; only words a shell would read differently
/// take quotes.
fn shell_word(word: &str) -> Cow<'_, str> {
    let plain = !word.starts_with('#')
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@#%:_-./".contains(&byte));
    if plain {
        return Cow::Borrowed(word);
    }
    shlex::try_quote(word).unwrap_or(Cow::Borrowed(word))
}

pub(super) struct Entry {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) meta: String,
    pub(super) detail: String,
    pub(super) kind: Option<String>,
    pub(super) action: Option<Action>,
}

pub(super) enum ChildOutcome {
    Success,
    Failure(String),
}

/// The snapshot's addressable agents, which every card looks its identity up in.
struct Roster<'a> {
    snapshot: &'a SidebarSnapshot,
    peers: Vec<&'a AgentState>,
}

struct Identity {
    target: String,
    label: String,
    name: String,
    agent: bool,
    compact: bool,
}

impl<'a> Roster<'a> {
    fn agent(&self, id: &str) -> Option<&'a AgentState> {
        self.peers
            .iter()
            .copied()
            .find(|agent| agent.agent_id.as_str() == id)
    }

    /// The first form that names exactly this agent whatever lane the child runs in: a handle
    /// counts only with its `#lane`, since a bare one scopes to the reader's lane.
    fn target(&self, agent: &AgentState, pane: &PaneId) -> String {
        let any_lane = AddressContext {
            channel: None,
            origin: ChannelOrigin::Explicit,
        };
        let handle =
            rimz::address::lane_label(agent).map(|_| agent_handle(agent, &self.peers, true));
        let session = (!agent.agent_id.is_provisional()).then(|| format!("@{}", agent.agent_id));
        [handle, session]
            .into_iter()
            .flatten()
            .find(|form| {
                resolve_many(self.snapshot, form, None, &any_lane)
                    .is_ok_and(|matches| matches.as_slice() == [agent])
            })
            .unwrap_or_else(|| pane.to_string())
    }

    /// A row with no agent behind it (a process, a wired pane before its session binds) has only
    /// its pane to name it by.
    fn identity(&self, agent: Option<&AgentState>, pane: &PaneId, fallback: &str) -> Identity {
        let Some(agent) = agent else {
            return Identity {
                target: pane.to_string(),
                label: fallback.to_owned(),
                name: String::new(),
                agent: false,
                compact: false,
            };
        };
        Identity {
            target: self.target(agent, pane),
            label: agent_handle(agent, &self.peers, true),
            name: agent.name.clone().unwrap_or_default(),
            agent: true,
            compact: spec_by_kind(agent.kind.as_str()).is_some_and(|spec| {
                spec.launch.compact_command.is_some()
                    && spec.lifecycle_hooks.turn_started.is_native()
            }),
        }
    }
}

pub(super) fn candidates(snapshot: &SidebarSnapshot) -> Vec<Candidate> {
    let roster = Roster {
        snapshot,
        peers: addressable_agents(snapshot),
    };
    let mut cards = Vec::new();
    for group in snapshot.worktree_groups.iter() {
        for row in group.rows.iter() {
            if let Some(pane) = &row.pane {
                cards.push(row_card(&roster, group, row, &pane.pane_id));
            }
            for child in row.sub_agents() {
                let Some(pane) = &child.pane else { continue };
                cards.push(child_card(&roster, group, row, child, pane));
            }
        }
    }
    cards
}

fn row_card(
    roster: &Roster<'_>,
    group: &SidebarWorktreeGroup,
    row: &SidebarRow,
    pane: &PaneId,
) -> Candidate {
    let card = row.as_agent();
    let status = card.map_or_else(
        || match row.process_state().unwrap_or_default() {
            ProcessState::Idle => "idle",
            ProcessState::Busy => "busy",
            ProcessState::Stuck => "stuck",
        },
        |card| card.status.as_str(),
    );
    let description = card
        .and_then(|card| {
            card.description
                .as_deref()
                .or(card.task.as_deref())
                .or(card.first_prompt.as_deref())
        })
        .or_else(|| {
            row.as_process()
                .and_then(|card| card.command_detail.as_deref())
        });
    let Identity {
        target,
        label,
        name,
        agent,
        compact,
    } = roster.identity(roster.agent(&row.id), pane, row.display_name());
    Candidate {
        id: row.id.clone(),
        pane: pane.clone(),
        target,
        agent,
        compact,
        label,
        name,
        kind: row.name.clone(),
        status: status.to_owned(),
        description: description
            .and_then(single_line_description)
            .unwrap_or_default(),
        worktree: group.label.clone(),
        branch: row.worktree_branch.clone().unwrap_or_default(),
        pr: group.pr_number,
        group_key: group.key.clone(),
        checkout: row.lane_path().map(PathBuf::from),
    }
}

/// A child renders under its parent's group but can run in another lane: it then takes that
/// lane's group when the room has one, else the lane alone with no PR. The group is found by its
/// key, or for trunk by its root marker, since another checkout's label may equal a lane.
fn child_card(
    roster: &Roster<'_>,
    group: &SidebarWorktreeGroup,
    row: &SidebarRow,
    child: &SidebarSubAgent,
    pane: &PaneId,
) -> Candidate {
    let state = roster.agent(&child.id);
    let parent_lane = roster.agent(&row.id).and_then(rimz::address::lane_label);
    let own_lane = state
        .and_then(rimz::address::lane_label)
        .filter(|lane| Some(lane) != parent_lane.as_ref());
    let home = match &own_lane {
        Some(lane) => {
            let at_root = state.is_some_and(AgentState::in_trunk);
            let lane_key = format!("channel:{lane}");
            roster.snapshot.worktree_groups.iter().find(|group| {
                if at_root {
                    group.root_lane
                } else {
                    group.key == lane_key
                }
            })
        }
        None => Some(group),
    };
    let worktree = home.map_or_else(
        || own_lane.clone().unwrap_or_default(),
        |home| home.label.clone(),
    );
    let branch = state.map_or(&row.worktree_branch, |state| &state.worktree_branch);
    let Identity {
        target,
        label,
        name,
        agent,
        compact,
    } = roster.identity(state, pane, &child.name);
    Candidate {
        id: child.id.clone(),
        pane: pane.clone(),
        target,
        agent,
        compact,
        label,
        name,
        kind: child.kind.to_string(),
        status: child.status.as_str().to_owned(),
        description: child
            .description
            .as_deref()
            .or(child.task.as_deref())
            .or(child.first_prompt.as_deref())
            .and_then(single_line_description)
            .unwrap_or_default(),
        branch: branch.clone().unwrap_or_default(),
        pr: home.and_then(|home| home.pr_number),
        group_key: home.map_or_else(|| format!("lane:{worktree}"), |home| home.key.clone()),
        worktree,
        checkout: state
            .and_then(AgentState::lane_path)
            .or(row.lane_path())
            .map(PathBuf::from),
    }
}

pub(super) fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let text = text.to_lowercase();
    let mut letters = text.chars().enumerate();
    let mut score = 0;
    let mut previous = None;
    for letter in query.to_lowercase().chars() {
        let (position, _) = letters.find(|(_, candidate)| *candidate == letter)?;
        score -= position as i64;
        if previous.is_some_and(|previous| previous + 1 == position) {
            score += 10;
        }
        previous = Some(position);
    }
    Some(score)
}

fn command(args: Vec<String>, effect: Effect) -> Entry {
    let action = Action { args, effect };
    let label = action.command_line();
    Entry {
        id: label.clone(),
        label,
        meta: String::new(),
        detail: String::new(),
        kind: None,
        action: Some(action),
    }
}

pub(super) fn command_entries(target: Option<&Candidate>, session: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(target) = target {
        entries.push(command(
            vec!["pane".into(), "focus".into(), target.target.clone()],
            Effect::Act,
        ));
        if target.agent {
            for verb in ["restart", "stop", "compact", "show", "logs"] {
                if verb == "compact" && !target.compact {
                    continue;
                }
                let mut args = vec!["agents".into(), verb.into(), target.target.clone()];
                let effect = if matches!(verb, "show" | "logs") {
                    Effect::Show
                } else {
                    Effect::Act
                };
                if verb == "logs" {
                    args.extend(["-n".into(), "40".into()]);
                }
                entries.push(command(args, effect));
            }
        }
        entries.push(command(
            vec!["pane".into(), "capture".into(), target.target.clone()],
            Effect::Show,
        ));
    }
    for args in [
        vec!["agents", "list"],
        vec!["asks"],
        vec!["teams", "list"],
        vec!["worktree", "list"],
        vec!["doctor"],
        vec!["stats"],
    ] {
        entries.push(command(
            args.into_iter().map(str::to_owned).collect(),
            Effect::Show,
        ));
    }
    for mut args in [
        vec!["walk".into(), "next".into()],
        vec!["walk".into(), "prev".into()],
    ]
    .into_iter()
    .chain((1..=9).map(|n| vec!["slot".into(), n.to_string()]))
    .chain([vec!["last".into()]])
    {
        args.insert(0, "sidebar".into());
        args.extend(["--session-name".into(), session.to_owned()]);
        entries.push(command(args, Effect::Act));
    }
    entries.push(command(
        vec!["sidebar".into(), "repair".into()],
        Effect::Act,
    ));
    entries
}

/// Entries matching `query` over their label, meta and detail, best first, ties in order.
fn ranked(query: &str, entries: impl IntoIterator<Item = Entry>) -> Vec<Entry> {
    let mut ranked = entries
        .into_iter()
        .filter_map(|entry| {
            fuzzy_score(
                query,
                &format!("{} {} {}", entry.label, entry.meta, entry.detail),
            )
            .map(|score| (score, entry))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, _)| Reverse(*score));
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

fn card_entry(card: &Candidate) -> Entry {
    let mut details = vec![
        card.name.clone(),
        card.description.clone(),
        card.worktree.clone(),
        card.branch.clone(),
    ];
    if let Some(pr) = card.pr {
        details.push(format!("#{pr}"));
    }
    Entry {
        id: card.id.clone(),
        label: card.label.clone(),
        meta: format!("{}  {}", card.kind, card.status),
        detail: details
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  "),
        kind: Some(card.kind.clone()),
        action: Some(Action {
            args: vec!["pane".into(), "focus".into(), card.target.clone()],
            effect: Effect::Act,
        }),
    }
}

fn card_score(query: &str, card: &Candidate) -> Option<(usize, i64)> {
    [
        card.label.as_str(),
        &card.name,
        &card.kind,
        &card.worktree,
        &card.branch,
        &card.pr.map(|pr| pr.to_string()).unwrap_or_default(),
        &card.description,
    ]
    .into_iter()
    .enumerate()
    .find_map(|(index, field)| fuzzy_score(query, field).map(|score| (7 - index, score)))
}

/// The new id of a card whose row was republished on the same pane: a row moves from a pane or
/// provisional id to its session id when the agent registers.
fn same_card(previous: &[Candidate], cards: &[Candidate], id: &str) -> Option<String> {
    if cards.iter().any(|card| card.id == id) {
        return None;
    }
    let pane = &previous.iter().find(|card| card.id == id)?.pane;
    cards
        .iter()
        .find(|card| card.pane == *pane && previous.iter().all(|old| old.id != card.id))
        .map(|card| card.id.clone())
}

#[derive(Clone)]
pub(super) struct PaletteState {
    pub(super) query: String,
    pub(super) selected: usize,
    pub(super) notice: Option<String>,
    pub(super) closed: bool,
    cards: Vec<Candidate>,
    opener: Option<PaneId>,
    session: String,
    card_target: Option<String>,
    published: bool,
    launch: LaunchSetup,
    flow: Option<LaunchFlow>,
}

impl PaletteState {
    pub(super) fn new(mode: Mode, opener: Option<PaneId>, session: &str) -> Self {
        Self {
            query: mode.prefix().to_owned(),
            selected: 0,
            notice: None,
            closed: false,
            cards: Vec::new(),
            opener,
            session: session.to_owned(),
            card_target: None,
            published: false,
            launch: LaunchSetup::default(),
            flow: None,
        }
    }

    pub(super) fn with_launch(mut self, launch: LaunchSetup) -> Self {
        self.launch = launch;
        self
    }

    /// The `+` step past the profile, which the query no longer drives; `None` on the profile
    /// step and in every other mode.
    #[cfg(test)]
    pub(super) fn launch_step(&self) -> Option<Step> {
        self.flow.as_ref().map(|flow| flow.step)
    }

    pub(super) fn input_line(&self) -> String {
        let Some(flow) = &self.flow else {
            return format!("palette: {}", self.query);
        };
        let step = match flow.step {
            Step::Where => "where",
            Step::Name => "name",
            Step::Prompt => "prompt",
        };
        format!("palette: +{} › {step}: {}", flow.profile, flow.field_text())
    }

    pub(super) fn type_char(&mut self, character: char) {
        let Some(flow) = &mut self.flow else {
            let mut query = self.query.clone();
            query.push(character);
            return self.set_query(query);
        };
        flow.field().push(character);
        self.selected = 0;
        self.notice = None;
    }

    /// Erases the field's last character; on an empty field, returns to the previous step.
    pub(super) fn backspace(&mut self) {
        let Some(flow) = &mut self.flow else {
            let mut query = self.query.clone();
            query.pop();
            return self.set_query(query);
        };
        self.notice = None;
        if flow.field().pop().is_some() {
            self.selected = 0;
            return;
        }
        match flow.step {
            Step::Prompt if flow.place == Some(Place::New) => flow.step = Step::Name,
            Step::Prompt | Step::Name => {
                flow.step = Step::Where;
                let place = flow.place.as_ref().map(Place::id);
                self.select(place);
            }
            Step::Where => {
                let profile = format!("profile:{}", flow.profile);
                let query = format!("+{}", flow.profile_query());
                self.set_query(query);
                self.select(Some(profile));
            }
        }
    }

    fn select(&mut self, id: Option<String>) {
        self.selected = self
            .entries()
            .iter()
            .position(|entry| Some(&entry.id) == id.as_ref())
            .unwrap_or(0);
    }

    /// Enter: whether to run the selection now rather than move to the next `+` step.
    pub(super) fn advance(&mut self) -> bool {
        if self.mode() != Mode::New {
            return true;
        }
        let Some(id) = self
            .entries()
            .get(self.selected)
            .filter(|entry| entry.action.is_some())
            .map(|entry| entry.id.clone())
        else {
            return false;
        };
        let current = self.current_checkout();
        let Some(flow) = &mut self.flow else {
            let profile = id.strip_prefix("profile:").unwrap_or(&id).to_owned();
            let query = self.query.strip_prefix('+').unwrap_or_default().to_owned();
            self.flow = Some(LaunchFlow::new(profile, query));
            let places = places(&self.launch, current.as_deref());
            self.selected = self.launch.default_place(&places);
            return false;
        };
        match flow.step {
            Step::Where => {
                let place = places(&self.launch, current.as_deref())
                    .into_iter()
                    .map(|(place, ..)| place)
                    .find(|place| place.id() == id);
                flow.step = if place == Some(Place::New) {
                    Step::Name
                } else {
                    Step::Prompt
                };
                flow.place = place;
            }
            Step::Name => flow.step = Step::Prompt,
            Step::Prompt => return true,
        }
        self.selected = 0;
        false
    }

    fn current_checkout(&self) -> Option<PathBuf> {
        let opener = self.opener.as_ref()?;
        self.cards
            .iter()
            .find(|card| card.pane == *opener)
            .and_then(|card| card.checkout.clone())
    }

    fn launch_entries(&self, query: &str) -> Vec<Entry> {
        if let Some(reason) = &self.launch.unavailable {
            return vec![Entry {
                id: "launch:unavailable".to_owned(),
                label: reason.clone(),
                meta: String::new(),
                detail: String::new(),
                kind: None,
                action: None,
            }];
        }
        let current = self.current_checkout();
        let places = places(&self.launch, current.as_deref());
        let launch = |args| {
            Some(Action {
                args,
                effect: Effect::Launch,
            })
        };
        let Some(flow) = &self.flow else {
            let place = &places[self.launch.default_place(&places)].0;
            let entries = self.launch.profiles.iter().map(|profile| Entry {
                id: format!("profile:{}", profile.name),
                label: profile.name.clone(),
                meta: profile.model.clone(),
                detail: profile.description.clone(),
                kind: None,
                action: launch(launch_args(&profile.name, place, "", "")),
            });
            return ranked(query, entries);
        };
        let place = flow.place.clone().unwrap_or(Place::Root);
        match flow.step {
            Step::Where => {
                let entries =
                    places
                        .into_iter()
                        .enumerate()
                        .map(|(index, (place, label, detail))| Entry {
                            id: place.id(),
                            label,
                            meta: if index == 0 && place != Place::New {
                                "current".to_owned()
                            } else {
                                String::new()
                            },
                            detail,
                            kind: None,
                            action: launch(launch_args(&flow.profile, &place, "", "")),
                        });
                ranked(&flow.filter, entries)
            }
            Step::Name => {
                let name = flow.name.trim();
                let refusal = self.launch.name_refusal(name);
                vec![Entry {
                    id: "launch:name".to_owned(),
                    label: refusal.clone().unwrap_or_else(|| {
                        if name.is_empty() {
                            "generated name".to_owned()
                        } else {
                            name.to_owned()
                        }
                    }),
                    meta: "new worktree from trunk".to_owned(),
                    detail: String::new(),
                    kind: None,
                    action: refusal
                        .is_none()
                        .then(|| launch(launch_args(&flow.profile, &place, name, "")))
                        .flatten(),
                }]
            }
            Step::Prompt => vec![Entry {
                id: "launch:prompt".to_owned(),
                label: format!("launch {}", flow.profile),
                meta: String::new(),
                detail: if flow.prompt.trim().is_empty() {
                    "no prompt".to_owned()
                } else {
                    flow.prompt.clone()
                },
                kind: None,
                action: launch(launch_args(
                    &flow.profile,
                    &place,
                    flow.name.trim(),
                    &flow.prompt,
                )),
            }],
        }
    }

    pub(super) fn mode(&self) -> Mode {
        Mode::from_query(&self.query)
    }

    pub(super) fn set_query(&mut self, query: String) {
        self.flow = None;
        self.query = query;
        self.selected = 0;
        self.notice = None;
    }

    pub(super) fn refresh(&mut self, cards: Vec<Candidate>, published: bool) {
        let holds_a_choice = self.selected > 0 || !self.cards.is_empty();
        let selected = self
            .entries()
            .get(self.selected)
            .filter(|_| holds_a_choice)
            .map(|entry| entry.id.clone());
        let previous = std::mem::replace(&mut self.cards, cards);
        self.published = published;
        let selected = selected.map(|id| same_card(&previous, &self.cards, &id).unwrap_or(id));
        self.card_target = self
            .card_target
            .take()
            .map(|id| same_card(&previous, &self.cards, &id).unwrap_or(id))
            .filter(|id| self.cards.iter().any(|card| card.id == *id));
        self.selected = self
            .entries()
            .iter()
            .position(|entry| Some(&entry.id) == selected.as_ref())
            .unwrap_or(0);
    }

    pub(super) fn entries(&self) -> Vec<Entry> {
        let mode = self.mode();
        let query = self
            .query
            .strip_prefix(mode.prefix())
            .unwrap_or(&self.query)
            .trim();
        let entries = match mode {
            Mode::Cards => return self.card_entries(query),
            Mode::Commands => command_entries(self.target(), &self.session),
            Mode::Worktrees => self.worktree_entries(),
            Mode::New => return self.launch_entries(query),
        };
        ranked(query, entries)
    }

    fn card_entries(&self, query: &str) -> Vec<Entry> {
        let mut cards = self
            .cards
            .iter()
            .filter_map(|card| card_score(query, card).map(|score| (score, card)))
            .collect::<Vec<_>>();
        cards.sort_by_key(|(score, _)| Reverse(*score));
        cards
            .into_iter()
            .map(|(_, card)| card_entry(card))
            .collect()
    }

    pub(super) fn target(&self) -> Option<&Candidate> {
        self.card_target
            .as_ref()
            .and_then(|id| self.cards.iter().find(|card| card.id == *id))
            .or_else(|| {
                self.opener
                    .as_ref()
                    .and_then(|pane| self.cards.iter().find(|card| card.pane == *pane))
            })
    }

    fn worktree_entries(&self) -> Vec<Entry> {
        let mut seen = HashSet::new();
        self.cards
            .iter()
            .filter(|card| seen.insert(&card.group_key))
            .map(|card| {
                let mut entry = card_entry(card);
                entry.id = format!("group:{}", card.group_key);
                entry.label = format!("#{}", card.worktree);
                entry.kind = None;
                entry
            })
            .collect()
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let entries = self.entries();
        if entries.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(entries.len() - 1);
        if self.mode() == Mode::Cards {
            self.card_target = Some(entries[self.selected].id.clone());
        }
    }

    pub(super) fn selected_action(&self) -> Option<Action> {
        self.entries()
            .get(self.selected)
            .and_then(|entry| entry.action.clone())
    }

    pub(super) fn footer(&self) -> String {
        self.selected_action().map_or_else(
            || "nothing to run".to_owned(),
            |action| format!("$ {}", action.command_line()),
        )
    }

    pub(super) fn notice_text(&self) -> Option<&str> {
        self.notice.as_deref().or_else(|| {
            (!self.published)
                .then_some("no sidebar has published this room yet; run `rimz sidebar repair`")
        })
    }

    pub(super) fn activate(&mut self, runner: impl FnOnce(&Action) -> ChildOutcome) {
        let Some(action) = self.selected_action() else {
            return;
        };
        match runner(&action) {
            ChildOutcome::Success => self.closed = true,
            ChildOutcome::Failure(error) => {
                self.notice = Some(error.lines().next().unwrap_or_default().to_owned());
                self.closed = matches!(action.effect, Effect::Show | Effect::Launch);
            }
        }
    }
}
