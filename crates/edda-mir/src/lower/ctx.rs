//! `LoweringContext` — the mutable state the lowering pass threads through
//! every HIR walk.
//!
//! The context carries an `&TyInterner` alongside the symbol [`Interner`] so
//! `TyId` -> `MirType` lowering can resolve handles directly; the loop stack
//! gains a `loop_value` slot for `break expr` lowering; and an explicit
//! `return_slot` field caches the body's return-slot local so `return` /
//! trailing-expression lowering does not have to walk `body.locals` to find
//! it. The struct is crate-`pub(crate)` on its fields so the per-shape
//! dispatch modules (`expr`, `stmt`, `pattern`, `effect`) can mutate it
//! directly without having to thread accessors.
//!
//! The `scope_stack: Vec<Vec<LocalId>>` field mirrors lexical
//! block nesting. Each inner `Vec` is one scope's user-`let` bindings, in
//! declaration order. The pass enters a fresh frame at every block entry
//! and pops it on exit, emitting `StorageDead` for each recorded local in
//! reverse order so structural validation sees paired `StorageLive` /
//! `StorageDead` markers.

use std::collections::HashMap;

use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, Resolutions};
use edda_span::Span;
use edda_types::{FnSig, IntrinsicKind, TyInterner};

use crate::builder::{BodyBuilder, ProgramBuilder};
use crate::error::MirError;
use crate::ids::{AdtId, BlockId, BodyId, EffectId, LocalId, VariantIdx};
use crate::program::MirProgram;

use super::{AllocFmtBindings, FmtBindings};

/// One frame of the lowering pass's loop-label stack.
pub(crate) struct LoopFrame {
    /// Block to branch to for `continue` — re-enters the loop body.
    pub continue_bb: BlockId,
    /// Block to branch to for `break` — joins after the loop.
    pub break_bb: BlockId,
    /// Local holding the loop's value, when the loop is in expression
    /// position. `break expr` writes through this local before branching to
    /// `break_bb`; loops in statement position carry `None`.
    pub loop_value: Option<LocalId>,
}

/// One frame of the lowering pass's effect-handler stack. Pushed by
/// [`super::handle::lower_handle`] before walking the body and popped
/// before lowering the recovery expression.
pub(crate) struct HandlerFrame {
    /// Err ADT discharged by this handler.
    pub handled_adt: AdtId,
    /// Block control flow branches to when a `?` inside the body
    /// propagates a matching error.
    pub recovery_bb: BlockId,
    /// Block control flow joins at after either branch finishes.
    pub join_bb: BlockId,
    /// Local that receives the handler expression's value — written by
    /// both the body's success path and the recovery branch.
    pub result_local: LocalId,
    /// Local that receives the caught err payload, when the source
    /// wrote `handle err: T as <binder> -> ...`. The per-`?`
    /// dispatcher (or the checked-arith err arm) extracts the
    /// payload into this local before jumping to `recovery_bb` so
    /// the recovery expression can read the binder. `None` for the
    /// legacy `handle err: T -> recovery` form.
    pub binder_local: Option<LocalId>,
}

/// Mutable state threaded through the typed-HIR -> MIR lowering pass.
pub struct LoweringContext<'a> {
    /// Symbol interner shared with the caller (used to resolve `Symbol`
    /// -> `&str` for diagnostic messages and capability-name classification).
    pub(crate) interner: &'a Interner,
    /// Type interner used to resolve `edda_types::TyId` handles into the
    /// `TyKind`s they were interned from.
    pub(crate) ty_interner: &'a TyInterner,
    /// Resolver-produced span-keyed resolution map. Looked up by
    /// multi-segment Path lowering (callee position via
    /// [`super::call::resolve_callee_binding`] and value position via
    /// [`super::expr`]'s variant-constructor branch) to translate the
    /// path into the resolver-issued `BindingId` it refers to.
    pub(crate) resolutions: &'a Resolutions,
    /// Typechecker-produced MethodCall span -> free-function
    /// `BindingId` map. Consumed by [`super::call::lower_method_call`]
    /// to desugar each `HirExprKind::MethodCall` into a regular `Call`
    /// against the resolved binding (the receiver is prepended as
    /// argument 0).
    pub(crate) method_resolutions: &'a HashMap<Span, BindingId>,
    /// Typechecker-produced intrinsic method call map. Keyed by the same
    /// span as `method_resolutions`. Consumed by the `MethodCall` arm in
    /// `lower_expr_to_operand` to emit rvalues instead of `Call` terminators.
    pub(crate) intrinsic_calls: &'a HashMap<Span, IntrinsicKind>,
    /// Builder for the program being constructed.
    pub(crate) program: ProgramBuilder,
    /// Accumulated errors. The pass keeps walking after the first error so
    /// callers get a complete failure report per invocation.
    pub(crate) errors: Vec<MirError>,

    /// Program-wide ADT registry. Populated by the type-decl pre-pass
    /// before any function body is walked. Read by `lower_ty` to translate
    /// `TyKind::Nominal(BindingId)` into `MirTypeKind::Adt(AdtId)`, and by
    /// the MakeRecord / MakeVariant / ExtractField lowering.
    pub(crate) adt_map: HashMap<BindingId, AdtId>,
    /// Program-wide function registry. Populated by the function
    /// pre-pass before any body is walked. Maps each function's resolver-side
    /// `BindingId` to the `BodyId` it will be assigned when pushed onto the
    /// program. Read by the `Call` terminator lowering.
    pub(crate) function_map: HashMap<BindingId, BodyId>,
    /// Program-wide function name registry. Populated alongside
    /// `function_map` by `register_function_bodies`.
    /// Maps each function's declared name (the `Symbol` issued by the
    /// interner) to its resolver-side `BindingId`. Read by the `Call`
    /// terminator lowering to resolve a single-segment callee path to a
    /// `BindingId` without re-walking `function_map`'s values.
    pub(crate) function_symbols: HashMap<Symbol, BindingId>,
    /// Program-wide function signature registry. Populated alongside
    /// `function_map` by `register_function_bodies`.
    /// Maps each function's `BindingId` to a borrow of its [`FnSig`]. Read
    /// by the `Call` terminator lowering to look up the callee's
    /// parameter modes and effect-row capability declarations.
    pub(crate) function_sigs: HashMap<BindingId, &'a FnSig>,
    /// Program-wide raising-function result ADT registry. Populated by the
    /// `synthesize_result_adt` pre-pass alongside `function_map`.
    /// Maps each raising function's `BodyId` to the synthesized
    /// `Result<T, E>` sum ADT. Non-raising functions have no entry.
    pub(crate) function_result_adts: HashMap<BodyId, AdtId>,
    /// Program-wide extern function registry. Populated by the
    /// `register_externs` pre-pass before any body is walked. Maps
    /// each extern function's resolver-side `BindingId` to its
    /// linker-visible symbol and pre-lowered MIR-side signature.
    /// Read by call-terminator lowering: when a callee binding is in
    /// this map (and not in `function_map`), the call emits
    /// `FuncRef::Extern { name, sig }` cloning the cached MIR signature
    /// instead of `FuncRef::Body(_)`.
    pub(crate) function_externs: HashMap<BindingId, (Symbol, crate::ty::FnSig)>,
    /// Program-wide module-level `let` constant registry. Populated
    /// by the `register_consts` pre-pass before any body is walked.
    /// Maps each `BindingKind::Const` binding to the `ConstId` of the
    /// pre-interned value in `program.consts`. Read by
    /// `super::expr::lower_path` to emit `Operand::Const(id)` at every
    /// single-segment reference to a module-level let.
    pub(crate) module_consts: HashMap<BindingId, crate::ids::ConstId>,

    /// Per-body builder. Populated when a function lowering starts; consumed
    /// (via `BodyBuilder::finish` + `program.push_body`) when it finishes.
    pub(crate) body: Option<BodyBuilder>,
    /// Per-body user-binding name -> local map. Cleared between functions.
    pub(crate) bindings: HashMap<Symbol, LocalId>,
    /// Per-body capability name -> slot id map. Cleared between functions.
    pub(crate) capabilities: HashMap<Symbol, EffectId>,
    /// Per-body narrowed-capability alias map: derived local name -> source
    /// effect-row capability symbol. Cleared between functions.
    pub(crate) capability_aliases: HashMap<Symbol, Symbol>,

    /// Loop-label stack for `break` / `continue` lowering. One frame per
    /// active lexical loop.
    pub(crate) loop_stack: Vec<LoopFrame>,
    /// Effect-handler stack for `handle err: T -> recovery { body }`
    /// lowering. One frame per active lexical handler — `lower_try`
    /// walks the stack from innermost to outermost looking for a
    /// frame whose `handled_adt` matches the propagated err and, if
    /// found, routes the call's `on_error` to that frame's
    /// `recovery_bb` instead of unwinding to the function's caller.
    pub(crate) handler_stack: Vec<HandlerFrame>,
    /// The current basic block being filled. `None` between blocks — set
    /// after a diverging terminator (`Return`, `Break`, `Continue`, `Panic`)
    /// until the next allocator-introduced block is entered.
    pub(crate) current_bb: Option<BlockId>,
    /// The body's return slot local — cached so `return [expr]` and trailing
    /// blocks don't have to scan locals for `LocalSource::ReturnSlot`.
    pub(crate) return_slot: Option<LocalId>,
    /// Lexical scope stack for `StorageLive` / `StorageDead` emission. Each
    /// inner `Vec` is one block's user-`let` bindings, recorded in
    /// declaration order so the exit traversal can drop them in reverse.
    /// Mid-expression compiler temps are not tracked here — see
    /// the TODO in `stmt.rs`.
    pub(crate) scope_stack: Vec<Vec<LocalId>>,
    /// Per-body: when the current function is raising, the synthesized
    /// `Result<T, E>` sum ADT and the Ok variant index. `None` for
    /// non-raising functions. Cleared by `reset_body_state`.
    pub(crate) result_adt: Option<(AdtId, VariantIdx)>,
    /// Per-body: `Some(pointee)` when the current function returns a
    /// position borrow (`-> let T` / `-> mutable T`). The body's MIR
    /// `return_ty` is then `HeapPtr`, and `return <place>` / the trailing
    /// expression lower to `RvalueKind::Ref` (address-of) rather than a
    /// value copy. `pointee` is the borrowed value type `T`, carried so
    /// the return slot's `Ref` and the caller-side `Projection::Deref`
    /// agree. Cleared by `reset_body_state`.
    pub(crate) return_borrow_pointee: Option<crate::ty::MirType>,
    /// Pre-resolved `std.fmt` bindings the fstring lowering targets.
    /// Carried from [`super::LoweringInput::fmt_bindings`]. Read by
    /// [`super::fstring_emit::emit_format_call`] and
    /// [`super::fstring_emit::emit_concat_call`] — when a relevant field is
    /// `Some(_)`, lowering routes the call through that binding;
    /// otherwise it falls back to the legacy hardcoded `__edda_*`
    /// extern symbol.
    pub(crate) fmt_bindings: FmtBindings,
    /// Pre-resolved allocator-taking pure-Edda retarget bindings.
    /// Carried from [`super::LoweringInput::alloc_fmt_bindings`]. Read
    /// by [`super::fstring_emit`] before it consults `fmt_bindings` —
    /// when the enclosing function holds its own `Allocator` capability
    /// and [`Self::alloc_error_adt`] is already in its row's errors, the
    /// call retargets through the matching field here instead.
    pub(crate) alloc_fmt_bindings: AllocFmtBindings,
    /// `std.mem.alloc.AllocError`'s [`AdtId`] once `adt::register_type_decls`
    /// has run. Resolved from [`super::LoweringInput::alloc_error`] by
    /// [`super::lower`] right after that pre-pass populates `adt_map` —
    /// `LoweringContext::new` runs before `adt_map` exists, so this
    /// field starts `None` and is filled in by the caller. Read by
    /// [`super::fstring_emit`]'s row-admission check.
    pub(crate) alloc_error_adt: Option<AdtId>,
    /// Target pointer width in bytes. Set from
    /// [`super::LoweringInput::pointer_width_bytes`] at construction and
    /// consumed by [`super::layout::compute_size_align`] for primitive
    /// sizing and by [`super::call`]'s alloc-family call rewrite.
    pub(crate) pointer_width_bytes: u32,
    /// Active build target — set from
    /// [`super::LoweringInput::target_cfg`]. Carried so the
    /// `HirExprKind::Comptime` arm can hand it to
    /// [`edda_comptime::EvalCx`] (§C10). `None` falls back to the
    /// "comptime not yet wired" diagnostic.
    pub(crate) target_cfg: Option<&'a edda_target::TargetCfg>,
    /// Reference into the upstream `edda_types::TyCx` (§C10). Used by
    /// the comptime evaluator to resolve `TyKind::Nominal(BindingId)`
    /// to its field table.
    pub(crate) ty_cx: Option<&'a edda_types::TyCx>,
    /// Typechecker-side path-as-type resolution map (§C10). See
    /// [`super::LoweringInput::comptime_type_paths`].
    pub(crate) comptime_type_paths: &'a ahash::AHashMap<Span, edda_types::TyId>,
    /// Typechecker-side built-in call resolution map (§C10). See
    /// [`super::LoweringInput::comptime_builtin_calls`].
    #[allow(dead_code)]
    pub(crate) comptime_builtin_calls:
        &'a ahash::AHashMap<Span, edda_types::ComptimeBuiltin>,
    /// Typechecker-side comptime user-function call resolution map.
    /// See [`super::LoweringInput::comptime_fn_calls`].
    pub(crate) comptime_fn_calls: &'a ahash::AHashMap<Span, BindingId>,
    /// Program-wide function-declaration registry for the comptime
    /// evaluator: each function's `BindingId` mapped to
    /// its `(name, signature, typed body)`. Built once at context
    /// construction from [`super::LoweringInput::functions`]; the
    /// `Comptime` / `ComptimeBlock` arms wrap it in a
    /// [`edda_comptime::FnDeclLookup`] so the evaluator can interpret
    /// user-function callees across every file of the package.
    pub(crate) comptime_fn_decls:
        HashMap<BindingId, (Symbol, &'a edda_types::FnSig, &'a edda_types::HirBlock)>,
    /// Typechecker-side primitive-headed static-method dispatch map
    /// (currently empty — see [`edda_types::PrimitiveStaticMethod`]).
    /// See [`super::LoweringInput::primitive_static_calls`]. Consumed
    /// by [`super::call::lower_call`] before any resolver/method-table
    /// lookup so a catalogued static call short-circuits to a
    /// `FuncRef::Extern` over its runtime symbol.
    pub(crate) primitive_static_calls:
        &'a ahash::AHashMap<Span, edda_types::PrimitiveStaticMethod>,
    /// Typechecker-side capability-method dispatch map. See
    /// [`super::LoweringInput::capability_method_calls`]. Consumed by
    /// [`super::call::lower_method_call`] before any
    /// `method_resolutions` lookup so
    /// `allocator.alloc_array(T, n)` short-circuits to a
    /// `FuncRef::Extern` over `__edda_alloc_array` (which the
    /// alloc-family rewrite then promotes to `__edda_alloc_array_raw`
    /// with prepended `size_of(T)` / `align_of(T)`).
    pub(crate) capability_method_calls:
        &'a ahash::AHashMap<Span, edda_types::CapabilityMethod>,
    /// Driver-built `derive eq` target-type `BindingId` -> comparator `eq`
    /// fn `BindingId` map. See [`super::LoweringInput::eq_comparators`].
    /// Consumed by [`super::arith::lower_binary`] to lower `==` / `!=` on
    /// a nominal operand to a `Call` against the structural comparator.
    pub(crate) eq_comparators: &'a HashMap<BindingId, BindingId>,
    /// Driver-built `derive debug` target-type `BindingId` -> formatter
    /// `format` fn `BindingId` map. See
    /// [`super::LoweringInput::debug_formatters`]. Consumed by
    /// [`super::fstring_emit`] to lower an aggregate interpolation slot into
    /// a `Call` against the synthesised `std.core.fmt.debug_<T>.format`.
    pub(crate) debug_formatters: &'a HashMap<BindingId, BindingId>,
}

impl<'a> LoweringContext<'a> {
    /// Construct an empty context bound to the supplied interners, the
    /// resolver-produced [`Resolutions`] map, and the typechecker's
    /// [`method_resolutions`] map for the package being lowered.
    pub(crate) fn new(input: &super::LoweringInput<'a>) -> Self {
        // Registry for the comptime evaluator's user-function calls:
        // every function in the package, keyed by its
        // resolver binding, with borrowed signature + body.
        let comptime_fn_decls = input
            .functions
            .iter()
            .map(|f| (f.binding, (f.name, f.sig, f.body)))
            .collect();
        LoweringContext {
            interner: input.interner,
            ty_interner: input.ty_interner,
            resolutions: input.resolutions,
            method_resolutions: input.method_resolutions,
            intrinsic_calls: input.intrinsic_calls,
            program: ProgramBuilder::new(),
            errors: Vec::new(),
            adt_map: HashMap::new(),
            function_map: HashMap::new(),
            function_symbols: HashMap::new(),
            function_sigs: HashMap::new(),
            function_result_adts: HashMap::new(),
            function_externs: HashMap::new(),
            module_consts: HashMap::new(),
            body: None,
            bindings: HashMap::new(),
            capabilities: HashMap::new(),
            capability_aliases: HashMap::new(),
            loop_stack: Vec::new(),
            handler_stack: Vec::new(),
            current_bb: None,
            return_slot: None,
            scope_stack: Vec::new(),
            result_adt: None,
            return_borrow_pointee: None,
            fmt_bindings: input.fmt_bindings,
            alloc_fmt_bindings: input.alloc_fmt_bindings,
            alloc_error_adt: None,
            pointer_width_bytes: input.pointer_width_bytes,
            target_cfg: input.target_cfg,
            ty_cx: input.ty_cx,
            comptime_type_paths: input.comptime_type_paths,
            comptime_builtin_calls: input.comptime_builtin_calls,
            comptime_fn_calls: input.comptime_fn_calls,
            comptime_fn_decls,
            primitive_static_calls: input.primitive_static_calls,
            capability_method_calls: input.capability_method_calls,
            eq_comparators: input.eq_comparators,
            debug_formatters: input.debug_formatters,
        }
    }

    /// Consume the context and return the finished program plus the
    /// accumulated lowering errors.
    ///
    /// Asserts the per-body slot is empty (every function lowering must seal
    /// its body before the pass finishes). Structural validation is run by
    /// the [`super::lower`] entry point on the returned program, not here —
    /// keeping the context independent of the `validate` module preserves
    /// the `lower → validate` directional layering. Callers merge their own
    /// structural errors with the returned `errors` vector before producing
    /// the final `Result`.
    pub(crate) fn finish_with_errors(self) -> (MirProgram, Vec<MirError>) {
        assert!(
            self.body.is_none(),
            "LoweringContext::finish called while a body was still being lowered",
        );
        (self.program.finish(), self.errors)
    }

    /// The enclosing function's own `Allocator`-typed capability slot,
    /// if it holds one — `(effect_id, param_local)`. Used by
    /// [`super::fstring_emit`] to thread the allocator into a retargeted
    /// pure-Edda format/concat call.
    pub(crate) fn own_allocator(&self) -> Option<(EffectId, LocalId)> {
        let body = self.body.as_ref()?;
        body.body_ref()
            .effect_row
            .capabilities
            .iter()
            .find(|slot| slot.ty == crate::effect::CapabilityKind::Allocator)
            .map(|slot| (slot.id, slot.param_local))
    }

    /// Whether the enclosing function's row already admits
    /// `err: alloc.AllocError` — the second precondition (alongside
    /// [`Self::own_allocator`]) for retargeting a format/concat call
    /// through [`Self::alloc_fmt_bindings`]. The retargeted callee
    /// raises `err: alloc.AllocError`; introducing that possible raise
    /// into a caller whose row does not already declare it would be an
    /// undeclared-effect soundness gap (mirrors the native compiler's
    /// `EffectFacts.raises` check).
    pub(crate) fn row_admits_alloc_error(&self) -> bool {
        let Some(alloc_adt) = self.alloc_error_adt else {
            return false;
        };
        self.body
            .as_ref()
            .is_some_and(|b| b.body_ref().effect_row.errors.contains(&alloc_adt))
    }

    /// Clear all per-body scratch state. Called after a function's body is
    /// sealed and pushed into the program.
    pub(crate) fn reset_body_state(&mut self) {
        self.body = None;
        self.bindings.clear();
        self.capabilities.clear();
        self.capability_aliases.clear();
        self.loop_stack.clear();
        self.handler_stack.clear();
        self.current_bb = None;
        self.return_slot = None;
        self.scope_stack.clear();
        self.result_adt = None;
        self.return_borrow_pointee = None;
    }
}
