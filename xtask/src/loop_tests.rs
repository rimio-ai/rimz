use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::gates::failure_detail;
use crate::runner::run_streamed;

const PATTERN: &str = "loops/*/tests/test_*.py";

pub(crate) fn loop_tests(root: &Path) -> Result<()> {
    run_tests(root, "uv", &mut |_| {})
}

pub(crate) fn run_tests(root: &Path, program: &str, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let mut failures = Vec::new();
    for path in discover(root)? {
        let captured = run_streamed(
            root,
            program,
            [Path::new("run"), Path::new("--script"), &path],
            &[],
            &[],
            progress,
        );
        let detail = match captured {
            Ok(captured) if captured.status.success() => continue,
            Ok(captured) => failure_detail(&captured.output),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == ErrorKind::NotFound) =>
            {
                bail!(
                    "uv is required to run the loop tests; install it: https://docs.astral.sh/uv/getting-started/installation/"
                );
            }
            Err(error) => format!("{error:#}"),
        };
        failures.push(format!(
            "{} failed:\n{detail}\nRerun: uv run --script {}",
            path.display(),
            path.display()
        ));
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("\n\n"));
    }
    Ok(())
}

fn discover(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let tasks = match fs::read_dir(root.join("loops")) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            bail!("no loop tests match {PATTERN}");
        }
        Err(error) => return Err(error).context("reading loops directory"),
    };
    for task in tasks {
        let task = task?;
        if !task.file_type()?.is_dir() {
            continue;
        }
        let tests = Path::new("loops").join(task.file_name()).join("tests");
        let entries = match fs::read_dir(root.join(&tests)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", tests.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_file()
                && name
                    .to_str()
                    .is_some_and(|name| name.starts_with("test_") && name.ends_with(".py"))
            {
                paths.push(tests.join(name));
            }
        }
    }
    if paths.is_empty() {
        bail!("no loop tests match {PATTERN}");
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn discovery_is_sorted_and_only_matches_the_layout() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "loops/z/tests/test_b.py",
            "loops/a/tests/test_z.py",
            "loops/a/tests/test_a.py",
            "loops/a/tests/helper.py",
            "loops/a/test_outside.py",
            "loops/a/tests/nested/test_nested.py",
            "loops/a/tests/test_wrong.rs",
        ] {
            write(root.path(), path, "");
        }
        fs::create_dir_all(root.path().join("loops/a/tests/test_directory.py")).unwrap();
        assert_eq!(
            discover(root.path()).unwrap(),
            [
                "loops/a/tests/test_a.py",
                "loops/a/tests/test_z.py",
                "loops/z/tests/test_b.py",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn empty_discovery_names_the_pattern() {
        let root = tempfile::tempdir().unwrap();
        for create_loops in [false, true] {
            if create_loops {
                fs::create_dir(root.path().join("loops")).unwrap();
            }
            let result = discover(root.path());
            assert!(result.is_err(), "no loop tests must fail");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("loops/*/tests/test_*.py")
            );
        }
    }

    #[test]
    fn missing_uv_explains_how_to_install_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "loops/a/tests/test_a.py", "");
        let missing = root.path().join("missing-uv");
        let result = run_tests(root.path(), missing.to_str().unwrap(), &mut |_| {});
        assert!(result.is_err(), "missing uv must fail");
        let detail = format!("{:#}", result.unwrap_err());
        assert!(detail.contains("uv is required"), "{detail}");
        assert!(
            detail.contains("https://docs.astral.sh/uv/getting-started/installation/"),
            "{detail}"
        );
    }

    #[test]
    fn reports_all_failures_and_runs_later_files_from_root() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            write(
                root.path(),
                &format!("loops/a/tests/test_{name}.py"),
                &format!(
                    "# /// script\n# requires-python = \">=3.14\"\n# dependencies = []\n# ///\nimport sys\nprint('failure-{name}', file=sys.stderr)\nsys.exit(1)\n"
                ),
            );
        }
        write(
            root.path(),
            "loops/a/tests/test_c.py",
            "# /// script\n# requires-python = \">=3.14\"\n# dependencies = []\n# ///\nfrom pathlib import Path\nPath('completed').write_text('yes')\n",
        );
        let result = loop_tests(root.path());
        assert!(result.is_err(), "failing scripts must fail the task");
        let detail = format!("{:#}", result.unwrap_err());
        for name in ["a", "b"] {
            assert!(
                detail.contains(&format!("uv run --script loops/a/tests/test_{name}.py")),
                "{detail}"
            );
            assert!(detail.contains(&format!("failure-{name}")), "{detail}");
        }
        assert_eq!(
            fs::read_to_string(root.path().join("completed")).unwrap(),
            "yes"
        );
    }
}
