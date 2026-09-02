use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::facts::Facts;
use super::modules::{module_for_path, path_in_scope};
use super::syntax::FnBody;

const MIN_SHARED_CALLEES: usize = 3;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Member {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) name: String,
    pub(super) sloc: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Cluster {
    similarity_floor: f64,
    mean_sloc: f64,
    score: f64,
    distinct_files: usize,
    distinct_modules: usize,
    shared_callees: Vec<String>,
    members: Vec<Member>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ShapeFamily {
    pub(super) name: String,
    pub(super) members: Vec<Member>,
    pub(super) files: usize,
    pub(super) mean_sloc: f64,
    pub(super) sloc_in_play: f64,
    pub(super) score: f64,
    /// Distinct member files that occupy the same role in sibling
    /// directories (`agents/adapters/*/spend.rs`): parallel implementations
    /// of one responsibility, the strongest duplication signal.
    pub(super) siblings: usize,
    /// The sibling role pattern behind `siblings`, when there is one.
    pub(super) role: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(super) struct FamilyDrops {
    pub(super) vocabulary: usize,
    pub(super) below_gate: usize,
}

fn clusters(
    facts: &Facts,
    scope: &Path,
    min_sloc: usize,
    similarity: f64,
) -> (usize, Vec<Cluster>) {
    let functions = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .flat_map(|file| &file.fns)
        .filter(|function| function.sloc >= min_sloc)
        .cloned()
        .collect::<Vec<_>>();
    let callees = functions
        .iter()
        .map(|function| callee_set(&function.callees))
        .collect::<Vec<_>>();
    let mut similarities = vec![vec![0.0; functions.len()]; functions.len()];
    for left in 0..functions.len() {
        for right in left + 1..functions.len() {
            let similarity = callee_similarity(&callees[left], &callees[right]);
            similarities[left][right] = similarity;
            similarities[right][left] = similarity;
        }
    }
    let mut clusters = complete_linkage_groups(&similarities, similarity)
        .into_iter()
        .filter(|members| members.len() > 1)
        .map(|members| cluster(&functions, &callees, members, scope))
        .collect::<Vec<_>>();
    sort_clusters(&mut clusters);
    (functions.len(), clusters)
}

pub(super) fn families(facts: &Facts, scope: &Path) -> Vec<ShapeFamily> {
    families_with_dropped(facts, scope).0
}

/// Shape families keyed by crate vocabulary, ranked sibling roles first,
/// beside the count of families dropped because their key named only std
/// or external vocabulary.
pub(super) fn families_with_dropped(
    facts: &Facts,
    scope: &Path,
) -> (Vec<ShapeFamily>, FamilyDrops) {
    family_report(facts, scope, false)
}

/// Every candidate family, including std vocabulary and families below the
/// finding gate, alongside the counts the default report omits.
pub(super) fn families_all_with_dropped(
    facts: &Facts,
    scope: &Path,
) -> (Vec<ShapeFamily>, FamilyDrops) {
    family_report(facts, scope, true)
}

fn family_report(
    facts: &Facts,
    scope: &Path,
    include_all: bool,
) -> (Vec<ShapeFamily>, FamilyDrops) {
    let (_, clusters) = clusters(facts, scope, 40, 0.35);
    let all = merge_families(clusters);
    let mut drops = FamilyDrops::default();
    let mut families = all
        .into_iter()
        .filter(|family| {
            let crate_vocabulary = family
                .name
                .split('+')
                .any(|callee| is_crate_vocabulary(callee, &facts.defined_names));
            if !crate_vocabulary {
                drops.vocabulary += 1;
            } else if !passes_finding_gate(family) {
                drops.below_gate += 1;
            }
            include_all || (crate_vocabulary && passes_finding_gate(family))
        })
        .collect::<Vec<_>>();
    sort_families(&mut families);
    (families, drops)
}

fn passes_finding_gate(family: &ShapeFamily) -> bool {
    family.siblings >= 2 || (family.files >= 3 && family.mean_sloc >= 40.0)
}

fn sort_families(families: &mut [ShapeFamily]) {
    families.sort_by(|left, right| {
        right
            .siblings
            .cmp(&left.siblings)
            .then_with(|| right.sloc_in_play.total_cmp(&left.sloc_in_play))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// Whether one family-key callee names something the crate defines: a
/// crate type or module on the path, or a crate function that is not a std
/// trait method. `Vec::new` and `Line::from` are vocabulary; `Store::open`
/// and `decode_catalog_hook` are knowledge.
fn is_crate_vocabulary(callee: &str, defined_names: &BTreeSet<String>) -> bool {
    let segments = callee.split("::").collect::<Vec<_>>();
    let Some((last, prefix)) = segments.split_last() else {
        return false;
    };
    if segments
        .first()
        .is_some_and(|root| STD_ROOTS.contains(root))
    {
        return false;
    }
    if prefix
        .iter()
        .any(|segment| defined_names.contains(*segment))
    {
        return true;
    }
    !STD_METHODS.contains(last) && defined_names.contains(*last)
}

/// Path roots that mark a callee as std or well-known external vocabulary
/// regardless of what the crate happens to define.
const STD_ROOTS: &[&str] = &[
    "std",
    "core",
    "alloc",
    "Vec",
    "String",
    "Option",
    "Result",
    "Box",
    "Arc",
    "Rc",
    "Mutex",
    "RwLock",
    "BTreeMap",
    "BTreeSet",
    "HashMap",
    "HashSet",
    "VecDeque",
    "Path",
    "PathBuf",
    "Command",
    "Stdio",
    "Instant",
    "Duration",
    "SystemTime",
    "Some",
    "Ok",
    "Err",
    "io",
    "fs",
    "env",
    "fmt",
    "iter",
    "mem",
    "ptr",
    "thread",
    "process",
    "Default",
    "From",
    "Into",
    "TryFrom",
    "Iterator",
    "NonZeroU16",
    "NonZeroU32",
    "NonZeroUsize",
    "usize",
    "u8",
    "u16",
    "u32",
    "u64",
    "i32",
    "i64",
    "f32",
    "f64",
    "char",
    "str",
    "serde_json",
    "toml",
    "json",
    "E",
];

/// Method names the crate defines for its own types (trait impls, process
/// and terminal helpers) that carry no domain knowledge in a call shape.
const STD_METHODS: &[&str] = &[
    "new",
    "default",
    "from",
    "into",
    "try_from",
    "try_into",
    "from_str",
    "fmt",
    "clone",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "hash",
    "drop",
    "deref",
    "deref_mut",
    "as_ref",
    "as_mut",
    "borrow",
    "to_string",
    "to_owned",
    "next",
    "serialize",
    "deserialize",
    "deserialize_any",
    "visit_str",
    "custom",
    "write_str",
    "write_fmt",
    "index",
    "add",
    "sub",
    "mul",
    "div",
    "entry",
    "zip",
    "min",
    "max",
    "saturating_sub",
    "saturating_add",
    "push_str",
    "style",
    "fg",
    "bg",
    "spawn",
    "kill",
    "wait",
    "try_wait",
    "write_all",
    "current_dir",
    "stdin",
    "stdout",
    "stderr",
    "success",
    "status",
];

/// The sibling role shared by the most member files: the path pattern with
/// one directory component wildcarded, over distinct member paths.
fn sibling_role(members: &[Member]) -> (usize, Option<String>) {
    let paths = members
        .iter()
        .map(|member| member.path.as_path())
        .collect::<BTreeSet<_>>();
    let mut patterns = BTreeMap::<String, BTreeSet<&Path>>::new();
    for path in &paths {
        let components = path
            .iter()
            .map(|component| component.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for index in 0..components.len().saturating_sub(1) {
            let mut pattern = components.clone();
            pattern[index] = "*".to_owned();
            patterns.entry(pattern.join("/")).or_default().insert(path);
        }
    }
    patterns
        .into_iter()
        .map(|(pattern, paths)| (paths.len(), pattern))
        .filter(|(count, _)| *count >= 2)
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .map_or((0, None), |(count, pattern)| (count, Some(pattern)))
}

fn merge_families(clusters: Vec<Cluster>) -> Vec<ShapeFamily> {
    let mut groups = (0..clusters.len())
        .map(|index| vec![index])
        .collect::<Vec<_>>();
    let mut left = 0;
    while left < groups.len() {
        let mut right = left + 1;
        while right < groups.len() {
            let related = groups[left].iter().any(|left| {
                groups[right]
                    .iter()
                    .any(|right| clusters_related(&clusters[*left], &clusters[*right]))
            });
            if related {
                let merged = groups.remove(right);
                groups[left].extend(merged);
            } else {
                right += 1;
            }
        }
        left += 1;
    }
    let mut families = groups
        .into_iter()
        .map(|indexes| {
            let mut members = indexes
                .iter()
                .flat_map(|index| clusters[*index].members.iter().cloned())
                .collect::<Vec<_>>();
            members.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.line.cmp(&right.line))
                    .then_with(|| left.name.cmp(&right.name))
            });
            members.dedup_by(|left, right| {
                left.path == right.path && left.line == right.line && left.name == right.name
            });
            let name = family_key(&clusters, &indexes);
            let files = members
                .iter()
                .map(|member| &member.path)
                .collect::<BTreeSet<_>>()
                .len();
            let mean_sloc = members.iter().map(|member| member.sloc).sum::<usize>() as f64
                / members.len().max(1) as f64;
            let (siblings, role) = sibling_role(&members);
            ShapeFamily {
                name,
                score: files as f64 * mean_sloc,
                sloc_in_play: members.len() as f64 * mean_sloc,
                members,
                files,
                mean_sloc,
                siblings,
                role,
            }
        })
        .collect::<Vec<_>>();
    families.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.name.cmp(&right.name))
    });
    families
}

fn family_key(clusters: &[Cluster], indexes: &[usize]) -> String {
    let Some(first) = indexes.first() else {
        return "shape".to_owned();
    };
    let mut shared = clusters[*first]
        .shared_callees
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for index in &indexes[1..] {
        let callees = clusters[*index]
            .shared_callees
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        shared = shared.intersection(&callees).cloned().collect();
    }
    let key_callees = if shared.is_empty() {
        let mut largest = *first;
        for index in &indexes[1..] {
            if clusters[*index].members.len() > clusters[largest].members.len() {
                largest = *index;
            }
        }
        clusters[largest]
            .shared_callees
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
    } else {
        shared
    };
    let name = key_callees
        .iter()
        .map(|callee| callee.strip_prefix('.').unwrap_or(callee))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("+");
    if name.is_empty() {
        "shape".to_owned()
    } else {
        name
    }
}

fn clusters_related(left: &Cluster, right: &Cluster) -> bool {
    let left_names = left
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect::<BTreeSet<_>>();
    let right_names = right
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect::<BTreeSet<_>>();
    !left_names.is_disjoint(&right_names)
        || left
            .shared_callees
            .iter()
            .filter(|callee| right.shared_callees.contains(callee))
            .take(MIN_SHARED_CALLEES)
            .count()
            >= MIN_SHARED_CALLEES
}

fn sort_clusters(clusters: &mut [Cluster]) {
    clusters.sort_by(|left, right| {
        right
            .distinct_modules
            .cmp(&left.distinct_modules)
            .then_with(|| right.distinct_files.cmp(&left.distinct_files))
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.members[0].path.cmp(&right.members[0].path))
            .then_with(|| left.members[0].line.cmp(&right.members[0].line))
    });
}

fn cluster(
    functions: &[FnBody],
    callees: &[BTreeSet<String>],
    indexes: Vec<usize>,
    scope: &Path,
) -> Cluster {
    let mean_sloc = indexes
        .iter()
        .map(|index| functions[*index].sloc)
        .sum::<usize>() as f64
        / indexes.len() as f64;
    let similarity_floor = indexes
        .iter()
        .enumerate()
        .flat_map(|(position, left)| {
            indexes[position + 1..]
                .iter()
                .map(move |right| callee_similarity(&callees[*left], &callees[*right]))
        })
        .fold(1.0_f64, f64::min);
    let shared_callees = indexes
        .iter()
        .skip(1)
        .fold(callees[indexes[0]].clone(), |shared, index| {
            shared.intersection(&callees[*index]).cloned().collect()
        })
        .into_iter()
        .collect();
    let mut members = indexes
        .into_iter()
        .map(|index| {
            let function = &functions[index];
            Member {
                path: function.path.clone(),
                line: function.line,
                name: function.name.clone(),
                sloc: function.sloc,
            }
        })
        .collect::<Vec<_>>();
    members.sort_by(|left, right| {
        right
            .sloc
            .cmp(&left.sloc)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    let distinct_files = members
        .iter()
        .map(|member| &member.path)
        .collect::<BTreeSet<_>>()
        .len();
    let distinct_modules = members
        .iter()
        .map(|member| module_for_path(&member.path, scope))
        .collect::<BTreeSet<_>>()
        .len();
    Cluster {
        similarity_floor,
        mean_sloc,
        score: members.len() as f64 * mean_sloc,
        distinct_files,
        distinct_modules,
        shared_callees,
        members,
    }
}

fn callee_set(callees: &[String]) -> BTreeSet<String> {
    callees
        .iter()
        .filter(|callee| !is_generic_callee(callee))
        .cloned()
        .collect()
}

fn is_generic_callee(callee: &str) -> bool {
    const GENERIC_METHODS: &[&str] = &[
        ".all",
        ".and_then",
        ".any",
        ".as_deref",
        ".as_mut",
        ".as_ref",
        ".as_slice",
        ".as_str",
        ".clone",
        ".cloned",
        ".cmp",
        ".collect",
        ".context",
        ".copied",
        ".dedup",
        ".display",
        ".enumerate",
        ".expect",
        ".extend",
        ".filter",
        ".filter_map",
        ".find",
        ".file_name",
        ".first",
        ".flat_map",
        ".flatten",
        ".get",
        ".get_mut",
        ".insert",
        ".into_owned",
        ".into_iter",
        ".is_empty",
        ".is_err",
        ".is_none",
        ".is_none_or",
        ".is_ok",
        ".is_ok_and",
        ".is_some",
        ".is_some_and",
        ".iter",
        ".iter_mut",
        ".join",
        ".last",
        ".len",
        ".lines",
        ".lock",
        ".map",
        ".map_err",
        ".map_or",
        ".map_or_else",
        ".next",
        ".ok",
        ".ok_or_else",
        ".or",
        ".or_else",
        ".or_default",
        ".push",
        ".sort",
        ".sort_by",
        ".sort_by_key",
        ".split",
        ".starts_with",
        ".take",
        ".then",
        ".then_some",
        ".to_owned",
        ".to_path_buf",
        ".to_string",
        ".to_string_lossy",
        ".transpose",
        ".trim",
        ".unwrap",
        ".unwrap_or",
        ".unwrap_or_default",
        ".unwrap_or_else",
        ".with_context",
    ];
    matches!(callee, "Ok" | "Err" | "Some" | "None" | "drop")
        || callee.ends_with("::out")
        || callee.ends_with("::paint")
        || callee.split("::").any(|segment| segment == "palette")
        || GENERIC_METHODS.contains(&callee)
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

fn callee_similarity(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.intersection(right).count() < MIN_SHARED_CALLEES {
        0.0
    } else {
        jaccard(left, right)
    }
}

fn complete_linkage_groups(similarities: &[Vec<f64>], threshold: f64) -> Vec<Vec<usize>> {
    let mut pairs = (0..similarities.len())
        .flat_map(|left| {
            (left + 1..similarities.len())
                .map(move |right| (similarities[left][right], left, right))
        })
        .filter(|(similarity, _, _)| *similarity >= threshold)
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut groups = (0..similarities.len())
        .map(|index| vec![index])
        .collect::<Vec<_>>();
    for (_, left, right) in pairs {
        let left_group = groups
            .iter()
            .position(|group| group.contains(&left))
            .expect("each function remains in exactly one complete-linkage group");
        let right_group = groups
            .iter()
            .position(|group| group.contains(&right))
            .expect("each function remains in exactly one complete-linkage group");
        if left_group == right_group
            || !groups[left_group].iter().all(|left| {
                groups[right_group]
                    .iter()
                    .all(|right| similarities[*left][*right] >= threshold)
            })
        {
            continue;
        }
        let (keep, remove) = if left_group < right_group {
            (left_group, right_group)
        } else {
            (right_group, left_group)
        };
        let removed = groups.remove(remove);
        groups[keep].extend(removed);
        groups[keep].sort_unstable();
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn jaccard_scores_shape_similarity() {
        assert_eq!(jaccard(&set(&["open"]), &set(&["open"])), 1.0);
        assert_eq!(jaccard(&set(&["open", "load"]), &set(&["open"])), 0.5);
    }

    #[test]
    fn callee_sets_drop_generic_method_noise() {
        let callees = [
            "domain::load",
            ".map",
            ".context",
            "Some",
            ".render",
            "render::out",
            "render::paint",
            "render::palette::accent",
            "ui::out",
            "ui::paint",
            "ui::palette::accent",
            "crate::cli::render::out",
            "super::render::paint",
            "palette::muted",
        ]
        .map(str::to_owned);

        assert_eq!(callee_set(&callees), set(&["domain::load", ".render"]));
    }

    #[test]
    fn choreography_requires_three_shared_callees() {
        assert_eq!(
            callee_similarity(&set(&["load", "resolve"]), &set(&["load", "resolve"])),
            0.0
        );
        assert_eq!(
            callee_similarity(
                &set(&["load", "resolve", "launch"]),
                &set(&["load", "resolve", "launch"])
            ),
            1.0
        );
    }

    #[test]
    fn complete_linkage_does_not_chain_dissimilar_members() {
        let similarities = vec![
            vec![1.0, 0.8, 0.6],
            vec![0.8, 1.0, 0.8],
            vec![0.6, 0.8, 1.0],
        ];

        let groups = complete_linkage_groups(&similarities, 0.7);

        assert_eq!(groups, [vec![0, 1], vec![2]]);
        assert!(groups.iter().all(|group| {
            group.iter().enumerate().all(|(position, left)| {
                group[position + 1..]
                    .iter()
                    .all(|right| similarities[*left][*right] >= 0.7)
            })
        }));
    }

    #[test]
    fn cross_module_clusters_rank_before_larger_single_file_clusters() {
        let function = |path: &str, line, sloc| FnBody {
            name: format!("function_{line}"),
            owner: None,
            path: PathBuf::from(path),
            line,
            end_line: line + sloc - 1,
            sloc,
            callees: ["prepare", "resolve", "launch"].map(str::to_owned).to_vec(),
            forwards: None,
        };
        let functions = vec![
            function("src/fixture.rs", 1, 100),
            function("src/fixture.rs", 2, 100),
            function("src/fixture.rs", 3, 100),
            function("src/fixture.rs", 4, 100),
            function("src/fixture.rs", 5, 100),
            function("src/a/one.rs", 1, 50),
            function("src/b/two.rs", 1, 50),
            function("src/c/three.rs", 1, 50),
        ];
        let callees = functions
            .iter()
            .map(|function| callee_set(&function.callees))
            .collect::<Vec<_>>();
        let mut clusters = vec![
            cluster(&functions, &callees, vec![0, 1, 2, 3, 4], Path::new("src")),
            cluster(&functions, &callees, vec![5, 6, 7], Path::new("src")),
        ];

        sort_clusters(&mut clusters);

        assert_eq!(clusters[0].distinct_modules, 3);
        assert_eq!(clusters[0].distinct_files, 3);
        assert_eq!(clusters[1].score, 500.0);
    }

    #[test]
    fn families_merge_clusters_sharing_a_member_name() {
        let member = |path: &str, line, name: &str| Member {
            path: PathBuf::from(path),
            line,
            name: name.to_owned(),
            sloc: 50,
        };
        let cluster = |members: Vec<Member>, shared_callees: &[&str]| Cluster {
            similarity_floor: 0.5,
            mean_sloc: 50.0,
            score: members.len() as f64 * 50.0,
            distinct_files: members.len(),
            distinct_modules: members.len(),
            shared_callees: shared_callees
                .iter()
                .map(|callee| (*callee).to_owned())
                .collect(),
            members,
        };
        let clusters = vec![
            cluster(
                vec![
                    member("src/a.rs", 1, "decode_hook"),
                    member("src/b.rs", 1, "decode_a"),
                ],
                &[".decode", "load_a", "parse_a"],
            ),
            cluster(
                vec![
                    member("src/c.rs", 1, "decode_hook"),
                    member("src/d.rs", 1, "decode_b"),
                ],
                &[".decode", "load_b", "parse_b"],
            ),
        ];

        let families = merge_families(clusters);

        assert_eq!(families.len(), 1);
        assert_eq!(families[0].name, "decode");
        assert_eq!(families[0].members.len(), 4);
        assert_eq!(families[0].files, 4);
        assert_eq!(families[0].score, 200.0);
    }

    #[test]
    fn crate_vocabulary_needs_a_crate_defined_type_module_or_function() {
        let defined = [
            "Store",
            "open",
            "render",
            "decode_catalog_hook",
            "new",
            "from",
            "min",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert!(is_crate_vocabulary("Store::open", &defined));
        assert!(is_crate_vocabulary("render::Table::new", &defined));
        assert!(is_crate_vocabulary("decode_catalog_hook", &defined));
        assert!(!is_crate_vocabulary("Vec::new", &defined));
        assert!(!is_crate_vocabulary("Line::from", &defined));
        assert!(!is_crate_vocabulary("min", &defined));
        assert!(!is_crate_vocabulary("E::custom", &defined));
    }

    #[test]
    fn sibling_roles_wildcard_one_directory_over_distinct_paths() {
        let member = |path: &str| Member {
            path: PathBuf::from(path),
            line: 1,
            name: "load".to_owned(),
            sloc: 50,
        };
        let members = vec![
            member("src/agents/adapters/claude/spend.rs"),
            member("src/agents/adapters/claude/spend.rs"),
            member("src/agents/adapters/codex/spend.rs"),
            member("src/agents/adapters/kimi/spend.rs"),
            member("src/cli/stats.rs"),
        ];

        assert_eq!(
            sibling_role(&members),
            (3, Some("src/agents/adapters/*/spend.rs".to_owned()))
        );
        assert_eq!(
            sibling_role(&[member("src/a.rs"), member("src/b.rs")]),
            (0, None)
        );
    }

    #[test]
    fn finding_gate_requires_siblings_or_three_substantial_files() {
        let family = |files, mean_sloc, siblings| ShapeFamily {
            name: "shape".to_owned(),
            members: Vec::new(),
            files,
            mean_sloc,
            sloc_in_play: 0.0,
            score: 0.0,
            siblings,
            role: None,
        };

        assert!(passes_finding_gate(&family(1, 20.0, 2)));
        assert!(passes_finding_gate(&family(3, 40.0, 0)));
        assert!(!passes_finding_gate(&family(2, 100.0, 0)));
        assert!(!passes_finding_gate(&family(3, 39.9, 0)));
    }

    #[test]
    fn finding_families_rank_by_siblings_then_sloc_in_play() {
        let family = |name: &str, siblings, sloc_in_play| ShapeFamily {
            name: name.to_owned(),
            members: Vec::new(),
            files: 3,
            mean_sloc: 40.0,
            sloc_in_play,
            score: 0.0,
            siblings,
            role: None,
        };
        let mut families = vec![
            family("large", 2, 500.0),
            family("small", 3, 100.0),
            family("medium", 2, 300.0),
        ];

        sort_families(&mut families);

        assert_eq!(
            families
                .iter()
                .map(|family| family.name.as_str())
                .collect::<Vec<_>>(),
            ["small", "large", "medium"]
        );
    }

    #[test]
    fn family_key_falls_back_to_the_largest_clusters_callees() {
        let member = |path: &str, name: &str| Member {
            path: PathBuf::from(path),
            line: 1,
            name: name.to_owned(),
            sloc: 50,
        };
        let cluster = |members: Vec<Member>, shared_callees: &[&str]| Cluster {
            similarity_floor: 0.5,
            mean_sloc: 50.0,
            score: members.len() as f64 * 50.0,
            distinct_files: members.len(),
            distinct_modules: members.len(),
            shared_callees: shared_callees
                .iter()
                .map(|callee| (*callee).to_owned())
                .collect(),
            members,
        };
        let clusters = vec![
            cluster(
                vec![
                    member("src/a.rs", "walk"),
                    member("src/b.rs", "walk_a"),
                    member("src/c.rs", "walk_b"),
                ],
                &["read_dir", ".is_dir", ".file_name"],
            ),
            cluster(
                vec![member("src/d.rs", "walk"), member("src/e.rs", "walk_c")],
                &["load", "parse", "finish"],
            ),
        ];

        let families = merge_families(clusters);

        assert_eq!(families[0].name, "file_name+is_dir+read_dir");
    }
}
