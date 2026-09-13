//! Per-file install and uninstall report rows for multi-file installers.

use std::path::Path;

use super::HookInstallFileReport;

pub(super) fn report_files<'a>(
    files: impl IntoIterator<Item = (&'a Path, bool)>,
) -> Vec<HookInstallFileReport> {
    files
        .into_iter()
        .map(|(path, existed)| HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        })
        .collect()
}
