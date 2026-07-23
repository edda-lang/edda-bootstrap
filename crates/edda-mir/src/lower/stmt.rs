//! Statement dispatch for the typed-HIR -> MIR lowering pass.
//!
//! - `Let { pat, ty, init }` allocates a fresh user local for the initialiser
//!   (when present) and installs the irrefutable pattern bindings against it.
//!   Annotated-but-uninitialised lets (`var x: T`) allocate an uninitialised
//!   local for the binding and emit a `StorageLive` to mark its scope.
//! - `Assign { target, op, rhs }` lowers compound assignment via the same
//!   `BinOp` rvalue used by binary expressions, gated on a single-segment-path
//!   LHS for now.
//! - `Expr(expr)` lowers the expression purely for side-effects.

use edda_span::Span;
use edda_syntax::ast::{AssignOp, BindingMode};
use edda_types::{HirExpr, HirExprKind, HirPat, HirPatKind, HirStmt, HirStmtKind, TyKind};

use crate::adt::AdtKind;
use crate::body::Mutability;
use crate::error::{LoweringError, MirError};
use crate::ids::LocalId;
use crate::operand::Operand;
use crate::place::{Place, Projection};
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::ty::{MirPrim, MirType};

use super::adt_ops::{resolve_nominal_adt, resolve_product_field};
use super::args::capability_source_of_call;
use super::ctx::LoweringContext;
use super::expr::lower_expr_to_operand;
use super::cfg::{assign_into, push_assign, push_assign_place};
use super::scope::push_storage_live;
use super::pattern::install_bindings;
use super::ty::{lower_ty, ty_to_prim};

// TODO: emit StorageLive/StorageDead for compiler temps. Currently this
// emits these only for user `let` bindings via `lower_let_*`; mid-expression
// temps (binary-op results, `loop_value` slots, if/else / match join
// temps) remain un-marked.

/// Lower a single statement.
pub(super) fn lower_stmt(ctx: &mut LoweringContext<'_>, stmt: &HirStmt) {
    match &stmt.kind {
        HirStmtKind::Let { mutability, pat, ty, init } => {
            lower_let(ctx, stmt.span, *mutability, pat, ty.as_ref(), init.as_ref());
        }
        HirStmtKind::Assign { target, op, rhs } => {
            lower_assign(ctx, stmt.span, target, *op, rhs);
        }
        HirStmtKind::Expr(expr) => {
            let _ = lower_expr_to_operand(ctx, expr);
        }
    }
}

/// Lower `let pat [: T] [= init]` — allocate the user local(s) and install
/// the pattern's bindings.
fn lower_let(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    mutability: BindingMode,
    pat: &HirPat,
    annotated_ty: Option<&edda_types::TyId>,
    init: Option<&HirExpr>,
) {
    let _ = annotated_ty; // mirror diagnostics already emitted by `edda-types`
    match init {
        Some(init_expr) => lower_let_initialised(ctx, span, mutability, pat, init_expr),
        None => lower_let_uninitialised(ctx, mutability, pat),
    }
}

/// `let pat = init;` — allocate a user local seeded from `init` and install
/// the pattern bindings against it.
fn lower_let_initialised(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    mutability: BindingMode,
    pat: &HirPat,
    init: &HirExpr,
) {
    let init_op = lower_expr_to_operand(ctx, init);
    let ty = if matches!(init.kind, HirExprKind::Spawn(_)) {
        MirType::prim(MirPrim::HeapPtr)
    } else {
        lower_ty(ctx.ty_interner, &ctx.adt_map, init.ty)
    };
    let mut_kind = map_binding_mode(mutability);
    let binder = primary_binder(pat);
    let (local, is_user_local) = match ctx.body.as_mut() {
        Some(body) => match binder {
            Some(name) => (body.user_local(name, mut_kind, ty.clone(), span), true),
            None => (body.temp(ty.clone(), span), false),
        },
        None => return,
    };
    if is_user_local {
        push_storage_live(ctx, span, local);
    }
    assign_into(ctx, span, local, init_op, ty);
    install_bindings(ctx, pat, local);
    record_capability_binding(ctx, pat, init, local);
}

/// Record a capability-typed `let` binding: alias for narrowings,
/// value-bearing local slot for forked children.
fn record_capability_binding(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    init: &HirExpr,
    local: LocalId,
) {
    if !matches!(ctx.ty_interner.kind(init.ty), TyKind::Capability(_)) {
        return;
    }
    let Some(name) = primary_binder(pat) else {
        return;
    };
    // `let child = allocator.fork()` mints a DISTINCT capability value
    // (a fresh heap), never an alias of the receiver: register the
    // bound local as a value-bearing capability slot so call-site
    // threading loads the child's handle. Aliasing it to the parent
    // would silently route every child allocation (and the eventual
    // heap_destroy) to the parent heap.
    // `let det = random.deterministic(seed)` is the same shape: the
    // value is the splitmix64 state handle, distinct from the ambient
    // Random slot.
    match ctx.capability_method_calls.get(&init.span) {
        Some(edda_types::CapabilityMethod::AllocatorFork) => {
            register_local_capability(ctx, name, local, crate::effect::CapabilityKind::Allocator);
            return;
        }
        Some(edda_types::CapabilityMethod::RandomDeterministic) => {
            register_local_capability(
                ctx,
                name,
                local,
                crate::effect::CapabilityKind::Typed(
                    edda_types::CapabilityType::DeterministicRandom,
                ),
            );
            return;
        }
        _ => {}
    }
    // `let allocator = alloc.fork(allocator)` — the module-fn spelling
    // of fork, dispatched through `std.mem.alloc.fork`'s extern binding
    // rather than the capability-method catalogue. Same distinct-value
    // semantics as the receiver-method arm above: the minted handle
    // must be a value-bearing slot, never an alias, or every downstream
    // use silently threads the parent's handle and `alloc.close`
    // destroys the parent heap.
    if let Some(kind) = minted_capability_extern(ctx, init) {
        register_local_capability(ctx, name, local, kind);
        return;
    }
    if let Some(root) = capability_source_of_call(init) {
        ctx.capability_aliases.insert(name, root);
    }
}

/// The minted capability kind when `init` calls a distinct-value
/// capability extern through its module-fn (or resolved method-call)
/// spelling. The capability-method-catalogue spellings are handled by
/// `record_capability_binding`'s explicit arms; this covers the same
/// runtime symbols reached via `std.mem.alloc.fork` /
/// `std.os.random`-style declared externs.
fn minted_capability_extern(
    ctx: &LoweringContext<'_>,
    init: &HirExpr,
) -> Option<crate::effect::CapabilityKind> {
    let binding = match &init.kind {
        HirExprKind::Call { callee, .. } => super::call::try_resolve_function_binding(ctx, callee)?,
        HirExprKind::MethodCall { .. } => ctx.method_resolutions.get(&init.span).copied()?,
        _ => return None,
    };
    let (extern_sym, _) = ctx.function_externs.get(&binding)?;
    match ctx.interner.resolve(*extern_sym) {
        "__edda_heap_fork" => Some(crate::effect::CapabilityKind::Allocator),
        "__edda_random_deterministic" => Some(crate::effect::CapabilityKind::Typed(
            edda_types::CapabilityType::DeterministicRandom,
        )),
        _ => None,
    }
}

/// Register a body-local capability value (a forked child allocator, a
/// deterministic-random state handle) as a threading slot keyed by its
/// binding name.
pub(super) fn register_local_capability(
    ctx: &mut LoweringContext<'_>,
    name: edda_intern::Symbol,
    local: LocalId,
    kind: crate::effect::CapabilityKind,
) {
    let Some(body) = ctx.body.as_mut() else { return };
    let mir_body = body.body_mut();
    let id = crate::ids::EffectId::from_raw(
        (mir_body.effect_row.capabilities.len() + mir_body.local_capabilities.len()) as u32,
    );
    mir_body.local_capabilities.push(crate::effect::CapabilitySlot {
        id,
        param_local: local,
        ty: kind,
    });
    ctx.capabilities.insert(name, id);
}

/// `var x: T;` — allocate an uninitialised mutable local for the binding so
/// later assignments and `init`-mode writes have a target.
fn lower_let_uninitialised(
    ctx: &mut LoweringContext<'_>,
    mutability: BindingMode,
    pat: &HirPat,
) {
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, pat.ty);
    let mut_kind = map_binding_mode(mutability);
    let Some(name) = primary_binder(pat) else {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedPattern {
            kind: "non-binding pattern in uninitialised let",
            span: pat.span,
        }));
        return;
    };
    let local = {
        let Some(body_builder) = ctx.body.as_mut() else { return };
        body_builder.user_local(name, mut_kind, ty, pat.span)
    };
    push_storage_live(ctx, pat.span, local);
    ctx.bindings.insert(name, local);
}

/// Map [`BindingMode`] to the MIR mutability kind.
fn map_binding_mode(mode: BindingMode) -> Mutability {
    match mode {
        BindingMode::Immutable => Mutability::Imm,
        // `uninit` slots are filled by an `init`-mode call, then read — semantically mutable storage.
        BindingMode::Mutable | BindingMode::Uninit => Mutability::Mut,
    }
}

/// Return the top-level binder name when the pattern is a single
/// `Binding`; `None` for everything else.
fn primary_binder(pat: &HirPat) -> Option<edda_intern::Symbol> {
    match &pat.kind {
        HirPatKind::Binding(ident) => Some(ident.name),
        _ => None,
    }
}

//            effects in `Index` index expressions land in source order
/// Lower `target op rhs;`. Admits single-segment-path LHS plus
/// `Field` / `Index` projection chains rooted at one.
fn lower_assign(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    target: &HirExpr,
    op: AssignOp,
    rhs: &HirExpr,
) {
    let Some(dest_place) = resolve_place(ctx, target) else {
        return;
    };
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, target.ty);
    let rvalue = match op {
        AssignOp::Plain => Rvalue {
            span,
            kind: RvalueKind::Use(rhs_op),
            ty: ty.clone(),
        },
        _ => {
            let prim = ty_to_prim(ctx.ty_interner, target.ty).unwrap_or(MirPrim::I64);
            Rvalue {
                span,
                kind: RvalueKind::BinOp {
                    op: compound_to_binop(op),
                    lhs: Operand::Copy(dest_place.clone()),
                    rhs: rhs_op,
                    prim,
                },
                ty: ty.clone(),
            }
        }
    };
    let _ = ty;
    push_assign_place(ctx, span, dest_place, rvalue);
}

//            outer-to-inner walk: outermost `Field` / `Index` becomes the
//            *last* projection step (left-to-right pointer walk)
/// Resolve an lvalue expression to the [`Place`] it names.
///
/// Two callers: assignment targets (the LHS of `target = rhs`) and
/// `mutable` / `init` call arguments — whose write-back requires the
/// caller's actual place (with its projection chain), not a value-temp.
/// Lowering a by-reference arg to a temp would silently drop the
/// callee's writes.
///
/// Admitted shapes:
/// - `HirExprKind::Path([name])` — names a binding in `ctx.bindings`.
/// - `HirExprKind::Field { receiver, name }` — projects a product-ADT
///   field; appends `Projection::Field(idx)` to the receiver's place.
/// - `HirExprKind::TupleIndex { receiver, index }` — projects a tuple
///   element; appends the positional `Projection::Field(idx)`.
/// - `HirExprKind::Index { receiver, index }` — projects a slice
///   element; appends `Projection::Index(idx_local)` to the receiver's
///   place. The index expression is lowered and bound to a local if it
///   does not already evaluate to a projection-free operand.
///
/// - `HirExprKind::Call` / `HirExprKind::MethodCall` — a return-position
///   borrow (`-> let T` / `-> mutable T`). The call lowers to a value
///   that is a `Projection::Deref` place over the returned pointer
///   (see [`super::call::lower_call_to_binding`]); this arm hands that
///   place back so a `mutable at_mut(...).preds` argument or an
///   `at_mut(...).field = v` assignment threads the write through the
///   pointer to the borrowed storage.
///
/// Any other top-level shape — a bare multi-segment path, struct
/// literal, etc. — surfaces `UnsupportedAssignTarget`. Sum-typed Field
/// receivers surface `UnsupportedHirVariant`; non-slice Index receivers
/// surface the same. Unknown bindings surface `UnknownBinding`.
pub(super) fn resolve_place(
    ctx: &mut LoweringContext<'_>,
    target: &HirExpr,
) -> Option<Place> {
    match &target.kind {
        HirExprKind::Path(path) => {
            if path.segments.len() != 1 {
                ctx.errors.push(MirError::from(LoweringError::MultiSegmentPath {
                    span: target.span,
                }));
                return None;
            }
            let name = path.segments[0].name;
            match ctx.bindings.get(&name) {
                Some(local) => Some(Place::local(*local)),
                None => {
                    ctx.errors.push(MirError::from(LoweringError::UnknownBinding {
                        name,
                        span: target.span,
                    }));
                    None
                }
            }
        }
        HirExprKind::Field { receiver, name } => {
            let mut place = resolve_place(ctx, receiver)?;
            let (adt_id, adt_kind) = resolve_nominal_adt(ctx, receiver.ty, receiver.span)?;
            if adt_kind != AdtKind::Product {
                ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                    variant: "Field on sum type (assignment target)",
                    span: target.span,
                }));
                return None;
            }
            let (field_idx, _field_ty) =
                resolve_product_field(ctx, adt_id, name.name, target.span)?;
            place.projection.push(Projection::Field(field_idx));
            Some(place)
        }
        HirExprKind::TupleIndex { receiver, index } => {
            let mut place = resolve_place(ctx, receiver)?;
            if !matches!(ctx.ty_interner.kind(receiver.ty), TyKind::Tuple(_)) {
                ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                    variant: "TupleIndex on non-tuple receiver (assignment target)",
                    span: target.span,
                }));
                return None;
            }
            place
                .projection
                .push(Projection::Field(crate::ids::FieldIdx::from_raw(*index)));
            Some(place)
        }
        HirExprKind::Index { receiver, index } => {
            let mut place = resolve_place(ctx, receiver)?;
            // Confirm the receiver type is a slice; non-slice receivers
            // (tuples, ADTs) would need a different projection shape.
            if !matches!(ctx.ty_interner.kind(receiver.ty), TyKind::Slice(_)) {
                ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                    variant: "Index (non-slice receiver) (assignment target)",
                    span: target.span,
                }));
                return None;
            }
            let idx_local = lower_index_to_local(ctx, index)?;
            place.projection.push(Projection::Index(idx_local));
            Some(place)
        }
        HirExprKind::Call { .. } | HirExprKind::MethodCall { .. } => {
            // A return-position borrow (`-> let/mutable T`) lowers to a
            // call whose value is a `Projection::Deref` place over the
            // returned pointer; surfacing that place (rather than a
            // value temp) threads a `mutable` write back through the
            // pointer to the borrowed storage. A by-value call result
            // has a projection-free place — harmless here; the
            // return-borrow region check is what admits a call result
            // in lvalue position at all.
            match lower_expr_to_operand(ctx, target) {
                Operand::Copy(place) | Operand::Move(place) => Some(place),
                _ => {
                    ctx.errors.push(MirError::from(LoweringError::UnsupportedAssignTarget {
                        span: target.span,
                    }));
                    None
                }
            }
        }
        _ => {
            ctx.errors.push(MirError::from(LoweringError::UnsupportedAssignTarget {
                span: target.span,
            }));
            None
        }
    }
}

//            projections) — suitable as the `LocalId` payload of
//            `Projection::Index`
/// Lower an index expression and materialise it into a bare local.
/// A projection-free `Copy`/`Move` short-circuits to its underlying
/// local; anything else (constants, computed values, projected reads)
/// is spilled into a fresh temp so the [`Projection::Index`] payload
/// names a real local.
fn lower_index_to_local(
    ctx: &mut LoweringContext<'_>,
    index: &HirExpr,
) -> Option<LocalId> {
    let op = lower_expr_to_operand(ctx, index);
    if let Operand::Copy(p) | Operand::Move(p) = &op
        && p.projection.is_empty()
    {
        return Some(p.local);
    }
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, index.ty);
    let temp = ctx.body.as_mut()?.temp(ty.clone(), index.span);
    push_assign(
        ctx,
        index.span,
        temp,
        Rvalue {
            span: index.span,
            kind: RvalueKind::Use(op),
            ty,
        },
    );
    Some(temp)
}

/// Map a compound [`AssignOp`] to the matching [`BinOp`]. `Plain` is
/// handled by the caller — calling this on `Plain` panics.
fn compound_to_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Plain => unreachable!("compound_to_binop called on Plain"),
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
    }
}

