//! Machine-tier agent plugin scaffold and manifest validation.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::cli::render;

#[derive(Debug, Args)]
pub(super) struct RegisterArgs {
    /// Plugin kind to scaffold under agents.d.
    #[arg(value_name = "KIND", required_unless_present = "check")]
    kind: Option<String>,
    /// Validate every configured agent plugin without creating files.
    #[arg(long, conflicts_with = "kind")]
    check: bool,
}

pub(super) fn run_register(args: RegisterArgs) -> Result<()> {
    if args.check {
        return check_plugins();
    }
    let Some(kind) = args.kind.as_deref() else {
        bail!("plugin kind is required unless --check is set");
    };
    if !rimz::agents::plugin::valid_kind(kind) {
        bail!("plugin kind must match [a-z0-9-]+ and start and end with a letter or digit");
    }
    if rimz::agents::ADAPTERS
        .iter()
        .any(|adapter| adapter.descriptor().kind == kind)
    {
        bail!("agent kind `{kind}` is built in and cannot be registered as a plugin");
    }
    let target = rimz::agents::plugin::plugins_root().join(kind);
    if target.exists() {
        bail!(
            "agent plugin directory already exists at {}",
            target.display()
        );
    }
    fs::create_dir_all(&target)
        .with_context(|| format!("creating plugin directory {}", target.display()))?;
    let result = write_scaffold(&target, kind);
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&target);
        return Err(err);
    }
    writeln!(render::out(), "registered `{kind}` at {}", target.display())?;
    writeln!(
        render::out(),
        "edit agent.toml and README.md, then run `rimz agents register --check`"
    )?;
    Ok(())
}

fn check_plugins() -> Result<()> {
    let loaded = rimz::agents::plugin::load_from_root(&rimz::agents::plugin::plugins_root());
    if !loaded.errors.is_empty() {
        let details = loaded
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        bail!("agent plugin validation failed:\n{details}");
    }
    writeln!(
        render::out(),
        "{} agent plugin(s) valid",
        loaded.adapters.len()
    )?;
    Ok(())
}

fn write_scaffold(target: &Path, kind: &str) -> Result<()> {
    let manifest = format!(
        r#"# Canonical protocol events emitted by shim.sh. Add events only after the shim emits them.
protocol = 1
kind = "{kind}"
display-name = "{kind}"
process-names = ["{kind}"]
events = ["session_start"]
setup-doc = "README.md"

[brand]
color = 141
color-rgb = [175, 135, 255]

[capabilities]
native-ask-ui = false
subagents = false
context-usage = false

[tools]
mutating = []
editing = []

[launch]
bin = "{kind}"
args = []
# model-flag = "--model"
# effort-flag = "--effort"
# resume = ["{kind}", "--resume", "{{session_id}}"]
# compact-command = "/compact"

[launch.permission-args]
ask = []
auto = []
yolo = []
plan = []

# [transcripts]
# globs = ["~/.{kind}/sessions/*.jsonl"]
# thread-key = "per-file"

[probes]
# spend = ["./probes/spend"]
# account = ["./probes/account"]
# version = ["{kind}", "--version"]
"#
    );
    let readme = format!(
        "# {kind} Rimz plugin\n\nTranslate the agent's native events to the [canonical JSON protocol](https://github.com/rimio-ai/rimz/blob/main/docs/reference/agent-plugins.md), then pipe each envelope through `shim.sh <event>`. Hook installation stays self-managed and belongs in this file.\n"
    );
    let shim = format!(
        "#!/bin/sh\nset -eu\nevent=${{1:?canonical event name}}\nexec rimz hooks feed --source {kind} --event \"$event\"\n"
    );
    let spend =
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{\"entries\":[],\"cursor\":null}'\n";
    let account = "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{\"logged_out\":true}'\n";

    fs::create_dir(target.join("probes"))
        .with_context(|| format!("creating {}", target.join("probes").display()))?;
    write_file(&target.join("agent.toml"), manifest.as_bytes())?;
    write_file(&target.join("README.md"), readme.as_bytes())?;
    write_file(&target.join("shim.sh"), shim.as_bytes())?;
    write_file(&target.join("probes/spend"), spend.as_bytes())?;
    write_file(&target.join("probes/account"), account.as_bytes())?;
    set_executable(&target.join("shim.sh"))?;
    set_executable(&target.join("probes/spend"))?;
    set_executable(&target.join("probes/account"))?;
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    rimz::store::atomic::write_bytes_atomically(path, bytes)
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}
