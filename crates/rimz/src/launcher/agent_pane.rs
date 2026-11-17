//! The pane an agent-addressed act wakes and then acts on, found at the point of use.
//!
//! [`find`] reads the hosting lookup for the agent, wakes the pane it answers, and reads
//! again on the first read's backend: the act goes ahead only on the pane both reads
//! answer, and refuses when the agent has no pane, moved during the wake, or the
//! multiplexer could not be read. [`open_writer`] does the same with the pane's write
//! lock taken after the wake, so a command that types reads the lookup again once it holds
//! the lock and types only into the pane both reads answer. [`locate`] is the single read,
//! for an act that wakes nothing or that decides before work of its own, such as delivery,
//! which reads it before its claim, under its own pane lock, and before an interrupt key.
//!
//! One window is accepted after [`find`]: a focus's anchor samples the attached clients
//! between `find` and `focus_pane`. The sample moves no pane, and the anchor's lock
//! forbids the I/O a re-read there would take.

use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::mux::{MuxErr, PaneWriter};
use crate::sidebar::produce::hosting::{Hosting, HostingErr, read_hosting};
use crate::{ResolvedWorkspace, Store};

/// The pane the lookup answered for the agent, and the session it found there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: PaneId,
    pub session_name: String,
    /// The `(kind, session id)` of the agent's row the lookup holds in the pane: the
    /// launch's occupant, which another session of one launch can be after a revival.
    pub holder: (AgentKind, AgentSessionId),
}

/// Why [`find`] or [`open_writer`] found no pane to act on; the act refuses and changes
/// nothing.
#[derive(Debug, thiserror::Error)]
pub enum FindErr {
    #[error(transparent)]
    Read(HostingErr),
    #[error("no listed pane hosts the agent")]
    Nowhere,
    #[error("the agent left pane {pane} while it was woken")]
    Moved { pane: PaneId },
    #[error("waking pane {pane}: {source}")]
    Wake {
        pane: PaneId,
        source: crate::harness::hibernate::HibernateErr,
    },
    #[error("locking pane {pane} for writing: {source}")]
    Lock { pane: PaneId, source: MuxErr },
}

/// The pane hosting the agent `(kind, launch)`, `None` when no listed pane hosts it, by
/// one read of the lookup on `mux`, the backend pick's notices pushed to `notices` for
/// the caller to print once.
pub fn locate(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    kind: &AgentKind,
    launch: &AgentSessionId,
    notices: &mut Vec<String>,
) -> Result<Option<AgentPane>, HostingErr> {
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_AGENT_PANE_BEFORE_READ");
    read_answer(workspace, store, mux, kind, launch, notices).map(|(_, pane)| pane)
}

/// The pane hosting the agent `(kind, launch)`, read, woken, and read again; the
/// notices of both reads are pushed to `notices` for the caller to print once.
pub fn find(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    kind: &AgentKind,
    launch: &AgentSessionId,
    notices: &mut Vec<String>,
) -> Result<AgentPane, FindErr> {
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_AGENT_PANE_BEFORE_READ");
    find_with(
        mux,
        |mux| read_answer(workspace, store, mux, kind, launch, notices),
        |pane| crate::harness::hibernate::wake_pane(store, pane).map(drop),
        |_| Ok(()),
    )
    .map(|(pane, ())| pane)
}

/// The pane hosting the agent `(kind, launch)`, read, woken, locked for writing, and read
/// again; the writer comes back only when the second read answers the pane it locks, and is
/// dropped, releasing the lock, otherwise. The notices of both reads are pushed to `notices`
/// for the caller to print once.
pub fn open_writer(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    kind: &AgentKind,
    launch: &AgentSessionId,
    notices: &mut Vec<String>,
) -> Result<(AgentPane, PaneWriter), FindErr> {
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_AGENT_PANE_BEFORE_READ");
    find_with(
        mux,
        |mux| read_answer(workspace, store, mux, kind, launch, notices),
        |pane| crate::harness::hibernate::wake_pane(store, pane).map(drop),
        |pane| {
            PaneWriter::open(
                Some(store.runtime_paths()),
                &pane.pane_id,
                &pane.session_name,
            )
        },
    )
}

fn read_answer(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    kind: &AgentKind,
    launch: &AgentSessionId,
    notices: &mut Vec<String>,
) -> Result<(MuxName, Option<AgentPane>), HostingErr> {
    match read_hosting(workspace, store, mux) {
        Ok(mut read) => {
            notices.extend(read.take_notices());
            let answer = read.agent_hosting(kind, launch);
            let pane = match (answer.hosting, answer.holder) {
                (
                    Hosting::Pane {
                        pane_id,
                        session_name,
                    },
                    Some(holder),
                ) => Some(AgentPane {
                    pane_id,
                    session_name,
                    holder: (holder.kind.clone(), holder.agent_id.clone()),
                }),
                _ => None,
            };
            Ok((read.mux(), pane))
        }
        Err(mut err) => {
            notices.append(&mut err.notices);
            Err(err)
        }
    }
}

/// Read, wake the answered pane, `hold` it, and read again on the first read's backend:
/// the hold comes back with the second read's answer only when both reads answer the same
/// pane and session, and is dropped otherwise.
fn find_with<H>(
    mux: Option<MuxName>,
    mut read: impl FnMut(Option<MuxName>) -> Result<(MuxName, Option<AgentPane>), HostingErr>,
    wake: impl FnOnce(&PaneId) -> Result<(), crate::harness::hibernate::HibernateErr>,
    hold: impl FnOnce(&AgentPane) -> Result<H, MuxErr>,
) -> Result<(AgentPane, H), FindErr> {
    let (mux, first) = read(mux).map_err(FindErr::Read)?;
    let first = first.ok_or(FindErr::Nowhere)?;
    wake(&first.pane_id).map_err(|source| FindErr::Wake {
        pane: first.pane_id.clone(),
        source,
    })?;
    let held = hold(&first).map_err(|source| FindErr::Lock {
        pane: first.pane_id.clone(),
        source,
    })?;
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_AGENT_PANE_AFTER_WAKE");
    let (_, second) = read(Some(mux)).map_err(FindErr::Read)?;
    match second {
        Some(second)
            if second.pane_id == first.pane_id && second.session_name == first.session_name =>
        {
            Ok((second, held))
        }
        _ => Err(FindErr::Moved {
            pane: first.pane_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::produce::ProduceErr;

    fn held(id: &str, session: &str) -> Option<AgentPane> {
        Some(AgentPane {
            pane_id: PaneId::from_parts(MuxName::Zellij, id),
            session_name: "room".to_owned(),
            holder: (
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from(session),
            ),
        })
    }

    fn pane(id: &str) -> Option<AgentPane> {
        held(id, "planner")
    }

    fn failed() -> HostingErr {
        HostingErr {
            notices: vec!["skipped tmux".to_owned()],
            source: ProduceErr::PaneDiscovery(crate::mux::MuxErr::SessionNotFound {
                session: "room".to_owned(),
            }),
        }
    }

    /// Run `find_with` over `answers`, one per read, recording the backend each read
    /// was pinned to and the panes woken.
    fn run(
        answers: Vec<Result<Option<AgentPane>, HostingErr>>,
    ) -> (
        Result<AgentPane, FindErr>,
        Vec<Option<MuxName>>,
        Vec<PaneId>,
    ) {
        let mut answers = answers.into_iter();
        let mut pins = Vec::new();
        let mut woken = Vec::new();
        let found = find_with(
            None,
            |mux| {
                pins.push(mux);
                answers
                    .next()
                    .expect("no read past the answers")
                    .map(|pane| (MuxName::Tmux, pane))
            },
            |pane| {
                woken.push(pane.clone());
                Ok(())
            },
            |_| Ok(()),
        )
        .map(|(pane, ())| pane);
        (found, pins, woken)
    }

    /// What one `find_with` run under a writer's hold did, in order.
    #[derive(Debug, Default, PartialEq)]
    struct Steps {
        reads: usize,
        woken: usize,
        locked: usize,
        released: usize,
    }

    /// A pane lock stand-in that counts its release.
    struct Held<'a>(&'a std::cell::RefCell<Steps>);

    impl Drop for Held<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().released += 1;
        }
    }

    /// Run `find_with` as `open_writer` does, the wake and the lock failing when told to,
    /// over `answers`, one per read.
    fn run_writer(
        answers: Vec<Result<Option<AgentPane>, HostingErr>>,
        wake_fails: bool,
        lock_fails: bool,
    ) -> (Result<AgentPane, FindErr>, Steps) {
        let steps = std::cell::RefCell::new(Steps::default());
        let mut answers = answers.into_iter();
        let found = find_with(
            None,
            |_| {
                steps.borrow_mut().reads += 1;
                answers
                    .next()
                    .expect("no read past the answers")
                    .map(|pane| (MuxName::Tmux, pane))
            },
            |_| {
                steps.borrow_mut().woken += 1;
                if wake_fails {
                    return Err(crate::harness::hibernate::HibernateErr::Store(
                        crate::store::StoreErr::AgentTune("thaw failed".to_owned()),
                    ));
                }
                Ok(())
            },
            |_| {
                if lock_fails {
                    return Err(crate::mux::MuxErr::SessionNotFound {
                        session: "room".to_owned(),
                    });
                }
                steps.borrow_mut().locked += 1;
                Ok(Held(&steps))
            },
        )
        .map(|(pane, held)| {
            assert_eq!(steps.borrow().released, 0, "the writer comes back held");
            drop(held);
            pane
        });
        (found, steps.into_inner())
    }

    #[test]
    fn a_pane_both_reads_answer_is_found_after_its_wake_on_the_first_reads_backend() {
        let (found, pins, woken) = run(vec![Ok(pane("terminal_1")), Ok(pane("terminal_1"))]);
        assert_eq!(found.unwrap(), pane("terminal_1").unwrap());
        assert_eq!(pins, [None, Some(MuxName::Tmux)]);
        assert_eq!(woken, [PaneId::from_parts(MuxName::Zellij, "terminal_1")]);
    }

    #[test]
    fn the_holder_found_is_the_one_the_second_read_answers_in_the_pane() {
        let (found, _, _) = run(vec![
            Ok(held("terminal_1", "planner")),
            Ok(held("terminal_1", "revived")),
        ]);
        assert_eq!(found.unwrap(), held("terminal_1", "revived").unwrap());
    }

    #[test]
    fn an_agent_hosted_nowhere_refuses_without_a_wake_or_a_second_read() {
        let (found, pins, woken) = run(vec![Ok(None)]);
        assert!(matches!(found, Err(FindErr::Nowhere)), "{found:?}");
        assert_eq!(pins.len(), 1);
        assert!(woken.is_empty());
    }

    #[test]
    fn an_agent_that_moves_or_leaves_during_the_wake_refuses_naming_the_pane_it_left() {
        for after in [pane("terminal_2"), None] {
            let (found, _, woken) = run(vec![Ok(pane("terminal_1")), Ok(after)]);
            assert_eq!(woken.len(), 1);
            match found {
                Err(err @ FindErr::Moved { .. }) => assert_eq!(
                    err.to_string(),
                    "the agent left pane zellij:terminal_1 while it was woken"
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_failed_read_refuses_before_or_after_the_wake() {
        let (found, _, woken) = run(vec![Err(failed())]);
        assert!(matches!(found, Err(FindErr::Read(_))), "{found:?}");
        assert!(woken.is_empty());
        let (found, _, woken) = run(vec![Ok(pane("terminal_1")), Err(failed())]);
        assert!(matches!(found, Err(FindErr::Read(_))), "{found:?}");
        assert_eq!(woken.len(), 1);
    }

    #[test]
    fn a_writer_both_reads_agree_on_comes_back_locked_after_the_wake() {
        let (found, steps) = run_writer(
            vec![Ok(pane("terminal_1")), Ok(pane("terminal_1"))],
            false,
            false,
        );
        assert_eq!(found.unwrap(), pane("terminal_1").unwrap());
        assert_eq!(
            steps,
            Steps {
                reads: 2,
                woken: 1,
                locked: 1,
                released: 1,
            }
        );
    }

    #[test]
    fn a_writer_for_an_agent_hosted_nowhere_wakes_and_locks_nothing() {
        let (found, steps) = run_writer(vec![Ok(None)], false, false);
        assert!(matches!(found, Err(FindErr::Nowhere)), "{found:?}");
        assert_eq!(
            steps,
            Steps {
                reads: 1,
                ..Steps::default()
            }
        );
    }

    #[test]
    fn a_writer_whose_agent_moves_or_leaves_before_the_second_read_is_released() {
        for after in [pane("terminal_2"), None] {
            let (found, steps) = run_writer(vec![Ok(pane("terminal_1")), Ok(after)], false, false);
            assert!(
                matches!(&found, Err(FindErr::Moved { pane }) if pane.to_string() == "zellij:terminal_1"),
                "{found:?}"
            );
            assert_eq!((steps.locked, steps.released), (1, 1), "{steps:?}");
        }
    }

    #[test]
    fn a_writer_refuses_on_a_failed_read_wake_or_lock_and_holds_nothing() {
        let (found, steps) = run_writer(vec![Err(failed())], false, false);
        assert!(matches!(found, Err(FindErr::Read(_))), "{found:?}");
        assert_eq!((steps.woken, steps.locked), (0, 0));
        let (found, steps) = run_writer(vec![Ok(pane("terminal_1")), Err(failed())], false, false);
        assert!(matches!(found, Err(FindErr::Read(_))), "{found:?}");
        assert_eq!((steps.locked, steps.released), (1, 1));
        let (found, steps) = run_writer(vec![Ok(pane("terminal_1"))], true, false);
        assert!(matches!(found, Err(FindErr::Wake { .. })), "{found:?}");
        assert_eq!((steps.reads, steps.locked), (1, 0));
        let (found, steps) = run_writer(vec![Ok(pane("terminal_1"))], false, true);
        match found {
            Err(err @ FindErr::Lock { .. }) => assert!(
                err.to_string()
                    .starts_with("locking pane zellij:terminal_1 for writing: "),
                "{err}"
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!((steps.reads, steps.woken, steps.locked), (1, 1, 0));
    }

    #[test]
    fn a_pane_lock_a_moved_agent_left_can_be_taken_again() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/room")),
            root.path(),
        )
        .unwrap();
        let mut answers = vec![Ok(pane("terminal_1")), Ok(pane("terminal_2"))].into_iter();
        let found = find_with(
            None,
            |_| answers.next().unwrap().map(|pane| (MuxName::Tmux, pane)),
            |_| Ok(()),
            |pane| PaneWriter::open(Some(&runtime), &pane.pane_id, &pane.session_name),
        );
        assert!(matches!(found, Err(FindErr::Moved { .. })));
        let lock = runtime.pane_write_lock(&PaneId::from_parts(MuxName::Zellij, "terminal_1"));
        assert!(
            crate::disk::lock::WorkspaceLock::try_acquire(&lock)
                .unwrap()
                .is_some()
        );
    }
}
