use super::*;

#[test]
fn migration_dates_legacy_runs_preserves_contents_and_repoints_only_matching_links() {
    let mut actual = Vec::new();
    for case in [
        "linked",
        "foreign-link",
        "removed-checkout",
        "collision",
        "invalid-name",
        "aliased-home",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(checkout.join(".rimz")).unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let store = if case == "aliased-home" {
            let state =
                crate::StatePaths::under(crate::WorkspaceId::from_project_root(&checkout), &alias)
                    .unwrap();
            let runtime = crate::RuntimePaths::for_state_under(&state, temp.path());
            Store::open(state, runtime).unwrap()
        } else {
            store(&home, &checkout)
        };
        let name = if case == "invalid-name" {
            "not-a-stamp-forge-checkout"
        } else {
            "20260102T030405Z-forge-checkout"
        };
        let old = store.paths().team_notes_dir.join(name);
        fs::create_dir_all(&old).unwrap();
        let old_canonical = old.canonicalize().unwrap();
        let archive = store.paths().team_notes_dir.canonicalize().unwrap();
        let destination = archive.join(if case == "collision" {
            "20260102/030405Z-forge-checkout-2"
        } else {
            "20260102/030405Z-forge-checkout"
        });
        let manifest = serde_json::to_vec(&serde_json::json!({
            "version": 1, "team": "forge", "created_at": "2026-02-03T04:05:06Z",
            "checkout": checkout,
        }))
        .unwrap();
        fs::write(old.join("run.json"), &manifest).unwrap();
        fs::write(old.join("plan-notes.md"), "kept notes").unwrap();
        let foreign = temp.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        let link = checkout.join(TEAM_LINK);
        std::os::unix::fs::symlink(
            if case == "foreign-link" {
                &foreign
            } else {
                &old_canonical
            },
            &link,
        )
        .unwrap();
        if case == "removed-checkout" {
            fs::remove_dir_all(&checkout).unwrap();
        }
        let collision = archive.join("20260102/030405Z-forge-checkout");
        if case == "collision" {
            fs::create_dir_all(&collision).unwrap();
            fs::write(collision.join("sentinel"), "keep collision").unwrap();
        }
        migrate(store.paths()).unwrap();
        let valid = case != "invalid-name";
        let kept = if valid { &destination } else { &old_canonical };
        let expected_link = match case {
            "foreign-link" => Some(foreign),
            "removed-checkout" => None,
            _ => Some(kept.clone()),
        };
        let before = crate::store::notes::enumerate(&archive).unwrap().runs;
        let link_before = fs::read_link(&link).ok();
        migrate(store.paths()).unwrap();
        let after = crate::store::notes::enumerate(&archive).unwrap().runs;
        actual.push((
            case,
            old.exists() != valid,
            destination.exists() == valid,
            fs::read(kept.join("run.json")).ok().as_ref() == Some(&manifest),
            fs::read_to_string(kept.join("plan-notes.md"))
                .ok()
                .as_deref()
                == Some("kept notes"),
            fs::read_link(&link).ok() == expected_link,
            case != "collision"
                || fs::read_to_string(collision.join("sentinel"))
                    .ok()
                    .as_deref()
                    == Some("keep collision"),
            before == after && fs::read_link(&link).ok() == link_before,
        ));
    }
    assert_eq!(
        actual,
        [
            "linked",
            "foreign-link",
            "removed-checkout",
            "collision",
            "invalid-name",
            "aliased-home"
        ]
        .map(|case| (case, true, true, true, true, true, true, true))
    );
}
