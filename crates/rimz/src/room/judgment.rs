//! Configuration problems that a room can open without, retained for its readers.

use crate::ResolvedWorkspace;
use crate::config::definitions::{DefinitionCause, DefinitionErr};
use crate::config::{ConfigErr, MachineConfig, TmuxConfig};
use crate::harness::hibernate::Admission;
use crate::ids::{AgentKind, MuxName};
use crate::remote_control::{HostLoginEnvs, ReadinessSnapshot};
use crate::workspace::record::SkippedSetting;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) struct Judgment {
    pub skipped: Vec<SkippedSetting>,
    pub remote_control_off: BTreeSet<AgentKind>,
    pub readiness: ReadinessSnapshot,
}

pub(super) fn judge(
    machine: &MachineConfig,
    workspace: &ResolvedWorkspace,
    mux: MuxName,
    hibernation: &Admission,
    live: bool,
    agent_shell: Option<&crate::harness::launch::AgentShellResolution>,
) -> Result<Judgment> {
    let mut skipped = Vec::new();
    let mut warn = |setting: String, message: String| {
        let entry = SkippedSetting { setting, message };
        if !skipped.contains(&entry) {
            skipped.push(entry);
        }
    };
    let errors = crate::config::broken_machine_files();
    let definitions = errors
        .iter()
        .filter_map(|error| match error {
            ConfigErr::Definition(error) => Some(error),
            _ => None,
        })
        .collect::<Vec<_>>();
    let home = std::env::var("HOME").ok();
    let definitions = definition_summary(
        &definitions,
        &crate::disk::paths::agents_home(),
        home.as_deref(),
    );
    if !definitions.is_empty() {
        warn(
            crate::disk::paths::agents_home().display().to_string(),
            definitions.join("; "),
        );
    }
    for error in &errors {
        match error {
            ConfigErr::AccountBudget { .. }
            | ConfigErr::Account { .. }
            | ConfigErr::Definition(_) => {}
            ConfigErr::Notifications { .. } => warn(
                "notifications".into(),
                format!("{error}; fix the enabled notifications table"),
            ),
            _ => warn(
                error.path().display().to_string(),
                broken_config_notice(error),
            ),
        }
    }
    for (path, message) in &machine.notices.unreadable_files {
        if !errors.iter().any(|error| error.path() == path) {
            warn(
                path.display().to_string(),
                format!("{}: {message}", path.display()),
            );
        }
    }
    for key in &machine.notices.unknown_keys {
        warn(key.key.clone(), key.to_string());
    }
    for warning in &machine.notices.definition_warnings {
        warn(warning.path.display().to_string(), warning.to_string());
    }
    if let Ok(Some(packs)) = crate::trust::project_packs(&workspace.project_root)
        && let Ok(installed) = crate::packs::Record::read(&crate::disk::paths::agents_home())
    {
        for message in crate::packs::expectation_warnings(&packs, &installed) {
            warn("packs.expect".into(), message);
        }
    }
    for message in &machine.notices.host_isolation_fallback {
        warn(
            "agents.isolation".into(),
            message.trim_start_matches("warning: ").to_owned(),
        );
    }
    for (name, message) in &machine.notices.skipped_loop_tasks {
        warn(format!("loop.tasks.{name}"), message.clone());
    }
    for message in &machine.notices.hibernate_unavailable {
        warn("agents.hibernate".into(), message.clone());
    }
    if let Err(error) = machine.agents.hibernate.schedule() {
        warn("agents.hibernate".into(), error.to_string());
    } else if let Some(message) = hibernate_notice(&machine.agents.hibernate, hibernation) {
        warn("agents.hibernate".into(), message);
    }
    if let Err(error) = crate::sandbox::preflight(machine.agents.isolation) {
        warn(
            "agents.isolation".into(),
            crate::blocker::Blocker::from(error).to_string(),
        );
    }
    if let Err(error) = crate::harness::schedule::throttle::preflight(&machine.r#loop.throttle) {
        warn("loop.throttle".into(), error.to_string());
    }
    let state = crate::StatePaths::for_project_root(&workspace.project_root)?;
    let agent_shell = agent_shell.cloned().or_else(|| {
        machine.agents.shell.as_deref().map(|shell| {
            crate::harness::launch::resolve_agent_shell(shell, Some(&state.workspace_record))
        })
    });
    if let Some(shell) = agent_shell
        && let Err(error) = shell.path
    {
        warn(
            "agents.shell".into(),
            crate::blocker::Blocker::from(error).to_string(),
        );
    }
    for error in &crate::agents::plugins::loaded().errors {
        warn(
            error.path.display().to_string(),
            format!("{error}; fix or remove the manifest, then run `rimz agents register --check`"),
        );
    }
    for (setting, error) in machine.accounts.budget_errors() {
        warn(setting, error.to_string());
    }
    for (setting, message) in machine.accounts.label_warnings() {
        warn(setting, message);
    }
    let (catalog, declarations) = crate::agents::LoginCatalog::room_view(&machine.accounts);
    let mut account_warnings = BTreeMap::new();
    for (kind, error) in &declarations {
        warn(format!("accounts.{kind}"), error.to_string());
        account_warnings.insert(kind.clone(), format!("accounts.{kind}"));
    }
    let runtime = crate::RuntimePaths::for_state(&state)?;
    let scopes: BTreeSet<_> = catalog
        .all()
        .map(|login| crate::harness::budget::DailyBudgetScope::account(&catalog, &login.key()))
        .collect();
    for scope in scopes {
        let ledger = scope.read_ledger(&runtime, Some(&state));
        if let Some((setting, message)) = scope.subscription_warning(&ledger, machine, &runtime) {
            warn(setting, message);
        }
    }
    let logins = crate::agents::room_accounts(
        &state.workspace_record,
        Some(&workspace.project_root),
        machine,
    )?;
    let ambient = crate::agents::ambient_env();
    let mut envs = HostLoginEnvs::from_logins(&machine.accounts, &logins);
    for kind in logins.kinds() {
        if declarations.contains_key(kind)
            || machine
                .require_readable_core("select a room account")
                .is_err()
        {
            continue;
        }
        match logins.login(kind, &machine.accounts) {
            Ok(login) => {
                if let Err(error) = login.preflight(&ambient) {
                    let setting = format!("accounts.{kind}.{}", login.name());
                    warn(setting.clone(), error.to_string());
                    account_warnings.insert(kind.clone(), setting);
                    envs.reject(kind, Box::new(error).into());
                }
            }
            Err(error) => {
                warn(format!("accounts.{kind}"), format!("{kind}: {error}"));
                account_warnings.insert(kind.clone(), format!("accounts.{kind}"));
            }
        }
    }
    if let Some(default) = machine.mux.default
        && which::which(default.as_str()).is_err()
    {
        warn(
            "mux.default".into(),
            format!(
                "configured mux default `{default}` is not installed; this room uses {mux}; install {default} or change `mux.default`; a live room keeps its backend until reset"
            ),
        );
    }
    for (key, value) in [
        ("history_limit", machine.tmux.history_limit),
        ("escape_time_ms", machine.tmux.escape_time_ms),
    ] {
        if value > TmuxConfig::INT_MAX {
            warn(
                format!("tmux.{key}"),
                format!(
                    "tmux.{key} = {value} exceeds tmux's maximum {}; the option is skipped; lower `tmux.{key}`",
                    TmuxConfig::INT_MAX
                ),
            );
        }
    }
    let config = &machine.remote_control;
    let mut remote_control_off = BTreeSet::new();
    let (readiness, checked) = loop {
        let config = config.without(remote_control_off.iter().map(AgentKind::as_str));
        crate::remote_control::prepare_hosts(&config, &envs);
        let readiness = ReadinessSnapshot::probe(&config, &envs, Some(&workspace.project_root));
        let Err(mut block) = readiness.start_gate() else {
            break (readiness, config);
        };
        let kind = AgentKind::new_unchecked(block.host.kind());
        if envs.for_host(block.host).is_err()
            && let Some(setting) = account_warnings.get(&kind)
        {
            block.blocker.problem = format!(
                "{kind} remote control needs the {kind} account, which failed: see {setting}"
            );
        }
        let effect = if live {
            "is blocked; the live room's policy is unchanged"
        } else {
            "is skipped for this run"
        };
        warn(
            format!("remote_control.{kind}"),
            format!("{}\n{kind} remote control {effect}", block.blocker),
        );
        remote_control_off.insert(kind);
    };
    let readiness = if live {
        let applied = crate::remote_control::room_config_at(config, &state.workspace_record)
            .without(remote_control_off.iter().map(AgentKind::as_str));
        if applied == checked {
            readiness
        } else {
            ReadinessSnapshot::probe(&applied, &envs, Some(&workspace.project_root))
        }
    } else {
        readiness
    };
    Ok(Judgment {
        skipped,
        remote_control_off,
        readiness,
    })
}

fn definition_summary(
    errors: &[&DefinitionErr],
    agents_home: &Path,
    home: Option<&str>,
) -> Vec<String> {
    let mut missing = BTreeMap::<_, (BTreeSet<_>, BTreeSet<_>)>::new();
    let mut dependent = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    let mut pack_problems = BTreeSet::new();
    for error in errors {
        match &error.cause {
            DefinitionCause::MissingSkill { skill, roots } => {
                let (paths, searched) = missing.entry(skill).or_default();
                paths.insert(error.path.as_path());
                searched.extend(roots.iter().map(|root| root.as_path()));
            }
            DefinitionCause::DependsOnFailed { .. } => {
                dependent.insert(error.path.as_path());
            }
            DefinitionCause::Invalid => {
                invalid.insert(error.path.as_path());
            }
            DefinitionCause::PackProblem => {
                pack_problems.insert(format!(
                    "pack problem at {}: {}; unrelated launches remain available",
                    home_relative_path_to(home, &error.path),
                    error.message
                ));
            }
        }
    }
    let mut lines = Vec::new();
    for (skill, (paths, roots)) in missing {
        let roots = roots
            .into_iter()
            .map(|root| home_relative_path_to(home, root))
            .collect::<Vec<_>>()
            .join(" or ");
        lines.push(definition_group(
            &paths,
            agents_home,
            home,
            &format!("missing skill '{skill}' (not installed in {roots})"),
        ));
    }
    for (paths, reason) in [
        (dependent, "depending on one that failed"),
        (invalid, "with another error"),
    ] {
        if !paths.is_empty() {
            lines.push(definition_group(&paths, agents_home, home, reason));
        }
    }
    if !lines.is_empty() {
        lines.push("launches that select them are refused until the files are fixed; `rimz agents validate` lists every error".to_owned());
    }
    lines.extend(pack_problems);
    lines
}

fn definition_group(
    paths: &BTreeSet<&Path>,
    agents_home: &Path,
    home: Option<&str>,
    reason: &str,
) -> String {
    let mut names: Vec<_> = paths
        .iter()
        .map(|path| {
            path.strip_prefix(agents_home)
                .ok()
                .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
                .map_or_else(
                    || home_relative_path_to(home, path),
                    |path| path.with_extension("").display().to_string(),
                )
        })
        .collect();
    names.sort();
    let count = names.len();
    names.truncate(6);
    if count > 6 {
        names.push(format!("+{} more", count - 6));
    }
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} definition{suffix} {reason}: {}", names.join(", "))
}

fn broken_config_notice(err: &crate::config::ConfigErr) -> String {
    let home = std::env::var("HOME").ok();
    let path = home_relative_path_to(home.as_deref(), err.path());
    if let crate::config::ConfigErr::Theme { message, .. } = err {
        let detail = one_line(message);
        return format!("{path}: {detail}");
    }
    let detail = err
        .diagnosis()
        .map(crate::config::ConfigFileDiagnosis::summary)
        .unwrap_or_else(|| {
            one_line(
                &std::error::Error::source(err)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| err.to_string()),
            )
        });
    let state = if err.diagnosis().is_some() {
        "unparseable"
    } else {
        "invalid"
    };
    let effect = if err.path() == MachineConfig::config_path() {
        "built-in defaults apply, and launches that follow a configured account refuse"
    } else {
        "built-in defaults apply to the room display"
    };
    format!("{path} is {state} — {effect}: {detail}; fix the file, then restart")
}

fn home_relative_path_to(home: Option<&str>, path: &std::path::Path) -> String {
    let path = crate::utils::path::normalize_path_lexical(path);
    home.and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || path.display().to_string(),
            |tail| std::path::Path::new("~").join(tail).display().to_string(),
        )
}
fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}
fn hibernate_notice(
    config: &crate::config::HibernateConfig,
    admission: &crate::harness::hibernate::Admission,
) -> Option<String> {
    if !config.explicit() || admission.available {
        return None;
    }
    admission.reason.as_ref().map(|reason| {
        format!("agents.hibernate.enabled is set but hibernation is skipped: {reason}")
    })
}

#[cfg(test)]
mod tests;
