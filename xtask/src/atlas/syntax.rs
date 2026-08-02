use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Serialize;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Expr, ExprCall, ExprMethodCall, File, ImplItem, ImplItemFn, Item, ItemFn, ItemMod, TraitItem,
    UseTree, Visibility,
};

use super::modules::crate_module_for_path;
use super::sources::Source;

#[derive(Clone, Debug, Serialize)]
pub(super) struct PubItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) params: Option<usize>,
    pub(super) line: usize,
    pub(super) declared: String,
    pub(super) reach: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ImportedItem {
    pub(super) module_path: String,
    pub(super) item: String,
    pub(super) line: usize,
    pub(super) internal: bool,
    #[serde(skip)]
    pub(super) leaf_may_be_module: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct FnBody {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) sloc: usize,
    pub(super) callees: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct FileSyntax {
    pub(super) path: PathBuf,
    pub(super) module_path: String,
    pub(super) pub_items: Vec<PubItem>,
    pub(super) mod_decls: Vec<(String, String)>,
    pub(super) test_regions: Vec<Range<usize>>,
    pub(super) imports: Vec<ImportedItem>,
    pub(super) fns: Vec<FnBody>,
}

#[derive(Debug)]
pub(super) struct SyntaxReport {
    pub(super) files: Vec<FileSyntax>,
    pub(super) parse_failures: Vec<PathBuf>,
}

#[derive(Debug)]
pub(super) struct ModIndex {
    declarations: BTreeMap<String, String>,
}

impl ModIndex {
    pub(super) fn new(files: &[FileSyntax]) -> Self {
        let mut declarations = BTreeMap::<String, String>::new();
        for (module, reach) in files.iter().flat_map(|file| &file.mod_decls) {
            declarations
                .entry(module.clone())
                .and_modify(|existing| {
                    *existing = narrower_reach(existing, reach, module);
                })
                .or_insert_with(|| reach.clone());
        }
        Self { declarations }
    }

    pub(super) fn effective_reach(&self, module: &str, declared_reach: &str) -> String {
        let mut effective = declared_reach.to_owned();
        let parts = module.split("::").collect::<Vec<_>>();
        for end in 1..=parts.len() {
            let ancestor = parts[..end].join("::");
            if let Some(reach) = self.declarations.get(&ancestor) {
                effective = narrower_reach(&effective, reach, module);
            }
        }
        effective
    }
}

pub(super) fn analyze_sources(sources: &[Source]) -> SyntaxReport {
    let mut files = Vec::new();
    let mut parse_failures = Vec::new();
    for source in sources {
        if !source.is_production() {
            continue;
        }
        match syn::parse_file(&source.text) {
            Ok(file) => files.push(analyze_file(&source.path, &file)),
            Err(_) => parse_failures.push(source.path.clone()),
        }
    }
    SyntaxReport {
        files,
        parse_failures,
    }
}

fn analyze_file(path: &Path, file: &File) -> FileSyntax {
    let module_path = crate_module_for_path(path);
    let mut pub_items = Vec::new();
    let mut mod_decls = Vec::new();
    collect_public_items(
        &file.items,
        &module_path,
        "",
        &mut pub_items,
        &mut mod_decls,
    );
    let mut test_regions = Vec::new();
    collect_test_regions(&file.items, &mut test_regions);

    let imports = {
        let mut use_collector = UseCollector {
            file_module: &module_path,
            imports: Vec::new(),
        };
        use_collector.visit_file(file);
        use_collector.imports
    };

    let mut fn_collector = FnCollector {
        path,
        functions: Vec::new(),
    };
    fn_collector.visit_file(file);

    FileSyntax {
        path: path.to_path_buf(),
        module_path,
        pub_items,
        mod_decls,
        test_regions,
        imports,
        fns: fn_collector.functions,
    }
}

fn collect_public_items(
    items: &[Item],
    module: &str,
    enclosing_reach: &str,
    output: &mut Vec<PubItem>,
    mod_decls: &mut Vec<(String, String)>,
) {
    for item in items {
        if let Item::Impl(item) = item {
            for method in &item.items {
                if let ImplItem::Fn(method) = method
                    && is_boundary_visible(&method.vis)
                {
                    output.push(PubItem {
                        module: module.to_owned(),
                        name: method.sig.ident.to_string(),
                        kind: "fn".to_owned(),
                        params: Some(method.sig.inputs.len()),
                        line: method.sig.ident.span().start().line,
                        declared: render_visibility(&method.vis),
                        reach: narrower_reach(
                            &visibility_reach(&method.vis, module),
                            enclosing_reach,
                            module,
                        ),
                    });
                }
            }
            continue;
        }
        if let Item::Trait(item) = item {
            if is_boundary_visible(&item.vis) {
                let trait_reach = narrower_reach(
                    &visibility_reach(&item.vis, module),
                    enclosing_reach,
                    module,
                );
                output.push(PubItem {
                    module: module.to_owned(),
                    name: item.ident.to_string(),
                    kind: "trait".to_owned(),
                    params: None,
                    line: item.ident.span().start().line,
                    declared: render_visibility(&item.vis),
                    reach: trait_reach.clone(),
                });
                for method in &item.items {
                    if let TraitItem::Fn(method) = method {
                        output.push(PubItem {
                            module: module.to_owned(),
                            name: method.sig.ident.to_string(),
                            kind: "fn".to_owned(),
                            params: Some(method.sig.inputs.len()),
                            line: method.sig.ident.span().start().line,
                            declared: "inherited".to_owned(),
                            reach: trait_reach.clone(),
                        });
                    }
                }
            }
            continue;
        }
        if let Item::Mod(item) = item {
            if is_cfg_test(&item.attrs) {
                continue;
            }
            let nested_module = join_module(module, &item.ident.to_string());
            let module_reach = narrower_reach(
                &visibility_reach(&item.vis, module),
                enclosing_reach,
                module,
            );
            mod_decls.push((nested_module.clone(), module_reach.clone()));
            if is_boundary_visible(&item.vis) {
                output.push(PubItem {
                    module: module.to_owned(),
                    name: item.ident.to_string(),
                    kind: "mod".to_owned(),
                    params: None,
                    line: item.ident.span().start().line,
                    declared: render_visibility(&item.vis),
                    reach: module_reach.clone(),
                });
            }
            if let Some((_, items)) = &item.content {
                collect_public_items(items, &nested_module, &module_reach, output, mod_decls);
            }
            continue;
        }
        let (visibility, name, kind, params, line) = match item {
            Item::Const(item) => (
                &item.vis,
                item.ident.to_string(),
                "const",
                None,
                item.ident.span().start().line,
            ),
            Item::Enum(item) => (
                &item.vis,
                item.ident.to_string(),
                "enum",
                None,
                item.ident.span().start().line,
            ),
            Item::Fn(item) => (
                &item.vis,
                item.sig.ident.to_string(),
                "fn",
                Some(item.sig.inputs.len()),
                item.sig.ident.span().start().line,
            ),
            Item::Static(item) => (
                &item.vis,
                item.ident.to_string(),
                "static",
                None,
                item.ident.span().start().line,
            ),
            Item::Struct(item) => (
                &item.vis,
                item.ident.to_string(),
                "struct",
                None,
                item.ident.span().start().line,
            ),
            Item::TraitAlias(item) => (
                &item.vis,
                item.ident.to_string(),
                "trait-alias",
                None,
                item.ident.span().start().line,
            ),
            Item::Type(item) => (
                &item.vis,
                item.ident.to_string(),
                "type",
                None,
                item.ident.span().start().line,
            ),
            Item::Union(item) => (
                &item.vis,
                item.ident.to_string(),
                "union",
                None,
                item.ident.span().start().line,
            ),
            Item::Use(item) if is_boundary_visible(&item.vis) => {
                let mut names = Vec::new();
                flatten_use(&item.tree, &mut Vec::new(), &mut names);
                for (_, name, _) in names {
                    output.push(PubItem {
                        module: module.to_owned(),
                        name,
                        kind: "use".to_owned(),
                        params: None,
                        line: item.span().start().line,
                        declared: render_visibility(&item.vis),
                        reach: narrower_reach(
                            &visibility_reach(&item.vis, module),
                            enclosing_reach,
                            module,
                        ),
                    });
                }
                continue;
            }
            _ => continue,
        };
        if is_boundary_visible(visibility) {
            output.push(PubItem {
                module: module.to_owned(),
                name,
                kind: kind.to_owned(),
                params,
                line,
                declared: render_visibility(visibility),
                reach: narrower_reach(
                    &visibility_reach(visibility, module),
                    enclosing_reach,
                    module,
                ),
            });
        }
    }
}

fn collect_test_regions(items: &[Item], output: &mut Vec<Range<usize>>) {
    for item in items {
        let Item::Mod(item_mod) = item else {
            continue;
        };
        if is_cfg_test(&item_mod.attrs) {
            let start = item_mod.attrs.first().map_or_else(
                || item_mod.span().start().line,
                |attr| attr.span().start().line,
            );
            output.push(start..item_mod.span().end().line.saturating_add(1));
        } else if let Some((_, nested)) = &item_mod.content {
            collect_test_regions(nested, output);
        }
    }
}

fn join_module(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_owned()
    } else {
        format!("{module}::{name}")
    }
}

fn parent_module(module: &str) -> String {
    module
        .rsplit_once("::")
        .map_or_else(String::new, |(parent, _)| parent.to_owned())
}

fn visibility_reach(visibility: &Visibility, module: &str) -> String {
    match visibility {
        Visibility::Public(_) => String::new(),
        Visibility::Inherited => module.to_owned(),
        Visibility::Restricted(restricted) => {
            let path = restricted
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            match path.first().map(String::as_str) {
                Some("crate") => path[1..].join("::"),
                Some("self") => path[1..]
                    .iter()
                    .fold(module.to_owned(), |base, part| join_module(&base, part)),
                Some("super") => {
                    let mut base = module.to_owned();
                    let mut index = 0;
                    while path.get(index).is_some_and(|part| part == "super") {
                        base = parent_module(&base);
                        index += 1;
                    }
                    path[index..]
                        .iter()
                        .fold(base, |base, part| join_module(&base, part))
                }
                _ => path.join("::"),
            }
        }
    }
}

fn render_visibility(visibility: &Visibility) -> String {
    match visibility {
        Visibility::Public(_) => "pub".to_owned(),
        Visibility::Inherited => "inherited".to_owned(),
        Visibility::Restricted(restricted) => {
            let path = restricted
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            match path.as_str() {
                "crate" => "pub(crate)".to_owned(),
                "self" => "pub(self)".to_owned(),
                "super" => "pub(super)".to_owned(),
                _ => format!("pub(in {path})"),
            }
        }
    }
}

fn is_within(module: &str, ancestor: &str) -> bool {
    ancestor.is_empty() || module == ancestor || module.starts_with(&format!("{ancestor}::"))
}

fn narrower_reach(left: &str, right: &str, fallback: &str) -> String {
    if is_within(left, right) {
        left.to_owned()
    } else if is_within(right, left) {
        right.to_owned()
    } else {
        fallback.to_owned()
    }
}

fn is_boundary_visible(visibility: &Visibility) -> bool {
    match visibility {
        Visibility::Public(_) => true,
        Visibility::Restricted(restricted) => {
            let path = restricted
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            path.as_slice() != ["self"]
        }
        Visibility::Inherited => false,
    }
}

struct UseCollector<'a> {
    file_module: &'a str,
    imports: Vec<ImportedItem>,
}

impl<'ast> Visit<'ast> for UseCollector<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut flattened = Vec::new();
        flatten_use(&item.tree, &mut Vec::new(), &mut flattened);
        for (path, imported_item, grouped) in flattened {
            let internal = path
                .first()
                .is_some_and(|segment| matches!(segment.as_str(), "crate" | "self" | "super"));
            let leaf_may_be_module = path.len() == 1 && !grouped;
            let mut module_path = resolve_import_path(self.file_module, &path);
            if module_path.is_empty() && internal {
                module_path = "(crate)".to_owned();
            }
            if !module_path.is_empty() {
                self.imports.push(ImportedItem {
                    module_path,
                    item: imported_item,
                    line: item.span().start().line,
                    internal,
                    leaf_may_be_module,
                });
            }
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !is_cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }
}

pub(super) fn resolved_internal_import(
    import: &ImportedItem,
    known_modules: &std::collections::BTreeSet<String>,
    workspace_crates: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let module_path = if import.internal {
        import.module_path.clone()
    } else {
        let mut parts = import.module_path.split("::");
        let crate_name = parts.next()?;
        if !workspace_crates.contains(crate_name) {
            return None;
        }
        let path = parts.collect::<Vec<_>>().join("::");
        if path.is_empty() {
            "(crate)".to_owned()
        } else {
            path
        }
    };
    if !import.leaf_may_be_module {
        return Some(module_path);
    }
    let candidate = if module_path == "(crate)" {
        import.item.clone()
    } else {
        format!("{module_path}::{}", import.item)
    };
    if known_modules
        .iter()
        .any(|module| module == &candidate || module.starts_with(&format!("{candidate}::")))
    {
        Some(candidate)
    } else {
        Some(module_path)
    }
}

fn flatten_use(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    output: &mut Vec<(Vec<String>, String, bool)>,
) {
    flatten_use_inner(tree, prefix, output, false);
}

fn flatten_use_inner(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    output: &mut Vec<(Vec<String>, String, bool)>,
    grouped: bool,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use_inner(&path.tree, prefix, output, grouped);
            prefix.pop();
        }
        UseTree::Name(name) => {
            output.push((prefix.clone(), name.ident.to_string(), grouped));
        }
        UseTree::Rename(rename) => {
            output.push((prefix.clone(), rename.rename.to_string(), grouped));
        }
        UseTree::Glob(_) => output.push((prefix.clone(), "*".to_owned(), grouped)),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_inner(item, prefix, output, true);
            }
        }
    }
}

fn resolve_import_path(file_module: &str, path: &[String]) -> String {
    let mut base = file_module
        .split("::")
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut index = 0;
    match path.first().map(String::as_str) {
        Some("crate") => {
            base.clear();
            index = 1;
        }
        Some("self") => index = 1,
        Some("super") => {
            while path.get(index).is_some_and(|part| part == "super") {
                base.pop();
                index += 1;
            }
        }
        _ => {
            base.clear();
        }
    }
    base.extend(path[index..].iter().cloned());
    base.join("::")
}

struct FnCollector<'a> {
    path: &'a Path,
    functions: Vec<FnBody>,
}

impl FnCollector<'_> {
    fn push(&mut self, name: String, span: proc_macro2::Span, block: &syn::Block) {
        let start = span.start().line;
        let end = span.end().line;
        let mut calls = CallCollector::default();
        calls.visit_block(block);
        self.functions.push(FnBody {
            name,
            path: self.path.to_path_buf(),
            line: start,
            sloc: end.saturating_sub(start) + 1,
            callees: calls.callees,
        });
    }
}

impl<'ast> Visit<'ast> for FnCollector<'_> {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.push(item.sig.ident.to_string(), item.span(), &item.block);
        visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        self.push(item.sig.ident.to_string(), item.span(), &item.block);
        visit::visit_impl_item_fn(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !is_cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }
}

fn is_cfg_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && match &attribute.meta {
                syn::Meta::List(list) => list.tokens.to_string() == "test",
                _ => false,
            }
    })
}

#[derive(Default)]
struct CallCollector {
    callees: Vec<String>,
}

impl<'ast> Visit<'ast> for CallCollector {
    fn visit_expr_call(&mut self, expression: &'ast ExprCall) {
        if let Expr::Path(path) = expression.func.as_ref() {
            self.callees.push(
                path.path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
            );
        }
        visit::visit_expr_call(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        self.callees.push(format!(".{}", expression.method));
        visit::visit_expr_method_call(self, expression);
    }
}

#[cfg(test)]
mod tests;
