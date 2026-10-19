//! Read-only condition inspection, using the same scope and readings as tasks.

use super::*;
use schedule::when::WhenExpr;

pub(super) fn run(args: ExplainArgs) -> Result<()> {
    if let Some(kind) = args.provider.as_deref()
        && rimz::agents::find_definition(kind).is_none()
    {
        bail!("unknown provider kind `{kind}`");
    }
    let workspace = if args.root == Path::new(".") {
        resolve_caller_workspace()
    } else {
        WorkspaceResolver::resolve(&args.root, None).map_err(Into::into)
    }
    .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    condition::validate(&args.expressions, &workspace.project_root).map_err(|err| {
        match err.downcast::<schedule::when::WhenError>() {
            Ok(refusal) => crate::cli::usage::UsageError::new(refusal.to_string()).into(),
            Err(err) => err,
        }
    })?;
    let expr = WhenExpr::parse(&args.expressions)
        .map_err(|err| crate::cli::usage::UsageError::new(err.to_string()))?;
    let needs_provider =
        args.provider.is_none() && expr.terms().any(|term| term.key.starts_with("window."));
    let entry = TaskEntry {
        root: workspace.project_root.clone(),
        dir: (workspace.worktree_root != workspace.project_root).then_some(workspace.worktree_root),
        provider: args.provider.map(rimz::ids::AgentKind::new_unchecked),
        account: args.account,
        ..Default::default()
    };
    let verdict = condition::reading(&entry, &expr);
    let mut out = ui::out();
    writeln!(out, "when: {expr}")?;
    writeln!(out, "scope: {}", entry.run_dir().display())?;
    condition::write_terms(&mut out, &entry, &expr, &verdict)?;
    writeln!(out, "now: {}", verdict.ok)?;
    if !render::room_open(&entry.resolved_root()) {
        condition::write_no_room_hint(
            &mut out,
            &schedule::ParsedTrigger {
                trigger: schedule::Trigger::Condition { expr, hold: None },
                once: false,
            },
        )?;
    }
    if needs_provider {
        writeln!(
            out,
            "window readings need --provider <KIND>; without it they are unknown"
        )?;
    }
    if !verdict.ok {
        return Err(ui::AlreadyReported.into());
    }
    Ok(())
}
