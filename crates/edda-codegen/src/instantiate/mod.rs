//! Per-invocation spec instantiation — sequences the body-source
//! emission and the cascade session into a single callable entry point.
//!
//! Per `docs/roadmap.md`, this connects the
//! substitution engine and the body-source emitter to the
//! [`CodegenSession`].

mod format;
mod head_collect;

use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast::{Ident, Import, Item, ItemKind, Path, Spec, SpecInvocation};
use time::OffsetDateTime;

use crate::argument::ArgumentTuple;
use crate::body::{Encoder, QualifiedNameResolver};
use crate::canonical::{CanonicalForm, NestedDependency};
use crate::cascade::{CodegenSession, StageRequest, StagedArtifact};
use crate::emit::emit_items;
use crate::error::CodegenError;
use crate::substitution::{SubstitutionMap, substitute_spec_body};
use edda_cache::{ReachableFrom, Tier};

use self::format::{compose_module_path, format_invocation};
use self::head_collect::collect_used_heads;

/// Instantiate one spec invocation and stage the materialized artifact.
///
/// Sequences the instantiation pipeline (`docs/roadmap.md`):
///
/// 1. Encode canonical body bytes (pre-substitution, via `write_spec_body`)
/// 2. Build [`CanonicalForm`] hash-input tuple
/// 3. Bind comptime args via [`SubstitutionMap`]
/// 4. Substitute the spec body via [`substitute_spec_body`]
/// 5. Emit UTF-8 source bytes via [`emit_items`], prepending the
///    parent module's `import` items + a self-import so the generated
///    artifact's references resolve in the cascade-restart pass.
/// 6. Stage via [`CodegenSession::stage`]
///
/// `parent_imports`, `parent_qualified`, and `parent_sibling_names`
/// together carry the parent-module context the spec body assumed. Pass
/// `&[]` / `""` / `&[]` when no inheritance is needed.
///
/// Returns the [`StagedArtifact`] alongside the list of nested
/// `SpecInvocation` items extracted from the substituted body so the
/// caller can drive the cascade across transitively-demanded specs
/// (e.g. `Vec(T)`'s body invokes `Option(T)`; the caller enqueues
/// `Option(T)` once it sees it in the returned vector). `where`-clause
/// discharge is deferred to later waves.
pub fn instantiate_spec(
    session: &mut CodegenSession<'_>,
    spec: &Spec,
    spec_qualified: &str,
    args: &ArgumentTuple,
    tier: Tier,
    resolver: &dyn QualifiedNameResolver,
    interner: &Interner,
    now: OffsetDateTime,
    parent_imports: &[Item],
    parent_qualified: &str,
    parent_sibling_names: &[Symbol],
) -> Result<(StagedArtifact, Vec<SpecInvocation>), CodegenError> {
    let canonical_body = encode_canonical_body(spec, interner, resolver);
    let form = CanonicalForm::new(
        spec_qualified,
        args.clone(),
        canonical_body,
        Vec::<NestedDependency>::new(),
    );
    let parent_leaf = parent_qualified
        .rsplit_once('.')
        .map(|(_, leaf)| leaf)
        .unwrap_or(parent_qualified);
    let subst = SubstitutionMap::bind(spec_qualified, &spec.generics, args, interner)?
        .with_parent_siblings(parent_leaf, parent_sibling_names, interner);
    let substituted = substitute_spec_body(spec, &subst, interner);
    let nested_invocations = collect_nested_invocations(&substituted);
    // Collect the path heads the substituted body actually references so
    // we can filter out parent imports whose binding is never used. B-018
    // landed this filter to stop the codegen template from emitting
    // `import compare` / `import fmt` into every `eq_<T>` / `debug_<T>`
    // artifact whose body doesn't touch the parent module — those dead
    // imports flooded `unused_import` across every consumer.
    let used_heads = collect_used_heads(&substituted);
    let parent_leaf_sym = if parent_qualified.is_empty() {
        None
    } else {
        Some(interner.intern(parent_leaf))
    };
    let mut items: Vec<Item> = Vec::with_capacity(parent_imports.len() + substituted.len() + 1);
    for parent_item in parent_imports {
        let keep = match &parent_item.kind {
            ItemKind::Import(import) => import_binding_in_use(import, &used_heads),
            // Non-Import items in the parent_imports slice (defensive — the
            // driver currently only supplies imports here, but the function
            // signature carries `&[Item]`) propagate unconditionally.
            _ => true,
        };
        if keep {
            items.push(parent_item.clone());
        }
    }
    if !parent_qualified.is_empty()
        && !parent_sibling_names.is_empty()
        && let Some(leaf_sym) = parent_leaf_sym
        && used_heads.contains(&leaf_sym)
    {
        items.push(synthesize_parent_import(parent_qualified, interner));
    }
    items.extend(substituted);
    let module_path = compose_module_path(spec_qualified, args);
    let body_source = emit_items(items, interner, Some(&module_path));
    let invocation_str = format_invocation(spec_qualified, args);
    let req = StageRequest {
        form: &form,
        tier,
        body_source: &body_source,
        spec_invocation: &invocation_str,
        nested_for_header: &[],
        reachable_from: ReachableFrom::default(),
    };
    let staged = session.stage(req, now)?;
    Ok((staged, nested_invocations))
}

/// Collect every nested `SpecInvocation` from the post-substitution
/// item slice. The returned vector preserves source order; the caller
/// uses it to enqueue follow-up cascade roots so transitively-demanded
/// specs (`Vec(T)`'s body invokes `Option(T)`) materialise alongside
/// their parents.
fn collect_nested_invocations(items: &[Item]) -> Vec<SpecInvocation> {
    let mut out = Vec::new();
    for item in items {
        if let ItemKind::SpecInvocation(si) = &item.kind {
            out.push((**si).clone());
        }
    }
    out
}

/// Pick the [`Symbol`] an import puts into the artifact's local scope:
/// `import X.Y.Z as W` binds `W`; `import X.Y.Z` binds `Z`. Matches
/// `edda-resolve`'s leaf-table key choice.
fn import_binding(import: &Import) -> Symbol {
    if let Some(alias) = &import.alias {
        return alias.name;
    }
    import
        .path
        .segments
        .last()
        .expect("import path must have at least one segment")
        .name
}

/// Whether an import's binding name appears as the head of any path
/// reference in the substituted body. Used to filter dead
/// `parent_imports` out of the artifact (B-018).
fn import_binding_in_use(import: &Import, used_heads: &std::collections::HashSet<Symbol>) -> bool {
    used_heads.contains(&import_binding(import))
}

/// Construct a synthetic `Item::Import(Import)` whose `path` segments
/// are the dot-split `parent_qualified` interned through `interner`.
fn synthesize_parent_import(parent_qualified: &str, interner: &Interner) -> Item {
    let segments: Vec<Ident> = parent_qualified
        .split('.')
        .map(|seg| Ident { name: interner.intern(seg), span: Span::DUMMY })
        .collect();
    debug_assert!(!segments.is_empty(), "synthesize_parent_import: empty path");
    let path = Path { segments, span: Span::DUMMY };
    Item {
        span: Span::DUMMY,
        doc: Vec::new(),
        attributes: Vec::new(),
        kind: ItemKind::Import(Import { span: Span::DUMMY, path, alias: None, selection: None }),
    }
}

/// Encode the pre-substitution canonical body bytes for `spec`.
fn encode_canonical_body(
    spec: &Spec,
    interner: &Interner,
    resolver: &dyn QualifiedNameResolver,
) -> Vec<u8> {
    let mut enc = Encoder::new(interner, resolver);
    enc.write_spec_body(spec);
    enc.into_bytes()
}

#[cfg(test)]
mod tests;
