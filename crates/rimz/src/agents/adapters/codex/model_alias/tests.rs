use super::*;
use crate::agents::ProviderLogin;
use crate::agents::capabilities::*;
use crate::ids::AgentKind;
use crate::{RuntimePaths, WorkspaceId};
use std::collections::BTreeMap;

fn catalog() -> Vec<ModelCatalogEntry> {
    [
        ("gpt-6.1-sol", false, None, true),
        ("gpt-6-astra", false, None, true),
        ("gpt-6-sol", false, None, true),
        ("gpt-6-luna", false, None, false),
        ("gpt-reserve", true, None, false),
        ("gpt-5.6-sol", false, None, true),
        ("gpt-5.6-terra", false, None, true),
        ("gpt-5.6-luna", false, None, false),
        ("gpt-5.5", false, Some("gpt-5.6-sol"), false),
        ("codex-auto-review", true, None, false),
    ]
    .into_iter()
    .map(|(id, hidden, upgrade, ultra)| ModelCatalogEntry {
        id: id.into(),
        hidden,
        upgrade: upgrade.map(str::to_owned),
        efforts: ["low", "medium", "high", "xhigh", "max", "ultra"]
            .into_iter()
            .filter(|effort| (*effort != "ultra" || ultra) && (*effort != "max" || id != "gpt-5.5"))
            .map(str::to_owned)
            .collect(),
    })
    .collect()
}

#[test]
fn pure_rule_uses_probe_families() {
    let entries = catalog();
    for (baked, expected) in [
        ("gpt-6-sol", "gpt-6.1-sol"),
        ("gpt-6-astra", "gpt-6-astra"),
        ("gpt-6-luna", "gpt-6-luna"),
        ("gpt-5.6-terra", "gpt-5.6-terra"),
    ] {
        assert_eq!(select(&entries, baked, None), Some((expected, Vec::new())));
    }
    let (id, warnings) = select(&entries, "gpt-6-luna", Some("ultra")).unwrap();
    assert_eq!(id, "gpt-6-luna");
    assert_eq!(warnings.len(), 1);
}

struct Source {
    entries: Option<Vec<ModelCatalogEntry>>,
    calls: usize,
}

impl ModelCatalogSource for Source {
    fn fetch(
        &mut self,
        _: &RuntimePaths,
        _: &BTreeMap<String, String>,
    ) -> Result<Vec<ModelCatalogEntry>, ModelCatalogErr> {
        self.calls += 1;
        self.entries
            .clone()
            .ok_or_else(|| ModelCatalogErr::Unavailable("offline".into()))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: RuntimePaths,
    login: ProviderLogin,
    env: BTreeMap<String, String>,
    source: Source,
}

impl Fixture {
    fn new(entries: Option<Vec<ModelCatalogEntry>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        Self {
            _dir: dir,
            paths,
            login: ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
            env: BTreeMap::new(),
            source: Source { entries, calls: 0 },
        }
    }

    fn resolve(&mut self, alias: &str, effort: Option<&str>) -> ModelAliasResolution {
        super::super::CodexAdapter
            .resolve_model_alias(
                ModelAliasRequest {
                    alias,
                    effort,
                    login: &self.login,
                    login_env: &self.env,
                    paths: &self.paths,
                },
                Some(&mut self.source),
            )
            .expect("a baked alias resolves")
    }

    fn edit_cache(&self, key: &str, value: serde_json::Value) {
        let path = self.paths.shared_model_catalog_path(&self.login.key());
        let mut cache: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cache[key] = value;
        std::fs::write(path, serde_json::to_vec(&cache).unwrap()).unwrap();
    }
}

#[test]
fn probe_families_and_effort_fallback() {
    let mut fixture = Fixture::new(Some(catalog()));
    for (alias, expected) in [
        ("sol", "gpt-6.1-sol"),
        ("astra", "gpt-6-astra"),
        ("luna", "gpt-6-luna"),
        ("terra", "gpt-5.6-terra"),
    ] {
        let result = fixture.resolve(alias, None);
        assert_eq!(result.id, expected);
        assert!(result.warnings.is_empty());
    }
    let result = fixture.resolve("luna", Some("ultra"));
    assert_eq!(result.id, "gpt-6-luna");
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("ultra"));
}

#[test]
fn exact_family_numeric_versions_and_hidden_entries() {
    let mut entries = catalog();
    for id in [
        "gpt-7-sol-mini",
        "gpt-6.2-sol-preview",
        "gpt-6.9-sol",
        "gpt-6.10-sol",
        "gpt-6.11-sol-20260929",
        "gpt--sol",
    ] {
        let mut entry = entries[0].clone();
        entry.id = id.into();
        entries.push(entry);
    }
    let mut hidden = entries[0].clone();
    hidden.id = "gpt-99-sol".into();
    hidden.hidden = true;
    entries.push(hidden);
    assert_eq!(
        select(&entries, "gpt-6-sol", None).unwrap().0,
        "gpt-6.10-sol"
    );
}

#[test]
fn upgrades_follow_visible_targets_and_cycles_terminate() {
    let mut entries = catalog();
    entries[0].upgrade = Some("gpt-6-astra".into());
    assert_eq!(
        select(&entries, "gpt-6-sol", None).unwrap().0,
        "gpt-6-astra"
    );
    entries[1].upgrade = Some("gpt-6.1-sol".into());
    assert_eq!(
        select(&entries, "gpt-6-sol", None).unwrap().0,
        "gpt-6-astra"
    );
    for target in ["gpt-reserve", "missing"] {
        entries[0].upgrade = Some(target.into());
        assert_eq!(
            select(&entries, "gpt-6-sol", None).unwrap().0,
            "gpt-6.1-sol"
        );
    }
}

#[test]
fn skips_newest_without_requested_effort() {
    let mut entries = catalog();
    entries[0].efforts.retain(|effort| effort != "ultra");
    let (id, warnings) = select(&entries, "gpt-6-sol", Some("ultra")).unwrap();
    assert_eq!(id, "gpt-6-sol");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("gpt-6.1-sol"));
}

#[test]
fn failed_fetch_is_throttled_with_and_without_stale_catalog() {
    for stale in [false, true] {
        let mut fixture = Fixture::new(stale.then(catalog));
        if stale {
            fixture.resolve("sol", None);
            fixture.edit_cache("fetched_at", 0.into());
        }
        fixture.source.entries = None;
        fixture.source.calls = 0;
        for _ in 0..3 {
            let result = fixture.resolve("sol", None);
            assert_eq!(
                result.rung,
                if stale {
                    ModelAliasRung::CachedCatalog
                } else {
                    ModelAliasRung::Baked
                }
            );
            assert!(!result.warnings.is_empty());
            assert!(result.movement.is_none());
        }
        assert_eq!(fixture.source.calls, 1);
        fixture.edit_cache("failed_at", 0.into());
        fixture.source.entries = Some(catalog());
        assert_eq!(
            fixture.resolve("sol", None).rung,
            ModelAliasRung::FreshCatalog
        );
        assert_eq!(fixture.source.calls, 2);
    }
}

#[test]
fn mixed_efforts_record_only_the_family_head_move() {
    let mut entries = catalog();
    entries[0].efforts.retain(|effort| effort != "ultra");
    let mut fixture = Fixture::new(Some(entries));
    let skipped = fixture.resolve("sol", Some("ultra"));
    assert_eq!(skipped.id, "gpt-6-sol");
    assert!(!skipped.warnings.is_empty());
    assert_eq!(skipped.movement.unwrap().to, "gpt-6.1-sol");
    assert!(fixture.resolve("sol", None).movement.is_none());
    assert!(fixture.resolve("sol", Some("ultra")).movement.is_none());
}

#[test]
fn fresh_cache_deduplicates_moves_and_expires() {
    let mut fixture = Fixture::new(Some(catalog()));
    let first = fixture.resolve("sol", None);
    assert_eq!(first.rung, ModelAliasRung::FreshCatalog);
    assert_eq!(
        first.movement.unwrap(),
        ModelAliasMove {
            alias: "sol".into(),
            from: "gpt-6-sol".into(),
            to: "gpt-6.1-sol".into()
        }
    );
    assert!(fixture.resolve("sol", None).movement.is_none());
    assert_eq!(fixture.source.calls, 1);
    assert_eq!(
        known_model(&fixture.paths, &fixture.login.key(), "gpt-6.1-sol"),
        Some(true)
    );
    assert_eq!(
        known_model(&fixture.paths, &fixture.login.key(), "missing"),
        Some(false)
    );
    fixture.edit_cache("fetched_at", 0.into());
    fixture.resolve("sol", None);
    assert_eq!(fixture.source.calls, 2);
}

#[test]
fn stale_then_baked_fallback_and_wrong_schema() {
    let mut fixture = Fixture::new(Some(catalog()));
    fixture.resolve("sol", None);
    fixture.edit_cache("fetched_at", 0.into());
    fixture.source.entries = None;
    let stale = fixture.resolve("sol", None);
    assert_eq!(stale.id, "gpt-6.1-sol");
    assert_eq!(stale.rung, ModelAliasRung::CachedCatalog);
    assert_eq!(stale.warnings.len(), 1);
    assert!(stale.movement.is_none());
    fixture.edit_cache("version", 999.into());
    assert_eq!(
        known_model(&fixture.paths, &fixture.login.key(), "gpt-6.1-sol"),
        None
    );
    let baked = fixture.resolve("sol", None);
    assert_eq!(baked.id, "gpt-6-sol");
    assert_eq!(baked.rung, ModelAliasRung::Baked);
    assert_eq!(baked.warnings.len(), 1);
    assert!(baked.movement.is_none());
}

#[test]
fn absent_family_uses_stale_family_before_baked() {
    let mut fixture = Fixture::new(Some(catalog()));
    fixture.resolve("sol", None);
    fixture.edit_cache("fetched_at", 0.into());
    fixture.source.entries = Some(Vec::new());
    let stale = fixture.resolve("sol", None);
    assert_eq!(stale.id, "gpt-6.1-sol");
    assert_eq!(stale.rung, ModelAliasRung::CachedCatalog);
    let baked = Fixture::new(Some(Vec::new())).resolve("sol", None);
    assert_eq!(baked.rung, ModelAliasRung::Baked);
    assert!(baked.movement.is_none());
}

#[test]
fn pins_and_unknown_names_never_fetch() {
    let mut fixture = Fixture::new(Some(catalog()));
    for alias in ["gpt-6-sol", "unknown"] {
        assert!(
            super::super::CodexAdapter
                .resolve_model_alias(
                    ModelAliasRequest {
                        alias,
                        effort: None,
                        login: &fixture.login,
                        login_env: &fixture.env,
                        paths: &fixture.paths
                    },
                    Some(&mut fixture.source)
                )
                .is_none()
        );
    }
    assert_eq!(fixture.source.calls, 0);
    assert_eq!(
        known_model(&fixture.paths, &fixture.login.key(), "gpt-6-sol"),
        None
    );
    assert_eq!(fixture.resolve("sol", None).id, "gpt-6.1-sol");
}

#[test]
fn concurrent_resolutions_fetch_and_report_one_move() {
    let fixture = Fixture::new(Some(catalog()));
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let mut source = Source {
                        entries: Some(catalog()),
                        calls: 0,
                    };
                    let result = super::super::CodexAdapter
                        .resolve_model_alias(
                            ModelAliasRequest {
                                alias: "sol",
                                effort: None,
                                login: &fixture.login,
                                login_env: &fixture.env,
                                paths: &fixture.paths,
                            },
                            Some(&mut source),
                        )
                        .unwrap();
                    (result, source.calls)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().map(|(_, calls)| calls).sum::<usize>(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|(result, _)| result.movement.is_some())
            .count(),
        1
    );
    assert!(results.iter().all(|(result, _)| result.id == "gpt-6.1-sol"));
}
