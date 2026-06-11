use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::Env;

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
        "while [ \"$#\" -gt 0 ]; do\n\
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
