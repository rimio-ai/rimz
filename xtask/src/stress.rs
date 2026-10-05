//! `cargo xtask stress`: loop one test's own binary under CPU load and count
//! the failing copies.
//!
//! A flake that shows only under full-suite contention needs hundreds of runs
//! of one test with every CPU busy. Nextest retries, groups, and throttles, so
//! the copies run the test binary directly, in the sandbox roots and with the
//! session scrub `cargo xtask test` gives it. The CPU hogs are this binary
//! re-entered at a hidden argument; each watches a pipe from its owner and
//! exits when it closes, so no hog outlives the command however it ends.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::deadline;
use crate::gates::{self, LocatedTest};
use crate::sandbox::{self, HostSandbox};

const HOG_ARG: &str = "__stress-hog";
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
struct Options {
    test: String,
    copies: usize,
    jobs: usize,
    hogs: usize,
    env: Vec<(String, String)>,
    out: Option<PathBuf>,
}

struct Report {
    text: String,
    failed: usize,
    summary: PathBuf,
    stopped: Option<anyhow::Error>,
}

#[expect(
    clippy::print_stdout,
    reason = "the stress summary is the command's stdout contract"
)]
#[expect(
    clippy::print_stderr,
    reason = "xtask tells the operator where the summary was kept"
)]
pub(crate) fn run(root: &Path, args: &[String]) -> Result<()> {
    let cpus = thread::available_parallelism().map_or(1, usize::from);
    let options = parse(args, cpus)?;
    let out = output_dir(options.out.as_deref())?;
    eprintln!("output: {}", out.display());
    let sandbox = HostSandbox::for_tests(root)?;
    let test = gates::locate_test(root, &options.test, &sandbox)?;
    let executable = sandbox::self_executable()?;
    let hog = || {
        let mut command = Command::new(&executable);
        command.arg(HOG_ARG);
        command
    };
    let report = stress(root, &options, &test, &sandbox, cpus, &out, &hog)?;
    print!("{}", report.text);
    eprintln!("summary: {}", report.summary.display());
    if let Some(stopped) = report.stopped {
        return Err(stopped);
    }
    if report.failed > 0 {
        bail!("{} of {} copies failed", report.failed, options.copies);
    }
    Ok(())
}

/// Becomes a CPU hog, and never returns, when xtask was started as one.
pub(crate) fn run_hog_mode(args: &[String]) {
    if args.len() == 1 && args[0] == HOG_ARG {
        hog();
    }
}

fn parse(args: &[String], cpus: usize) -> Result<Options> {
    let mut test: Option<&str> = None;
    let mut options = Options {
        test: String::new(),
        copies: 400,
        jobs: cpus * 2,
        hogs: cpus,
        env: Vec::new(),
        out: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') {
            if let Some(first) = test {
                bail!("cargo xtask stress takes one test name; got `{first}` and `{arg}`\n{HELP}");
            }
            test = Some(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        if !matches!(flag, "--copies" | "--jobs" | "--hogs" | "--env" | "--out") {
            bail!("cargo xtask stress does not accept `{flag}`\n{HELP}");
        }
        let Some(value) = inline.or_else(|| args.next().map(String::as_str)) else {
            bail!("cargo xtask stress `{flag}` requires a value\n{HELP}");
        };
        match flag {
            "--copies" => options.copies = count(flag, value, 1)?,
            "--jobs" => options.jobs = count(flag, value, 1)?,
            "--hogs" => options.hogs = count(flag, value, 0)?,
            "--env" => {
                let Some((key, value)) = value.split_once('=').filter(|(key, _)| !key.is_empty())
                else {
                    bail!("cargo xtask stress `--env` expects KEY=VALUE; got `{value}`\n{HELP}");
                };
                options.env.push((key.to_owned(), value.to_owned()));
            }
            _ => options.out = Some(PathBuf::from(value)),
        }
    }
    let Some(test) = test else {
        bail!("cargo xtask stress requires a test name\n{HELP}");
    };
    options.test = test.to_owned();
    Ok(options)
}

const HELP: &str = "NEXT: see `cargo xtask stress --help`";

fn count(flag: &str, value: &str, least: usize) -> Result<usize> {
    match value.parse::<usize>() {
        Ok(count) if count >= least => Ok(count),
        _ => bail!(
            "cargo xtask stress `{flag}` expects a whole number of at least {least}; got `{value}`\n{HELP}"
        ),
    }
}

/// The batch's output directory: the caller's or a fresh one, never removed.
/// A directory an earlier batch kept copies in is refused untouched.
fn output_dir(out: Option<&Path>) -> Result<PathBuf> {
    match out {
        Some(out) => {
            fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
            let entries =
                fs::read_dir(out).with_context(|| format!("reading {}", out.display()))?;
            for entry in entries {
                let name = entry
                    .with_context(|| format!("reading {}", out.display()))?
                    .file_name();
                let name = name.to_string_lossy();
                if name.starts_with("run-") && name.ends_with(".out") {
                    bail!(
                        "`--out` {} already holds run-*.out from an earlier batch, which this batch would mix with or overwrite\nNEXT: name an empty directory with `--out`, or omit it for a fresh one",
                        out.display()
                    );
                }
            }
            Ok(out.to_path_buf())
        }
        None => Ok(tempfile::Builder::new()
            .prefix("rimz-stress-")
            .tempdir()
            .context("creating the stress output directory")?
            .keep()),
    }
}

/// Run the batch, keep each failing copy's output, and write the summary
/// beside them in `out`.
fn stress(
    root: &Path,
    options: &Options,
    test: &LocatedTest,
    sandbox: &HostSandbox,
    cpus: usize,
    out: &Path,
    hog: &dyn Fn() -> Command,
) -> Result<Report> {
    let mut text = format!(
        "test: {}\nbinary: {}\ncommit: {}\ncopies: {}\njobs: {}\nhogs: {}\ncpus: {cpus}\n",
        test.name,
        test.binary.display(),
        commit(root),
        options.copies,
        options.jobs,
        options.hogs,
    );

    text.push_str(&format!("load before: {}\n", load_average()));
    let hogs = Hogs::start(options.hogs, hog)?;
    let batch = run_copies(options, test, sandbox, out);
    text.push_str(&format!("load after: {}\n", load_average()));
    drop(hogs);

    let failed = batch.failed.len();
    let completed = batch.completed;
    match &batch.error {
        None => text.push_str(&format!("failed: {failed}/{}\n", options.copies)),
        Some(_) => text.push_str(&format!(
            "failed: {failed}/{completed} (batch stopped: {completed} of {} copies completed)\n",
            options.copies
        )),
    }
    for path in &batch.failed {
        text.push_str(&format!("{}\n", path.display()));
    }
    for path in &batch.aborted {
        text.push_str(&format!("aborted: {}\n", path.display()));
    }
    if let Some(error) = &batch.error {
        let first = error.to_string();
        text.push_str(&format!(
            "error: {}\n",
            first.lines().next().unwrap_or_default()
        ));
    }
    let summary = out.join("summary.txt");
    fs::write(&summary, &text).with_context(|| {
        format!(
            "writing {} after {completed} of {} copies completed, {failed} failed; their output stays under {}",
            summary.display(),
            options.copies,
            out.display()
        )
    })?;
    let stopped = batch.error.map(|error| {
        error.context(format!(
            "stress batch stopped after {completed} of {} copies completed, {failed} failed; output under {}",
            options.copies,
            out.display()
        ))
    });
    Ok(Report {
        text,
        failed,
        summary,
        stopped,
    })
}

/// What a batch left behind: the count of copies that ran to their end, the
/// kept output of those that failed, and the output of copies a fatal error
/// stopped part-way, each in copy order. An unrun copy is in none of them.
#[derive(Default)]
struct Batch {
    completed: usize,
    failed: Vec<PathBuf>,
    aborted: Vec<PathBuf>,
    error: Option<anyhow::Error>,
}

/// Run the copies until all have run or one hits a fatal error (the xtask
/// budget, or I/O). After that no worker claims another copy, and the copies
/// already running end on their own or by the same budget.
fn run_copies(options: &Options, test: &LocatedTest, sandbox: &HostSandbox, out: &Path) -> Batch {
    let env = sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let width = options.copies.to_string().len().max(3);
    let next = AtomicUsize::new(1);
    let stop = AtomicBool::new(false);
    let worker = || -> Batch {
        let mut batch = Batch::default();
        while !stop.load(Ordering::Relaxed) {
            let copy = next.fetch_add(1, Ordering::Relaxed);
            if copy > options.copies {
                break;
            }
            let mut command = Command::new(&test.binary);
            command
                .args(["--exact", &test.name, "--test-threads=1"])
                .current_dir(&test.cwd)
                .envs(env.iter().map(|(key, value)| (*key, value)));
            for key in &removed {
                command.env_remove(key);
            }
            command
                .env("RUST_BACKTRACE", "1")
                .envs(options.env.iter().map(|(key, value)| (key, value)));
            let output = out.join(format!("run-{copy:0width$}.out"));
            match run_copy(&mut command, &output) {
                Ok(passed) => {
                    batch.completed += 1;
                    if !passed {
                        batch.failed.push(output);
                    }
                }
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    if output.exists() {
                        batch.aborted.push(output);
                    }
                    batch.error = Some(error);
                }
            }
        }
        batch
    };
    let mut batch = thread::scope(|scope| {
        let workers: Vec<_> = (0..options.jobs.min(options.copies))
            .map(|_| scope.spawn(worker))
            .collect();
        let mut batch = Batch::default();
        for worker in workers {
            let part = worker.join().unwrap_or_else(|_| Batch {
                error: Some(anyhow::anyhow!("stress worker panicked")),
                ..Batch::default()
            });
            batch.completed += part.completed;
            batch.failed.extend(part.failed);
            batch.aborted.extend(part.aborted);
            batch.error = batch.error.or(part.error);
        }
        batch
    });
    batch.failed.sort();
    batch.aborted.sort();
    batch
}

/// Run one copy with stdout and stderr together in `output`, which survives
/// only when the copy fails. A test that leaves a child holding its stdio
/// would block a pipe reader past the copy's exit; a file does not.
fn run_copy(command: &mut Command, output: &Path) -> Result<bool> {
    let file = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    let spawned = command
        .stdin(Stdio::null())
        .stdout(file.try_clone().context("sharing the copy's output file")?)
        .stderr(file)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            // A copy that never started has no output to keep.
            let _ = fs::remove_file(output);
            return Err(error)
                .with_context(|| format!("running {}", command.get_program().display()));
        }
    };
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for a stress copy")? {
            break status;
        }
        if let Some(overrun) = deadline::overrun() {
            let _ = child.kill();
            let _ = child.wait();
            append(output, &format!("stress: copy stopped: {overrun}"))?;
            bail!(
                "{overrun}: stopped the stress batch\n{}",
                overrun.next_step()
            );
        }
        thread::sleep(POLL_INTERVAL);
    };
    if status.success() {
        fs::remove_file(output).with_context(|| format!("removing {}", output.display()))?;
        return Ok(true);
    }
    // A copy killed by a signal prints nothing about its own end.
    append(output, &format!("stress: copy ended with {status}"))?;
    Ok(false)
}

fn append(output: &Path, trailer: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(output)
        .with_context(|| format!("reopening {}", output.display()))?;
    writeln!(file, "\n{trailer}").with_context(|| format!("writing {}", output.display()))
}

fn commit(root: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(head) = git(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".to_owned();
    };
    match git(&["status", "--porcelain"]) {
        Some(changes) if changes.is_empty() => head,
        _ => format!("{head} (dirty)"),
    }
}

/// The 1-minute load average: `/proc/loadavg` leads with it, and where that
/// file does not exist `sysctl` prints it first inside braces.
fn load_average() -> String {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .or_else(|| {
            let output = Command::new("sysctl").args(["-n", "vm.loadavg"]).output();
            output
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        })
        .and_then(|raw| {
            raw.split_whitespace()
                .find(|field| field.parse::<f64>().is_ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Busy processes, one CPU each, that die with this guard or with the process
/// that holds it: each hog's stdin is a pipe only the owner has open.
struct Hogs(Vec<(Child, ChildStdin)>);

impl Hogs {
    fn start(count: usize, hog: &dyn Fn() -> Command) -> Result<Self> {
        let mut hogs = Self(Vec::new());
        for _ in 0..count {
            let mut child = hog()
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("starting a CPU hog")?;
            let keepalive = child
                .stdin
                .take()
                .context("CPU hog has no keepalive pipe")?;
            hogs.0.push((child, keepalive));
        }
        Ok(hogs)
    }

    #[cfg(test)]
    fn pids(&self) -> Vec<u32> {
        self.0.iter().map(|(child, _)| child.id()).collect()
    }
}

impl Drop for Hogs {
    fn drop(&mut self) {
        for (child, _) in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spin until the owner's end of stdin closes. A drop guard in the owner
/// cannot cover a SIGKILL; the closed pipe does, since the kernel closes it.
fn hog() -> ! {
    thread::spawn(|| {
        let _ = std::io::stdin().lock().read_to_end(&mut Vec::new());
        std::process::exit(0);
    });
    loop {
        std::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use tempfile::TempDir;

    use super::*;

    const ROLE_ENV: &str = "XTASK_STRESS_TEST_ROLE";
    const HOGS_GONE_WITHIN: Duration = Duration::from_secs(30);

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// This test binary re-entered at one of the role tests below, which is how
    /// a test gets a process that runs the real `hog` or owns real hogs.
    fn role_command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("stress::tests::{role}_role"),
                "--nocapture",
            ])
            .env(ROLE_ENV, role);
        command
    }

    fn in_role(role: &str) -> bool {
        std::env::var(ROLE_ENV).is_ok_and(|current| current == role)
    }

    #[test]
    fn hog_role() {
        if in_role("hog") {
            hog();
        }
    }

    #[test]
    fn owner_role() {
        if !in_role("owner") {
            return;
        }
        let hogs = Hogs::start(2, &|| role_command("hog")).unwrap();
        let pids: Vec<String> = hogs.pids().iter().map(u32::to_string).collect();
        let mut stdout = std::io::stdout();
        writeln!(stdout, "hogs: {}", pids.join(" ")).unwrap();
        stdout.flush().unwrap();
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }

    /// A reaped process and a zombie both count as gone: an orphaned hog is
    /// reaped by whichever process adopted it, on that process's schedule.
    fn alive(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }
    }

    fn assert_gone(pids: &[u32]) {
        let deadline = Instant::now() + HOGS_GONE_WITHIN;
        while pids.iter().any(|pid| alive(*pid)) {
            assert!(Instant::now() < deadline, "a hog outlived its owner");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn defaults_follow_the_cpu_count_and_each_flag_overrides_one() {
        assert_eq!(
            parse(&args(&["doctor::mixed"]), 8).unwrap(),
            Options {
                test: "doctor::mixed".to_owned(),
                copies: 400,
                jobs: 16,
                hogs: 8,
                env: Vec::new(),
                out: None,
            }
        );
        assert_eq!(
            parse(
                &args(&[
                    "--copies",
                    "1600",
                    "--jobs=1",
                    "doctor::mixed",
                    "--hogs",
                    "0",
                    "--env",
                    "RIMZ_HOME=/fill",
                    "--env=A=b=c",
                    "--out",
                    "/tmp/kept",
                ]),
                8
            )
            .unwrap(),
            Options {
                test: "doctor::mixed".to_owned(),
                copies: 1600,
                jobs: 1,
                hogs: 0,
                env: vec![
                    ("RIMZ_HOME".to_owned(), "/fill".to_owned()),
                    ("A".to_owned(), "b=c".to_owned()),
                ],
                out: Some(PathBuf::from("/tmp/kept")),
            }
        );
    }

    #[test]
    fn a_bad_argument_refuses_with_the_fix() {
        for (argv, expected) in [
            (vec![], "requires a test name"),
            (vec!["a", "b"], "takes one test name; got `a` and `b`"),
            (
                vec!["a", "--copies", "0"],
                "`--copies` expects a whole number of at least 1; got `0`",
            ),
            (
                vec!["a", "--jobs=many"],
                "`--jobs` expects a whole number of at least 1; got `many`",
            ),
            (
                vec!["a", "--hogs", "-1"],
                "`--hogs` expects a whole number of at least 0; got `-1`",
            ),
            (
                vec!["a", "--env", "RIMZ_HOME"],
                "`--env` expects KEY=VALUE; got `RIMZ_HOME`",
            ),
            (vec!["a", "--out"], "`--out` requires a value"),
            (vec!["a", "--serial"], "does not accept `--serial`"),
        ] {
            let err = parse(&args(&argv), 8).unwrap_err().to_string();
            assert!(err.contains(expected), "{argv:?}: {err}");
            assert!(err.contains("cargo xtask stress --help"), "{argv:?}: {err}");
        }
    }

    /// Stands in for the test binary: of all its copies, exactly one exits 3
    /// and exactly one kills itself, whichever claim each directory first.
    const STUB: &str = r#"#!/bin/sh
echo "args: $*"
echo "home: $RIMZ_HOME backtrace: $RUST_BACKTRACE cwd: $(pwd -P)" >&2
mkdir "$STUB_STATE/exit" 2>/dev/null && exit 3
mkdir "$STUB_STATE/signal" 2>/dev/null && kill -KILL $$
exit 0
"#;

    /// A batch of `copies` copies of `script`, `jobs` at a time, with its
    /// output under `<dir>/out`; `STUB_STATE` names `dir`.
    struct StubBatch {
        _home: TempDir,
        dir: PathBuf,
        out: PathBuf,
        binary: PathBuf,
        options: Options,
        test: LocatedTest,
        sandbox: HostSandbox,
    }

    impl StubBatch {
        fn new(script: &str, copies: usize, jobs: usize) -> Self {
            let home = TempDir::new().unwrap();
            let dir = home.path().canonicalize().unwrap();
            let binary = dir.join("stub");
            std::fs::write(&binary, script).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
            let out = dir.join("out");
            let options = Options {
                test: "stub::test".to_owned(),
                copies,
                jobs,
                hogs: 0,
                env: vec![
                    ("STUB_STATE".to_owned(), dir.display().to_string()),
                    ("RIMZ_HOME".to_owned(), "/stub-home".to_owned()),
                ],
                out: Some(out.clone()),
            };
            let test = LocatedTest {
                name: "stub::test".to_owned(),
                binary: binary.clone(),
                cwd: dir.clone(),
            };
            let sandbox = HostSandbox::for_tests(&dir).unwrap();
            Self {
                _home: home,
                dir,
                out,
                binary,
                options,
                test,
                sandbox,
            }
        }

        fn run(&self) -> Result<Report> {
            let out = output_dir(self.options.out.as_deref()).unwrap();
            stress(
                Path::new("."),
                &self.options,
                &self.test,
                &self.sandbox,
                4,
                &out,
                &|| Command::new("false"),
            )
        }

        fn listing(&self) -> Vec<PathBuf> {
            let mut listing: Vec<PathBuf> = std::fs::read_dir(&self.out)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            listing.sort();
            listing
        }
    }

    #[test]
    fn a_batch_counts_exits_and_signal_deaths_and_keeps_only_their_output() {
        let batch = StubBatch::new(STUB, 6, 3);
        let (dir, out, binary) = (&batch.dir, &batch.out, &batch.binary);

        let report = batch.run().unwrap();

        assert_eq!(report.failed, 2);
        assert!(report.stopped.is_none());
        let mut kept = batch.listing();
        let summary = kept.pop().unwrap();
        assert_eq!(summary, out.join("summary.txt"));
        assert_eq!(std::fs::read_to_string(summary).unwrap(), report.text);
        assert_eq!(kept.len(), 2, "{kept:?}");

        let lines: Vec<&str> = report.text.lines().collect();
        assert_eq!(
            &lines[..2],
            [
                "test: stub::test".to_owned(),
                format!("binary: {}", binary.display())
            ]
        );
        assert!(lines[2].starts_with("commit: "), "{}", report.text);
        assert_eq!(&lines[3..7], ["copies: 6", "jobs: 3", "hogs: 0", "cpus: 4"]);
        assert!(lines[7].starts_with("load before: "), "{}", report.text);
        assert!(lines[8].starts_with("load after: "), "{}", report.text);
        assert_eq!(lines[9], "failed: 2/6");
        let listed: Vec<PathBuf> = lines[10..].iter().map(PathBuf::from).collect();
        assert_eq!(listed, kept);

        let outputs: Vec<String> = kept
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        for output in &outputs {
            assert!(
                output.contains("args: --exact stub::test --test-threads=1"),
                "{output}"
            );
            assert!(
                output.contains(&format!(
                    "home: /stub-home backtrace: 1 cwd: {}",
                    dir.display()
                )),
                "{output}"
            );
        }
        assert!(
            outputs
                .iter()
                .any(|output| output.contains("exit status: 3")),
            "{outputs:?}"
        );
        assert!(
            outputs.iter().any(|output| output.contains("signal: 9")),
            "{outputs:?}"
        );
    }

    /// Run serially: copy 1 exits 3, copy 2 passes, and copy 3 marks itself
    /// started, which proves the first two completed, then hangs until killed.
    const HANGING_STUB: &str = r#"#!/bin/sh
mkdir "$STUB_STATE/exit" 2>/dev/null && exit 3
mkdir "$STUB_STATE/pass" 2>/dev/null && exit 0
echo "hanging copy started"
mkdir "$STUB_STATE/hanging"
exec sleep 120
"#;

    /// Bounds each wait in a test that drives a batch from outside; reaching
    /// it means the batch is wedged, not slow.
    const BATCH_LIVENESS: Duration = Duration::from_secs(60);

    /// Wait for `done`, failing the test with `what` past the liveness bound.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + BATCH_LIVENESS;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "{what} within {BATCH_LIVENESS:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    // The budget arms once per process; nextest runs each test in its own, so
    // this test owns the armed budget for the whole process. It stays unarmed
    // until the hanging copy starts, then arms already spent.
    #[test]
    fn a_spent_budget_stops_the_batch_and_keeps_its_partial_count() {
        let batch = StubBatch::new(HANGING_STUB, 6, 1);
        let hanging = batch.dir.join("hanging");
        let running = thread::spawn(move || {
            let report = batch.run();
            (batch, report)
        });
        wait_for("the third copy starts", || hanging.exists());

        deadline::arm_with("stress", Some(Duration::from_millis(1)));

        wait_for("the batch stops", || running.is_finished());
        let (batch, report) = running.join().unwrap();
        assert!(
            report.is_ok(),
            "a stopped batch reports what it ran: {:#}",
            report.err().unwrap()
        );
        let report = report.unwrap();
        let stopped = format!("{:#}", report.stopped.expect("the batch was stopped"));
        assert!(
            stopped.contains("stopped after 2 of 6 copies completed, 1 failed"),
            "{stopped}"
        );
        assert!(stopped.contains("exceeded its 1ms budget"), "{stopped}");
        assert!(stopped.contains("RIMZ_XTASK_TIMEOUT="), "{stopped}");
        assert_eq!(report.failed, 1);

        let mut listing = batch.listing();
        let summary = listing.pop().unwrap();
        assert_eq!(summary, batch.out.join("summary.txt"));
        assert_eq!(std::fs::read_to_string(&summary).unwrap(), report.text);
        assert_eq!(
            listing,
            [batch.out.join("run-001.out"), batch.out.join("run-003.out")],
            "a passed copy and the unrun ones leave nothing"
        );

        let lines: Vec<&str> = report.text.lines().collect();
        assert!(lines[8].starts_with("load after: "), "{}", report.text);
        assert_eq!(
            &lines[9..12],
            [
                "failed: 1/2 (batch stopped: 2 of 6 copies completed)".to_owned(),
                listing[0].display().to_string(),
                format!("aborted: {}", listing[1].display()),
            ]
        );
        assert!(
            lines[12].starts_with("error: xtask `stress` exceeded its 1ms budget after "),
            "{}",
            report.text
        );
        assert_eq!(lines.len(), 13, "{}", report.text);
        let failed = std::fs::read_to_string(&listing[0]).unwrap();
        assert!(failed.contains("exit status: 3"), "{failed}");
        let aborted = std::fs::read_to_string(&listing[1]).unwrap();
        assert!(aborted.contains("hanging copy started"), "{aborted}");
        assert!(aborted.contains("stress: copy stopped"), "{aborted}");
    }

    /// Run serially: copy 1 exits 3, copy 2 passes and deletes the binary, so
    /// copy 3 cannot start.
    const VANISHING_STUB: &str = r#"#!/bin/sh
mkdir "$STUB_STATE/exit" 2>/dev/null && exit 3
rm "$0"
exit 0
"#;

    #[test]
    fn a_copy_that_cannot_start_stops_the_batch_and_keeps_its_partial_count() {
        let batch = StubBatch::new(VANISHING_STUB, 6, 1);

        let report = batch.run().unwrap();

        let stopped = format!("{:#}", report.stopped.expect("the batch was stopped"));
        assert!(
            stopped.contains("stopped after 2 of 6 copies completed, 1 failed"),
            "{stopped}"
        );
        assert!(
            stopped.contains(&format!("running {}", batch.binary.display())),
            "{stopped}"
        );
        assert_eq!(
            batch.listing(),
            [batch.out.join("run-001.out"), batch.out.join("summary.txt")],
            "a copy that never started leaves nothing"
        );
        let lines: Vec<&str> = report.text.lines().collect();
        assert_eq!(
            &lines[9..],
            [
                "failed: 1/2 (batch stopped: 2 of 6 copies completed)".to_owned(),
                batch.out.join("run-001.out").display().to_string(),
                format!("error: running {}", batch.binary.display()),
            ]
        );
    }

    #[test]
    fn an_output_directory_holding_kept_copies_is_refused_untouched() {
        let dir = TempDir::new().unwrap();
        let kept = dir.path().join("run-007.out");
        std::fs::write(&kept, "an earlier batch").unwrap();

        let result = output_dir(Some(dir.path()));

        assert!(result.is_err(), "a used directory is taken: {result:?}");
        let err = result.unwrap_err().to_string();

        assert!(err.contains("already holds run-*.out"), "{err}");
        assert!(err.contains("NEXT: name an empty directory"), "{err}");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "an earlier batch");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let fresh = dir.path().join("fresh");
        assert_eq!(output_dir(Some(&fresh)).unwrap(), fresh);
    }

    #[test]
    fn no_hog_is_alive_once_its_guard_drops() {
        let hogs = Hogs::start(2, &|| role_command("hog")).unwrap();
        let pids = hogs.pids();
        assert_eq!(pids.len(), 2);
        assert!(pids.iter().all(|pid| alive(*pid)));

        drop(hogs);

        assert!(pids.iter().all(|pid| !alive(*pid)), "{pids:?}");
    }

    #[test]
    fn no_hog_is_alive_after_its_owner_is_killed_outright() {
        let mut owner = role_command("owner")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pids: Vec<u32> = BufReader::new(owner.stdout.take().unwrap())
            .lines()
            .map(Result::unwrap)
            .find_map(|line| {
                line.strip_prefix("hogs: ").map(|pids| {
                    pids.split_whitespace()
                        .map(|pid| pid.parse().unwrap())
                        .collect()
                })
            })
            .unwrap();
        assert_eq!(pids.len(), 2);
        assert!(pids.iter().all(|pid| alive(*pid)));

        owner.kill().unwrap();
        owner.wait().unwrap();

        assert_gone(&pids);
    }
}
