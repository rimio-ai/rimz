//! SSH argv builder for installing RimZ on a remote host.
//!
//! The CLI owns process I/O. This module keeps the SSH argv and shell snippet
//! testable beside the remote attach and web builders.

use crate::mux::CommandSpec;
use crate::update::{DARWIN_AARCH64_ARCHIVE, DARWIN_X86_64_ARCHIVE, LINUX_X86_64_ARCHIVE};

use super::{sh_quote, ssh_program};

const RELEASE_BASE: &str = "https://github.com/rimio-ai/rimz/releases/latest/download/";

pub fn setup_install_spec(destination: &str, host: &str, version: Option<&str>) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args(["-o", "ConnectTimeout=10", "-t", "--"])
        .arg(destination)
        .arg(install_snippet(host, version))
}

fn install_snippet(host: &str, version: Option<&str>) -> String {
    let success = sh_quote(&format!("installed on {host} at ~/.local/bin/rimz"));
    let base = sh_quote(&version.map_or_else(
        || RELEASE_BASE.to_owned(),
        |version| format!("https://github.com/rimio-ai/rimz/releases/download/v{version}/"),
    ));
    let download_failure = version.map(|version| {
        let notice = sh_quote(&format!("rimz: no release v{version} on GitHub (your rimz is ahead of the latest release); rerun with --latest to install the newest release"));
        format!("if [ \"$rc\" -eq 22 ]; then printf '%s\\n' {notice} >&2; fi")
    }).unwrap_or_default();
    format!(
        r#"set -e
os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
  Linux:x86_64)
    asset="{LINUX_X86_64_ARCHIVE}"
    verify() {{ sha256sum -c --ignore-missing SHA256SUMS; }}
    ;;
  Darwin:arm64|Darwin:aarch64)
    asset="{DARWIN_AARCH64_ARCHIVE}"
    verify() {{ shasum -a 256 -c --ignore-missing SHA256SUMS; }}
    ;;
  Darwin:x86_64)
    asset="{DARWIN_X86_64_ARCHIVE}"
    verify() {{ shasum -a 256 -c --ignore-missing SHA256SUMS; }}
    ;;
  *)
    echo "rimz: no prebuilt for $os/$arch; install rimz manually: https://github.com/rimio-ai/rimz/blob/main/docs/guide/installation.md" >&2
    exit 1
    ;;
esac
mkdir -p "$HOME/.local/bin"
dir="$(mktemp -d "$HOME/.local/bin/.rimz-setup.XXXXXX")"
cleanup() {{ rm -rf "$dir"; }}
trap cleanup EXIT
trap 'cleanup; exit 129' HUP
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM
cd "$dir"
base={base}
download() {{
  curl -fLO "$1" || {{
    rc=$?
    {download_failure}
    exit "$rc"
  }}
}}
download "${{base}}${{asset}}"
download "${{base}}SHA256SUMS"
verify
tar -xzf "$asset"
chmod 0755 "${{asset%.tar.gz}}/rimz"
mv -f "${{asset%.tar.gz}}/rimz" "$HOME/.local/bin/rimz"
installed_version="$("$HOME/.local/bin/rimz" --version)"
printf '%s %s\n' "$installed_version" {success}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_install_spec_builds_ssh_pty_installer() {
        let spec = setup_install_spec("alice@dev-box", "dev-box", Some("0.4.3"));
        assert_eq!(spec.program, "ssh");
        assert_eq!(
            spec.args[0..5],
            ["-o", "ConnectTimeout=10", "-t", "--", "alice@dev-box"]
        );
        assert_eq!(spec.args.len(), 6);
        let snippet = spec.args.last().expect("snippet");
        for needle in [
            "https://github.com/rimio-ai/rimz/releases/download/v0.4.3/",
            LINUX_X86_64_ARCHIVE,
            DARWIN_AARCH64_ARCHIVE,
            DARWIN_X86_64_ARCHIVE,
            "sha256sum -c --ignore-missing SHA256SUMS",
            "shasum -a 256 -c --ignore-missing SHA256SUMS",
            r#"dir="$(mktemp -d "$HOME/.local/bin/.rimz-setup.XXXXXX")""#,
            "trap 'cleanup; exit 129' HUP",
            r#"chmod 0755 "${asset%.tar.gz}/rimz""#,
            r#"mv -f "${asset%.tar.gz}/rimz" "$HOME/.local/bin/rimz""#,
            "$HOME/.local/bin/rimz",
            "installed on dev-box at ~/.local/bin/rimz",
            "rimz: no release v0.4.3 on GitHub (your rimz is ahead of the latest release); rerun with --latest to install the newest release",
        ] {
            assert!(snippet.contains(needle), "missing {needle}: {snippet}");
        }
        // A copy of a just-written file can wait on a filesystem block clone; the snippet renames.
        assert!(
            !snippet.lines().any(|line| ["install ", "cp ", "cat "]
                .iter()
                .any(|copy| line.trim_start().starts_with(copy))),
            "{snippet}"
        );
        let latest = setup_install_spec("alice@dev-box", "dev-box", None);
        assert!(latest.args.last().unwrap().contains(RELEASE_BASE));
        assert!(!latest.args.last().unwrap().contains("no release v"));
    }
}
