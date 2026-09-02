use std::collections::BTreeSet;
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
    pub(super) score: f64,
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
    let (_, clusters) = clusters(facts, scope, 40, 0.35);
    merge_families(clusters)
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
            ShapeFamily {
                name,
                score: files as f64 * mean_sloc,
                members,
                files,
                mean_sloc,
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
