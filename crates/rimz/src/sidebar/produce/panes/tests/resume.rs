use super::*;
use crate::agents::ResumeProbe;
use crate::proc::HostedAgentProcess;
use std::cell::{Cell, RefCell};

struct ResumeFixture {
    hosted: RefCell<Option<HostedAgentProcess>>,
    root_command: RefCell<String>,
    hosted_command: RefCell<String>,
    hosted_absent: Cell<bool>,
    live_start: Cell<Option<jiff::Timestamp>>,
    answer: RefCell<ResumeProbe>,
    calls: Cell<usize>,
    reads: RefCell<Vec<(&'static str, u32)>>,
}

impl ResumeFixture {
    fn new() -> Self {
        Self {
            hosted: RefCell::new(Some(HostedAgentProcess {
                kind: AgentKind::new_unchecked("claude"),
                pid: 76,
                started_at: jiff::Timestamp::from_second(10).unwrap(),
                cwd: None,
                lineage: vec![76],
            })),
            root_command: RefCell::new("zsh".into()),
            hosted_command: RefCell::new(
                "rimz agents exec claude --request-file /runtime/exec.json".into(),
            ),
            hosted_absent: Cell::new(false),
            live_start: Cell::new(None),
            answer: RefCell::new(ResumeProbe::Settled),
            calls: Cell::new(0),
            reads: RefCell::new(Vec::new()),
        }
    }

    fn candidate(&self) -> PaneFrame {
        let mut pane = pane(
            "terminal_1",
            Some(&self.root_command.borrow()),
            Some("/repo"),
        );
        pane.pane_pid = Some(42);
        pane.spawn_command = Some(self.root_command.borrow().clone());
        frame(vec![pane])
    }

    fn repair(&self, frame: &mut PaneFrame, memo: &mut SettledResumes) {
        self.repair_with_prior(frame, None, memo);
    }

    fn repair_with_prior(
        &self,
        frame: &mut PaneFrame,
        prior: Option<&PaneFrame>,
        memo: &mut SettledResumes,
    ) {
        self.reads.borrow_mut().clear();
        let commands = RefCell::new(HashMap::new());
        let cmdline = |pid| {
            // Count source reads, matching the production-local process read memo.
            commands
                .borrow_mut()
                .entry(pid)
                .or_insert_with(|| {
                    self.reads.borrow_mut().push(("cmdline", pid));
                    Some(if pid == 42 {
                        self.root_command.borrow().clone()
                    } else {
                        self.hosted_command.borrow().clone()
                    })
                })
                .clone()
        };
        let children = |pid| {
            self.reads.borrow_mut().push(("children", pid));
            self.hosted
                .borrow()
                .as_ref()
                .map_or_else(Vec::new, |hosted| vec![hosted.pid])
        };
        let start = |pid| {
            self.reads.borrow_mut().push(("start", pid));
            self.hosted
                .borrow()
                .as_ref()
                .map(|hosted| hosted.started_at)
        };
        let hosted = |root| {
            assert_eq!(root, 42);
            cmdline(root);
            let hosted = self.hosted.borrow().clone()?;
            if hosted.pid != root {
                children(root);
            }
            cmdline(hosted.pid);
            start(hosted.pid);
            Some(hosted)
        };
        let resumed = |root| {
            assert_eq!(root, 42);
            self.calls.set(self.calls.get() + 1);
            self.answer.borrow().clone()
        };
        let readers = RepairReaders {
            cmdline: &cmdline,
            comm: &|_| None,
            children: &children,
            hosted_process: &hosted,
            cwd: &|_| None,
            resumed_session: &resumed,
            root_start: &|_, pid| self.live_start.get().or_else(|| start(pid)),
            cwd_starts: &|_, _| Vec::new(),
            hosted_absent: &|_, _| self.hosted_absent.get(),
            process_start: &|_| self.live_start.get(),
            elevated_agent: &|_| None,
            backfill_pids: &|_, _| {},
        };
        repair_pane_frame(
            frame,
            prior,
            "s",
            None,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            memo,
            &readers,
        );
    }
}

#[test]
fn resumed_session_follows_a_new_hosted_agent_under_the_same_shell() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-x".into());
    let mut prior = fixture.candidate();
    fixture.repair(&mut prior, &mut memo);
    assert_eq!(
        first(&prior).current.resumed_session_id.as_deref(),
        Some("session-x")
    );
    assert_eq!(fixture.calls.get(), 1);

    {
        let mut hosted = fixture.hosted.borrow_mut();
        let hosted = hosted.as_mut().unwrap();
        hosted.pid = 77;
        hosted.lineage = vec![77];
        hosted.started_at = jiff::Timestamp::from_second(20).unwrap();
    }
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-y".into());
    let mut fresh = fixture.candidate();
    fixture.repair_with_prior(&mut fresh, Some(&prior), &mut memo);
    assert_eq!(
        first(&fresh).current.resumed_session_id.as_deref(),
        Some("session-y")
    );
    assert_eq!(
        fixture.calls.get(),
        2,
        "a new hosted agent needs one new walk"
    );
}

#[test]
fn resumed_session_disappears_when_the_hosted_agent_is_proven_absent() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-x".into());
    let mut prior = fixture.candidate();
    fixture.repair(&mut prior, &mut memo);
    assert_eq!(
        first(&prior).current.resumed_session_id.as_deref(),
        Some("session-x")
    );

    fixture.hosted.borrow_mut().take();
    fixture.hosted_absent.set(true);
    let mut fresh = fixture.candidate();
    fixture.repair_with_prior(&mut fresh, Some(&prior), &mut memo);
    assert_eq!(first(&fresh).current.resumed_session_id, None);
    assert_eq!(fixture.calls.get(), 1, "an idle shell needs no resume walk");
}

#[test]
fn resumed_session_survives_an_indeterminate_miss_only_until_the_hosted_carry_expires() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-x".into());
    let mut prior = fixture.candidate();
    fixture.repair(&mut prior, &mut memo);
    assert_eq!(
        first(&prior).current.resumed_session_id.as_deref(),
        Some("session-x")
    );

    fixture.hosted.borrow_mut().take();
    let mut carried = fixture.candidate();
    fixture.repair_with_prior(&mut carried, Some(&prior), &mut memo);
    assert_eq!(
        first(&carried).current.resumed_session_id.as_deref(),
        Some("session-x")
    );
    assert!(first(&carried).hosted_carry_since_ms.is_some());
    assert_eq!(fixture.calls.get(), 1);

    first_mut(&mut carried).hosted_carry_since_ms =
        Some(unix_now_ms() - crate::sidebar::timing::PANE_CARRY_TTL.as_millis() as u64 - 1);
    let mut expired = fixture.candidate();
    fixture.repair_with_prior(&mut expired, Some(&carried), &mut memo);
    assert_eq!(first(&expired).current.resumed_session_id, None);
    assert_eq!(
        fixture.calls.get(),
        1,
        "an expired idle shell needs no resume walk"
    );
}

#[test]
fn resumed_session_is_withheld_when_the_pid_reuse_check_drops_the_binding() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-x".into());
    let mut prior = fixture.candidate();
    fixture.repair(&mut prior, &mut memo);
    assert_eq!(
        first(&prior).current.resumed_session_id.as_deref(),
        Some("session-x")
    );

    // The hosted scan still matches the prior identity; the live start does not.
    fixture
        .live_start
        .set(Some(jiff::Timestamp::from_second(20).unwrap()));
    let mut reused = fixture.candidate();
    fixture.repair_with_prior(&mut reused, Some(&prior), &mut memo);
    assert_eq!(first(&reused).current.pid, None);
    assert_eq!(first(&reused).current.resumed_session_id, None);
    assert_eq!(
        fixture.calls.get(),
        1,
        "a dropped binding has no root to walk"
    );
}

#[test]
fn resumed_session_stays_without_new_walks_for_an_unchanged_or_parked_hosted_agent() {
    for hosted_root in [false, true] {
        let fixture = ResumeFixture::new();
        if hosted_root {
            let mut hosted = fixture.hosted.borrow_mut();
            let hosted = hosted.as_mut().unwrap();
            hosted.pid = 42;
            hosted.lineage.clear();
            *fixture.root_command.borrow_mut() = fixture.hosted_command.borrow().clone();
        }
        let mut memo = SettledResumes::new();
        *fixture.answer.borrow_mut() = ResumeProbe::Resumed("session-x".into());
        let mut prior = fixture.candidate();
        fixture.repair(&mut prior, &mut memo);
        assert_eq!(
            first(&prior).current.resumed_session_id.as_deref(),
            Some("session-x")
        );
        assert_eq!(fixture.calls.get(), 1);

        let mut unchanged = fixture.candidate();
        fixture.repair_with_prior(&mut unchanged, Some(&prior), &mut memo);
        assert_eq!(
            first(&unchanged).current.resumed_session_id.as_deref(),
            Some("session-x")
        );
        assert_eq!(
            fixture.calls.get(),
            1,
            "an unchanged agent keeps its resolved id"
        );

        fixture
            .hosted_command
            .borrow_mut()
            .push_str(" --supervise /runtime/park.json");
        if hosted_root {
            *fixture.root_command.borrow_mut() = fixture.hosted_command.borrow().clone();
        }
        let mut parked = fixture.candidate();
        // A park re-execs in place, so the pane keeps the argv it was born with.
        first_mut(&mut parked).current.spawn_command =
            first(&unchanged).current.spawn_command.clone();
        fixture.repair_with_prior(&mut parked, Some(&unchanged), &mut memo);
        assert_eq!(
            first(&parked).current.resumed_session_id.as_deref(),
            Some("session-x")
        );
        assert_eq!(
            fixture.calls.get(),
            1,
            "a park changes argv, not the hosted identity"
        );
    }
}

#[test]
fn settled_resume_probes_cost_the_same_reads_as_bound_panes_and_track_hosted_identity() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 1);
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(
        fixture.calls.get(),
        1,
        "settled wrapper must not walk again"
    );
    let settled_reads = fixture.reads.borrow().clone();
    let mut bound = fixture.candidate();
    first_mut(&mut bound).current.resumed_session_id = Some("bound".into());
    fixture.repair(&mut bound, &mut memo);
    assert_eq!(*fixture.reads.borrow(), settled_reads);

    fixture
        .hosted_command
        .borrow_mut()
        .push_str(" --supervise /runtime/park.json");
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 2, "park re-exec changes argv");
    fixture.hosted.borrow_mut().as_mut().unwrap().started_at =
        jiff::Timestamp::from_second(20).unwrap();
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 3, "pid reuse changes start");
    {
        let mut hosted = fixture.hosted.borrow_mut();
        let hosted = hosted.as_mut().unwrap();
        hosted.pid = 77;
        hosted.lineage = vec![77];
    }
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(
        fixture.calls.get(),
        4,
        "new hosted child under the same shell"
    );
    {
        let mut hosted = fixture.hosted.borrow_mut();
        let hosted = hosted.as_mut().unwrap();
        hosted.pid = 42;
        hosted.lineage.clear();
    }
    *fixture.root_command.borrow_mut() = fixture.hosted_command.borrow().clone();
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 5, "hosted root uses an empty lineage");
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 5);

    fixture.repair(&mut frame(Vec::new()), &mut memo);
    assert!(memo.is_empty(), "departed panes release their memo entries");
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(fixture.calls.get(), 6);
}

#[test]
fn unsettled_or_unstamped_resume_probes_keep_retrying_and_can_bind() {
    let fixture = ResumeFixture::new();
    let mut memo = SettledResumes::new();
    *fixture.answer.borrow_mut() = ResumeProbe::Unsettled;
    for calls in 1..=2 {
        fixture.repair(&mut fixture.candidate(), &mut memo);
        assert_eq!(fixture.calls.get(), calls);
        assert!(memo.is_empty());
    }
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("resumed".into());
    let mut resumed = fixture.candidate();
    fixture.repair(&mut resumed, &mut memo);
    assert_eq!(
        first(&resumed).current.resumed_session_id.as_deref(),
        Some("resumed")
    );

    fixture.hosted.borrow_mut().take();
    *fixture.root_command.borrow_mut() = "claude".into();
    *fixture.answer.borrow_mut() = ResumeProbe::Settled;
    for calls in 4..=5 {
        fixture.repair(&mut fixture.candidate(), &mut memo);
        assert_eq!(
            fixture.calls.get(),
            calls,
            "no hosted stamp cannot settle a pane"
        );
        assert!(memo.is_empty());
    }
}

#[test]
fn a_shell_that_later_hosts_a_resumed_codex_still_binds() {
    let fixture = ResumeFixture::new();
    let hosted = fixture.hosted.borrow_mut().take().unwrap();
    *fixture.answer.borrow_mut() = ResumeProbe::Resumed("shell-resume".into());
    let mut memo = SettledResumes::new();
    fixture.repair(&mut fixture.candidate(), &mut memo);
    assert_eq!(
        fixture.calls.get(),
        0,
        "plain shells do not pass the adapter gate"
    );
    *fixture.hosted.borrow_mut() = Some(HostedAgentProcess {
        kind: AgentKind::new_unchecked("codex"),
        ..hosted
    });
    *fixture.hosted_command.borrow_mut() = "codex resume shell-resume".into();
    let mut resumed = fixture.candidate();
    fixture.repair(&mut resumed, &mut memo);
    assert_eq!(fixture.calls.get(), 1);
    assert_eq!(
        first(&resumed).current.resumed_session_id.as_deref(),
        Some("shell-resume")
    );
}
