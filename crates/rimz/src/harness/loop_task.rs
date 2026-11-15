//! The environment keys naming the loop task, and its booking chain, to the processes and
//! launches its fire starts, and the reading of a task's command as the agent or team launch
//! it types. It belongs to the sidebar import graph; the account a fire's launches use is
//! `harness::task_login`.

use std::iter::Peekable;
use std::path::Path;
use std::slice::Iter;

pub(crate) const LOOP_TASK_ENV: &str = "RIMZ_LOOP_TASK";

/// The environment key carrying the fire id (`ids::FireId`) of the fire that started the run,
/// beside [`LOOP_TASK_ENV`]; the id names its task, so a reader never resolves it by name.
pub(crate) const LOOP_FIRE_ENV: &str = "RIMZ_LOOP_FIRE";

/// The environment key carrying, as a JSON list, the booking chain the firing task held when
/// its run started (`RunFire::booked_by`), so an agent its check launches records it.
pub(crate) const LOOP_BOOKED_BY_ENV: &str = "RIMZ_LOOP_BOOKED_BY";

/// The environment key carrying what fired the run (the trigger's evidence, as loop words it
/// for a launch prompt) to a fire's command, so a launch under it can append it to its prompt.
pub(crate) const LOOP_TRIGGER_ENV: &str = "RIMZ_LOOP_TRIGGER";

/// A task's command read as the launch a person would type: `rimz agents [launch] <spec>` or
/// `rimz teams [launch] <team[#lane]>`, blocking or `--detach`, flags before or after the
/// positional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FireLaunch {
    /// The launch came through `rimz teams`, so its target is a team.
    pub team: bool,
    /// The spec or team it starts, without a team's lane.
    pub target: String,
    pub worktree: Option<LaunchTree>,
    pub detach: bool,
}

/// The worktree a launch asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchTree {
    /// `-w NAME`, or a team's `#lane`.
    Named(String),
    /// A bare `-w`, which generates a name, or `--from-pr`.
    Unnamed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    Switch,
    Value,
    /// Takes the next word unless it starts with `-` (`-w`).
    Optional,
}

/// The cohort flags both doorways take. A flag missing from these tables (a resume, a listing
/// flag, help, a global option) leaves the command unread.
const COHORT_FLAGS: &[(&str, Option<char>, Arity)] = &[
    ("description", None, Arity::Value),
    ("worktree", Some('w'), Arity::Optional),
    ("channel", None, Arity::Value),
    ("from-pr", None, Arity::Value),
    ("fresh", None, Arity::Switch),
    ("budget", None, Arity::Value),
    ("bg", None, Arity::Switch),
    ("new-tab", None, Arity::Switch),
    ("detach", None, Arity::Switch),
];

const TEAM_FLAGS: &[(&str, Option<char>, Arity)] = &[("isolation", None, Arity::Value)];

const AGENT_FLAGS: &[(&str, Option<char>, Arity)] = &[
    ("cwd", None, Arity::Value),
    ("from-trunk", None, Arity::Switch),
    ("name", Some('n'), Arity::Value),
    ("new-pane", None, Arity::Switch),
    ("account", None, Arity::Value),
    ("ask", None, Arity::Switch),
    ("yolo", None, Arity::Switch),
    ("model", None, Arity::Value),
    ("tier", None, Arity::Value),
    ("agent", None, Arity::Value),
    ("system-prompt-file", None, Arity::Value),
    ("append-system-prompt-file", None, Arity::Value),
    ("effort", None, Arity::Value),
    ("fast", None, Arity::Switch),
    ("no-fast", None, Arity::Switch),
    ("isolation", None, Arity::Value),
    ("print", Some('p'), Arity::Switch),
    ("stdin", None, Arity::Switch),
    ("timeout", None, Arity::Value),
    ("keep", None, Arity::Switch),
    ("output-format", None, Arity::Value),
    ("input-format", None, Arity::Value),
    ("max-turns", None, Arity::Value),
    ("retries", None, Arity::Value),
    ("verify", None, Arity::Value),
    ("max-attempts", None, Arity::Value),
];

/// The verbs and aliases each doorway dispatches before a launch: a first positional among
/// them as the first word is no launch target.
const AGENT_VERBS: &[&str] = &[
    "mark",
    "validate",
    "launch",
    "explain",
    "check",
    "register",
    "profiles",
    "list",
    "ls",
    "ps",
    "show",
    "inspect",
    "logs",
    "history",
    "attribution",
    "top",
    "focus",
    "fork",
    "molt",
    "wait",
    "stop",
    "restart",
    "compact",
    "resume",
    "refresh",
    "tune",
    "budget",
    "exec",
    "session-step",
    "run-command",
    "auto-continue",
    "idle-compact",
    "soft-compact",
    "idle-stop",
    "hibernate",
    "wake",
    "wake-refault",
    "cache-keepalive",
    "auto-redeem",
    "budget-park",
    "park-notice",
    "stall-notice",
    "stall-nudge",
    "run-timeout",
    "orphan-subagent",
    "subagent-digest",
    "supervise-duty",
    "refresh-usage",
    "refresh-context",
    "help",
];

const TEAM_VERBS: &[&str] = &[
    "show", "inspect", "launch", "list", "ls", "profiles", "resume", "stop", "focus", "restart",
    "flip", "record", "wait", "help",
];

/// Reads `argv` as a launch, or `None` where it is any other command or a launch these
/// tables cannot read whole: an unknown flag, a third positional, a lane given twice.
pub fn read_fire_launch(argv: &[String]) -> Option<FireLaunch> {
    let (program, rest) = argv.split_first()?;
    if Path::new(program).file_name()? != "rimz" {
        return None;
    }
    let (doorway, mut rest) = rest.split_first()?;
    let (team, own, verbs) = match doorway.as_str() {
        "agents" => (false, AGENT_FLAGS, AGENT_VERBS),
        "teams" => (true, TEAM_FLAGS, TEAM_VERBS),
        _ => return None,
    };
    let verb = rest.first().is_some_and(|word| word == "launch");
    if verb {
        rest = &rest[1..];
    }
    let flag = |long: &str| {
        COHORT_FLAGS
            .iter()
            .chain(own)
            .find(|(name, _, _)| *name == long)
            .map(|(name, _, arity)| (*name, *arity))
    };
    let short = |letter: char| {
        COHORT_FLAGS
            .iter()
            .chain(own)
            .find(|(_, short, _)| *short == Some(letter))
            .map(|(name, _, arity)| (*name, *arity))
    };
    let mut positionals = Vec::new();
    let mut worktree = None;
    let mut from_pr = false;
    let mut detach = false;
    let mut words = rest.iter().peekable();
    while let Some(word) = words.next() {
        // Past `--`, an agent launch's words go to every agent cell; a team's are positionals.
        if word == "--" {
            if team {
                positionals.extend(words.map(String::as_str));
            }
            break;
        }
        let (name, value) = if let Some(long) = word.strip_prefix("--") {
            let (long, attached) = match long.split_once('=') {
                Some((long, value)) => (long, Some(value)),
                None => (long, None),
            };
            let (name, arity) = flag(long)?;
            (name, flag_value(arity, attached, &mut words)?)
        } else if let Some(letters) = word.strip_prefix('-').filter(|rest| !rest.is_empty()) {
            let mut chars = letters.chars();
            let (name, arity) = short(chars.next()?)?;
            let attached = Some(chars.as_str())
                .filter(|rest| !rest.is_empty())
                .map(|rest| rest.strip_prefix('=').unwrap_or(rest));
            (name, flag_value(arity, attached, &mut words)?)
        } else {
            positionals.push(word.as_str());
            continue;
        };
        match name {
            "worktree" => worktree = value,
            "from-pr" => from_pr = true,
            "detach" => detach = true,
            _ => {}
        }
    }
    let target = match positionals.as_slice() {
        [target] | [target, _] => *target,
        _ => return None,
    };
    // Clap dispatches a verb only as the first word; after a flag the word is a spec.
    let first = rest.first().map(String::as_str);
    if !verb && first == Some(target) && verbs.contains(&target) {
        return None;
    }
    let target = match target.split_once('#') {
        None => target,
        Some((team_name, lane)) if team && !lane.is_empty() && worktree.is_none() => {
            worktree = Some(lane.to_owned());
            team_name
        }
        Some(_) => return None,
    };
    let worktree = match worktree {
        Some(name) if name.is_empty() => Some(LaunchTree::Unnamed),
        Some(name) => Some(LaunchTree::Named(name)),
        None => from_pr.then_some(LaunchTree::Unnamed),
    };
    Some(FireLaunch {
        team,
        target: target.to_owned(),
        worktree,
        detach,
    })
}

/// A flag's value: the attached one, else the next word where the flag takes one; `None` (the
/// outer) where the argv cannot hold it.
fn flag_value(
    arity: Arity,
    attached: Option<&str>,
    words: &mut Peekable<Iter<'_, String>>,
) -> Option<Option<String>> {
    match (arity, attached) {
        (Arity::Switch, Some(_)) => None,
        (Arity::Switch, None) => Some(None),
        (_, Some(value)) => Some(Some(value.to_owned())),
        (Arity::Value, None) => Some(Some(words.next()?.clone())),
        (Arity::Optional, None) => Some(Some(
            words
                .next_if(|word| !word.starts_with('-'))
                .cloned()
                .unwrap_or_default(),
        )),
    }
}
