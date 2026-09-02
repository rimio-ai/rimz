use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use super::calls::{AssemblyGroup, Caller, FunctionRow};
use super::surface::SurfaceSection;

#[derive(Debug, Serialize)]
pub(super) struct InspectVerdict {
    escaping_items: usize,
    outside_production_sites: usize,
    head_items: usize,
    internal_only: usize,
    /// Items that can narrow, counted by the visibility they narrow to,
    /// most frequent first.
    narrowable: Vec<(String, usize)>,
    top_assembly_items: Vec<String>,
    top_assembly_callers: usize,
    heaviest_caller: Option<String>,
    heaviest_caller_items: usize,
    heaviest_caller_also: Vec<(String, usize)>,
    vestigial_candidates: usize,
    pins_fix: usize,
    one_caller_flags: usize,
    constant_parameters: usize,
}

impl InspectVerdict {
    pub(super) fn from_report_data(
        surface: &SurfaceSection,
        assembly: &[AssemblyGroup],
        callers: &[Caller],
        functions: &[FunctionRow],
        one_caller_flags: usize,
        constant_parameters: usize,
    ) -> Self {
        let top_assembly = assembly.first();
        let heaviest_caller = callers.first().and_then(|caller| caller.top_fns.first());
        let mut narrowable = BTreeMap::<&str, usize>::new();
        for row in surface.items.iter().filter(|row| row.narrow_to != "keep") {
            *narrowable.entry(&row.narrow_to).or_default() += 1;
        }
        let mut narrowable = narrowable
            .into_iter()
            .map(|(visibility, items)| (visibility.to_owned(), items))
            .collect::<Vec<_>>();
        narrowable.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Self {
            escaping_items: surface.items.len(),
            outside_production_sites: surface.outside_sites,
            head_items: surface.head_items,
            internal_only: surface.internal_only,
            narrowable,
            top_assembly_items: top_assembly.map_or_else(Vec::new, |group| group.items.clone()),
            top_assembly_callers: top_assembly.map_or(0, |group| group.functions.len()),
            heaviest_caller: heaviest_caller.map(|function| function.function.clone()),
            heaviest_caller_items: heaviest_caller.map_or(0, |function| function.items),
            heaviest_caller_also: heaviest_caller
                .and_then(|caller| {
                    functions.iter().find(|function| {
                        function.function == caller.function
                            && function.path == caller.path
                            && function.line == caller.line
                    })
                })
                .map_or_else(Vec::new, |function| function.also.clone()),
            vestigial_candidates: surface.vestigial.len(),
            pins_fix: surface
                .vestigial
                .iter()
                .filter(|item| item.pins_fix)
                .count(),
            one_caller_flags,
            constant_parameters,
        }
    }
}

pub(super) fn render_verdict(out: &mut String, verdict: &InspectVerdict) {
    writeln!(
        out,
        "# Verdict\n\n{} escaping items, {} outside production sites; {} items carry 80%",
        verdict.escaping_items, verdict.outside_production_sites, verdict.head_items
    )
    .expect("writing to a String cannot fail");
    let narrowable = verdict
        .narrowable
        .iter()
        .map(|(visibility, items)| format!("{visibility} {items}"))
        .collect::<Vec<_>>();
    writeln!(
        out,
        "{} items only the module itself reaches; {} items can narrow{}",
        verdict.internal_only,
        verdict
            .narrowable
            .iter()
            .map(|(_, items)| items)
            .sum::<usize>(),
        if narrowable.is_empty() {
            String::new()
        } else {
            format!(" ({})", narrowable.join(", "))
        }
    )
    .expect("writing to a String cannot fail");
    if verdict.top_assembly_items.is_empty() {
        out.push_str("top assembly cluster: none\n");
    } else {
        writeln!(
            out,
            "top assembly cluster: {} across {} callers",
            verdict.top_assembly_items.join(" + "),
            verdict.top_assembly_callers
        )
        .expect("writing to a String cannot fail");
    }
    if let Some(function) = &verdict.heaviest_caller {
        let mut also = verdict
            .heaviest_caller_also
            .iter()
            .take(3)
            .map(|(module, items)| format!("{module} {items}"))
            .collect::<Vec<_>>();
        if verdict.heaviest_caller_also.len() > 3 {
            also.push(format!("+{}", verdict.heaviest_caller_also.len() - 3));
        }
        let also = also.join(", ");
        writeln!(
            out,
            "heaviest caller: {function} at {} items{}",
            verdict.heaviest_caller_items,
            if also.is_empty() {
                String::new()
            } else {
                format!(" (also wires {also})")
            }
        )
        .expect("writing to a String cannot fail");
    } else {
        out.push_str("heaviest caller: none\n");
    }
    writeln!(
        out,
        "{} items without production sites, {} pin a fix",
        verdict.vestigial_candidates, verdict.pins_fix
    )
    .expect("writing to a String cannot fail");
    writeln!(
        out,
        "{} one-caller flags, {} constant parameters",
        verdict.one_caller_flags, verdict.constant_parameters
    )
    .expect("writing to a String cannot fail");
}
