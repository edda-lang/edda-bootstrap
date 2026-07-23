//! Item-walking: turning AST items (functions, type decls, specs) into the
//! per-directory model entries, plus the path / module-name helpers.

use std::path::{Component, Path};

use edda_intern::Interner;
use edda_span::{BytePos, FileId, Span};
use edda_syntax::ast::{
    FnBody, FnDecl, Item, ItemKind, RefinementClause, RefinementKind,
    Spec, Stability, TypeDecl, TypeDeclKind, Visibility as AstVis,
};

use crate::EmitInput;
use crate::model::{
    DirEntry, FunctionEntry, InvariantEntry, ModuleEntry,
    StabilityMarker, TrustEntry, TypeEntry, TypeKind as MapTypeKind,
    Visibility as MapVis,
};
use super::calls::{collect_calls_block, trust_kind_for, trust_reason};
use super::render::{effect_member_text, expr_text, interner_text, render_sig_only};

pub(super) fn push_module(
    entry: &mut DirEntry,
    input: &EmitInput,
    ast: &edda_syntax::ast::File,
    file_path: &Path,
    dir: &Path,
    file_id: FileId,
) {
    let file_rel = relative_str(dir, file_path);
    let module_name = module_name_from_filename(&file_rel);
    entry.modules.push(ModuleEntry {
        name: module_name.clone(),
        file: file_rel.clone(),
        line: 1,
        visibility: MapVis::Public,
    });

    for item in &ast.items {
        walk_item(entry, input, item, &file_rel, &module_name, file_id);
    }
}

pub(super) fn is_under_build_cache(file_path: &Path) -> bool {
    file_path
        .components()
        .any(|c| matches!(c, Component::Normal(name) if name == ".edda"))
}

fn module_name_from_filename(rel: &str) -> String {
    if let Some(s) = rel.strip_suffix(".edda") {
        return s.to_string();
    }
    rel.strip_suffix(".ea").unwrap_or(rel).to_string()
}

fn relative_str(dir: &Path, file_path: &Path) -> String {
    match file_path.strip_prefix(dir) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => file_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn line_of(input: &EmitInput, file_id: FileId, span: Span) -> u32 {
    if span.lo == BytePos(0) && span.hi == BytePos(0) {
        return 0;
    }
    input.source_map.byte_to_linecol(file_id, span.lo).line
}

/// `(start, end)` 1-based source lines for an item — the schema v8
/// `line` / `end` columns. Both are `0` for the dummy `(0, 0)` span the
/// parser synthesises on recovery.
fn span_lines(input: &EmitInput, file_id: FileId, span: Span) -> (u32, u32) {
    if span.lo == BytePos(0) && span.hi == BytePos(0) {
        return (0, 0);
    }
    let start = input.source_map.byte_to_linecol(file_id, span.lo).line;
    let end = input.source_map.byte_to_linecol(file_id, span.hi).line;
    (start, end)
}

fn walk_item(
    entry: &mut DirEntry,
    input: &EmitInput,
    item: &Item,
    file_rel: &str,
    module_path: &str,
    file_id: FileId,
) {
    // Resolve the "target" qualified name for trust attribution.
    let target = item_target_qualified(item, input.interner, module_path)
        .unwrap_or_else(|| format!("{}.<item>", module_path));

    // Trust-point attributes (`@unverified` / `@trust`).
    for attr in &item.attributes {
        if let Some(tk) = trust_kind_for(attr, input.interner) {
            entry.trust_points.push(TrustEntry {
                target: target.clone(),
                kind: tk,
                file: file_rel.to_string(),
                line: line_of(input, file_id, attr.span),
                reason: trust_reason(attr, input.interner).unwrap_or_default(),
            });
        }
    }

    match &item.kind {
        ItemKind::Function(fd) => {
            push_fn(entry, input, fd, item.span, file_rel, module_path, file_id)
        }
        ItemKind::TypeDecl(td) => push_type_decl(entry, input, td, item.span, file_rel, file_id),
        ItemKind::Spec(s) => {
            push_spec(entry, input, s, item.span, file_rel, module_path, file_id)
        }
        ItemKind::SpecInvocation(_)
        | ItemKind::Let(_)
        | ItemKind::Import(_)
        | ItemKind::Module(_)
        | ItemKind::Derive(_) => {
            // Spec invocations are codegen seams, not patterns — schema v2
            // drops the `patterns[]` push that conflated the two.
            // `let` / `import` / `module` / `derive` carry no structmap
            // surface beyond their doc-comments (already pushed above).
        }
    }
}

fn item_target_qualified(item: &Item, interner: &Interner, module_path: &str) -> Option<String> {
    let bare: String = match &item.kind {
        ItemKind::Function(fd) => interner_text(interner, fd.name.name).to_string(),
        ItemKind::TypeDecl(td) => interner_text(interner, td.name.name).to_string(),
        ItemKind::Spec(s) => interner_text(interner, s.name.name).to_string(),
        ItemKind::SpecInvocation(si) => path_string(&si.path, interner),
        ItemKind::Let(ld) => interner_text(interner, ld.name.name).to_string(),
        ItemKind::Import(im) => path_string(&im.path, interner),
        ItemKind::Module(m) => path_string(&m.path, interner),
        ItemKind::Derive(_) => return Some(format!("{}.<derive>", module_path)),
    };
    Some(format!("{}.{}", module_path, bare))
}

pub(super) fn path_string(path: &edda_syntax::ast::Path, interner: &Interner) -> String {
    path.segments
        .iter()
        .map(|s| interner_text(interner, s.name))
        .collect::<Vec<_>>()
        .join(".")
}

fn push_fn(
    entry: &mut DirEntry,
    input: &EmitInput,
    fd: &FnDecl,
    item_span: Span,
    file_rel: &str,
    module_path: &str,
    file_id: FileId,
) {
    let bare = interner_text(input.interner, fd.name.name);
    let qualified = format!("{module_path}.{bare}");
    let sig = render_sig_only(input.interner, fd);

    // Attach invariants for every refinement clause. This is the
    // canonical home for `requires`/`ensures`/`decreases`/`where` (with
    // line numbers) — schema v6 dropped the per-function columns that
    // duplicated them.
    for clause in &fd.refinements {
        entry.invariants.push(InvariantEntry {
            target: qualified.clone(),
            file: file_rel.to_string(),
            line: line_of(input, file_id, clause.span),
            rule: refinement_rule_text(input.interner, clause),
        });
    }

    // Calls — walk the body, collect every `Call(Path(...))` or
    // `MethodCall` site. `Extern` bodies have no Edda-side body and
    // therefore no calls. Method-call records are textual only (`.name`);
    // receiver-type resolution would require typecheck output that the
    // structmap doesn't currently consume.
    let mut calls: Vec<String> = Vec::new();
    if let FnBody::Block(b) = &fd.body {
        collect_calls_block(b, input.interner, &mut calls);
    }
    calls.sort();
    calls.dedup();

    let declared_effects: Vec<String> = match &fd.effects {
        Some(row) => row
            .members
            .iter()
            .map(|m| effect_member_text(input.interner, m))
            .collect(),
        None => Vec::new(),
    };

    let (line, end) = span_lines(input, file_id, item_span);
    entry.functions.push(FunctionEntry {
        qualified_name: qualified,
        file: file_rel.to_string(),
        line,
        end,
        visibility: vis_to_map(fd.visibility),
        stability: fn_stability_to_map(fd),
        sig,
        calls,
        // Populated by `analyze::compute_effect_cones` after build_tree returns.
        effect_cone: Vec::new(),
        declared_effects,
    });
}

fn push_type_decl(
    entry: &mut DirEntry,
    input: &EmitInput,
    td: &TypeDecl,
    item_span: Span,
    file_rel: &str,
    file_id: FileId,
) {
    let name: String = interner_text(input.interner, td.name.name).to_string();
    let kind = match &td.kind {
        TypeDeclKind::Product { .. } => MapTypeKind::Struct,
        TypeDeclKind::Sum { .. } => MapTypeKind::Enum,
    };
    let fields_or_variants = match &td.kind {
        TypeDeclKind::Product { fields } => fields
            .iter()
            .map(|f| interner_text(input.interner, f.name.name))
            .collect::<Vec<_>>()
            .join(" "),
        TypeDeclKind::Sum { variants } => variants
            .iter()
            .map(|v| interner_text(input.interner, v.name.name))
            .collect::<Vec<_>>()
            .join(" "),
    };
    // Field-level `where` clauses surface as invariants.
    let mut refinements = Vec::new();
    if let TypeDeclKind::Product { fields } = &td.kind {
        for f in fields {
            if let Some(pred) = &f.refinement {
                let rule = format!(
                    "{}: {}",
                    interner_text(input.interner, f.name.name),
                    expr_text(input.interner, pred)
                );
                entry.invariants.push(InvariantEntry {
                    target: name.clone(),
                    file: file_rel.to_string(),
                    line: line_of(input, file_id, f.span),
                    rule: rule.clone(),
                });
                refinements.push(rule);
            }
        }
    }
    let (line, end) = span_lines(input, file_id, item_span);
    entry.types.push(TypeEntry {
        name,
        kind,
        file: file_rel.to_string(),
        line,
        end,
        visibility: vis_to_map(td.visibility),
        stability: stability_to_map(td.stability),
        // `type` decls don't expose generics on the structmap
        // surface yet (the AST carries `TypeDecl.generics`, but no
        // user-facing structmap consumer reads them yet). Reserved.
        generics: Vec::new(),
        fields_or_variants,
        refinements,
    });
}

fn push_spec(
    entry: &mut DirEntry,
    input: &EmitInput,
    s: &Spec,
    item_span: Span,
    file_rel: &str,
    module_path: &str,
    file_id: FileId,
) {
    let name: String = interner_text(input.interner, s.name.name).to_string();
    // `spec` surfaces as a TypeEntry with kind `spec` per codex §5.2 ("type
    // kind: struct/enum/alias/spec"). Schema v2: generics live in their
    // own column; fields_or_variants is empty for specs.
    let generics: Vec<String> = s
        .generics
        .iter()
        .map(|g| interner_text(input.interner, g.name.name).to_string())
        .collect();
    let mut refinements = Vec::new();
    for clause in &s.where_clauses {
        let rule = refinement_rule_text(input.interner, clause);
        entry.invariants.push(InvariantEntry {
            target: name.clone(),
            file: file_rel.to_string(),
            line: line_of(input, file_id, clause.span),
            rule: rule.clone(),
        });
        refinements.push(rule);
    }
    let (line, end) = span_lines(input, file_id, item_span);
    entry.types.push(TypeEntry {
        name: name.clone(),
        kind: MapTypeKind::Spec,
        file: file_rel.to_string(),
        line,
        end,
        visibility: vis_to_map(s.visibility),
        stability: stability_to_map(s.stability),
        generics,
        fields_or_variants: String::new(),
        refinements,
    });
    // Spec bodies admit nested items; walk them with the spec name appended
    // to the module path so nested function qualified names don't collide
    // with same-named top-level functions in the file.
    let inner_path = format!("{}.{}", module_path, name);
    for inner in &s.body {
        walk_item(entry, input, inner, file_rel, &inner_path, file_id);
    }
}

fn vis_to_map(v: AstVis) -> MapVis {
    match v {
        AstVis::Public => MapVis::Public,
        AstVis::Module => MapVis::Module,
    }
}

fn stability_to_map(s: Option<Stability>) -> StabilityMarker {
    match s {
        Some(Stability::Stable { .. }) => StabilityMarker::Stable,
        Some(Stability::Unstable { .. }) => StabilityMarker::Unstable,
        None => StabilityMarker::Absent,
    }
}

/// Stability marker for a function row.
///
/// Unlike `type` / `spec`, whose `stable` / `unstable` keyword lands in
/// [`FnDecl::stability`], a function's `stable` marker is
/// [`FnDecl::refinement_stable`]. Reading `stability` alone (as
/// `stability_to_map` does for types) emits a blank column for every
/// `stable function`.
fn fn_stability_to_map(fd: &FnDecl) -> StabilityMarker {
    if fd.refinement_stable {
        StabilityMarker::Stable
    } else {
        stability_to_map(fd.stability)
    }
}

fn refinement_rule_text(interner: &Interner, c: &RefinementClause) -> String {
    let kw = match c.kind {
        RefinementKind::Requires => "requires",
        RefinementKind::Ensures => "ensures",
        RefinementKind::Where => "where",
        RefinementKind::Decreases => "decreases",
    };
    format!("{} {}", kw, expr_text(interner, &c.pred))
}
