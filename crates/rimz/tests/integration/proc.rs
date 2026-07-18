#[cfg(target_os = "linux")]
mod linux {
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn children_unions_child_lists_across_process_tasks() {
        let main_child = ChildGuard::new(spawn_sleep().expect("spawn child from main thread"));

        let (child_tx, child_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let child = match spawn_sleep() {
                Ok(child) => ChildGuard::new(child),
                Err(err) => {
                    let _ = child_tx.send(Err(err));
                    return;
                }
            };
            let _ = child_tx.send(Ok(child.id()));
            let _ = release_rx.recv();
        });

        let worker_child_pid = child_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker reported child")
            .expect("spawn child from worker thread");
        let expected = [main_child.id(), worker_child_pid];

        let actual = wait_for_expected_children(&expected);

        drop(main_child);
        let _ = release_tx.send(());
        worker.join().expect("worker thread exits");

        assert!(
            expected.iter().all(|pid| actual.contains(pid)),
            "children() returned {actual:?}; expected main-thread and worker-thread children {expected:?}"
        );
    }

    #[test]
    fn process_domain_distinguishes_inherited_and_sandboxed_children() {
        let inherited = ChildGuard::new(spawn_sleep().expect("spawn inherited child"));
        let sandbox = tempfile::tempdir().expect("sandbox tempdir");
        let mut sandbox_command = sleep_command();
        sandbox_command
            .env("HOME", sandbox.path().join("home"))
            .env("XDG_STATE_HOME", sandbox.path().join("state"))
            .env("XDG_RUNTIME_DIR", sandbox.path().join("runtime"))
            .env("TMUX_TMPDIR", sandbox.path().join("tmux"))
            .env("TMPDIR", sandbox.path().join("tmp"));
        let sandboxed = ChildGuard::new(sandbox_command.spawn().expect("spawn sandboxed child"));

        let current = rimz::mux::domain::ProcessDomain::current();
        let inherited_domain = rimz::mux::domain::ProcessDomain::of_process(inherited.id())
            .expect("read inherited child environment");
        let sandboxed_domain = rimz::mux::domain::ProcessDomain::of_process(sandboxed.id())
            .expect("read sandboxed child environment");

        assert!(current.same_world(&inherited_domain));
        assert!(!current.same_world(&sandboxed_domain));
        assert_eq!(rimz::mux::domain::ProcessDomain::of_process(u32::MAX), None,);
    }

    fn spawn_sleep() -> std::io::Result<Child> {
        sleep_command().spawn()
    }

    fn sleep_command() -> Command {
        let mut command = Command::new("sleep");
        command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn wait_for_expected_children(expected: &[u32]) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let actual = rimz::proc::children(std::process::id());
            if expected.iter().all(|pid| actual.contains(pid)) || Instant::now() >= deadline {
                return actual;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    struct ChildGuard {
        child: Child,
    }

    impl ChildGuard {
        fn new(child: Child) -> Self {
            Self { child }
        }

        fn id(&self) -> u32 {
            self.child.id()
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
