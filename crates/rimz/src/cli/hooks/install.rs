use super::*;

pub(super) fn run_install(agent: String) -> Result<()> {
    let integration = adapter_by_kind(&agent)?;
    let report = integration.install_hooks()?;
    // User-facing JSON. Report struct derives Serialize so the shape stays in
    // lockstep with `HookInstallReport`.
    let rendered = serde_json::to_string_pretty(&report)?;
    #[expect(clippy::print_stdout, reason = "user-visible install report")]
    {
        println!("{rendered}");
    }
    Ok(())
}

pub(super) fn run_uninstall(agent: String) -> Result<()> {
    let integration = adapter_by_kind(&agent)?;
    let report = integration.uninstall_hooks()?;
    let rendered = serde_json::to_string_pretty(&report)?;
    #[expect(clippy::print_stdout, reason = "user-visible uninstall report")]
    {
        println!("{rendered}");
    }
    Ok(())
}
