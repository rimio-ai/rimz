use super::*;

struct FakeHost {
    root: tempfile::TempDir,
    missing: Option<&'static str>,
}

impl FakeHost {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let slice = root.path().join("app.slice");
        std::fs::create_dir(&slice).unwrap();
        std::fs::write(slice.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        std::fs::write(slice.join("cgroup.freeze"), "0\n").unwrap();
        std::fs::write(slice.join("memory.reclaim"), "").unwrap();
        std::fs::write(slice.join("memory.swap.max"), "max\n").unwrap();
        std::fs::write(root.path().join("memory.swap.max"), "max\n").unwrap();
        Self {
            root,
            missing: None,
        }
    }
}

impl Host for FakeHost {
    fn systemd_run(&self) -> bool {
        self.missing != Some("systemd-run")
    }
    fn manager(&self) -> bool {
        self.missing != Some("user manager")
    }
    fn app_slice(&self) -> Result<PathBuf, String> {
        Ok(self.root.path().join("app.slice"))
    }
    fn root(&self) -> &Path {
        self.root.path()
    }
    fn writable(&self, path: &Path) -> bool {
        path.exists()
            && self.missing != path.file_name().and_then(|name| name.to_str())
            && !(self.missing == Some("memory controller") && path.is_dir())
    }
    fn read(&self, path: &Path) -> std::io::Result<String> {
        if path == Path::new("/proc/meminfo") {
            let swap = if self.missing == Some("swap") { 0 } else { 100 };
            return Ok(format!(
                "MemTotal: 100 kB\nMemAvailable: 50 kB\nSwapTotal: {swap} kB\nSwapFree: 20 kB\n"
            ));
        }
        std::fs::read_to_string(path)
    }
}

#[test]
fn preflight_checks_each_precondition_and_ends_errors_with_fixes() {
    let config = HibernateConfig {
        enabled: true,
        ..Default::default()
    };
    for (name, fix) in [
        (
            "systemd-run",
            "install systemd or set agents.hibernate.enabled = false",
        ),
        (
            "user manager",
            "run loginctl enable-linger <user> or log in through a systemd session",
        ),
        (
            "memory controller",
            "enable delegation with systemctl edit user@.service and Delegate=memory pids cpu",
        ),
        (
            "swap",
            "add zram, zswap or a swapfile; in a container, allow swap",
        ),
        ("swap limit", "raise the ancestor memory.swap.max limit"),
        (
            "cgroup.freeze",
            "enable writable cgroup-v2 freezer delegation or set agents.hibernate.enabled = false",
        ),
        (
            "memory.reclaim",
            "use Linux 5.19 or newer with writable memory controller delegation or set agents.hibernate.enabled = false",
        ),
    ] {
        let mut host = FakeHost::new();
        host.missing = Some(name);
        if name == "swap limit" {
            std::fs::write(host.root.path().join("memory.swap.max"), "0\n").unwrap();
        }
        let report = inspect_with(&config, &host);
        let failures: Vec<_> = report
            .checks
            .iter()
            .filter(|check| matches!(check.status, CheckStatus::Error))
            .collect();
        assert_eq!(failures.len(), 1, "{name}: {report:?}");
        assert_eq!(failures[0].name, name);
        assert!(failures[0].detail.ends_with(fix), "{report:?}");
    }
    let host = FakeHost::new();
    let report = inspect_with(&config, &host);
    assert_eq!(report.checks.len(), 7);
    assert!(
        report
            .checks
            .iter()
            .all(|check| matches!(check.status, CheckStatus::Ok))
    );
}

#[test]
fn disabled_preflight_does_not_probe() {
    struct NoHost;
    impl Host for NoHost {
        fn systemd_run(&self) -> bool {
            panic!("disabled probe")
        }
        fn manager(&self) -> bool {
            panic!("disabled probe")
        }
        fn app_slice(&self) -> Result<PathBuf, String> {
            panic!("disabled probe")
        }
        fn read(&self, _: &Path) -> std::io::Result<String> {
            panic!("disabled probe")
        }
        fn writable(&self, _: &Path) -> bool {
            panic!("disabled probe")
        }
        fn root(&self) -> &Path {
            panic!("disabled probe")
        }
    }
    assert!(
        inspect_with(&HibernateConfig::default(), &NoHost)
            .checks
            .is_empty()
    );
}

#[test]
fn scope_argv_preserves_inner_command_and_names_workspace() {
    let workspace = WorkspaceId::from_project_root(Path::new("/project"));
    let argv = vec!["/bin/bwrap".into(), "--".into(), "provider".into()];
    let wrapped = scope_argv(&workspace, "claude", &argv);
    assert_eq!(wrapped.len(), 12 + argv.len());
    assert_eq!(
        &wrapped[..8],
        [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "-p",
            "MemoryAccounting=yes",
            "--unit"
        ]
    );
    assert!(wrapped[8].starts_with(&scope_prefix(&workspace)));
    assert!(wrapped[8].ends_with(".scope"));
    assert!(
        wrapped[8]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    );
    assert_eq!(
        &wrapped[9..12],
        ["--description", "RimZ agent claude", "--"]
    );
    assert_eq!(&wrapped[12..], argv);
    assert_ne!(wrapped[8], scope_argv(&workspace, "claude", &argv)[8]);
}
