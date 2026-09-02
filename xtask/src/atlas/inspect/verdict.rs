use std::fmt::Write as _;

use serde::Serialize;

use super::calls::{AssemblyGroup, Caller};
use super::surface::SurfaceSection;

#[derive(Debug, Serialize)]
pub(super) struct InspectVerdict {
    escaping_items: usize,
    outside_production_sites: usize,
    head_items: usize,
    internal_only: usize,
    top_assembly_items: Vec<String>,
    top_assembly_callers: usize,
    heaviest_caller: Option<String>,
    heaviest_caller_items: usize,
    vestigial_candidates: usize,
    pins_fix: usize,
}

impl InspectVerdict {
    pub(super) fn from_report_data(
        surface: &SurfaceSection,
        assembly: &[AssemblyGroup],
        callers: &[Caller],
    ) -> Self {
        let top_assembly = assembly.iter().max_by_key(|group| group.functions.len());
        let heaviest_caller = callers.first().and_then(|caller| caller.top_fns.first());
        Self {
            escaping_items: surface.items.len(),
            outside_production_sites: surface.outside_sites,
            head_items: surface.head_items,
            internal_only: surface.internal_only,
            top_assembly_items: top_assembly.map_or_else(Vec::new, |group| group.items.clone()),
            top_assembly_callers: top_assembly.map_or(0, |group| group.functions.len()),
            heaviest_caller: heaviest_caller.map(|function| function.function.clone()),
            heaviest_caller_items: heaviest_caller.map_or(0, |function| function.items),
            vestigial_candidates: surface.vestigial.len(),
            pins_fix: surface
                .vestigial
                .iter()
                .filter(|item| item.pins_fix)
                .count(),
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
    writeln!(
        out,
        "{} items only the module itself reaches (narrow to pub(super)/pub(crate): see surface)",
        verdict.internal_only
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
        writeln!(
            out,
            "heaviest caller: {function} at {} items",
            verdict.heaviest_caller_items
        )
        .expect("writing to a String cannot fail");
    } else {
        out.push_str("heaviest caller: none\n");
    }
    writeln!(
        out,
        "{} vestigial candidates, {} pin a fix",
        verdict.vestigial_candidates, verdict.pins_fix
    )
    .expect("writing to a String cannot fail");
}
