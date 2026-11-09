//! The `+` mode: pick a profile, a checkout, an optional new tree name, and a prompt, then run
//! the `rimz agents launch` command the footer shows.

use std::path::{Path, PathBuf};

use rimz::utils::path::normalize_path_lexical;

/// One launchable profile, as `rimz agents profiles` lists it.
#[derive(Clone, Debug)]
pub(super) struct Profile {
    pub(super) name: String,
    pub(super) model: String,
    pub(super) description: String,
}

/// A RimZ-managed worktree `-w NAME` enters.
#[derive(Clone, Debug)]
pub(super) struct Tree {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) branch: String,
}

/// What the `+` mode reads once when the palette opens.
#[derive(Clone, Debug, Default)]
pub(super) struct LaunchSetup {
    pub(super) profiles: Vec<Profile>,
    pub(super) root: PathBuf,
    /// `None` when the room cannot hold worktrees, so no `-w` entry is offered.
    pub(super) trees: Option<Vec<Tree>>,
    /// Why no launch can run now; launches are offered only while the room's mux is live.
    pub(super) unavailable: Option<String>,
    pub(super) new_worktree: bool,
}

impl LaunchSetup {
    /// The where step's first highlight: the new tree under `--new-worktree`, else the current
    /// checkout.
    pub(super) fn default_place(&self, places: &[(Place, String, String)]) -> usize {
        places
            .iter()
            .position(|(place, ..)| self.new_worktree && *place == Place::New)
            .unwrap_or(0)
    }

    /// Why `name` cannot name the new tree; an empty name generates one.
    pub(super) fn name_refusal(&self, name: &str) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let requested = match rimz::worktree::parse_requested_name(name) {
            Ok(requested) => requested,
            Err(error) => return Some(error.to_string()),
        };
        self.trees
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|tree| tree.name == requested.name)
            .then(|| {
                format!(
                    "worktree `{}` is listed; pick it in the where step",
                    requested.name
                )
            })
    }
}

/// The `+` steps after the profile; the profile step is the query itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Where,
    Name,
    Prompt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Place {
    Root,
    Tree(String),
    Cwd(PathBuf),
    New,
}

impl Place {
    pub(super) fn id(&self) -> String {
        match self {
            Self::Root => "where:root".to_owned(),
            Self::Tree(name) => format!("where:tree:{name}"),
            Self::Cwd(path) => format!("where:cwd:{}", path.display()),
            Self::New => "where:new".to_owned(),
        }
    }
}

/// The steps past the profile, which the query no longer drives.
#[derive(Clone, Debug)]
pub(super) struct LaunchFlow {
    pub(super) step: Step,
    pub(super) profile: String,
    profile_query: String,
    pub(super) place: Option<Place>,
    pub(super) filter: String,
    pub(super) name: String,
    pub(super) prompt: String,
}

impl LaunchFlow {
    pub(super) fn new(profile: String, profile_query: String) -> Self {
        Self {
            step: Step::Where,
            profile,
            profile_query,
            place: None,
            filter: String::new(),
            name: String::new(),
            prompt: String::new(),
        }
    }

    pub(super) fn profile_query(&self) -> &str {
        &self.profile_query
    }

    pub(super) fn field(&mut self) -> &mut String {
        match self.step {
            Step::Where => &mut self.filter,
            Step::Name => &mut self.name,
            Step::Prompt => &mut self.prompt,
        }
    }

    pub(super) fn field_text(&self) -> &str {
        match self.step {
            Step::Where => &self.filter,
            Step::Name => &self.name,
            Step::Prompt => &self.prompt,
        }
    }
}

/// The checkouts the where step lists, current first and the new tree last.
pub(super) fn places(setup: &LaunchSetup, current: Option<&Path>) -> Vec<(Place, String, String)> {
    let trees = setup.trees.as_deref().unwrap_or_default();
    // A tree's marker keeps its configured `dir` unresolved (`<root>/../<root>-worktrees/NAME`),
    // while a card names the same checkout normalized.
    let same =
        |left: &Path, right: &Path| normalize_path_lexical(left) == normalize_path_lexical(right);
    let current_place = match current.filter(|path| !same(path, &setup.root)) {
        None => Place::Root,
        Some(path) => trees
            .iter()
            .find(|tree| same(&tree.path, path))
            .map_or_else(
                || Place::Cwd(path.to_owned()),
                |tree| Place::Tree(tree.name.clone()),
            ),
    };
    let describe = |place: &Place| match place {
        Place::Root => (
            setup.root.file_name().map_or_else(
                || setup.root.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            "root checkout".to_owned(),
        ),
        Place::Tree(name) => (
            name.clone(),
            trees
                .iter()
                .find(|tree| tree.name == *name)
                .map(|tree| tree.branch.clone())
                .unwrap_or_default(),
        ),
        Place::Cwd(path) => (path.display().to_string(), "checkout".to_owned()),
        Place::New => ("new worktree from trunk".to_owned(), String::new()),
    };
    let mut places = vec![current_place.clone()];
    if current_place != Place::Root {
        places.push(Place::Root);
    }
    places.extend(
        trees
            .iter()
            .map(|tree| Place::Tree(tree.name.clone()))
            .filter(|place| *place != current_place),
    );
    if setup.trees.is_some() {
        places.push(Place::New);
    }
    places
        .into_iter()
        .map(|place| {
            let (label, detail) = describe(&place);
            (place, label, detail)
        })
        .collect()
}

pub(super) fn launch_args(profile: &str, place: &Place, name: &str, prompt: &str) -> Vec<String> {
    let mut args = vec!["agents".to_owned(), "launch".to_owned(), profile.to_owned()];
    match place {
        Place::Root => {}
        Place::Tree(name) => args.extend(["-w".to_owned(), name.clone()]),
        Place::Cwd(path) => args.extend(["--cwd".to_owned(), path.display().to_string()]),
        Place::New => {
            args.push("-w".to_owned());
            if !name.is_empty() {
                args.push(name.to_owned());
            }
            args.push("--from-trunk".to_owned());
        }
    }
    if !prompt.trim().is_empty() {
        args.push(prompt.to_owned());
    }
    args
}
