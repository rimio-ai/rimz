use super::*;
use crate::agents::ResumeProbe;
use crate::proc::HostedAgentProcess;
use std::cell::{Cell, RefCell};

struct ResumeFixture {
    hosted: RefCell<Option<HostedAgentProcess>>,
    root_command: RefCell<String>,
    hosted_command: RefCell<String>,
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
            root_start: &|_, pid| start(pid),
            cwd_starts: &|_, _| Vec::new(),
            hosted_absent: &|_, _| false,
            process_start: &|_| None,
            elevated_agent: &|_| None,
            backfill_pids: &|_, _| {},
        };
        repair_pane_frame(
            frame,
            None,
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
