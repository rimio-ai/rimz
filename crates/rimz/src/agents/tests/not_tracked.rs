use super::*;
use crate::agents::capabilities::*;
use crate::agents::definition::{CoverageAnnotations, ToolClassification};

struct Unregistered {
    spec: &'static AgentSpec,
    hooks: &'static Hooks,
}

struct Hooks {
    installed: bool,
    untrusted: bool,
    disabled: bool,
}

impl CoreCapability for Unregistered {
    fn spec(&self) -> &'static AgentSpec {
        self.spec
    }
}
impl HookCapability for Unregistered {}
impl LaunchCapability for Unregistered {}
impl SessionCapability for Unregistered {}
impl TranscriptCapability for Unregistered {}
impl ContextCapability for Unregistered {}
impl AccountCapability for Unregistered {}
impl SpendingCapability for Unregistered {}
impl RuntimeControlCapability for Unregistered {}
impl InstallationCapability for Unregistered {
    fn managed_integration(&self) -> Option<&'static dyn ManagedIntegration> {
        Some(self.hooks)
    }

    fn untracked_mode_fix(&self, command_line: &str) -> Option<String> {
        (command_line == "unregistered --detached")
            .then(|| "unregistered without --detached, then restart".to_owned())
    }
}

impl ManagedIntegration for Hooks {
    fn installed(&self, _env: &BTreeMap<String, String>) -> bool {
        self.installed
    }

    fn hooks_disabled(&self, _env: &BTreeMap<String, String>) -> Option<HookPreflightErr> {
        self.disabled.then(|| HookPreflightErr::HooksDisabled {
            reason: "policy blocks hooks".to_owned(),
            fix: "enable hooks".to_owned(),
        })
    }

    fn untrusted_installed_hooks(&self, _env: &BTreeMap<String, String>) -> Vec<String> {
        if self.untrusted {
            vec!["Stop".to_owned()]
        } else {
            Vec::new()
        }
    }

    fn install(
        &self,
        _: &BTreeMap<String, String>,
        _: HookInstallOptions,
    ) -> Result<HookInstallReport> {
        unreachable!("the preflight fixture never installs hooks")
    }

    fn preview(
        &self,
        _: &BTreeMap<String, String>,
        _: HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        unreachable!("the preflight fixture never previews hooks")
    }

    fn uninstall(&self, _: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        unreachable!("the preflight fixture never uninstalls hooks")
    }

    fn outdated_files(&self, _: &BTreeMap<String, String>, _: HookInstallOptions) -> Vec<PathBuf> {
        Vec::new()
    }
}

fn adapter(hooks: Hooks, discovery: LocalSessionDiscovery, wired: bool) -> AgentDefinition {
    let base = registry::BUILTINS[0].spec();
    let spec = AgentSpec {
        kind: "unregistered",
        capabilities: Capabilities {
            local_session_discovery: discovery,
            ..base.capabilities
        },
        coverage: CoverageAnnotations {
            hook_install: if wired {
                ConcernCoverage::Wired { via: "fixture" }
            } else {
                ConcernCoverage::Unsupported {
                    reason: "self-managed",
                }
            },
            ..base.coverage
        },
        aliases: &[],
        display_name: "Unregistered",
        brand: Brand {
            emblem: None,
            color: 252,
            color_rgb: (216, 216, 216),
        },
        plan_label: definition::PlanLabel::TitleCaseOnly,
        plan_capacities: None,
        sub_providers: &[],
        expected_windows: &[],
        tools: ToolClassification {
            input_key: None,
            mutating: &[],
            editing: &[],
            blocking: &[],
        },
        user_coverage: base.user_coverage,
        lifecycle_hooks: base.lifecycle_hooks,
        default_context_window: None,
        default_model: None,
        process_names: &["unregistered"],
        bin_names: &["unregistered"],
        bin_identity: None,
        extra_bin_dirs: &[],
        thread_key: base.thread_key,
        launch: LaunchSpec::EMPTY,
        host_skills: skills::HostSkills::Unsupported,
        tool_rules: skills::ToolRules::Unsupported,
    };
    AgentDefinition::new(Box::leak(Box::new(Unregistered {
        spec: Box::leak(Box::new(spec)),
        hooks: Box::leak(Box::new(hooks)),
    })))
}

#[test]
fn hook_reasons_follow_preflight_order_and_only_known_installations() {
    let actual: Vec<_> = [
        (
            false,
            false,
            false,
            LocalSessionDiscovery::Unsupported,
            true,
        ),
        (true, true, false, LocalSessionDiscovery::Unsupported, true),
        (true, false, true, LocalSessionDiscovery::Unsupported, true),
        (false, false, true, LocalSessionDiscovery::Unsupported, true),
        (true, false, false, LocalSessionDiscovery::Unsupported, true),
        (false, false, true, LocalSessionDiscovery::FullScan, true),
        (false, false, true, LocalSessionDiscovery::PerPane, true),
        (
            false,
            false,
            true,
            LocalSessionDiscovery::Unsupported,
            false,
        ),
    ]
    .into_iter()
    .map(|(installed, untrusted, disabled, discovery, wired)| {
        not_tracked_hooks(
            &adapter(
                Hooks {
                    installed,
                    untrusted,
                    disabled,
                },
                discovery,
                wired,
            ),
            &BTreeMap::new(),
        )
    })
    .collect();
    assert_eq!(
        actual,
        vec![
            Some(NotTracked {
                reason: NotTrackedReason::HooksMissing,
                fix: "rimz hooks install unregistered, then restart".to_owned()
            }),
            Some(NotTracked {
                reason: NotTrackedReason::HooksBlocked,
                fix: "run /hooks inside unregistered and trust the RimZ hooks, then restart"
                    .to_owned()
            }),
            Some(NotTracked {
                reason: NotTrackedReason::HooksBlocked,
                fix: "enable hooks, then restart".to_owned()
            }),
            Some(NotTracked {
                reason: NotTrackedReason::HooksBlocked,
                fix: "enable hooks, then restart".to_owned()
            }),
            None,
            None,
            None,
            None,
        ]
    );
}

#[test]
fn mode_fix_defaults_to_none_and_definition_reaches_override() {
    struct DefaultMode;
    impl CoreCapability for DefaultMode {
        fn spec(&self) -> &'static AgentSpec {
            unreachable!("the default mode check needs no spec")
        }
    }
    impl InstallationCapability for DefaultMode {}
    assert_eq!(
        DefaultMode.untracked_mode_fix("unregistered --detached"),
        None
    );
    let definition = adapter(
        Hooks {
            installed: true,
            untrusted: false,
            disabled: false,
        },
        LocalSessionDiscovery::Unsupported,
        true,
    );
    assert_eq!(definition.untracked_mode_fix("unregistered"), None);
    assert_eq!(
        definition.untracked_mode_fix("unregistered --detached"),
        Some("unregistered without --detached, then restart".to_owned())
    );
}
