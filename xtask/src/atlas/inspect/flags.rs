use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use super::super::facts::Facts;
use super::super::modules::module_is_within;
use super::super::references::EdgeKind;
use super::super::syntax::{CallSite, FnBody, FnParam};
use super::selector::ModuleSelector;
use super::surface::SurfaceSection;

#[derive(Clone, Debug, Serialize)]
pub(super) struct FlagRow {
    pub(super) function: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) param: String,
    pub(super) ty: String,
    pub(super) values: Vec<(String, usize, Vec<String>)>,
    pub(super) finding: &'static str,
    pub(super) finding_value: String,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct FlagSection {
    pub(super) rows: Vec<FlagRow>,
    pub(super) skipped_sites: usize,
    #[serde(skip)]
    pub(super) flag_functions: usize,
}

impl FlagSection {
    pub(super) fn one_caller_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.finding == "one-caller")
            .count()
    }

    pub(super) fn constant_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.finding == "constant")
            .count()
    }
}

#[derive(Default)]
struct ValueSites {
    sites: usize,
    callers: BTreeSet<String>,
}

struct JoinedSite<'a> {
    call: &'a CallSite,
    caller: String,
}

pub(super) fn flag_section(
    facts: &Facts,
    target: &ModuleSelector,
    surface: &SurfaceSection,
) -> FlagSection {
    let enum_names = facts
        .syntax
        .files
        .iter()
        .flat_map(|file| &file.pub_items)
        .filter(|item| item.kind == "enum")
        .map(|item| item.name.as_str())
        .collect::<BTreeSet<_>>();
    let references = facts
        .references
        .as_ref()
        .expect("inspect loads exact references");
    let mut section = FlagSection::default();
    let mut ranked = Vec::<(usize, FlagRow)>::new();

    for row in surface
        .items
        .iter()
        .filter(|row| matches!(row.kind.as_str(), "fn" | "method"))
    {
        let Some(file) = facts.syntax.files.iter().find(|file| file.path == row.path) else {
            continue;
        };
        let Some(function) = file.fns.iter().find(|function| {
            function.name == row.name && function.line <= row.line && row.line <= function.end_line
        }) else {
            continue;
        };
        let params = function
            .params
            .iter()
            .filter(|param| flag_like(&param.ty, &enum_names))
            .collect::<Vec<_>>();
        if params.is_empty() {
            continue;
        }
        section.flag_functions += 1;

        let mut sites = Vec::new();
        for edge in references.edges.iter().filter(|edge| {
            edge.kind == EdgeKind::Reference
                && !edge.test
                && !module_is_within(&edge.from, &target.module)
                && edge.to_path == row.path
                && edge.to_line == row.line
                && edge.item == function.name
        }) {
            let Some(caller_file) = facts
                .syntax
                .files
                .iter()
                .find(|file| file.path == edge.from_path)
            else {
                section.skipped_sites += 1;
                continue;
            };
            let Some(call) = caller_file
                .calls
                .iter()
                .find(|call| call.line == edge.from_line && call.callee == function.name)
            else {
                section.skipped_sites += 1;
                continue;
            };
            sites.push(JoinedSite {
                call,
                caller: edge
                    .from_fn
                    .as_ref()
                    .map_or_else(|| edge.from.clone(), |function| function.label.clone()),
            });
        }

        for param in params {
            if let Some(row) = finding(function, param, &sites) {
                ranked.push((sites.len(), row));
            }
        }
    }

    ranked.sort_by(|(left_sites, left), (right_sites, right)| {
        finding_rank(left.finding)
            .cmp(&finding_rank(right.finding))
            .then_with(|| right_sites.cmp(left_sites))
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.param.cmp(&right.param))
    });
    section.rows = ranked.into_iter().map(|(_, row)| row).collect();
    section
}

fn flag_like(ty: &str, enum_names: &BTreeSet<&str>) -> bool {
    if ty == "bool" {
        return true;
    }
    let head = ty.split('<').next().unwrap_or(ty);
    let last = head.rsplit("::").next().unwrap_or(head);
    (last == "Option" && ty.contains('<')) || enum_names.contains(last)
}

fn finding(function: &FnBody, param: &FnParam, sites: &[JoinedSite<'_>]) -> Option<FlagRow> {
    let mut values = BTreeMap::<String, ValueSites>::new();
    let mut unknown = 0;
    for site in sites {
        let shape = site
            .call
            .args
            .get(param.index)
            .map_or("_", |shape| shape.label());
        if shape == "_" {
            unknown += 1;
            continue;
        }
        let aggregate = values.entry(shape.to_owned()).or_default();
        aggregate.sites += 1;
        aggregate.callers.insert(site.caller.clone());
    }
    let shaped_sites = values.values().map(|value| value.sites).sum::<usize>();
    let singleton = (values.len() >= 2)
        .then(|| {
            values
                .iter()
                .find(|(_, aggregate)| aggregate.sites == 1)
                .map(|(value, _)| value.clone())
        })
        .flatten();
    let (finding, finding_value) = if let Some(value) = singleton {
        ("one-caller", value)
    } else if values.len() == 1 && shaped_sites >= 2 && unknown == 0 {
        (
            "constant",
            values
                .first_key_value()
                .expect("one shaped value was established")
                .0
                .clone(),
        )
    } else {
        return None;
    };
    let mut values = values
        .into_iter()
        .map(|(value, aggregate)| {
            (
                value,
                aggregate.sites,
                aggregate.callers.into_iter().collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    Some(FlagRow {
        function: function.label(),
        path: function.path.clone(),
        line: function.line,
        param: param.name.clone(),
        ty: param.ty.clone(),
        values,
        finding,
        finding_value,
    })
}

fn finding_rank(finding: &str) -> usize {
    usize::from(finding != "one-caller")
}

pub(super) fn render_flags(out: &mut String, section: &FlagSection, top: usize) {
    out.push_str("\n# Flags\n\n");
    out.push_str("| function | param | type | finding | value | sites | callers |\n");
    out.push_str("|---|---|---|---|---|---:|---|\n");
    for row in section.rows.iter().take(top) {
        let (_, sites, callers) = row
            .values
            .iter()
            .find(|(value, _, _)| value == &row.finding_value)
            .expect("the finding value belongs to the row's values");
        writeln!(
            out,
            "| `{}` | `{}` | `{}` | {} | `{}` | {} | {} |",
            row.function,
            row.param,
            row.ty,
            row.finding,
            row.finding_value,
            sites,
            bounded_callers(callers)
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "\nskipped {} sites (no call site on the reference line)",
        section.skipped_sites
    )
    .expect("writing to a String cannot fail");
}

fn bounded_callers(callers: &[String]) -> String {
    let mut rendered = callers
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if callers.len() > 3 {
        write!(rendered, " +{}", callers.len() - 3).expect("writing to a String cannot fail");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use scip::types::Index;

    use super::super::super::facts::{Facets, Facts};
    use super::super::super::references::References;
    use super::super::testkit::{crate_with_files, occurrence, selector};
    use super::*;

    #[test]
    fn finds_one_caller_constants_and_ignores_binding_only_params() {
        let root = crate_with_files(&[
            ("src/lib.rs", "mod target;\nmod caller;\n"),
            (
                "src/target.rs",
                "pub enum Mode { Fast }\npub fn deliver(x: u32, steer: bool) {}\npub fn open(mode: Mode) {}\npub fn bound(steer: bool) {}\n",
            ),
            (
                "src/caller.rs",
                "fn first(a: u32, steer: bool) {\n    crate::target::deliver(a, false);\n    crate::target::open(crate::target::Mode::Fast);\n    crate::target::bound(steer);\n}\nfn second(b: u32) {\n    crate::target::deliver(b, false);\n    crate::target::open(crate::target::Mode::Fast);\n}\nfn rare(c: u32) {\n    crate::target::deliver(c, true);\n    crate::target::open(crate::target::Mode::Fast);\n    let _handler = crate::target::deliver;\n}\n",
            ),
        ]);
        let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
        let mode = "rust-analyzer cargo probe 0.0.0 Mode#";
        let deliver = "rust-analyzer cargo probe 0.0.0 deliver().";
        let open = "rust-analyzer cargo probe 0.0.0 open().";
        let bound = "rust-analyzer cargo probe 0.0.0 bound().";
        let index = Index {
            documents: vec![
                scip::types::Document {
                    relative_path: "src/target.rs".to_owned(),
                    occurrences: vec![
                        occurrence(0, mode, true),
                        occurrence(1, deliver, true),
                        occurrence(2, open, true),
                        occurrence(3, bound, true),
                    ],
                    ..scip::types::Document::default()
                },
                scip::types::Document {
                    relative_path: "src/caller.rs".to_owned(),
                    occurrences: vec![
                        occurrence(1, deliver, false),
                        occurrence(2, open, false),
                        occurrence(3, bound, false),
                        occurrence(6, deliver, false),
                        occurrence(7, open, false),
                        occurrence(10, deliver, false),
                        occurrence(11, open, false),
                        occurrence(12, deliver, false),
                    ],
                    ..scip::types::Document::default()
                },
            ],
            ..Index::default()
        };
        let index_path = root.path().join("index.scip");
        scip::write_message_to_file(&index_path, index).unwrap();
        facts.references =
            Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());
        let module = selector("target");
        let (surface, _) = super::super::surface::surface_section(&facts, &module);

        let flags = flag_section(&facts, &module, &surface);

        assert_eq!(flags.flag_functions, 3);
        assert_eq!(flags.skipped_sites, 1);
        assert_eq!(flags.rows.len(), 2);
        assert_eq!(
            (
                flags.rows[0].function.as_str(),
                flags.rows[0].param.as_str(),
                flags.rows[0].finding,
                flags.rows[0].finding_value.as_str(),
            ),
            ("deliver", "steer", "one-caller", "true")
        );
        assert_eq!(
            flags.rows[0].values,
            [
                (
                    "false".to_owned(),
                    2,
                    vec!["first".to_owned(), "second".to_owned()]
                ),
                ("true".to_owned(), 1, vec!["rare".to_owned()]),
            ]
        );
        assert_eq!(
            (
                flags.rows[1].function.as_str(),
                flags.rows[1].param.as_str(),
                flags.rows[1].finding,
                flags.rows[1].finding_value.as_str(),
            ),
            ("open", "mode", "constant", "crate::target::Mode::Fast")
        );
        assert!(!flags.rows.iter().any(|row| row.function == "bound"));
    }
}
