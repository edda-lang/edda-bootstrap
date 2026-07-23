//! Call-site argument-mode projection and capability threading.
//!
//! Shared by both the regular call path and the `?`-propagation path.
//! Single-purpose conversion table per public type: ParamMode → CallMode
//! mapping for argument shape, and callee-row Capability entries → caller
//! `EffectId`s for capability threading.

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::{EffectEntry, FnSig, HirCallArg, HirCallMode, HirExpr, HirExprKind};

use crate::error::{LoweringError, MirError};
use crate::operand::Operand;
use crate::terminator::{CallArg, CallMode, ThreadedCapability};
use crate::ty::{MirTypeKind, ParamMode};

use super::ctx::LoweringContext;

/// Bound for the path-chain traversal that resolves a capability argument to
/// its caller-side root symbol. Mirrors `edda_types::infer::call`'s
/// `CAPABILITY_DERIVATION_DEPTH` constant; kept local so the MIR layer
/// does not reach across crates for a literal value.
const CAPABILITY_DERIVATION_DEPTH: usize = 8;

/// Project the call-site `CallMode` for the `i`-th argument. If the callee's
/// signature has a matching `Param`, the projection follows from its
/// `ParamMode`; otherwise the caller's keyword (when present) is used as a
/// fallback, defaulting to `Read`.
pub(super) fn call_arg_mode(
    sig: &FnSig,
    i: usize,
    caller_keyword: Option<HirCallMode>,
) -> CallMode {
    if let Some(param) = sig.params.get(i) {
        let mir_mode = map_types_param_mode(param.mode);
        return CallMode::from_param_mode(mir_mode);
    }
    match caller_keyword {
        Some(HirCallMode::Mutable) => CallMode::Mutable,
        Some(HirCallMode::Take) => CallMode::Take,
        Some(HirCallMode::Init) => CallMode::Init,
        None => CallMode::Read,
    }
}

/// `edda_types::ParamMode -> mir::ParamMode`. Mirrors the registration-pass
/// helper in `super::register`; kept here so call lowering does not reach
/// into a sibling module's private item.
pub(super) fn map_types_param_mode(mode: edda_types::ParamMode) -> ParamMode {
    match mode {
        edda_types::ParamMode::Default => ParamMode::Let,
        edda_types::ParamMode::Mutable => ParamMode::Mutable,
        edda_types::ParamMode::Take => ParamMode::Take,
        edda_types::ParamMode::Init => ParamMode::Init,
    }
}

/// Resolve every capability entry in the callee's effect row against the
/// caller's `ctx.capabilities` map and return the matching
/// [`ThreadedCapability`] pairings in callee-row order.
///
/// The callee row names its own parameters (`with {random}` on
/// `function fill(random: Random, ...)`), not the caller's bindings.
/// For each `Capability(callee_name)` we therefore locate the matching
/// positional parameter in `sig.params`, walk to the caller-side
/// argument occupying that slot (the receiver when present and `i == 0`,
/// otherwise `args[i - receiver_offset]`), and extract the leftmost
/// path symbol from that argument — that is the caller's binding name
/// the caller's `ctx.capabilities` is keyed by.
///
/// Falling back to the raw callee name (i.e. `ctx.capabilities.get(name)`)
/// preserves the same-name fast path where the caller's parameter happens
/// to share a name with the callee's parameter (`fs: Filesystem` on both
/// sides), and the param→arg walk would otherwise be a no-op.
///
/// The `id` resolved this way is the effect-accounting channel (which
/// caller slot the callee's authority derives from). The capability
/// *value* the callee receives is the positional `call_args` operand at
/// the parameter's index, recorded as `value_arg` when that operand is
/// a direct capability-typed local — the accounting slot and the value
/// can name different handles (`alloc.fork(allocator)` bound over the
/// parameter name), and codegen must draw from the value.
///
/// If the caller did not declare a required capability — neither via the
/// argument source nor as a same-name fallback — push `UnknownCapability`
/// so the user sees the missing declaration.
///
/// `extern_param_order` selects the threading ORDER. `false` (body-backed
/// callees) keeps the canonical row-entry order — the callee's own
/// `lower_effect_row` registers its capability slots by walking the same
/// canonical row, so caller and callee agree by construction. `true`
/// (extern-bodied callees) reorders the threaded slots by the callee's
/// DECLARED PARAMETER position: an extern's implementation (the Rust
/// `edda-rt` externs and any `@abi`-claimed Edda implementation with
/// explicit handle params) declares its capability slots in source
/// parameter order per the caps-first wire contract,
/// while the canonical row order is
/// `Symbol`-Ord (interning-order) — effectively arbitrary. Threading a
/// multi-capability extern in canonical order swaps handles whenever the
/// two orders disagree: `read_to_string(rfs, path, allocator)` threaded
/// `(allocator, rfs)` hands the heap pointer to the `cap_fs` slot, which
/// the runtime then dereferences as a `ScopedFs` and walks region-header
/// bytes as a `PathBuf`. Row capabilities
/// naming no parameter keep their canonical relative order after the
/// param-matched ones (stable sort).
pub(super) fn thread_capabilities(
    ctx: &mut LoweringContext<'_>,
    sig: &FnSig,
    implicit_receiver: Option<&HirExpr>,
    args: &[HirCallArg],
    call_args: &[CallArg],
    call_span: Span,
    extern_param_order: bool,
) -> Vec<ThreadedCapability> {
    let cap_names = capability_thread_order(sig, extern_param_order);
    let mut out = Vec::new();
    for callee_name in &cap_names {
        let caller_source =
            resolve_caller_source(sig, implicit_receiver, args, *callee_name);
        let value_arg = sig
            .params
            .iter()
            .position(|p| p.name == *callee_name)
            .and_then(|idx| positional_cap_value_arg(ctx, call_args, idx));
        // Chase the caller-side source through the narrowed-capability
        // alias map (`let rfs = wfs.read_only()` records `rfs -> wfs`)
        // so a derived local resolves to the threaded parameter symbol
        // before the `ctx.capabilities` lookup.
        let lookup_key =
            resolve_alias_root(ctx, caller_source.unwrap_or(*callee_name));
        match ctx.capabilities.get(&lookup_key).copied() {
            Some(id) => out.push(ThreadedCapability { id, value_arg }),
            None => {
                // Fall back to the same-name lookup if the param→arg walk
                // produced a different symbol that wasn't in scope. This
                // preserves the pre-fix behaviour for cases where the
                // callee declares a capability the caller's row already
                // names directly.
                if caller_source.is_some()
                    && let Some(id) = ctx.capabilities.get(callee_name).copied()
                {
                    out.push(ThreadedCapability { id, value_arg });
                    continue;
                }
                ctx.errors.push(MirError::from(LoweringError::UnknownCapability {
                    name: *callee_name,
                    span: call_span,
                }));
            }
        }
    }
    out
}

/// The capability threading order for a call against `sig`: the row's
/// capability entries, reordered by callee declared-parameter position
/// when `extern_param_order` is set (extern-bodied callees).
fn capability_thread_order(sig: &FnSig, extern_param_order: bool) -> Vec<Symbol> {
    let mut cap_names: Vec<Symbol> = sig
        .effects
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            EffectEntry::Capability(name) => Some(*name),
            _ => None,
        })
        .collect();
    if extern_param_order {
        cap_names.sort_by_key(|name| {
            sig.params
                .iter()
                .position(|p| p.name == *name)
                .unwrap_or(usize::MAX)
        });
    }
    cap_names
}

/// The `value_arg` pairing for a callee capability parameter at
/// `param_idx`: the call's MIR argument at the same index (the receiver
/// occupies position 0 when present, so callee-param and MIR-arg
/// indices coincide), admitted only when that operand is a direct
/// capability-typed local.
fn positional_cap_value_arg(
    ctx: &LoweringContext<'_>,
    call_args: &[CallArg],
    param_idx: usize,
) -> Option<u32> {
    let arg = call_args.get(param_idx)?;
    let place = match &arg.operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        return None;
    }
    let body = ctx.body.as_ref()?.body_ref();
    let decl = body.locals.get(place.local)?;
    if matches!(decl.ty.kind, MirTypeKind::Capability(_)) {
        Some(param_idx as u32)
    } else {
        None
    }
}

/// Walk the param→arg binding to find the caller-side capability source
/// for a callee capability name.
fn resolve_caller_source(
    sig: &FnSig,
    implicit_receiver: Option<&HirExpr>,
    args: &[HirCallArg],
    callee_name: Symbol,
) -> Option<Symbol> {
    let idx = sig.params.iter().position(|p| p.name == callee_name)?;
    match (implicit_receiver, idx) {
        (Some(receiver), 0) => capability_source(receiver),
        (Some(_), i) => {
            let arg = args.get(i - 1)?;
            capability_source(&arg.expr)
        }
        (None, i) => {
            let arg = args.get(i)?;
            capability_source(&arg.expr)
        }
    }
}

/// Chase `sym` through `ctx.capability_aliases` to the root effect-row
/// capability symbol it derives from. Aliases are single-level by
/// construction (see [`LoweringContext::capability_aliases`]), so the
/// common case is at most one hop; the bounded loop tolerates a chained
/// alias table without risking an unbounded walk on a cyclic entry.
pub(super) fn resolve_alias_root(ctx: &LoweringContext<'_>, sym: Symbol) -> Symbol {
    let mut current = sym;
    for _ in 0..CAPABILITY_DERIVATION_DEPTH {
        match ctx.capability_aliases.get(&current).copied() {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    current
}

/// Extract the capability source of a narrowing call expression.
///
/// Fires only for call-shaped expressions and delegates to
/// [`capability_source`], which traces the call to the capability it
/// narrows: the receiver (`wfs.read_only()` → `wfs`) or the leading
/// positional argument (`fsmod.scoped_to(fsmod.read_only(fs), p)` →
/// `fs`). Returns `None` for any non-call shape so a plain rebind
/// (`let x = wfs`) does not record a spurious alias.
pub(super) fn capability_source_of_call(expr: &HirExpr) -> Option<Symbol> {
    match &expr.kind {
        HirExprKind::MethodCall { .. } | HirExprKind::Call { .. } => capability_source(expr),
        _ => None,
    }
}

/// Walk a HIR expression to find the originating capability source.
/// Admits a single-segment path naming a binding (`fs`), a chain of
/// field projections rooted at a single-segment path
/// (`world.network.local_addr`), and a narrowing call, which is traced
/// to the capability it derives from — the receiver in method form
/// (`fs.read_only()`) or the leading positional argument in
/// free-function form (`fsmod.scoped_to(fsmod.read_only(fs), p)`). The
/// capability source is the root of the chain — the leftmost binding
/// name.
pub(super) fn capability_source(expr: &HirExpr) -> Option<Symbol> {
    capability_source_depth(expr, CAPABILITY_DERIVATION_DEPTH)
}

fn capability_source_depth(expr: &HirExpr, depth: usize) -> Option<Symbol> {
    if depth == 0 {
        return None;
    }
    match &expr.kind {
        HirExprKind::Path(p) if p.segments.len() == 1 => Some(p.segments[0].name),
        HirExprKind::Field { receiver, .. } => {
            capability_source_depth(receiver, depth - 1)
        }
        // A capability produced by a narrowing call derives from the
        // capability it narrows: the receiver in method form
        // (`fs.read_only()`), or the leading positional argument in
        // free-function form (`fsmod.scoped_to(fsmod.read_only(fs), p)`).
        // Every locked narrowing method takes its source capability in
        // that slot, so tracing it lets an inline narrowing chain resolve
        // to its ambient root parameter.
        HirExprKind::MethodCall { receiver, .. } => {
            capability_source_depth(receiver, depth - 1)
        }
        HirExprKind::Call { callee, args } => match &callee.kind {
            HirExprKind::Field { receiver, .. } => {
                capability_source_depth(receiver, depth - 1)
            }
            _ => capability_source_depth(&args.first()?.expr, depth - 1),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::Interner;
    use edda_syntax::ast::Ident;
    use edda_types::{EffectRow, HirPath, Param, TyInterner};

    fn ident(interner: &Interner, name: &str) -> Ident {
        Ident {
            name: interner.intern(name),
            span: Span::DUMMY,
        }
    }

    fn hir_path(interner: &Interner, name: &str, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Path(HirPath {
                span: Span::DUMMY,
                segments: Box::from([ident(interner, name)]),
            }),
        }
    }

    fn hir_field(receiver: HirExpr, interner: &Interner, name: &str, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Field {
                receiver: Box::new(receiver),
                name: ident(interner, name),
            },
        }
    }

    fn hir_call_arg(expr: HirExpr) -> HirCallArg {
        HirCallArg {
            span: Span::DUMMY,
            mode: None,
            name: None,
            expr,
        }
    }

    fn param(interner: &Interner, name: &str, ty: &TyInterner) -> Param {
        Param {
            span: Span::DUMMY,
            name: interner.intern(name),
            mode: edda_types::ParamMode::Default,
            ty: ty.error(),
        }
    }

    fn sig_with(params: Vec<Param>, effects: EffectRow) -> FnSig {
        FnSig {
            params: params.into_boxed_slice(),
            return_ty: TyInterner::new().error(),
            return_mode: edda_types::ReturnMode::ByValue,
            effects,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        }
    }

    #[test]
    fn extern_capability_order_follows_declared_params_not_canonical_row() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        // Intern `allocator` FIRST so the canonical (Symbol-Ord) row order
        // puts it ahead of `rfs` — the opposite of the declared parameter
        // order, mirroring `fs.read_to_string(rfs, path, allocator)` with
        // `with {rfs, allocator}`.
        let allocator = interner.intern("allocator");
        let rfs = interner.intern("rfs");
        let sig = sig_with(
            vec![
                param(&interner, "rfs", &ty),
                param(&interner, "path", &ty),
                param(&interner, "allocator", &ty),
            ],
            EffectRow::from_entries([
                EffectEntry::Capability(rfs),
                EffectEntry::Capability(allocator),
            ]),
        );
        assert_eq!(
            capability_thread_order(&sig, false),
            vec![allocator, rfs],
            "body-backed callees keep the canonical row order",
        );
        assert_eq!(
            capability_thread_order(&sig, true),
            vec![rfs, allocator],
            "extern-bodied callees thread in declared-parameter order",
        );
    }

    #[test]
    fn capability_source_resolves_single_segment_path() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let world = hir_path(&interner, "world", &ty);
        let expected = interner.intern("world");
        assert_eq!(capability_source(&world), Some(expected));
    }

    #[test]
    fn capability_source_resolves_field_chain_root() {
        // `world.network.local_addr` → leftmost root is `world`.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let world = hir_path(&interner, "world", &ty);
        let world_sym = interner.intern("world");
        let net = hir_field(world, &interner, "network", &ty);
        let leaf = hir_field(net, &interner, "local_addr", &ty);
        assert_eq!(capability_source(&leaf), Some(world_sym));
    }

    #[test]
    fn capability_source_returns_none_for_multi_segment_module_path() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let expr = HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Path(HirPath {
                span: Span::DUMMY,
                segments: Box::from([
                    ident(&interner, "std"),
                    ident(&interner, "io"),
                ]),
            }),
        };
        assert_eq!(capability_source(&expr), None);
    }

    #[test]
    fn capability_source_recursion_bounded() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let root_sym = interner.intern("root");
        let mut expr = hir_path(&interner, "root", &ty);
        for i in 0..(CAPABILITY_DERIVATION_DEPTH - 1) {
            expr = hir_field(expr, &interner, &format!("f{i}"), &ty);
        }
        assert_eq!(capability_source(&expr), Some(root_sym));
        // One more level pushes past the bound.
        expr = hir_field(expr, &interner, "fbeyond", &ty);
        assert_eq!(capability_source(&expr), None);
    }

    #[test]
    fn resolve_caller_source_returns_arg_root_for_direct_call() {
        // Callee parameter is named
        // `random` (shadowing the imported module's leaf); caller passes
        // `rng` (its own capability binding). Without param→arg walking,
        // lowering looked up `random` in caller-side `ctx.capabilities`
        // and crashed with `UnknownCapability`. After the fix, the walk
        // routes to `rng` — the caller's actual binding.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let random_sym = interner.intern("random");
        let rng_sym = interner.intern("rng");

        let params = vec![param(&interner, "random", &ty)];
        let args = vec![hir_call_arg(hir_path(&interner, "rng", &ty))];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(random_sym)]),
        );

        assert_eq!(
            resolve_caller_source(&sig, None, &args, random_sym),
            Some(rng_sym),
        );
    }

    #[test]
    fn resolve_caller_source_uses_receiver_for_method_call_param_zero() {
        // `rng.fill(buf)` after method-call desugaring: receiver = `rng`,
        // args[0] = `buf`. Param[0] is the callee's `random: Random` slot,
        // so `random` resolves to `rng` via the receiver.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let random_sym = interner.intern("random");
        let rng_sym = interner.intern("rng");

        let params = vec![
            param(&interner, "random", &ty),
            param(&interner, "buf", &ty),
        ];
        let receiver = hir_path(&interner, "rng", &ty);
        let buf_arg = hir_call_arg(hir_path(&interner, "buf", &ty));
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(random_sym)]),
        );

        assert_eq!(
            resolve_caller_source(&sig, Some(&receiver), &[buf_arg], random_sym),
            Some(rng_sym),
        );
    }

    #[test]
    fn resolve_caller_source_resolves_field_arg_to_root() {
        // Callee parameter `fs: Filesystem`; caller passes `world.fs`.
        // The capability source is `world` — derived bindings shadow
        // the parameter name (`effect-tracking.md §2`).
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");
        let world_sym = interner.intern("world");

        let params = vec![param(&interner, "fs", &ty)];
        let world = hir_path(&interner, "world", &ty);
        let world_fs = hir_field(world, &interner, "fs", &ty);
        let args = vec![hir_call_arg(world_fs)];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(fs_sym)]),
        );

        assert_eq!(
            resolve_caller_source(&sig, None, &args, fs_sym),
            Some(world_sym),
        );
    }

    #[test]
    fn resolve_caller_source_returns_none_for_untraceable_arg() {
        // Computed arg (literal int) — no statically-derivable
        // capability source. Caller of `resolve_caller_source` falls
        // back to the callee-name lookup.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");

        let params = vec![param(&interner, "fs", &ty)];
        let lit = HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Literal(edda_syntax::ast::Literal::Int {
                value: 0,
                base: edda_syntax::IntBase::Dec,
            }),
        };
        let args = vec![hir_call_arg(lit)];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(fs_sym)]),
        );

        assert_eq!(resolve_caller_source(&sig, None, &args, fs_sym), None);
    }

    #[test]
    fn resolve_caller_source_returns_none_for_unknown_callee_param() {
        // Callee row mentions a name not in `params` — malformed row.
        // Walker returns `None`; the caller's lookup falls back to the
        // raw callee name.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let bogus_sym = interner.intern("notaparam");

        let params = vec![param(&interner, "fs", &ty)];
        let args = vec![hir_call_arg(hir_path(&interner, "fs", &ty))];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(bogus_sym)]),
        );

        assert_eq!(resolve_caller_source(&sig, None, &args, bogus_sym), None);
    }

    #[test]
    fn resolve_caller_source_preserves_same_name_pass_through() {
        // Callee `function read(fs: Filesystem, ...)` with row `{fs}`;
        // caller passes its own `fs` binding. Walker returns the caller-
        // side symbol — which happens to be the same `fs` symbol. The
        // pre-fix behaviour (looking up the raw callee name) and the new
        // walked-source behaviour produce identical results for this
        // shape.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");

        let params = vec![param(&interner, "fs", &ty)];
        let args = vec![hir_call_arg(hir_path(&interner, "fs", &ty))];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(fs_sym)]),
        );

        assert_eq!(
            resolve_caller_source(&sig, None, &args, fs_sym),
            Some(fs_sym),
        );
    }

    fn lit_int(ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Literal(edda_syntax::ast::Literal::Int {
                value: 0,
                base: edda_syntax::IntBase::Dec,
            }),
        }
    }

    fn hir_call(callee: HirExpr, args: Vec<HirCallArg>, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Call {
                callee: Box::new(callee),
                args: args.into_boxed_slice(),
            },
        }
    }

    fn hir_method_call(
        receiver: HirExpr,
        interner: &Interner,
        name: &str,
        args: Vec<HirCallArg>,
        ty: &TyInterner,
    ) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::MethodCall {
                receiver: Box::new(receiver),
                name: ident(interner, name),
                args: args.into_boxed_slice(),
            },
        }
    }

    #[test]
    fn capability_source_traces_inline_narrowing_chain() {
        // `scoped_to(read_only(fs), 0)` (free-function form) and
        // `fs.read_only().scoped_to(0)` (method form) both root at `fs`
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");

        let read_only = hir_call(
            hir_path(&interner, "read_only", &ty),
            vec![hir_call_arg(hir_path(&interner, "fs", &ty))],
            &ty,
        );
        let scoped = hir_call(
            hir_path(&interner, "scoped_to", &ty),
            vec![hir_call_arg(read_only), hir_call_arg(lit_int(&ty))],
            &ty,
        );
        assert_eq!(capability_source(&scoped), Some(fs_sym));

        let ro_m = hir_method_call(hir_path(&interner, "fs", &ty), &interner, "read_only", vec![], &ty);
        let sc_m = hir_method_call(ro_m, &interner, "scoped_to", vec![hir_call_arg(lit_int(&ty))], &ty);
        assert_eq!(capability_source(&sc_m), Some(fs_sym));
    }

    #[test]
    fn resolve_caller_source_traces_inline_narrowing_chain_to_root() {
        // Callee `path_exists(rfs: ReadOnlyFilesystem, path)` with row
        // `{rfs}`; the caller passes an inline narrowing chain
        // `scoped_to(read_only(fs), 0)` as the capability argument. The
        // resolved caller-side source is the ambient root `fs`, not the
        // callee's own `rfs` parameter name.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let rfs_sym = interner.intern("rfs");
        let fs_sym = interner.intern("fs");

        let read_only = hir_call(
            hir_path(&interner, "read_only", &ty),
            vec![hir_call_arg(hir_path(&interner, "fs", &ty))],
            &ty,
        );
        let scoped = hir_call(
            hir_path(&interner, "scoped_to", &ty),
            vec![hir_call_arg(read_only), hir_call_arg(lit_int(&ty))],
            &ty,
        );
        let params = vec![param(&interner, "rfs", &ty), param(&interner, "path", &ty)];
        let args = vec![hir_call_arg(scoped), hir_call_arg(lit_int(&ty))];
        let sig = sig_with(
            params,
            EffectRow::from_entries([EffectEntry::Capability(rfs_sym)]),
        );

        assert_eq!(resolve_caller_source(&sig, None, &args, rfs_sym), Some(fs_sym));
    }
}
