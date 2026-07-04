use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::Env;

pub fn cargo_bin(name: &str, cargo_env_path: &str) -> PathBuf {
    archive_extracted_bin(name).unwrap_or_else(|| PathBuf::from(cargo_env_path))
}

fn archive_extracted_bin(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let candidate = debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

#[cfg(unix)]
pub fn write_env_dump_shim(env: &Env, agent: &str) -> PathBuf {
    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let shim = dir.join(agent);
    std::fs::write(
        &shim,
        "#!/bin/sh\n\
         {\n\
           printf 'ARGV=%s\\n' \"$*\"\n\
           printf 'ARGC=%s\\n' \"$#\"\n\
           i=1\n\
           for arg in \"$@\"; do\n\
             printf 'ARGV_%s=%s\\n' \"$i\" \"$arg\"\n\
             i=$((i + 1))\n\
           done\n\
           env\n\
         } > \"$RIMZ_TEST_AGENT_ENV_DUMP\"\n",
    )
    .expect("write agent shim");
    chmod_executable(&shim);
    dir
}

#[cfg(unix)]
pub fn write_failing_agent_shim(env: &Env, agent: &str, code: u8) -> PathBuf {
    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let shim = dir.join(agent);
    std::fs::write(&shim, format!("#!/bin/sh\nexit {code}\n")).expect("write agent shim");
    chmod_executable(&shim);
    dir
}

#[cfg(unix)]
pub fn write_hook_firing_agent(env: &Env, agent: &str) -> PathBuf {
    assert!(matches!(agent, "codex" | "claude"));
    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let shim = dir.join(agent);
    let version = match agent {
        "claude" => "2.1.158 (Claude Code)",
        _ => "0.139.0",
    };
    let body = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = \"--version\" ]; then\n\
           printf '%s\\n' {version}\n\
           exit 0\n\
         fi\n\
         rimz={rimz}\n\
         agent={agent}\n\
         session=${{RIMZ_TEST_AGENT_SESSION:-sess-hook-agent}}\n\
         worktree=${{PWD:-.}}\n\
         branch=${{RIMZ_TEST_AGENT_BRANCH:-main}}\n\
         exit_code=${{RIMZ_TEST_AGENT_EXIT:-0}}\n\
         sleep_ms=${{RIMZ_TEST_AGENT_SLEEP_MS:-0}}\n\
         feed() {{\n\
           printf '%s\\n' \"$1\" | RIMZ_AGENT_PID=${{RIMZ_AGENT_PID:-$$}} \"$rimz\" hooks feed --source \"$agent\" >/dev/null\n\
         }}\n\
         feed '{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"'\"$session\"'\",\"model\":\"GPT-5.5\",\"reasoning_effort\":\"high\",\"worktree_path\":\"'\"$worktree\"'\",\"worktree_branch\":\"'\"$branch\"'\"}}'\n\
         feed '{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"'\"$session\"'\",\"prompt\":\"summarize the diff\"}}'\n\
         feed '{{\"hook_event_name\":\"PostToolUse\",\"session_id\":\"'\"$session\"'\",\"tool_name\":\"apply_patch\"}}'\n\
         if [ \"$sleep_ms\" != 0 ]; then\n\
           sleep_sec=$((sleep_ms / 1000))\n\
           sleep_rem=$((sleep_ms % 1000))\n\
           if [ \"$sleep_sec\" -gt 0 ]; then sleep \"$sleep_sec\"; fi\n\
           if [ \"$sleep_rem\" -gt 0 ]; then sleep 1; fi\n\
         fi\n\
         if [ \"$exit_code\" != 0 ]; then\n\
           exit \"$exit_code\"\n\
         fi\n\
         feed '{{\"hook_event_name\":\"Stop\",\"session_id\":\"'\"$session\"'\",\"last_assistant_message\":\"stub done\"}}'\n\
         exit 0\n",
        version = sh_quote(version),
        rimz = sh_quote(&env.rimz_bin().display().to_string()),
        agent = sh_quote(agent),
    );
    std::fs::write(&shim, body).expect("write hook-firing agent shim");
    chmod_executable(&shim);
    dir
}

#[cfg(unix)]
pub fn write_fake_login_shell(env: &Env, name: &str, exports: &[(&str, &str)]) -> PathBuf {
    let dir = env.home_root.join("shell-bin");
    std::fs::create_dir_all(&dir).expect("mkdir shell bin");
    let shell = dir.join(name);
    let mut body = "#!/bin/sh\n".to_owned();
    for (key, value) in exports {
        assert!(
            key.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        );
        assert!(
            value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        );
        body.push_str(&format!("export {key}='{value}'\n"));
    }
    body.push_str(
        "if [ \"$#\" -eq 0 ]; then\n\
           if [ -n \"${RIMZ_TEST_IDLE_SHELL_MARKER:-}\" ]; then\n\
             printf 'idle shell\\n' > \"$RIMZ_TEST_IDLE_SHELL_MARKER\"\n\
             exit 0\n\
           fi\n\
           exit 127\n\
         fi\n\
         while [ \"$#\" -gt 0 ]; do\n\
           case \"$1\" in\n\
             -c)\n\
               shift\n\
               script=$1\n\
               shift\n\
               exec /bin/sh -c \"$script\" \"$@\"\n\
               ;;\n\
             *) shift ;;\n\
           esac\n\
         done\n\
         exit 127\n",
    );
    std::fs::write(&shell, body).expect("write shell shim");
    chmod_executable(&shell);
    shell
}

#[cfg(unix)]
pub fn write_fake_bash_shell(env: &Env) -> PathBuf {
    let dir = env.home_root.join("shell-bin");
    std::fs::create_dir_all(&dir).expect("mkdir shell bin");
    let shell = dir.join("bash");
    std::fs::write(
        &shell,
        "#!/bin/sh\n\
         while [ \"$#\" -gt 0 ]; do\n\
           case \"$1\" in\n\
             -i)\n\
               if [ -f \"$HOME/.bashrc\" ]; then\n\
                 . \"$HOME/.bashrc\"\n\
               fi\n\
               shift\n\
               ;;\n\
             -c)\n\
               shift\n\
               script=$1\n\
               shift\n\
               exec /bin/sh -c \"$script\" \"$@\"\n\
               ;;\n\
             *) shift ;;\n\
           esac\n\
         done\n\
         exit 127\n",
    )
    .expect("write bash shell shim");
    chmod_executable(&shell);
    shell
}

#[cfg(unix)]
pub fn path_with_front(dir: &Path) -> OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths).expect("join PATH")
}

#[cfg(unix)]
fn chmod_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)
        .expect("shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod shim");
}

#[cfg(unix)]
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
