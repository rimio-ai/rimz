//! Launch shell enrichment.

use std::path::PathBuf;

pub(super) struct LaunchEnv {
    pub shell: Option<PathBuf>,
}

pub(super) fn read() -> LaunchEnv {
    LaunchEnv {
        shell: crate::proc::user_shell(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_env_reads_user_shell() {
        assert_eq!(read().shell, crate::proc::user_shell());
    }
}
