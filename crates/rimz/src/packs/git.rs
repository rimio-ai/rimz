use super::*;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const LOCAL_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

pub(super) struct Client {
    pub(super) env: BTreeMap<String, String>,
    pub(super) timeout: Duration,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Revision {
    pub(super) reference: String,
    pub(super) commit: String,
}

impl Client {
    pub(super) fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        source_url: &str,
        network: bool,
    ) -> Result<String> {
        let output = self.output(cwd, args, source_url, network)?;
        if !output.status.success() {
            return Err(failed(source_url, &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = crate::proc::git_command(cwd);
        command
            .envs(&self.env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "submodule.recurse=false",
            ])
            .stdin(Stdio::null());
        for key in LOCAL_ENV {
            command.env_remove(key);
        }
        command
    }

    fn output(&self, cwd: &Path, args: &[&str], source_url: &str, network: bool) -> Result<Output> {
        let mut command = self.command(cwd);
        let custom_ssh = ["GIT_SSH_COMMAND", "GIT_SSH"]
            .iter()
            .any(|key| self.env.contains_key(*key) || std::env::var_os(key).is_some());
        if network && !custom_ssh {
            let config = self.output(
                cwd,
                &["config", "--get", "core.sshCommand"],
                source_url,
                false,
            )?;
            if config.status.code() == Some(1) {
                command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
            } else if !config.status.success() {
                return Err(failed(source_url, &config));
            }
        }
        let output = crate::proc::run_bounded_output(command.args(args), self.timeout)
            .map_err(|error| git_error(source_url, error.to_string()))?;
        if output.timed_out {
            return Err(git_error(
                source_url,
                format!("timed out after {:?}", self.timeout),
            ));
        }
        Ok(Output {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    pub(super) fn resolve(&self, cwd: &Path, source: &GitSource) -> Result<Revision> {
        let requested = source
            .reference
            .clone()
            .or_else(|| source.official().map(|_| format!("v{RUNNING_VERSION}")));
        let tag = requested
            .as_ref()
            .map(|reference| format!("refs/tags/{reference}"));
        let peeled = tag.as_ref().map(|tag| format!("{tag}^{{}}"));
        let branch = requested
            .as_ref()
            .map(|reference| format!("refs/heads/{reference}"));
        let mut args = vec!["ls-remote", "--symref", "--", source.url.as_str(), "HEAD"];
        for pattern in [&tag, &peeled, &branch].into_iter().flatten() {
            args.push(pattern);
        }
        let output = self.run(cwd, &args, &source.url, true)?;
        let mut refs = BTreeMap::new();
        let mut default_branch = None;
        for line in output.lines() {
            let Some((value, name)) = line.split_once('\t') else {
                continue;
            };
            if name == "HEAD"
                && let Some(branch) = value.strip_prefix("ref: refs/heads/")
            {
                default_branch = Some(branch.to_owned());
            } else if is_commit(value) {
                refs.insert(name.to_owned(), value.to_owned());
            }
        }
        let reference = requested
            .clone()
            .or_else(|| default_branch.clone())
            .ok_or_else(|| {
                git_error(
                    &source.url,
                    "remote does not advertise a default branch; pin a tag, branch or full commit",
                )
            })?;
        let commit = if requested.is_some() {
            refs.get(&format!("refs/tags/{reference}^{{}}"))
                .or_else(|| refs.get(&format!("refs/tags/{reference}")))
                .or_else(|| refs.get(&format!("refs/heads/{reference}")))
                .or_else(|| (reference == "HEAD").then(|| refs.get("HEAD")).flatten())
                .cloned()
                .or_else(|| is_commit(&reference).then(|| reference.to_ascii_lowercase()))
        } else {
            refs.get("HEAD").cloned()
        };
        let commit = commit.ok_or_else(|| {
            let message = if source.reference.is_none() && source.official().is_some() {
                let spelling = source.spelling();
                let branch = default_branch.as_deref().unwrap_or("HEAD");
                let argument = format!("{spelling}@{branch}");
                // Git refs and validated source spellings cannot contain NUL.
                let command = shlex::try_join(["rimz", "packs", "install", &argument])
                    .expect("Git refs and source spellings carry no NUL byte");
                format!(
                    "missing tag {reference}; use `{command}` \
                     to follow the default branch"
                )
            } else {
                format!("ref {reference:?} was not found")
            };
            git_error(&source.url, message)
        })?;
        Ok(Revision { reference, commit })
    }

    pub(super) fn checkout(
        &self,
        path: &Path,
        source: &GitSource,
        revision: &Revision,
    ) -> Result<()> {
        fs::create_dir_all(path).map_err(|error| io_err(path, error))?;
        self.run(
            path,
            &["init", "--quiet", "--template="],
            &source.url,
            false,
        )?;
        self.run(
            path,
            &[
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &source.url,
                &revision.commit,
            ],
            &source.url,
            true,
        )?;
        let commit = self.run(
            path,
            &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
            &source.url,
            false,
        )?;
        if commit != revision.commit {
            return Err(git_error(
                &source.url,
                format!("expected commit {}, fetched {commit}", revision.commit),
            ));
        }
        self.run(
            path,
            &["checkout", "--quiet", "--force", "--detach", &commit, "--"],
            &source.url,
            false,
        )?;
        Ok(())
    }
}

fn git_error(source_url: &str, message: impl Into<String>) -> PackErr {
    PackErr::Git {
        source_url: source_url.to_owned(),
        message: message.into(),
    }
}

fn failed(source_url: &str, output: &Output) -> PackErr {
    let stderr = String::from_utf8_lossy(&output.stderr);
    git_error(
        source_url,
        if stderr.trim().is_empty() {
            output.status.to_string()
        } else {
            stderr.trim().to_owned()
        },
    )
}
