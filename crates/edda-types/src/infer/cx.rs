//! Inference-pass context bundle.
//!
//! [`InferCx`] is the read-only-interner + mut-diagnostics shape every
//! `synth_*` / `check_*` helper threads through. It carries the
//! enclosing function's declared row, the running effect accumulator,
//! the in-flight implicit-spec request list, and the post-pass
//! method-resolution / intrinsic-call maps the driver drains into
//! [`crate::TypedPackage`].

use ahash::AHashMap;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;

use crate::cx::TyCx;
use crate::effect::{EffectEntry, EffectRow};
use crate::implicit_spec::{ImplicitSpec, ImplicitSpecRequest};
use crate::lower::LowerCx;
use crate::ty::TyId;

use super::row_acc::EffectAcc;

/// Inference-pass context.
///
/// Bundles the four parameters every inference helper used to take —
/// the [`LowerCx`] (read-only interners + optional resolved package),
/// the [`TyCx`] (typed cross-module surface), the [`LintConfig`]
/// (severity overrides), and the [`Diagnostics`] take — into a
/// single `&mut InferCx`. Reduces every synth / check signature by
/// three parameters and gives helpers a single place to
/// reach for the resolved-package context, the function-signature
/// table, and the type-decl layout table.
///
/// The effect-row plumbing: [`declared_row`] holds the
/// enclosing function's declared row and `row` accumulates row
/// contributions from each sub-walk. [`check_fn_body`] manages both —
/// callers outside `check_fn_body` see the default-constructed empty
/// row, which is correct for rule sites that do not consult the row.
pub(crate) struct InferCx<'a> {
    /// Read-only AST-lowering context. `lower.package` is `Some`
    /// whenever inference runs against a resolved package, which
    /// every production caller does; tests that exercise
    /// simpler surface (literals, blocks, operators) may pass
    /// `package: None` since those rules do not consult it.
    pub lower: &'a LowerCx<'a>,
    /// Typed cross-module surface — function signatures and
    /// type-decl layouts keyed by [`edda_resolve::BindingId`].
    pub ty_cx: &'a TyCx,
    /// Lint configuration controlling diagnostic severities.
    pub lint_cfg: &'a LintConfig,
    /// Diagnostics take. Accessed only via [`InferCx::emit_typecheck_error`];
    /// the field is `pub(super)` so callers cannot bypass severity gating.
    pub(super) diags: &'a mut Diagnostics,
    /// Declared effect row of the enclosing function. Empty when no
    /// function context is active (e.g. an `InferCx` constructed by a
    /// test that exercises rules in isolation). [`check_fn_body`]
    /// installs the function's declared row before walking the body
    /// and restores the previous value on return.
    pub(super) declared_row: EffectRow,
    /// Accumulator that synth / check / infer_stmt push into as they
    /// walk. [`check_fn_body`] resets the accumulator before the body
    /// walk and finalises it after to verify ⊆ `declared_row`.
    pub(super) row: EffectAcc,
    /// Implicit-spec invocation requests recorded by inference per
    /// `inference-rules.md §3`. Range / `none`-pattern uses register
    /// the corresponding `std.range.Range(<T>)` /
    /// `std.option.Option(<T>)` instantiation here; codegen consumes
    /// the list when it materialises generated modules. Deduplicated
    /// by `(kind, type_arg)`.
    pub implicit_specs: Vec<ImplicitSpecRequest>,
    /// `true` while inference is walking the body of a
    /// `HirExprKind::Comptime` or `HirExprKind::ComptimeBlock` node.
    /// `call::synth_call` consults the flag to enforce P-CompTimePure
    /// at every callee site (`inference-rules.md §1a.6`): a callee
    /// whose declared row contains any entry other than `Panic` or
    /// `Yield(_)` triggers a [`DiagnosticClass::ComptimePurityLoss`]
    /// diagnostic. The Comptime arms in `synth_expr` save/restore the
    /// flag so nested non-comptime expressions inside a comptime body
    /// (none in the current surface, but the seam is forward-compatible)
    /// are not affected.
    pub(super) in_comptime_context: bool,
    /// Declared return type of the enclosing function. The
    /// [`crate::ty::TyInterner::error`] sentinel when no function
    /// context is active. [`check_fn_body`] installs the signature's
    /// return type before walking the body and restores the previous
    /// value on return.
    ///
    /// `synth_return` consults this to switch from synthesis to
    /// checking when lowering a `return <expr>` payload — without it,
    /// integer literals stay at their default width (`i64`) and the
    /// MIR / LLVM lowering ends up storing a wider value into a
    /// narrower slot. On Linux x86-64 the red zone absorbs the
    /// overrun; on Windows x86-64 it clobbers the return address.
    pub(super) fn_return_ty: TyId,
    /// Capability alias map: `derived_sym → root_capability_sym`.
    ///
    /// Populated by `infer_stmt` when it sees `let name = receiver.method(...)`
    /// and `receiver` has a single-segment capability source. Used by
    /// `call::translate_callee_entry` so that passing `mono` (derived from
    /// `clock.monotonic()`) propagates as `Capability(clock)` rather than
    /// the unresolvable `Capability(mono)`.
    pub(super) capability_aliases: AHashMap<Symbol, Symbol>,
    /// Stack of effect entries discharged by enclosing handlers.
    ///
    /// `synth_handle` pushes the handler's `EffectEntry::Pure(Err(_))`
    /// before walking its body and pops it after. Inside the body,
    /// `synth_try`'s declared-row check accepts any entry that matches
    /// either the function's declared row or some entry on this stack
    /// — discharged effects do not need to be redeclared at the
    /// function boundary.
    pub(super) discharged_effects: Vec<EffectEntry>,
    /// Nesting depth of an enclosing `?` operand walk (B22).
    ///
    /// `synth_try` increments this counter before walking its inner
    /// operand and decrements it after. The call typers (`synth_call`,
    /// `synth_method_call`, `synth_indirect_call`) consult it: a call
    /// whose translated row contains an `err: T` entry is admitted
    /// without further checks when this counter is non-zero (the
    /// surrounding `?` is the propagator). When the counter is zero
    /// and no enclosing handler is discharging the entry, the call
    /// site emits an `effect_row_mismatch` diagnostic — catching the
    /// statement-position fallible call whose err propagation was
    /// silently dropped at runtime.
    ///
    /// A counter (not a bool) so nested `?` operands like
    /// `inner_fallible()??` decrement correctly. The flag authorises
    /// ONLY the call the `?` is syntactically attached to: the call-argument walks
    /// (`check_arg_expr` / `synth_arg_expr`), the method-receiver walk,
    /// and the indirect-callee walk save the counter, zero it for the
    /// sub-walk, and restore it — so a fallible call in argument /
    /// receiver / callee position needs its own `?` (or an enclosing
    /// `handle`). MIR's `lower_try` wires an on-error successor for
    /// exactly one call; the prior transitively-covering semantics
    /// accepted nested unwrapped calls whose err-edges were never
    /// emitted, miscompiling them into crashes / runaway-alloc OOMs.
    pub(super) try_position_depth: usize,
    /// MethodCall span → resolved free-function `BindingId` map.
    ///
    /// `synth_method_call` finds the free function whose first
    /// parameter type matches the method-call receiver and records the
    /// resolution here. `check_function` drains this into the
    /// package-level [`crate::TypedPackage::method_resolutions`] so
    /// the MIR-lowering pass can desugar each `MethodCall` site to a
    /// `Call` against the resolved `BindingId`.
    pub(crate) method_resolutions: AHashMap<Span, edda_resolve::BindingId>,
    /// MethodCall span → compiler intrinsic kind map.
    ///
    /// `synth_method_call` inserts here when the receiver type + method
    /// name pair matches a compiler intrinsic (`bytes` on `String`,
    /// `len` on a slice). `check_function` drains this into
    /// [`crate::TypedPackage::intrinsic_calls`] so the MIR-lowering
    /// pass emits an rvalue instead of a `Call` terminator.
    pub(crate) intrinsic_calls: AHashMap<Span, crate::IntrinsicKind>,
    /// Path span → comptime type-as-value resolution map (§C10).
    ///
    /// Populated by [`super::path::synth_path`] when a path inside a
    /// `comptime` body names a primitive (`u32`, `i64`, …) or a user
    /// type. The recorded [`TyId`] is the concrete type the path refers
    /// to — `TyKind::Primitive` for catalogue heads, `TyKind::Nominal`
    /// for user / spec-invocation types. The expression's surface type
    /// stays at the `Type` meta-primitive; this map captures what that
    /// `Value::Type` resolves to once evaluated.
    ///
    /// `check_function` drains this into
    /// [`crate::TypedPackage::comptime_type_paths`] so MIR lowering and
    /// the codegen-side comptime evaluator share a single resolution
    /// surface.
    pub(crate) comptime_type_paths: AHashMap<Span, TyId>,
    /// Call-site span → comptime built-in kind map (§C10).
    ///
    /// Populated by [`super::call::synth_call`] when the callee names a
    /// locked comptime built-in (`size_of`, `align_of`,
    /// `target_has`; `offset_of` is registered but always errors on
    /// dispatch until the `@layout` path lands). MIR lowering and the
    /// codegen-side comptime evaluator consult this map to recognise
    /// the call site without re-resolving the name through the
    /// resolver (resolver returns `Resolved::Error` for catalogue
    /// heads).
    pub(crate) comptime_builtin_calls: AHashMap<Span, crate::ComptimeBuiltin>,
    /// Call-site span → resolved user-function `BindingId` map.
    ///
    /// Populated by [`super::call`]'s resolved-function arm for every
    /// direct named-function call — not only lexically-comptime ones,
    /// because the comptime evaluator interprets the bodies of
    /// functions a comptime block calls, and call sites inside those
    /// bodies were type-checked as ordinary code. `check_function`
    /// drains this into [`crate::TypedPackage::comptime_fn_calls`] so
    /// the codegen-side comptime evaluator can recover a callee's
    /// declaration (signature + HIR body) without re-walking the
    /// resolver.
    pub(crate) comptime_fn_calls: AHashMap<Span, edda_resolve::BindingId>,
    /// Call-site span → primitive-headed static-method dispatch map.
    ///
    /// Populated by [`super::call::synth_call`] when the callee is a
    /// two-segment path whose head names a primitive type and whose
    /// `(head, leaf)` pair matches the locked catalogue in
    /// [`crate::intrinsic::resolve_primitive_static_method`]
    /// (currently empty). The resolver returns `Resolved::Error` for
    /// catalogue-headed paths and stays silent (see
    /// `edda_resolve::resolver::resolve_path`), so the typechecker is
    /// the only place that can dispatch these. MIR lowering reads the
    /// map to emit a `Call` with `FuncRef::Extern` targeting the
    /// variant's `__edda_*` runtime symbol.
    pub(crate) primitive_static_calls: AHashMap<Span, crate::PrimitiveStaticMethod>,
    /// MethodCall span → capability-method dispatch map.
    ///
    /// Populated by [`super::method::synth_method_call`] when the
    /// receiver's type is a [`crate::CapabilityType`] and the
    /// `(capability, method-name)` pair matches the locked catalogue in
    /// [`crate::resolve_capability_method`] — `allocator.alloc_array`
    /// is the only entry so far. The catalogue exists because the
    /// method's first argument is a comptime `Type` value: free-function
    /// resolution would force the stdlib to declare a generic
    /// `alloc_array(allocator: Allocator, comptime T: Type, n: usize)`,
    /// which signature lowering does not yet admit. Recording
    /// the dispatch here lets MIR lowering synthesise the call against
    /// the legacy `__edda_alloc_array` extern (the same name the per-T
    /// `_alloc_array` wrappers route through) so `alloc_family.rs`'s
    /// rewrite kicks in.
    pub(crate) capability_method_calls: AHashMap<Span, crate::CapabilityMethod>,
    /// Specialized-function lookup surface from the outbound-generic
    /// mono pass. `call::synth_call`
    /// consults it when a single-segment callee path carries no
    /// recorded resolution (the mono pass rewrites template call sites
    /// to mangled names with unresolved spans), and
    /// `synth_resolved_function_call` falls back to it for the
    /// signature of a minted specialization binding absent from
    /// [`TyCx`]. `None` outside `check_package` (isolated tests).
    pub(crate) mono_fns: Option<&'a crate::mono::MonoFns>,
}

impl<'a> InferCx<'a> {
    /// Build an [`InferCx`] with an empty declared row and a fresh
    /// accumulator. Production callers that know the enclosing
    /// function's declared row should reach for [`super::check_fn_body`],
    /// which installs the row before walking the body and restores
    /// the previous value on return.
    pub fn new(
        lower: &'a LowerCx<'a>,
        ty_cx: &'a TyCx,
        lint_cfg: &'a LintConfig,
        diags: &'a mut Diagnostics,
    ) -> Self {
        let fn_return_ty = lower.ty_interner.error();
        Self {
            lower,
            ty_cx,
            lint_cfg,
            diags,
            declared_row: EffectRow::empty(),
            row: EffectAcc::new(),
            implicit_specs: Vec::new(),
            in_comptime_context: false,
            fn_return_ty,
            capability_aliases: AHashMap::default(),
            discharged_effects: Vec::new(),
            try_position_depth: 0,
            method_resolutions: AHashMap::default(),
            intrinsic_calls: AHashMap::default(),
            comptime_type_paths: AHashMap::default(),
            comptime_builtin_calls: AHashMap::default(),
            comptime_fn_calls: AHashMap::default(),
            primitive_static_calls: AHashMap::default(),
            capability_method_calls: AHashMap::default(),
            mono_fns: None,
        }
    }

    /// Record an implicit-spec invocation request. Deduplicates by
    /// `(kind, type_arg)`: a second range over the same element type
    /// produces no additional entry.
    pub(super) fn register_implicit_spec(
        &mut self,
        kind: ImplicitSpec,
        type_arg: TyId,
        span: Span,
    ) {
        if self
            .implicit_specs
            .iter()
            .any(|r| r.kind == kind && r.type_arg == type_arg)
        {
            return;
        }
        self.implicit_specs.push(ImplicitSpecRequest {
            kind,
            type_arg,
            span,
        });
    }

    /// Convenience: the `Error` sentinel handle.
    #[inline]
    pub fn ty_error(&self) -> TyId {
        self.lower.ty_interner.error()
    }

    /// Render a [`TyId`] for diagnostics with nominal types named.
    ///
    /// Wraps [`crate::ty::TyInterner::display_cx`] with the inference
    /// pass's symbol interner and resolved package (both present in
    /// production; `package` is `None` only in isolated unit tests, where
    /// it degrades to the `<nominal M:I>` coordinate form). Prefer this
    /// over `self.lower.ty_interner.display(...)` at every diagnostic
    /// site so a nominal mismatch reads as `ast.tree.Expr` rather than
    /// `<nominal 875:39>`.
    pub(crate) fn display_ty(&self, id: TyId) -> crate::ty::TyDisplayCx<'a> {
        self.lower
            .ty_interner
            .display_cx(id, self.lower.interner, self.lower.package)
    }

    /// Push a `TypecheckError` diagnostic, honouring the effective
    /// severity from [`LintConfig`]. A `None` severity (the class is
    /// suppressed via the manifest's `lints` block) silently drops
    /// the diagnostic per `edda-diag`'s emission contract.
    pub fn emit_typecheck_error(&mut self, span: Span, message: impl Into<String>) {
        self.emit_diagnostic(DiagnosticClass::TypecheckError, span, message);
    }

    /// Push a diagnostic with an explicit class. Honours the effective
    /// severity from [`LintConfig`] — a `None` severity (the class is
    /// suppressed) silently drops the diagnostic. Sites that
    /// emit `effect_row_mismatch` (or any class other than
    /// `typecheck_error`) reach for this directly; the
    /// [`InferCx::emit_typecheck_error`] convenience wraps the common
    /// case.
    pub(super) fn emit_diagnostic(
        &mut self,
        class: DiagnosticClass,
        span: Span,
        message: impl Into<String>,
    ) {
        let sev = self.lint_cfg.effective(class);
        self.diags.push(Diagnostic::new(class, sev, span, message));
    }

    /// Push a single entry into the accumulator. The
    /// originator arms (`Raise`, `Panic`) and `T-FunCall`'s row
    /// union step reach here.
    pub(super) fn push_effect_entry(&mut self, entry: EffectEntry) {
        self.row.push(entry);
    }
}
