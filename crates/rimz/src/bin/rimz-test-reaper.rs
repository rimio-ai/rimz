#![allow(clippy::print_stderr, clippy::print_stdout)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use rimz::testkit::sandbox::{SandboxSpec, TestSandbox};
use serde::Serialize;

#[derive(Serialize)]
struct FakeOwnerReport<'a> {
    spec: &'a SandboxSpec,
    child_pid: u32,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rimz-test-reaper: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("missing sandbox spec")?;
    if first == "--fake-owner" {
        let encoded = args.next().ok_or("--fake-owner requires a sandbox spec")?;
        if args.next().is_some() {
            return Err("--fake-owner expects exactly one sandbox spec".to_owned());
        }
        let spec = parse_spec(&encoded)?;
        return run_fake_owner(spec);
    }
    if args.next().is_some() {
        return Err("expected exactly one sandbox spec".to_owned());
    }
    let spec = parse_spec(&first)?;

    let mut buffer = [0_u8; 64];
    while std::io::stdin()
        .read(&mut buffer)
        .map_err(|err| format!("reading keepalive: {err}"))?
        != 0
    {}
    rimz::testkit::sandbox::cleanup(&spec);
    Ok(())
}

fn parse_spec(encoded: &str) -> Result<SandboxSpec, String> {
    let spec =
        serde_json::from_str(encoded).map_err(|err| format!("invalid sandbox spec JSON: {err}"))?;
    rimz::testkit::sandbox::validate(&spec).map_err(|err| err.to_string())?;
    Ok(spec)
}

fn run_fake_owner(spec: SandboxSpec) -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|err| format!("resolving reaper executable: {err}"))?;
    let sandbox = TestSandbox::arm(spec, Path::new(&executable)).map_err(|err| err.to_string())?;
    let mut command = Command::new("sleep");
    command
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    sandbox.pin_identity(&mut command);
    let child = command
        .spawn()
        .map_err(|err| format!("spawning marker child: {err}"))?;
    let report = FakeOwnerReport {
        spec: sandbox.spec(),
        child_pid: child.id(),
    };
    serde_json::to_writer(std::io::stdout().lock(), &report)
        .map_err(|err| format!("writing fake-owner report: {err}"))?;
    std::io::stdout()
        .lock()
        .write_all(b"\n")
        .map_err(|err| format!("terminating fake-owner report: {err}"))?;
    std::io::stdout()
        .lock()
        .flush()
        .map_err(|err| format!("flushing fake-owner report: {err}"))?;

    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
