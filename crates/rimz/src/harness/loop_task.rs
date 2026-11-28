//! The environment keys naming the loop fire, its task and its booking chain to the processes
//! and launches the fire starts, the one reader of them a launch entry stamps from
//! ([`read_fire_env`]). It belongs to the sidebar import graph; the account a fire's launches
//! use is `harness::task_login`.

use crate::agents::LaunchedBy;
use crate::ids::FireId;

pub(crate) const LOOP_TASK_ENV: &str = "RIMZ_LOOP_TASK";

/// The environment key carrying the fire id (`ids::FireId`) of the fire that started the run,
/// beside [`LOOP_TASK_ENV`]; the id names its task, so a reader never resolves it by name.
pub(crate) const LOOP_FIRE_ENV: &str = "RIMZ_LOOP_FIRE";

/// The environment key carrying, as a JSON list, the booking chain the firing task held when
/// its run started (`RunFire::booked_by`), so an agent its check launches records it.
pub(crate) const LOOP_BOOKED_BY_ENV: &str = "RIMZ_LOOP_BOOKED_BY";

/// The environment key carrying, as JSON, the firing booking's `on`, so an agent or team the
/// fire launches reports to its booker only when the booking allows a report.
pub(crate) const LOOP_ON_ENV: &str = "RIMZ_LOOP_ON";

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
    pub on: Option<crate::config::CheckOn>,
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
        on: var(LOOP_ON_ENV).and_then(|raw| serde_json::from_str(&raw).ok()),
    })
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
            (LOOP_ON_ENV, r#""fail""#),
        ];
        let read = fire_env_from(Some(fire.as_str().to_owned()), false, env(&pairs)).unwrap();
        assert_eq!(read.fire.task(), task);
        assert_eq!(read.task, "nightly");
        assert_eq!(read.trigger, None);
        assert_eq!(read.on, Some(crate::config::CheckOn::Fail));
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
