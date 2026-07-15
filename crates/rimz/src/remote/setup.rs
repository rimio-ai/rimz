//! SSH argv builder for installing RimZ on a remote host.
//!
//! The CLI owns process I/O. This module keeps the SSH argv and shell snippet
//! testable beside the remote attach and web builders.

use crate::mux::CommandSpec;

use super::{sh_quote, ssh_program};

const RELEASE_BASE: &str = "https://github.com/rimio-ai/rimz/releases/latest/download/";
const LINUX_X86_64_ARCHIVE: &str = "rimz-x86_64-unknown-linux-gnu.tar.gz";
const DARWIN_AARCH64_ARCHIVE: &str = "rimz-aarch64-apple-darwin.tar.gz";
const DARWIN_X86_64_ARCHIVE: &str = "rimz-x86_64-apple-darwin.tar.gz";

pub fn setup_install_spec(destination: &str, host: &str) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args(["-o", "ConnectTimeout=10", "-t", "--"])
        .arg(destination)
        .arg(install_snippet(host))
}

fn install_snippet(host: &str) -> String {
    let success = sh_quote(&format!("rimz installed on {host} at ~/.local/bin/rimz"));
    format!(
        r#"set -e
os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
  Linux:x86_64)
    asset="{LINUX_X86_64_ARCHIVE}"
    verify="sha256sum -c --ignore-missing SHA256SUMS"
    ;;
  Darwin:arm64|Darwin:aarch64)
    asset="{DARWIN_AARCH64_ARCHIVE}"
    verify="shasum -a 256 -c --ignore-missing SHA256SUMS"
    ;;
  Darwin:x86_64)
    asset="{DARWIN_X86_64_ARCHIVE}"
    verify="shasum -a 256 -c --ignore-missing SHA256SUMS"
    ;;
  *)
    echo "rimz: no prebuilt for $os/$arch; install rimz manually: https://github.com/rimio-ai/rimz/blob/main/docs/guide/installation.md" >&2
    exit 1
    ;;
esac
mkdir -p "$HOME/.local/bin"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
cd "$dir"
base="{RELEASE_BASE}"
curl -fLO "${{base}}${{asset}}"
curl -fLO "${{base}}SHA256SUMS"
$verify
tar -xzf "$asset"
install -m 0755 "${{asset%.tar.gz}}/rimz" "$HOME/.local/bin/rimz"
"$HOME/.local/bin/rimz" --version
echo {success}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_install_spec_builds_ssh_pty_installer() {
        let spec = setup_install_spec("alice@dev-box", "dev-box");
        assert_eq!(spec.program, "ssh");
        assert_eq!(
            spec.args[0..5],
            ["-o", "ConnectTimeout=10", "-t", "--", "alice@dev-box"]
        );
        assert_eq!(spec.args.len(), 6);
        let snippet = spec.args.last().expect("snippet");
        for needle in [
            RELEASE_BASE,
            LINUX_X86_64_ARCHIVE,
            DARWIN_AARCH64_ARCHIVE,
            DARWIN_X86_64_ARCHIVE,
            "sha256sum -c --ignore-missing SHA256SUMS",
            "shasum -a 256 -c --ignore-missing SHA256SUMS",
            "install -m 0755",
            "$HOME/.local/bin/rimz",
            "rimz installed on dev-box at ~/.local/bin/rimz",
        ] {
            assert!(snippet.contains(needle), "missing {needle}: {snippet}");
        }
    }
}
