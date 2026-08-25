use super::*;

fn hash6(root: &Path) -> String {
    WorkspaceId::from_project_root(root).as_str()[3..9].to_owned()
}

fn expected_session(root: &Path, slug: &str) -> String {
    format!("rimz-{slug}-{}", hash6(root))
}

#[test]
fn session_name_uses_bounded_basename_and_workspace_hash() {
    let root = Path::new("/home/user/xxx");
    assert_eq!(session_name_for(root), expected_session(root, "xxx"));
    assert!(session_name_for(root).len() <= 20);
}

#[test]
fn session_name_truncates_long_basename() {
    let root = Path::new("/tmp/abcdefghijklmnop");
    assert_eq!(session_name_for(root), expected_session(root, "abcdefgh"));
}

#[test]
fn session_name_distinguishes_roots_with_the_same_basename() {
    let a = Path::new("/tmp/one/project");
    let b = Path::new("/tmp/two/project");

    assert_ne!(session_name_for(a), session_name_for(b));
    assert!(session_name_for(a).starts_with("rimz-project-"));
    assert!(session_name_for(b).starts_with("rimz-project-"));
}

#[test]
fn session_name_hash_matches_workspace_id_prefix() {
    let root = Path::new("/home/user/rimio");
    let name = session_name_for(root);

    assert_eq!(name, format!("rimz-rimio-{}", hash6(root)));
}

#[test]
fn known_workspaces_reads_records_and_skips_recordless_dirs() {
    use crate::store::paths::{StatePaths, workspaces_dir_under};
    use crate::store::workspace_record::{self, WorkspaceRecord};

    let dir = tempfile::TempDir::new().expect("tempdir");
    let state_root = dir.path();
    let root = workspaces_dir_under(state_root);

    // Two workspaces with records, written through the canonical path.
    for project in ["/home/user/alpha", "/home/user/beta"] {
        let project_root = std::path::PathBuf::from(project);
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let paths = StatePaths::under(workspace_id.clone(), state_root).expect("state paths");
        std::fs::create_dir_all(&paths.root).expect("mkdir workspace");
        workspace_record::write(
            &paths,
            &WorkspaceRecord {
                workspace_id,
                project_root: project_root.clone(),
                worktree_root: None,
                session_name: session_name_for(&project_root),
                root_class: RootClass::Repo,
                rimz_bin: None,
                rimz_build: None,
                updated_at: jiff::Timestamp::UNIX_EPOCH,
            },
        )
        .expect("write record");
    }
    // A directory whose name isn't a workspace id, and a workspace dir with no
    // record, are both skipped silently.
    std::fs::create_dir_all(root.join("not-a-workspace-id")).expect("mkdir junk");

    let mut sessions: Vec<String> = known_workspaces_under(&root)
        .expect("enumerate")
        .into_iter()
        .map(|ws| ws.session_name)
        .collect();
    sessions.sort();
    assert_eq!(
        sessions,
        ["/home/user/alpha", "/home/user/beta"]
            .into_iter()
            .map(|project| session_name_for(Path::new(project)))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn known_workspaces_repairs_record_fields_for_the_canonical_workspace_dir() {
    use crate::store::paths::{StatePaths, workspaces_dir_under};
    use crate::store::workspace_record::{self, WorkspaceRecord};

    let dir = tempfile::TempDir::new().expect("tempdir");
    let state_root = dir.path().join("state");
    let project_root = dir.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");

    let canonical_root = project_root.canonicalize().expect("canonical project");
    let noncanonical_root = project_root.join("..").join("project");
    let workspace_id = WorkspaceId::from_project_root(&canonical_root);
    let paths = StatePaths::under(workspace_id.clone(), &state_root).expect("state paths");
    std::fs::create_dir_all(&paths.root).expect("mkdir workspace");
    workspace_record::write(
        &paths,
        &WorkspaceRecord {
            workspace_id: workspace_id.clone(),
            project_root: noncanonical_root,
            worktree_root: None,
            session_name: "rimz-stale".to_owned(),
            root_class: RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
        },
    )
    .expect("write stale record");

    let known = known_workspaces_under(&workspaces_dir_under(&state_root)).expect("enumerate");
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].workspace_id, workspace_id);
    assert_eq!(known[0].project_root, canonical_root);
    assert_eq!(known[0].session_name, session_name_for(&canonical_root));

    let repaired = workspace_record::read(&paths.workspace_record).expect("read repaired");
    assert_eq!(repaired.workspace_id, workspace_id);
    assert_eq!(repaired.project_root, project_root.canonicalize().unwrap());
    assert_eq!(repaired.session_name, session_name_for(&canonical_root));
}

#[test]
fn known_workspaces_skips_obsolete_noncanonical_duplicate_records() {
    use crate::store::paths::{StatePaths, workspaces_dir_under};
    use crate::store::workspace_record::{self, WorkspaceRecord};

    let dir = tempfile::TempDir::new().expect("tempdir");
    let state_root = dir.path().join("state");
    let project_root = dir.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");

    let canonical_root = project_root.canonicalize().expect("canonical project");
    let canonical_id = WorkspaceId::from_project_root(&canonical_root);
    let canonical_paths =
        StatePaths::under(canonical_id.clone(), &state_root).expect("canonical paths");
    std::fs::create_dir_all(&canonical_paths.root).expect("mkdir canonical");
    workspace_record::write(
        &canonical_paths,
        &WorkspaceRecord {
            workspace_id: canonical_id.clone(),
            project_root: canonical_root.clone(),
            worktree_root: None,
            session_name: session_name_for(&canonical_root),
            root_class: RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
        },
    )
    .expect("write canonical record");

    let noncanonical_root = project_root.join("..").join("project");
    let stale_id = WorkspaceId::from_project_root(&noncanonical_root);
    assert_ne!(stale_id, canonical_id);
    let stale_paths = StatePaths::under(stale_id.clone(), &state_root).expect("stale paths");
    std::fs::create_dir_all(&stale_paths.root).expect("mkdir stale");
    workspace_record::write(
        &stale_paths,
        &WorkspaceRecord {
            workspace_id: stale_id,
            project_root: noncanonical_root,
            worktree_root: None,
            session_name: session_name_for(&canonical_root),
            root_class: RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            updated_at: jiff::Timestamp::now(),
        },
    )
    .expect("write stale duplicate");

    let known = known_workspaces_under(&workspaces_dir_under(&state_root)).expect("enumerate");
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].workspace_id, canonical_id);
    assert_eq!(known[0].project_root, canonical_root);
    assert_eq!(known[0].session_name, session_name_for(&canonical_root));
}

#[test]
fn known_workspaces_under_missing_root_is_empty() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("nope");
    assert!(known_workspaces_under(&missing).expect("ok").is_empty());
}

#[test]
fn recorded_room_bin_prefers_stable_then_recorded_then_current() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = crate::store::StatePaths::for_workspace(workspace_id.clone()).expect("state paths");
    let recorded = dir.path().join("recorded-rimz");
    std::fs::write(&recorded, b"recorded").expect("write recorded");
    std::fs::create_dir_all(&paths.root).expect("create workspace state");
    std::fs::write(&paths.room_bin, b"stable").expect("write stable");

    assert_eq!(
        resolve_recorded_rimz_bin(&workspace_id, Some(&recorded)),
        paths.room_bin
    );
    std::fs::remove_file(&paths.room_bin).expect("remove stable");
    assert_eq!(
        resolve_recorded_rimz_bin(&workspace_id, Some(&recorded)),
        recorded
    );
    std::fs::remove_file(&recorded).expect("remove recorded");
    assert_eq!(
        resolve_recorded_rimz_bin(&workspace_id, Some(&recorded)),
        crate::proc::rimz_exe()
    );

    std::fs::remove_dir_all(&paths.root).expect("clean workspace state");
}

#[test]
fn session_name_collapses_unsafe_runs() {
    // Spaces and `/` both fold to `-`, and runs collapse to a single `-`.
    let root = Path::new("/tmp/my repo");
    assert_eq!(session_name_for(root), expected_session(root, "my-repo"));
}

#[test]
fn session_name_is_stable_for_same_root() {
    let a = session_name_for(Path::new("/repo"));
    let b = session_name_for(Path::new("/repo"));
    assert_eq!(a, b);
}

#[test]
fn resolve_marker_finds_cargo_toml_ancestor() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let resolved = resolve_marker(here).expect("Cargo.toml above us");
    assert!(resolved.join("Cargo.toml").exists());
}

use std::ffi::OsString;

/// An injected env carrying the identity pin, the test-side twin of the
/// session environment a real pane inherits.
fn pin_of(workspace_id: String, project_root: PathBuf) -> impl Fn(&str) -> Option<OsString> {
    move |key: &str| match key {
        ENV_WORKSPACE_ID => Some(workspace_id.clone().into()),
        ENV_PROJECT_ROOT => Some(project_root.clone().into_os_string()),
        _ => None,
    }
}

fn no_env(_key: &str) -> Option<OsString> {
    None
}

/// A bare directory and a marker directory, the fixture every pin test
/// shares: the pin names the bare dir, the cwd sits in the marker dir.
fn pin_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let pinned_root = dir.path().join("room");
    let marker_dir = dir.path().join("project");
    std::fs::create_dir_all(&pinned_root).expect("mkdir room");
    std::fs::create_dir_all(&marker_dir).expect("mkdir project");
    std::fs::write(marker_dir.join("Cargo.toml"), "[package]\n").expect("marker");
    (dir, pinned_root, marker_dir)
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("mkdir repo");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "rimz@example.test"]);
    run(&["config", "user.name", "RimZ Test"]);
    std::fs::write(path.join("README.md"), "base\n").expect("base file");
    run(&["add", "README.md"]);
    run(&["commit", "-m", "base"]);
}

#[test]
fn participant_pin_beats_the_static_ladder() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root.clone(),
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.project_root, pinned_root);
    assert_eq!(
        resolved.workspace_id,
        WorkspaceId::from_project_root(&pinned_root),
    );
    // The cwd still names the worktree the participant works in.
    assert_eq!(
        resolved.worktree_root,
        marker_dir.canonicalize().expect("canonical project"),
    );
    assert_eq!(resolved.root_class, RootClass::Directory);
    assert_eq!(resolved.cwd_project_root, None);
}

#[test]
fn participant_pin_tracks_the_starting_paths_repo_root() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let pinned_root = dir.path().join("room");
    let current_root = dir.path().join("current");
    init_git_repo(&pinned_root);
    init_git_repo(&current_root);
    let start = current_root.join("nested");
    std::fs::create_dir(&start).expect("nested cwd");
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let current_root = current_root.canonicalize().expect("canonical current");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root.clone(),
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &start, None, &env, NO_SCAN)
            .expect("resolve");

    assert_eq!(resolved.project_root, pinned_root);
    assert_eq!(resolved.cwd_project_root, Some(current_root));
}

#[test]
fn participant_pin_tracks_main_repo_from_a_linked_worktree() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let pinned_root = dir.path().join("room");
    let current_root = dir.path().join("current");
    let linked = dir.path().join("linked");
    std::fs::create_dir_all(&pinned_root).expect("room");
    init_git_repo(&current_root);
    let status = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            "linked",
            linked.to_str().expect("utf8 path"),
        ])
        .current_dir(&current_root)
        .status()
        .expect("add linked worktree");
    assert!(status.success());
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let current_root = current_root.canonicalize().expect("canonical current");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root,
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &linked, None, &env, NO_SCAN)
            .expect("resolve");

    assert_eq!(resolved.cwd_project_root, Some(current_root));
}

#[test]
fn create_mode_ignores_the_pin() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root,
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &marker_dir, None, &env, NO_SCAN)
            .expect("resolve");
    assert_eq!(
        resolved.project_root,
        marker_dir.canonicalize().expect("canonical project"),
    );
    assert_eq!(resolved.root_class, RootClass::Marker);
    assert_eq!(resolved.cwd_project_root, None);
}

#[test]
fn create_mode_refuses_a_missing_starting_path() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let err =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &missing, None, &no_env, NO_SCAN)
            .expect_err("missing path must fail");

    assert_eq!(
        err.to_string(),
        format!(
            "could not resolve workspace from {}: the path does not exist; check the path or create the directory first",
            missing.display()
        )
    );
}

#[test]
fn participate_mode_keeps_missing_path_fallback() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &missing, None, &no_env, NO_SCAN)
            .expect("participant resolution must not fail");

    assert_eq!(resolved.project_root, missing);
}

#[test]
fn root_override_beats_the_pin() {
    let (dir, pinned_root, marker_dir) = pin_fixture();
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root,
    );
    let forced = dir.path().join("forced");
    init_git_repo(&forced);

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::Participate,
        &marker_dir,
        Some(forced.clone()),
        &env,
        NO_SCAN,
    )
    .expect("resolve");
    assert_eq!(
        resolved.project_root,
        forced.canonicalize().expect("canonical forced"),
    );
    assert_eq!(
        resolved.cwd_project_root,
        Some(resolved.project_root.clone())
    );
}

#[test]
fn mismatched_pin_falls_back_to_the_static_ladder() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    // An id that does not hash from the pinned root: stale or corrupt env.
    let env = pin_of(
        WorkspaceId::from_project_root(Path::new("/somewhere/else")).to_string(),
        pinned_root,
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env, NO_SCAN)
            .expect("resolve");
    assert_eq!(
        resolved.project_root,
        marker_dir.canonicalize().expect("canonical project"),
    );
    assert_eq!(resolved.root_class, RootClass::Marker);
}

#[test]
fn vanished_pin_root_falls_back_to_the_static_ladder() {
    let (dir, pinned_root, marker_dir) = pin_fixture();
    let gone = dir.path().join("gone");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        gone,
    );

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Marker);
}

#[test]
fn unparseable_pin_falls_back_to_the_static_ladder() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    let env = pin_of("not-a-workspace-id".to_owned(), pinned_root);

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Marker);
}

#[test]
fn bare_directory_resolves_as_a_directory_workspace() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).expect("mkdir scratch");

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &scratch, None, &no_env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Directory);
    assert_eq!(
        resolved.project_root,
        scratch.canonicalize().expect("canonical scratch"),
    );
    assert_eq!(resolved.project_root, resolved.worktree_root);
    assert_eq!(resolved.cwd_project_root, None);
}

#[test]
fn persisted_workspace_id_absolutizes_a_vanished_relative_root() {
    let relative = Path::new("target/rimz-vanished-persisted-root");
    assert!(!relative.exists(), "fixture root must stay nonexistent");
    let absolute = crate::worktree::normalize_path_lexical(
        &std::env::current_dir().expect("current dir").join(relative),
    );

    assert_eq!(
        WorkspaceResolver::persisted_workspace_id(relative).expect("resolve persisted identity"),
        WorkspaceId::from_project_root(&absolute)
    );
}

#[test]
fn fresh_git_resolution_tracks_the_project_root() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    init_git_repo(&repo);

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &repo, None, &no_env, NO_SCAN)
            .expect("resolve");

    assert_eq!(
        resolved.cwd_project_root.as_deref(),
        Some(resolved.project_root.as_path())
    );
}

#[test]
fn nonexistent_dotted_root_override_is_normalized_before_identity() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let dotted = dir.path().join("missing").join("..").join("project");
    assert!(!dotted.exists(), "fixture root must stay nonexistent");

    let resolved = WorkspaceResolver::resolve(dir.path(), Some(dotted)).expect("resolve");
    let expected = crate::worktree::normalize_path_lexical(&dir.path().join("project"));

    assert!(resolved.project_root.is_absolute());
    assert!(
        resolved
            .project_root
            .components()
            .all(|component| !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            ))
    );
    assert_eq!(resolved.project_root, expected);
    assert_eq!(
        resolved.workspace_id,
        WorkspaceId::from_project_root(&expected),
    );
}

#[test]
fn lexical_normalization_folds_bare_cur_dir_to_empty() {
    // The sharp edge `normalized_root` guards against: folding `.` lexically
    // erases it, so an unresolved relative root must never reach the hash.
    assert_eq!(
        crate::worktree::normalize_path_lexical(std::path::Path::new(".")),
        std::path::PathBuf::new(),
    );
}

#[test]
fn relative_root_refuses_when_the_cwd_is_unlinked() {
    // nextest runs each test in its own process, so unlinking this process's
    // cwd cannot disturb a sibling test.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).expect("create scratch");
    std::env::set_current_dir(&scratch).expect("enter scratch");
    std::fs::remove_dir(&scratch).expect("unlink scratch");

    // `canonicalize(".")` and `current_dir()` now fail together — the exact
    // shape that used to fold to `""` and mint one shared identity.
    let err = super::normalized_root(std::path::PathBuf::from("."))
        .expect_err("an unlinked cwd must refuse, not resolve to an empty root");
    assert!(
        err.to_string().contains("current directory is unreadable"),
        "error should name the unreadable cwd and its fix, got: {err}"
    );
}

#[test]
fn resolution_refuses_rather_than_minting_the_empty_root_identity() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).expect("create scratch");
    std::env::set_current_dir(&scratch).expect("enter scratch");
    std::fs::remove_dir(&scratch).expect("unlink scratch");

    let resolved = WorkspaceResolver::resolve(std::path::Path::new("."), None);
    assert!(
        resolved.is_err(),
        "resolving from an unlinked cwd must fail; \
         succeeding collapses every caller into one shared store"
    );
}

#[test]
fn create_mode_accepts_home_like_directory_root() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let home_env = |key: &str| (key == "HOME").then(|| home.clone().into_os_string());

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &home, None, &home_env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Directory);
    assert_eq!(resolved.project_root, home.canonicalize().expect("home"));
}

#[test]
fn participants_never_refuse_a_pathological_root() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let home_env = |key: &str| (key == "HOME").then(|| home.clone().into_os_string());

    // A hook on the agent's critical path degrades, never errors: the
    // pinless fallback still resolves the directory tier at $HOME.
    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &home, None, &home_env, NO_SCAN)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Directory);
}

#[test]
fn pin_env_carries_both_identity_keys() {
    let root = Path::new("/repo");
    let env = pin_env(&WorkspaceId::from_project_root(root), root);
    assert_eq!(
        env.get(ENV_WORKSPACE_ID),
        Some(&WorkspaceId::from_project_root(root).to_string()),
    );
    assert_eq!(env.get(ENV_PROJECT_ROOT), Some(&"/repo".to_owned()));
}

/// The scan-side twin of [`pin_of`]: a sibling agent process carrying the
/// room's pin at the hook's cwd.
fn scan_of(root: PathBuf) -> impl Fn(&Path) -> Vec<PathBuf> {
    move |_cwd: &Path| vec![root.clone()]
}

#[test]
fn recovered_pin_beats_the_static_ladder() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let scan = scan_of(pinned_root.clone());

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::Participate,
        &marker_dir,
        None,
        &no_env,
        &scan,
    )
    .expect("resolve");
    assert_eq!(resolved.project_root, pinned_root);
    assert_eq!(
        resolved.workspace_id,
        WorkspaceId::from_project_root(&pinned_root),
    );
    // The cwd still names the worktree the participant works in.
    assert_eq!(
        resolved.worktree_root,
        marker_dir.canonicalize().expect("canonical project"),
    );
    assert_eq!(resolved.root_class, RootClass::Directory);
}

#[test]
fn env_pin_beats_the_recovered_pin() {
    let (dir, pinned_root, marker_dir) = pin_fixture();
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let other_root = dir.path().join("other");
    std::fs::create_dir_all(&other_root).expect("mkdir other");
    let env = pin_of(
        WorkspaceId::from_project_root(&pinned_root).to_string(),
        pinned_root.clone(),
    );
    let scan = scan_of(other_root.canonicalize().expect("canonical other"));

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env, &scan)
            .expect("resolve");
    assert_eq!(resolved.project_root, pinned_root);
}

#[test]
fn daemon_mode_sibling_pin_beats_a_valid_ambient_pin() {
    let (dir, ambient_root, marker_dir) = pin_fixture();
    let ambient_root = ambient_root.canonicalize().expect("canonical ambient");
    let sibling_root = dir.path().join("sibling");
    std::fs::create_dir_all(&sibling_root).expect("mkdir sibling");
    let env = pin_of(
        WorkspaceId::from_project_root(&ambient_root).to_string(),
        ambient_root,
    );
    let sibling_root = sibling_root.canonicalize().expect("canonical sibling");
    let scan = scan_of(sibling_root.clone());

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::ParticipateDaemon,
        &marker_dir,
        None,
        &env,
        &scan,
    )
    .expect("resolve");
    assert_eq!(resolved.project_root, sibling_root);
}

#[test]
fn daemon_mode_without_sibling_ignores_the_ambient_pin() {
    let (_dir, ambient_root, marker_dir) = pin_fixture();
    let ambient_root = ambient_root.canonicalize().expect("canonical ambient");
    let env = pin_of(
        WorkspaceId::from_project_root(&ambient_root).to_string(),
        ambient_root,
    );

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::ParticipateDaemon,
        &marker_dir,
        None,
        &env,
        NO_SCAN,
    )
    .expect("resolve");
    assert_eq!(
        resolved.project_root,
        marker_dir.canonicalize().expect("canonical marker"),
    );
}

#[test]
fn daemon_mode_root_override_wins() {
    let (dir, ambient_root, marker_dir) = pin_fixture();
    let ambient_root = ambient_root.canonicalize().expect("canonical ambient");
    let sibling_root = dir.path().join("sibling");
    let forced_root = dir.path().join("forced");
    std::fs::create_dir_all(&sibling_root).expect("mkdir sibling");
    std::fs::create_dir_all(&forced_root).expect("mkdir forced");
    let env = pin_of(
        WorkspaceId::from_project_root(&ambient_root).to_string(),
        ambient_root,
    );
    let scan = scan_of(sibling_root.canonicalize().expect("canonical sibling"));

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::ParticipateDaemon,
        &marker_dir,
        Some(forced_root.clone()),
        &env,
        &scan,
    )
    .expect("resolve");
    assert_eq!(
        resolved.project_root,
        forced_root.canonicalize().expect("canonical forced"),
    );
}

#[test]
fn daemon_mode_split_sibling_pins_fall_back_to_the_static_ladder() {
    let (dir, ambient_root, marker_dir) = pin_fixture();
    let ambient_root = ambient_root.canonicalize().expect("canonical ambient");
    let sibling_root = dir.path().join("sibling");
    let other_sibling_root = dir.path().join("other-sibling");
    std::fs::create_dir_all(&sibling_root).expect("mkdir sibling");
    std::fs::create_dir_all(&other_sibling_root).expect("mkdir other sibling");
    let env = pin_of(
        WorkspaceId::from_project_root(&ambient_root).to_string(),
        ambient_root,
    );
    let sibling_root = sibling_root.canonicalize().expect("canonical sibling");
    let other_sibling_root = other_sibling_root
        .canonicalize()
        .expect("canonical other sibling");
    let scan = move |_cwd: &Path| vec![sibling_root.clone(), other_sibling_root.clone()];

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::ParticipateDaemon,
        &marker_dir,
        None,
        &env,
        &scan,
    )
    .expect("resolve");
    assert_eq!(
        resolved.project_root,
        marker_dir.canonicalize().expect("canonical marker"),
    );
}

#[test]
fn split_recovered_pins_fall_back_to_the_static_ladder() {
    let (dir, pinned_root, marker_dir) = pin_fixture();
    let other_root = dir.path().join("other");
    std::fs::create_dir_all(&other_root).expect("mkdir other");
    let pinned_root = pinned_root.canonicalize().expect("canonical room");
    let other_root = other_root.canonicalize().expect("canonical other");
    let scan = move |_cwd: &Path| vec![pinned_root.clone(), other_root.clone()];

    let resolved = WorkspaceResolver::resolve_with(
        ResolveMode::Participate,
        &marker_dir,
        None,
        &no_env,
        &scan,
    )
    .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Marker);
}

#[test]
fn agreeing_recovered_pins_dedup_to_one_root() {
    let root = PathBuf::from("/repo");
    assert_eq!(
        recover_pinned_root(Path::new("/cwd"), &|_| vec![root.clone(), root.clone()]),
        Some(root),
    );
}

#[test]
fn create_mode_ignores_the_recovered_pin() {
    let (_dir, pinned_root, marker_dir) = pin_fixture();
    let scan = scan_of(pinned_root.canonicalize().expect("canonical room"));

    let resolved =
        WorkspaceResolver::resolve_with(ResolveMode::Create, &marker_dir, None, &no_env, &scan)
            .expect("resolve");
    assert_eq!(resolved.root_class, RootClass::Marker);
}

#[test]
fn verify_pin_is_the_single_validation_path() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    let id = WorkspaceId::from_project_root(&root).to_string();

    assert_eq!(verify_pin(&id, &root), Some(root.clone()));
    assert_eq!(verify_pin("not-a-workspace-id", &root), None);
    assert_eq!(verify_pin(&id, &dir.path().join("gone")), None);
    let other = WorkspaceId::from_project_root(Path::new("/somewhere/else")).to_string();
    assert_eq!(verify_pin(&other, &root), None);
}
