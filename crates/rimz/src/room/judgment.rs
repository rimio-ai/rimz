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
mod tests {
    use super::*;

    #[test]
    fn judgment_retains_skipped_loop_task_warning() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let mut machine = MachineConfig::default();
        let message = "loop task `one` has no `root`; the task does not run".to_owned();
        machine
            .notices
            .skipped_loop_tasks
            .insert("one".into(), message.clone());
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        let warnings = judgment
            .skipped
            .iter()
            .filter(|entry| entry.setting == "loop.tasks.one")
            .collect::<Vec<_>>();
        assert_eq!(
            warnings,
            vec![&SkippedSetting {
                setting: "loop.tasks.one".into(),
                message
            }]
        );
    }

    #[test]
    fn account_label_warnings_do_not_refuse_the_room_or_drop_accounts() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        for (label, count) in [("", 1), ("work", 0)] {
            let mut machine = MachineConfig::default();
            machine.accounts.codex.insert(
                "team-1".parse().unwrap(),
                crate::config::NamedAccount {
                    label: Some(label.to_owned()),
                    home: Some(root.path().join("team-1")),
                    ..Default::default()
                },
            );
            let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
            let judgment =
                judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
            let warnings: Vec<_> = judgment
                .skipped
                .iter()
                .filter(|entry| entry.setting == "accounts.codex.team-1.label")
                .collect();
            assert_eq!(warnings.len(), count, "{warnings:?}");
            if count > 0 {
                let message = &warnings[0].message;
                for text in [
                    "accounts.codex.team-1.label",
                    "empty",
                    "name instead",
                    "rimz accounts add codex team-1 --label",
                    "--label ''",
                ] {
                    assert!(message.contains(text), "{message}");
                }
            }
            assert!(
                crate::agents::LoginCatalog::from_config(&machine.accounts)
                    .unwrap()
                    .select(
                        &AgentKind::new_unchecked("codex"),
                        &"team-1".parse().unwrap()
                    )
                    .is_ok()
            );
        }
    }

    #[test]
    fn failed_account_never_prepares_remote_control() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("missing-account");
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let machine = MachineConfig {
            accounts: toml::from_str(&format!(
                "[claude.work]\nhome = '{}'\n[use]\nclaude = 'work'\n",
                home.display()
            ))
            .unwrap(),
            remote_control: toml::from_str("claude = true\n").unwrap(),
            ..Default::default()
        };
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        assert!(
            !home.exists(),
            "judgment must not create a failed account's home"
        );
        let message = |setting: &str| {
            judgment
                .skipped
                .iter()
                .find(|entry| entry.setting == setting)
                .map(|entry| entry.message.as_str())
                .unwrap_or_else(|| panic!("missing {setting}"))
        };
        let account = message("accounts.claude.work");
        let waiver = message("remote_control.claude");
        assert!(waiver.contains("see accounts.claude.work"), "{waiver}");
        assert!(
            waiver.contains("rimz config set remote_control.claude false"),
            "{waiver}"
        );
        assert!(!waiver.contains(account), "{waiver}");
        assert!(
            judgment
                .remote_control_off
                .contains(&AgentKind::new_unchecked("claude"))
        );
    }

    #[test]
    fn judgment_warns_about_out_of_range_tmux_options() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let machine = MachineConfig {
            tmux: toml::from_str("history_limit = 4294967295\nescape_time_ms = 3000000000\n")
                .unwrap(),
            ..Default::default()
        };
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        for key in ["tmux.history_limit", "tmux.escape_time_ms"] {
            assert!(
                judgment.skipped.iter().any(|entry| entry.setting == key),
                "missing {key}"
            );
        }
    }

    #[test]
    fn judgment_warns_about_an_uninstalled_mux_default() {
        if std::env::var_os("RIMZ_TEST_JUDGMENT_NO_MUX").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "room::judgment::tests::judgment_warns_about_an_uninstalled_mux_default",
                    "--nocapture",
                ])
                .env("PATH", "/no-rimz-test-binaries")
                .env("RIMZ_TEST_JUDGMENT_NO_MUX", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let mut machine = MachineConfig::default();
        machine.mux.default = Some(MuxName::Zellij);
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        assert!(
            judgment
                .skipped
                .iter()
                .any(|entry| entry.setting == "mux.default"
                    && entry.message.contains("this room uses tmux"))
        );
    }

    #[test]
    fn judgment_warns_for_shared_account_budget_and_subscription_month_cap() {
        if std::env::var_os("RIMZ_TEST_JUDGMENT_BUDGET_HOME").is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "room::judgment::tests::judgment_warns_for_shared_account_budget_and_subscription_month_cap",
                    "--nocapture",
                ])
                .env("RIMZ_HOME", home.path())
                .env("RIMZ_TEST_JUDGMENT_BUDGET_HOME", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let machine = MachineConfig {
            accounts: toml::from_str(
                "[budget]\nclaude = '10/month'\n[claude.work]\nbudget = '5/day'\n",
            )
            .unwrap(),
            ..Default::default()
        };
        let state = crate::StatePaths::for_project_root(&workspace.project_root).unwrap();
        let runtime = crate::RuntimePaths::for_state(&state).unwrap();
        let record = crate::agents::account::ProviderRecord {
            consecutive_failures: 0,
            probed_at_ms: 0,
            version_probed_at_ms: None,
            ok: true,
            inputs_stamp: None,
            account: Some(crate::agents::AgentAccount {
                metered: Some(true),
                ..Default::default()
            }),
            login: None,
            probe_error: None,
            profile: None,
        };
        crate::disk::atomic::write_temp_then_rename_cache(
            &runtime.shared_accounts_path(),
            &crate::agents::account::AccountsCache {
                logins: [("claude@default".parse().unwrap(), record)].into(),
            },
        )
        .unwrap();
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        let warnings: BTreeMap<_, _> = judgment
            .skipped
            .iter()
            .map(|entry| (entry.setting.as_str(), entry.message.as_str()))
            .collect();
        assert!(
            warnings
                .get("accounts.claude.work.budget")
                .is_some_and(|text| text.contains("history = \"standalone\"")
                    && text.contains("claude@default")
                    && text.contains("[accounts.budget].claude")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .get("accounts.budget.claude")
                .is_some_and(|text| text.contains("claude@default")
                    && text.contains("subscription")
                    && text.contains("API-key billing only")
                    && text.contains("will never park")
                    && text.contains("extra-usage limit")),
            "{warnings:?}"
        );
    }

    #[test]
    fn judgment_names_a_bare_shell_missing_from_path() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let mut machine = MachineConfig::default();
        machine.agents.shell = Some("rimz-test-shell-not-installed".into());
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        let warning = judgment
            .skipped
            .iter()
            .find(|entry| entry.setting == "agents.shell")
            .unwrap();
        assert!(
            warning.message.contains("is not on PATH"),
            "{}",
            warning.message
        );
        assert!(
            warning
                .message
                .contains("rimz config set agents.shell bash"),
            "{}",
            warning.message
        );
    }

    #[test]
    fn judgment_names_independent_shell_budget_and_schedule_problems() {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let mut machine = MachineConfig::default();
        machine.agents.shell = Some(root.path().join("missing/bash"));
        machine.accounts = toml::from_str("[budget]\ncopilot = '10/day'\n").unwrap();
        machine.agents.hibernate.after = "bad".to_owned();
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        let settings: BTreeSet<_> = judgment
            .skipped
            .iter()
            .map(|entry| entry.setting.as_str())
            .collect();
        for setting in [
            "agents.shell",
            "accounts.budget.copilot",
            "agents.hibernate",
        ] {
            assert!(
                settings.contains(setting),
                "missing {setting}: {settings:?}"
            );
        }
    }
    use std::path::PathBuf;
    #[test]
    fn hibernate_notice_only_for_explicit_unavailable_capability() {
        let mut config = crate::config::HibernateConfig::default();
        let mut admission = crate::harness::hibernate::Admission {
            available: false,
            reason: Some("no swap".into()),
        };
        assert_eq!(hibernate_notice(&config, &admission), None);
        config.enabled = Some(false);
        assert_eq!(hibernate_notice(&config, &admission), None);
        config.enabled = Some(true);
        assert_eq!(
            hibernate_notice(&config, &admission).as_deref(),
            Some("agents.hibernate.enabled is set but hibernation is skipped: no swap")
        );
        admission.available = true;
        admission.reason = None;
        assert_eq!(hibernate_notice(&config, &admission), None);
    }

    #[test]
    fn broken_config_notice_is_one_line_and_names_the_fallback() {
        let err = crate::config::MachineConfig::parse_text(
            std::path::Path::new("/tmp/theme.toml"),
            "[theme.display]\nmax_cols = 64\nmax_cols = 72\n",
            std::path::Path::new("/tmp/missing-agents-home"),
        )
        .expect_err("duplicate key fails");

        let notice = broken_config_notice(&err);

        assert_eq!(
            notice.lines().count(),
            1,
            "notice stays on one line: {notice}"
        );
        assert!(
            notice.contains("/tmp/theme.toml is unparseable"),
            "{notice}"
        );
        assert!(notice.contains("line 3"), "{notice}");
        assert!(
            notice.contains("`max_cols` is defined more than once"),
            "{notice}"
        );
        assert!(notice.contains("built-in defaults apply"), "{notice}");
    }

    #[test]
    fn theme_notice_preserves_the_effective_palette_without_dropping_other_settings() {
        let message = "unknown sidebar theme scheme `missing`; the sidebar keeps the inline `[colors]` palette until it is fixed";
        let err = crate::config::ConfigErr::Theme {
            path: "/tmp/theme.toml".into(),
            message: message.to_owned(),
        };
        assert_eq!(
            broken_config_notice(&err),
            format!("/tmp/theme.toml: {message}")
        );
    }

    #[test]
    fn definition_summary_groups_causes_and_deduplicates_team_roles() {
        let home = PathBuf::from("/home/tester");
        let agents_home = home.join(".rimz");
        let missing = DefinitionCause::MissingSkill {
            skill: "rimz-lsp".parse().unwrap(),
            roots: vec![home.join(".claude/skills"), agents_home.join("skills")],
        };
        let failed = DefinitionCause::DependsOnFailed {
            name: "finder".to_owned(),
        };
        let names = [
            "agents/coder",
            "agents/planner",
            "subagents/finder",
            "subagents/newcomer",
            "subagents/surveyor",
        ];
        let mut errors: Vec<_> = names
            .iter()
            .map(|name| DefinitionErr {
                path: agents_home.join(format!("{name}.md")),
                message: "detail".to_owned(),
                cause: missing.clone(),
            })
            .collect();
        for name in ["agents/astra", "agents/reviewer", "teams/x", "teams/x"] {
            errors.push(DefinitionErr {
                path: agents_home.join(format!("{name}.md")),
                message: "detail".to_owned(),
                cause: failed.clone(),
            });
        }
        let lines = definition_summary(
            &errors.iter().collect::<Vec<_>>(),
            &agents_home,
            Some("/home/tester"),
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("5 definitions"));
        assert!(lines[0].contains("'rimz-lsp'"));
        assert!(lines[0].contains("~/.claude/skills or ~/.rimz/skills"));
        for name in names {
            assert!(lines[0].contains(name));
        }
        assert!(lines[1].starts_with("3 definitions"));
        assert_eq!(lines[1].matches("teams/x").count(), 1);
        assert!(lines[2].contains("rimz agents validate"));
        assert!(
            lines
                .iter()
                .all(|line| line.len() <= 210 && !line.contains(&home.display().to_string())),
            "{lines:?}"
        );
    }

    #[test]
    fn definition_summary_caps_names_and_handles_other_errors() {
        let home = PathBuf::from("/definitions");
        let mut errors: Vec<_> = (0..9)
            .rev()
            .map(|n| DefinitionErr {
                path: home.join(format!("agents/a{n}.md")),
                message: "detail".to_owned(),
                cause: DefinitionCause::MissingSkill {
                    skill: "one".parse().unwrap(),
                    roots: vec![home.join("skills")],
                },
            })
            .collect();
        errors.push(DefinitionErr {
            path: PathBuf::from("/outside/bad.md"),
            message: "detail".to_owned(),
            cause: DefinitionCause::Invalid,
        });
        let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home, None);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("9 definitions"));
        assert!(lines[0].ends_with(
            "agents/a0, agents/a1, agents/a2, agents/a3, agents/a4, agents/a5, +3 more"
        ));
        assert!(lines[1].starts_with("1 definition"));
        assert!(lines[1].ends_with("/outside/bad.md"));
        assert!(definition_summary(&[], &home, None).is_empty());
    }

    #[test]
    fn definition_summary_unions_roots_per_skill() {
        let home = PathBuf::from("/definitions");
        let errors: Vec<_> = [("one", "/a"), ("one", "/b"), ("two", "/a")]
            .into_iter()
            .map(|(skill, root)| DefinitionErr {
                path: home.join("agents/same.md"),
                message: "detail".to_owned(),
                cause: DefinitionCause::MissingSkill {
                    skill: skill.parse().unwrap(),
                    roots: vec![PathBuf::from(root)],
                },
            })
            .collect();
        let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home, None);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("1 definition"));
        assert!(lines[0].contains("'one'") && lines[0].contains("/a or /b"));
        assert!(lines[1].contains("'two'"));
    }
}
