//! Homebrew tap formula generator. Renders the tap's `rimz.rb` from the dist
//! `SHA256SUMS`, reading the asset base URL, homepage, and output path
//! from the environment so `main.rs` arg-parsing stays untouched. The release
//! workflow fills those inputs from `${GITHUB_SERVER_URL}` at CI time, so the
//! concrete release host is never committed to this repo — it lives only in the
//! generated formula, which ships in the separate `homebrew-rimz` tap.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::files::{target_dir, write_atomically};

const ARM_ARCHIVE: &str = "rimz-aarch64-apple-darwin.tar.gz";
const INTEL_ARCHIVE: &str = "rimz-x86_64-apple-darwin.tar.gz";

/// Render the tap formula from the dist checksums and the `RIMZ_BREW_*` inputs.
pub(crate) fn brew_formula(root: &Path) -> Result<()> {
    let base_url = required_env("RIMZ_BREW_BASE_URL")?;
    let homepage = required_env("RIMZ_BREW_HOMEPAGE")?;
    let out = PathBuf::from(required_env("RIMZ_BREW_OUT")?);

    let checksums_path = target_dir(root).join("dist").join("SHA256SUMS");
    let checksums = fs::read_to_string(&checksums_path)
        .with_context(|| format!("reading {}", checksums_path.display()))?;

    let formula = render_formula(&FormulaInputs {
        homepage: &homepage,
        base_url: base_url.trim_end_matches('/'),
        arm_sha: &parse_digest(&checksums, ARM_ARCHIVE)?,
        intel_sha: &parse_digest(&checksums, INTEL_ARCHIVE)?,
    });
    write_atomically(&out, formula.as_bytes())
}

fn required_env(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("{key} must be set"))
}

struct FormulaInputs<'a> {
    homepage: &'a str,
    base_url: &'a str,
    arm_sha: &'a str,
    intel_sha: &'a str,
}

/// The digest recorded for `archive` in a `sha256sum`-format document — one
/// entry per line, two spaces between digest and name.
fn parse_digest(checksums: &str, archive: &str) -> Result<String> {
    checksums
        .lines()
        .find_map(|line| {
            let (digest, name) = line.split_once("  ")?;
            (name == archive).then(|| digest.to_owned())
        })
        .with_context(|| format!("SHA256SUMS has no entry for {archive}"))
}

fn render_formula(inputs: &FormulaInputs<'_>) -> String {
    format!(
        r#"class Rimz < Formula
  desc "Routes your attention across a fleet of coding agents"
  homepage "{homepage}"
  license "MIT"

  on_macos do
    on_arm do
      url "{base_url}/rimz-aarch64-apple-darwin.tar.gz"
      sha256 "{arm_sha}"
    end
    on_intel do
      url "{base_url}/rimz-x86_64-apple-darwin.tar.gz"
      sha256 "{intel_sha}"
    end
  end

  def install
    bin.install "rimz"
  end

  test do
    system bin/"rimz", "--version"
  end
end
"#,
        homepage = inputs.homepage,
        base_url = inputs.base_url,
        arm_sha = inputs.arm_sha,
        intel_sha = inputs.intel_sha,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
a11111111111111111111111111111111111111111111111111111111111111a  rimz-aarch64-apple-darwin.tar.gz
b22222222222222222222222222222222222222222222222222222222222222b  rimz-x86_64-apple-darwin.tar.gz
c33333333333333333333333333333333333333333333333333333333333333c  rimz-x86_64-unknown-linux-gnu.tar.gz
";

    #[test]
    fn parse_digest_reads_the_named_entry() {
        assert_eq!(
            parse_digest(SAMPLE, ARM_ARCHIVE).unwrap(),
            "a11111111111111111111111111111111111111111111111111111111111111a"
        );
        assert_eq!(
            parse_digest(SAMPLE, INTEL_ARCHIVE).unwrap(),
            "b22222222222222222222222222222222222222222222222222222222222222b"
        );
    }

    #[test]
    fn parse_digest_errors_on_missing_entry() {
        assert!(parse_digest(SAMPLE, "rimz-absent.tar.gz").is_err());
    }

    #[test]
    fn render_formula_carries_both_urls_shas_and_license() {
        let formula = render_formula(&FormulaInputs {
            homepage: "https://host.example/rimz/rimz",
            base_url: "https://host.example/rimz/rimz/releases/download/v1.2.3",
            arm_sha: "aaaa",
            intel_sha: "bbbb",
        });
        assert!(formula.contains("license \"MIT\""));
        assert!(formula.contains("homepage \"https://host.example/rimz/rimz\""));
        assert!(formula.contains(
            "url \"https://host.example/rimz/rimz/releases/download/v1.2.3/rimz-aarch64-apple-darwin.tar.gz\""
        ));
        assert!(formula.contains(
            "url \"https://host.example/rimz/rimz/releases/download/v1.2.3/rimz-x86_64-apple-darwin.tar.gz\""
        ));
        assert!(formula.contains("sha256 \"aaaa\""));
        assert!(formula.contains("sha256 \"bbbb\""));
    }
}
