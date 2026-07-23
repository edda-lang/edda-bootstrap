//! Input types presented to the [`super::lower`] entry point.
//!
//! These structs are the lowering pass's public seam from upstream
//! typecheck (`edda-types`) and the driver. Re-exported from `lower/mod.rs`
//! so external paths (`edda_mir::LoweringInput` etc.) stay stable.

use std::collections::HashMap;

use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, Resolutions};
use edda_span::Span;
use edda_target::TargetCfg;
use edda_types::{
    CapabilityMethod, ComptimeBuiltin, FnSig, HirBlock, IntrinsicKind, PrimitiveStaticMethod,
    TyCx, TyId, TyInterner, TypeDeclInfo,
};

use crate::layout::{AbiTag, AlignBytes, LayoutPolicy, ReprKind};

/// One function presented to the lowering pass: its resolver-side binding
/// id, name, signature, and typed-HIR body.
pub struct FunctionInput<'a> {
    /// Resolver-issued binding id for this function. Threads through into
    /// `ctx.function_map` so `Call` lowering can look up the
    /// callee's [`crate::BodyId`].
    pub binding: BindingId,
    /// Source-declared function name (the `Symbol` issued by the interner
    /// shared with the rest of the compilation unit).
    pub name: Symbol,
    /// Defining span of the function header.
    pub span: Span,
    /// Lowered signature (params, return type, effect row).
    pub sig: &'a FnSig,
    /// Typed-HIR body block.
    pub body: &'a HirBlock,
    /// Linker-visible symbol name from an `@abi("name")` symbol override,
    /// if any. Threads onto the resulting
    /// [`crate::Body::export_symbol`].
    pub export_symbol: Option<Symbol>,
    /// Calling-convention override from `@abi("...")`, if any. Threads
    /// onto the resulting [`crate::Body::abi`].
    pub abi: Option<AbiTag>,
    /// Deterministic module-qualified symbol name (`<module.path>.<leaf>`,
    /// interned) the driver computed from this function's canonical module
    /// path. Threads onto [`crate::Body::qualified_name`] so codegen emits
    /// it with `linkonce_odr` + COMDAT. `None` only when the driver could
    /// not resolve the binding's module path.
    pub qualified_name: Option<Symbol>,
}

/// One user-declared `type` presented to the lowering pass.
pub struct TypeDeclInput<'a> {
    /// Resolver-issued binding id for this type declaration.
    pub binding: BindingId,
    /// Source-declared type name.
    pub name: Symbol,
    /// Borrowed layout information from the upstream `TyCx`.
    pub info: &'a TypeDeclInfo,
    /// Explicit alignment override from `@align(N)`, if any.
    pub align: Option<AlignBytes>,
    /// Memory-representation override from `@repr(K)`, if any.
    pub repr: Option<ReprKind>,
    /// Field-ordering policy from `@layout(P)`, if any.
    pub layout: Option<LayoutPolicy>,
    /// When `true`, MIR lowering synthesises a hidden `ptr: HeapPtr`
    /// field on the resulting [`crate::AdtDef`]. Set by the driver for
    /// the `type Box {}` declared inside a `Box_<T>`-named module
    /// generated from `spec std.mem.alloc.Box(T)` — the source-side type
    /// is empty by necessity (the parser does not admit
    /// `HeapPtr<T>` in field position) and MIR is the layer that gives
    /// the opaque heap pointer a typed storage slot.
    pub synthesize_box_ptr: bool,
}

/// Pre-resolved `BindingId`s for the `std.fmt` functions fstring lowering
/// targets. The driver populates these by querying the resolved package
/// for `std.fmt.format_i64`, `std.fmt.format_u64`, etc. Lowering uses
/// them so an `f"{x}"` slot lowers to a `Call` against the stdlib's
/// `format_<T>` binding (which itself carries the `extern "__edda_*"`
/// declaration) instead of a hardcoded `FuncRef::Extern`.
///
/// All fields default to `None`. When a field is `None`, the fstring
/// lowering falls back to emitting the legacy hardcoded `__edda_*`
/// extern symbol directly so builds work before the extern feature has
/// landed in the source language and `std.fmt` declares the bindings.
#[derive(Clone, Copy, Debug, Default)]
pub struct FmtBindings {
    /// `std.fmt.format_i64` — handles `i8..i64` / `isize` after widening.
    pub format_i64: Option<BindingId>,
    /// `std.fmt.format_u64` — handles `u8..u64` / `usize` after widening.
    pub format_u64: Option<BindingId>,
    /// `std.fmt.format_f64` — handles `f32` / `f64`.
    pub format_f64: Option<BindingId>,
    /// `std.fmt.format_bool`.
    pub format_bool: Option<BindingId>,
    /// `std.fmt.format_str` — pass-through for already-`String` slots.
    pub format_str: Option<BindingId>,
    /// `std.fmt.string_concat` — fstring fold target.
    pub string_concat: Option<BindingId>,
}

/// Pre-resolved `BindingId`s for the allocator-taking pure-Edda bodies
/// f-string number/bool interpolation and string concatenation retarget
/// to when the enclosing function holds its own `Allocator` capability
/// and already admits `err: alloc.AllocError` in its row. Mirrors the
/// native compiler's design: additive, not a hard
/// requirement — when a field is `None`, or the enclosing function
/// lacks the capability/row admission, lowering falls back unchanged to
/// the [`FmtBindings`] / hardcoded-extern path.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllocFmtBindings {
    /// `std.core.fmt.i64_to_string(n: i64, allocator: Allocator) -> String`.
    pub i64_to_string: Option<BindingId>,
    /// `std.core.fmt.u64_to_string(n: u64, allocator: Allocator) -> String`.
    pub u64_to_string: Option<BindingId>,
    /// `std.core.fmt.f64_to_string(x: f64, allocator: Allocator) -> String`.
    pub f64_to_string: Option<BindingId>,
    /// `std.core.fmt.bool_to_string(b: bool, allocator: Allocator) -> String`.
    pub bool_to_string: Option<BindingId>,
    /// `std.text.string.concat(a: String, b: String, allocator: Allocator) -> String`.
    pub concat: Option<BindingId>,
}

/// One extern-declared function presented to the lowering pass.
///
/// Extern declarations have no Edda-side body. The pass records the
/// `binding -> (symbol, sig)` pair in `ctx.function_externs` so call
/// lowering can emit a [`crate::FuncRef::Extern`] terminator at every
/// site that resolves to one of these bindings.
pub struct ExternInput<'a> {
    /// Resolver-issued binding id for this extern declaration.
    pub binding: BindingId,
    /// Source-declared function name (the in-language identifier
    /// callers reference; not the linker symbol).
    pub name: Symbol,
    /// Defining span of the function header (used for diagnostics
    /// raised during call-site lowering).
    pub span: Span,
    /// Linker-visible extern symbol — what the call-site
    /// [`crate::FuncRef::Extern`] will name.
    pub symbol: Symbol,
    /// Lowered signature (params, return type, effect row).
    pub sig: &'a FnSig,
}

/// The full input to one [`super::lower`] call.
pub struct LoweringInput<'a> {
    /// Symbol interner used by every `Symbol` carried in `functions` and
    /// `type_decls`.
    pub interner: &'a Interner,
    /// Type interner used by every `TyId` carried in `functions` and
    /// `type_decls`.
    pub ty_interner: &'a TyInterner,
    /// Resolver-produced span-keyed resolution map. Read by the call /
    /// path lowering to translate multi-segment paths into the
    /// resolver-issued `BindingId` they refer to (cross-module function
    /// calls and variant constructors both route through this).
    pub resolutions: &'a Resolutions,
    /// Typechecker-produced MethodCall span -> free-function
    /// `BindingId` map. Read by `lower_method_call` to desugar each
    /// `HirExprKind::MethodCall` into a `Call` whose first argument is
    /// the method receiver.
    pub method_resolutions: &'a HashMap<Span, BindingId>,
    /// Typechecker-produced intrinsic method call map. Keyed by the
    /// same span as `method_resolutions`. Read by the `MethodCall` arm
    /// in `lower_expr_to_operand` to emit rvalues instead of `Call`
    /// terminators for compiler-intrinsic methods (`bytes` on `String`,
    /// `len` on slices).
    pub intrinsic_calls: &'a HashMap<Span, IntrinsicKind>,
    /// User-declared type decls (product + sum ADTs) to register, in
    /// declaration order. Lowered to [`crate::AdtDef`]s before any
    /// function body is walked so nominal type references resolve.
    pub type_decls: &'a [TypeDeclInput<'a>],
    /// Function bodies to lower, in declaration order.
    pub functions: &'a [FunctionInput<'a>],
    /// Pre-resolved `std.fmt` bindings the fstring lowering targets.
    /// Default-constructed (`FmtBindings::default()`) skips the routing
    /// and keeps the legacy hardcoded extern emission.
    pub fmt_bindings: FmtBindings,
    /// Pre-resolved allocator-taking pure-Edda retarget bindings. See
    /// [`AllocFmtBindings`]. Default-constructed
    /// (`AllocFmtBindings::default()`) skips the retarget entirely.
    pub alloc_fmt_bindings: AllocFmtBindings,
    /// `std.mem.alloc.AllocError`'s resolver-side `BindingId`. Used to
    /// recognise, via `ctx.adt_map`, whether the enclosing function's
    /// row already admits `err: alloc.AllocError` — the precondition
    /// (alongside an in-scope `Allocator`) for retargeting through
    /// [`AllocFmtBindings`]. `None` when the driver could not resolve
    /// `std.mem.alloc` (the retarget is then skipped everywhere, same
    /// as an empty `AllocFmtBindings`).
    pub alloc_error: Option<BindingId>,
    /// Extern function declarations to register. Each one becomes a
    /// `BindingId -> (symbol, mir::FnSig)` entry in
    /// `ctx.function_externs`; call-site lowering routes through this
    /// map to emit `FuncRef::Extern` instead of `FuncRef::Body` when
    /// the callee binding refers to an extern.
    pub externs: &'a [ExternInput<'a>],
    /// Module-level `let` constants to pre-intern. Each one becomes
    /// a `BindingId -> ConstId` entry in `ctx.module_consts` so a
    /// single-segment path reference in `lower_path` can emit
    /// `Operand::Const(id)` instead of erroring out with
    /// `UnknownBinding`. Driver folds the AST initialiser to a
    /// [`edda_types::ConstInit`] at typecheck time and projects it
    /// into a [`ConstInput`] here.
    pub consts: &'a [ConstInput],
    /// Target pointer width in bytes. Used by [`super::call`]'s alloc-family
    /// rewrite when threading `size_of(T)` / `align_of(T)` constants
    /// at the call site, and by [`super::layout`] when sizing primitives whose
    /// width depends on the active target (`HeapPtr`, `Usize`, `Isize`).
    /// Driver populates this from `Arch::pointer_width / 8` — `4` on
    /// `wasm32`, `8` on every other v0.1 target.
    pub pointer_width_bytes: u32,
    /// Active build target. Threaded through so MIR lowering of
    /// `HirExprKind::Comptime` can call [`edda_comptime::eval_expr`]
    /// against the active target — `target_has` (§C10) and
    /// pointer-width primitive layout both consume it. When `None`,
    /// the `Comptime` arm falls back to its pre-§C10 "unsupported"
    /// diagnostic so callers that don't yet thread the target in keep
    /// working.
    pub target_cfg: Option<&'a TargetCfg>,
    /// Reference into the upstream `edda_types::TyCx` so MIR lowering
    /// can resolve `TyKind::Nominal(BindingId)` handles to their field
    /// tables when evaluating a comptime `size_of(<user type>)` /
    /// `align_of(<user type>)` (§C10). `None` falls back to the
    /// "nominal layout deferred" error.
    pub ty_cx: Option<&'a TyCx>,
    /// Typechecker-side path-as-type resolution map (§C10). Keyed by
    /// the spans of `HirPath` expressions that named a type-as-value
    /// inside a `comptime` body; values are the concrete `TyId` the
    /// path refers to. Consumed by the `HirExprKind::Comptime` arm
    /// when threading the [`edda_comptime::EvalCx`].
    pub comptime_type_paths: &'a ahash::AHashMap<Span, TyId>,
    /// Typechecker-side built-in call resolution map (§C10). Keyed by
    /// the spans of `HirExprKind::Call` expressions whose callee
    /// resolved to a comptime built-in. MIR lowering does not
    /// dispatch on this directly today — the evaluator re-resolves
    /// from the path text — but the map is threaded through so a
    /// future change can short-circuit name lookup.
    pub comptime_builtin_calls: &'a ahash::AHashMap<Span, ComptimeBuiltin>,
    /// Typechecker-side comptime user-function call resolution map.
    /// Keyed by the spans of direct-call expressions
    /// type-checked inside a `comptime` body; values are the callee's
    /// resolver-issued `BindingId`. The `HirExprKind::Comptime` /
    /// `ComptimeBlock` arms thread this into
    /// [`edda_comptime::EvalCx::with_fn_calls`] together with a
    /// [`edda_comptime::FnDeclLookup`] built over `functions`, so the
    /// evaluator can interpret user-function callees.
    pub comptime_fn_calls: &'a ahash::AHashMap<Span, BindingId>,
    /// Typechecker-side primitive-headed static-method dispatch map
    /// (currently empty — see [`PrimitiveStaticMethod`]). Keyed by the
    /// spans of `HirExprKind::Call` expressions whose callee matched
    /// [`edda_types::resolve_primitive_static_method`]. The lowering
    /// pass reads this in [`super::call::lower_call`] to emit a `Call`
    /// with `FuncRef::Extern` against the variant's `__edda_*` runtime
    /// symbol — the resolver does not record these paths because their
    /// heads are catalogue items.
    pub primitive_static_calls: &'a ahash::AHashMap<Span, PrimitiveStaticMethod>,
    /// Typechecker-side capability-method dispatch map. Keyed by the
    /// spans of `HirExprKind::MethodCall` expressions whose receiver is
    /// a capability type matched by
    /// [`edda_types::resolve_capability_method`]
    /// (e.g. `allocator.alloc_array(T, n)`). MIR lowering reads this in
    /// [`super::call::lower_method_call`] to emit a `Call` with
    /// `FuncRef::Extern` against the variant's `__edda_*` extern name
    /// — the existing alloc-family rewrite then prepends `size_of(T)` /
    /// `align_of(T)` constants. The `T` resolution flows through
    /// [`super::LoweringInput::comptime_type_paths`] keyed by the first
    /// argument's `HirPath` span.
    pub capability_method_calls: &'a ahash::AHashMap<Span, CapabilityMethod>,
    /// Driver-built map from a `derive eq` target type's `BindingId` to
    /// the `BindingId` of the materialised `std.core.compare.eq_<T>.eq`
    /// comparator function. Consumed by [`super::arith::lower_binary`] to
    /// lower `==` / `!=` on a `TyKind::Nominal` operand into a `Call`
    /// against the synthesised structural comparator.
    /// Empty when no in-scope type derives
    /// `eq`; a nominal `==` whose type is absent falls through to the
    /// existing `unsupported_and_unit` lowering error.
    pub eq_comparators: &'a HashMap<BindingId, BindingId>,
    /// Driver-built map from a `derive debug` target type's `BindingId` to
    /// the `BindingId` of the materialised `std.core.fmt.debug_<T>.format`
    /// formatter function. Consumed by the f-string fold
    /// ([`super::fstring_emit`]) to lower an aggregate interpolation slot
    /// (`f"{v}"` where `v` is a nominal that derives `debug`) into a `Call`
    /// against the synthesised structural formatter.
    /// Empty when no in-scope type derives
    /// `debug`; an aggregate slot whose type is absent falls through to the
    /// first-word `format_i64` fallback (prior behaviour).
    pub debug_formatters: &'a HashMap<BindingId, BindingId>,
}

/// One module-level `let` constant presented to the lowering pass.
///
/// Pre-pass `register_consts` interns each `value` into the program
/// once and records the resulting `ConstId` in `ctx.module_consts`
/// keyed by `binding`. Path lowering reads that map to emit
/// `Operand::Const(id)` at every single-segment reference.
pub struct ConstInput {
    /// Resolver-issued binding id for this `let` declaration.
    pub binding: BindingId,
    /// Annotated type of the constant. Lowered to MIR via
    /// `super::ty::lower_ty`.
    pub ty: TyId,
    /// Folded initialiser value the lowering pass interns into
    /// `program.consts`.
    pub value: crate::ConstValue,
}
