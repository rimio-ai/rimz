//! The environment keys naming the loop fire, its task and its booking chain to the processes
//! and launches the fire starts, the one reader of them a launch entry stamps from
//! ([`read_fire_env`]), and the reading of a task's command as the agent or team launch it
//! types. It belongs to the sidebar import graph; the account a fire's launches use is
//! `harness::task_login`.

use std::iter::Peekable;
use std::path::Path;
use std::slice::Iter;

use crate::agents::LaunchedBy;
use crate::ids::FireId;

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

/// The fire a launch is made under, read from its environment: what a launch entry stamps on
/// everything it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FireEnv {
    pub fire: FireId,
    /// The task's name at the fire, for display; the fire id is the identity.
    pub task: String,
    /// What fired it, for a launch to append to a nonblank prompt.
    pub trigger: Option<String>,
    pub booked_by: Vec<LaunchedBy>,
}

/// The fire this process's launches belong to: `RIMZ_LOOP_FIRE` parses as a fire id and no
/// agent identity is set, so a launch an agent makes in its own pane is never the fire's.
/// `RIMZ_LOOP_TASK` decides nothing.
pub fn read_fire_env() -> Option<FireEnv> {
    let var = |key: &str| std::env::var(key).ok();
    fire_env_from(
        var(LOOP_FIRE_ENV),
        std::env::var_os(crate::harness::launch::ENV_AGENT_ID).is_some(),
        |key| var(key),
    )
}

fn fire_env_from(
    fire: Option<String>,
    has_agent: bool,
    var: impl Fn(&str) -> Option<String>,
) -> Option<FireEnv> {
    if has_agent {
        return None;
    }
    let fire = FireId::parse(&fire?).ok()?;
    Some(FireEnv {
        fire,
        task: var(LOOP_TASK_ENV).unwrap_or_default(),
        trigger: var(LOOP_TRIGGER_ENV).filter(|text| !text.trim().is_empty()),
        booked_by: var(LOOP_BOOKED_BY_ENV)
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn a_fire_is_read_from_its_id_alone_and_never_inside_an_agent() {
        let task = crate::ids::TaskId::new();
        let fire = FireId::new(&task);
        let booked = r#"[{"kind":"claude","agent_id":"booker"}]"#;
        let pairs = [
            (LOOP_TASK_ENV, "nightly"),
            (LOOP_TRIGGER_ENV, "  "),
            (LOOP_BOOKED_BY_ENV, booked),
        ];
        let read = fire_env_from(Some(fire.as_str().to_owned()), false, env(&pairs)).unwrap();
        assert_eq!(read.fire.task(), task);
        assert_eq!(read.task, "nightly");
        assert_eq!(read.trigger, None);
        assert_eq!(
            read.booked_by,
            [LaunchedBy {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                agent_id: "booker".into(),
            }]
        );
        // An agent's own pane carries the fire's environment too: its launches are its own.
        assert_eq!(
            fire_env_from(Some(fire.as_str().to_owned()), true, env(&pairs)),
            None
        );
        // The task name decides nothing: without a fire id there is no fire.
        assert_eq!(fire_env_from(None, false, env(&pairs)), None);
        assert_eq!(fire_env_from(Some(String::new()), false, env(&pairs)), None);
        assert_eq!(
            fire_env_from(Some("nightly".into()), false, env(&pairs)),
            None
        );
        let unreadable = [(LOOP_BOOKED_BY_ENV, "not json")];
        let read = fire_env_from(Some(fire.as_str().to_owned()), false, env(&unreadable)).unwrap();
        assert!(read.booked_by.is_empty());
    }
}
