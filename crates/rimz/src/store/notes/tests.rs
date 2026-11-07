use super::*;
use std::fs;

#[test]
fn allocation_dates_each_kind_and_preserves_the_creation_sample() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let at: Timestamp = "2026-11-07T00:23:36Z".parse().unwrap();
    let checkout = temp.path().join("lane @");
    let id = crate::ids::WorkflowRunId::new();
    let actual: Vec<_> = [
        Kind::Team {
            team: "forge".into(),
            worktree: None,
        },
        Kind::Workflow {
            workflow: "land / test".into(),
            run: id.clone(),
        },
        Kind::Loop {
            task: "review".into(),
        },
        Kind::Loop {
            task: "review".into(),
        },
    ]
    .into_iter()
    .map(|kind| allocate(&paths, kind, &checkout, at).unwrap())
    .collect();
    assert_eq!(
        actual,
        [
            PathBuf::from("20261107/002336Z-forge-lane"),
            PathBuf::from("20261107/002336Z-workflow-land---test-lane"),
            PathBuf::from("20261107/002336Z-loop-review"),
            PathBuf::from("20261107/002336Z-loop-review-2"),
        ]
    );
    for (relative, root, kind) in [
        (&actual[0], "team-notes", "team"),
        (&actual[1], "automation-notes", "workflow"),
        (&actual[2], "automation-notes", "loop"),
    ] {
        let run = paths.root.join(root).join(relative);
        let manifest = Manifest::read(&run).unwrap();
        assert_eq!(manifest.created_at, at);
        assert_eq!(manifest.checkout, checkout);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(run.join("run.json")).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["kind"], kind);
        if kind == "workflow" {
            assert_eq!(json["run"], id.to_string());
        }
    }
}

#[test]
fn enumeration_accepts_both_layouts_without_following_links_or_deeper_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("team-notes");
    for relative in [
        "20261107/002336Z-forge-lane/nested",
        "20260101T000000Z-forge-lane",
        "20261108",
    ] {
        fs::create_dir_all(root.join(relative)).unwrap();
    }
    std::os::unix::fs::symlink(temp.path(), root.join("foreign")).unwrap();
    std::os::unix::fs::symlink(temp.path(), root.join("20261107/foreign")).unwrap();
    let folders = enumerate(&root).unwrap();
    assert_eq!(
        folders.runs,
        [
            root.join("20260101T000000Z-forge-lane"),
            root.join("20261107/002336Z-forge-lane")
        ]
    );
    assert_eq!(
        folders.dates,
        [root.join("20261107"), root.join("20261108")]
    );
}

#[test]
fn version_one_manifest_without_a_kind_reads_as_a_team() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("run.json"),
        br#"{"version":1,"team":"forge","created_at":"2026-11-07T00:23:36Z","checkout":"/repo"}"#,
    )
    .unwrap();
    let result = Manifest::read(temp.path());
    assert!(
        matches!(&result, Ok(Manifest { kind: Kind::Team { team, .. }, .. }) if team == "forge"),
        "{result:?}"
    );
}
