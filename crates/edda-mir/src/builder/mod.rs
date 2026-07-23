//! Construction-by-invariant API for [`crate::MirProgram`] and [`crate::Body`].
//!
//! The IR types are open structs with public fields; nothing stops a
//! caller from hand-building a malformed body. The builders in this module
//! produce well-formed values by construction, encoding the invariants the
//! types only document.
//!
//! [`ProgramBuilder`] grows a program. [`BodyBuilder`] grows a body and is
//! handed back as a finished [`crate::Body`] via [`BodyBuilder::finish`].
//! [`BlockBuilder`] is the consume-on-finish per-block builder; see
//! [`crate::builder::block`].

mod block;
mod local;

use edda_intern::Symbol;
use edda_span::Span;

use crate::adt::AdtDef;
use crate::body::{Body, LocalDecl, LocalSource, Mutability, ParamInfo};
use crate::constant::Const;
use crate::effect::{CapabilityKind, CapabilitySlot};
use crate::ids::{AdtId, BlockId, BodyId, ConstId, EffectId, LocalId};
use crate::layout::LayoutInfo;
use crate::program::MirProgram;
use crate::ty::{MirType, MirTypeKind, ParamMode};

pub use block::BlockBuilder;

/// Builder for a [`crate::MirProgram`].
pub struct ProgramBuilder {
    program: MirProgram,
}

impl Default for ProgramBuilder {
    fn default() -> Self {
        ProgramBuilder::new()
    }
}

impl ProgramBuilder {
    /// Construct an empty builder.
    pub fn new() -> Self {
        ProgramBuilder {
            program: MirProgram::new(),
        }
    }

    /// Append an ADT and return its [`AdtId`].
    pub fn push_adt(&mut self, adt: AdtDef) -> AdtId {
        self.program.adts.push(adt)
    }

    /// Append an interned constant and return its [`ConstId`].
    pub fn push_const(&mut self, c: Const) -> ConstId {
        self.program.consts.push(c)
    }

    /// Append a body and return its [`BodyId`].
    pub fn push_body(&mut self, body: Body) -> BodyId {
        self.program.bodies.push(body)
    }

    /// Reserve a body slot up front and return its [`BodyId`]. Installs a
    /// placeholder [`Body`] carrying the real `name` / `span` / `return_ty`
    /// so the slot is well-named before [`ProgramBuilder::fill_body`]
    /// overwrites it with the lowered body. Callers reserve every source
    /// function's slot before walking any body so synthetic shim / closure
    /// bodies pushed mid-walk append after them.
    pub fn reserve_body(&mut self, name: Symbol, span: Span, return_ty: MirType) -> BodyId {
        self.program.bodies.push(Body::new(name, span, return_ty))
    }

    /// Overwrite a reserved body slot with its lowered [`Body`]. The slot's
    /// `BodyId` is unchanged — only the stored body is replaced — so every
    /// `function_map` / `function_result_adts` entry keyed by the reserved
    /// id stays valid.
    pub fn fill_body(&mut self, id: BodyId, body: Body) {
        self.program.bodies[id] = body;
    }

    /// Set the binary entry body. Caller is responsible for ensuring `body`
    /// is in range (this is checked by validation, not here).
    pub fn set_entry(&mut self, body: BodyId) {
        self.program.entry = Some(body);
    }

    /// Read-only view of the in-progress program. Used by callers that need
    /// to look up an `AdtDef` while constructing a body.
    pub fn program(&self) -> &MirProgram {
        &self.program
    }

    /// Consume the builder and return the finished program.
    pub fn finish(self) -> MirProgram {
        self.program
    }
}

/// Builder for a [`crate::Body`].
pub struct BodyBuilder {
    body: Body,
    /// Tracks whether at least one non-capability param has been added; used
    /// to debug-assert the capability-first ordering invariant.
    non_capability_param_added: bool,
}

impl BodyBuilder {
    /// Construct an empty body builder.
    pub fn new(name: Symbol, span: Span, return_ty: MirType) -> Self {
        BodyBuilder {
            body: Body::new(name, span, return_ty),
            non_capability_param_added: false,
        }
    }

    /// Allocate the return-slot local. Must be the first local-allocating
    /// call on a fresh builder so the returned id equals
    /// [`LocalId::RETURN_SLOT`].
    pub fn return_slot(&mut self, ty: MirType, span: Span) -> LocalId {
        assert!(
            self.body.locals.is_empty(),
            "BodyBuilder::return_slot must be called before any other local-allocating method",
        );
        local::push(
            &mut self.body,
            LocalDecl {
                ty,
                mutability: Mutability::Mut,
                source: LocalSource::ReturnSlot,
                span,
            },
        )
    }

    /// Allocate a parameter and its backing local. The local's
    /// `LocalSource::Param(i)` index is set to the param's position in
    /// `body.params`, keeping the invariant by construction.
    pub fn param(&mut self, mode: ParamMode, ty: MirType, span: Span) -> LocalId {
        self.non_capability_param_added = true;
        let i = self.body.params.len() as u32;
        let local = local::push(
            &mut self.body,
            LocalDecl {
                ty: ty.clone(),
                mutability: param_mode_mutability(mode),
                source: LocalSource::Param(i),
                span,
            },
        );
        self.body.params.push(ParamInfo {
            mode,
            ty,
            local,
            span,
        });
        local
    }

    /// Allocate a compiler-introduced immutable temporary.
    pub fn temp(&mut self, ty: MirType, span: Span) -> LocalId {
        local::push(
            &mut self.body,
            LocalDecl {
                ty,
                mutability: Mutability::Imm,
                source: LocalSource::Temp,
                span,
            },
        )
    }

    /// Allocate a user-introduced `let` binding with the supplied mutability.
    pub fn user_local(
        &mut self,
        name: Symbol,
        mutability: Mutability,
        ty: MirType,
        span: Span,
    ) -> LocalId {
        local::push(
            &mut self.body,
            LocalDecl {
                ty,
                mutability,
                source: LocalSource::UserBinding(name),
                span,
            },
        )
    }

    /// Allocate a capability parameter local and its [`CapabilitySlot`].
    /// Returns the slot's [`EffectId`].
    pub fn add_capability(
        &mut self,
        kind: CapabilityKind,
        ty: MirType,
        span: Span,
    ) -> EffectId {
        debug_assert!(
            !self.non_capability_param_added,
            "BodyBuilder::add_capability must precede non-capability params",
        );
        let id = EffectId::from_raw(self.body.effect_row.capabilities.len() as u32);
        let i = self.body.params.len() as u32;
        let local = local::push(
            &mut self.body,
            LocalDecl {
                ty: ty.clone(),
                mutability: Mutability::Imm,
                source: LocalSource::Param(i),
                span,
            },
        );
        self.body.params.push(ParamInfo {
            mode: ParamMode::Let,
            ty: MirType {
                kind: MirTypeKind::Capability(kind.clone()),
                layout: LayoutInfo::natural(),
            },
            local,
            span,
        });
        self.body.effect_row.capabilities.push(CapabilitySlot {
            id,
            param_local: local,
            ty: kind,
        });
        id
    }

    /// Record that the body may raise this error ADT via `?` propagation.
    pub fn add_error(&mut self, err_adt: AdtId) {
        self.body.effect_row.errors.push(err_adt);
    }

    /// Set the body's `has_panic` bit.
    pub fn set_panic(&mut self, value: bool) {
        self.body.effect_row.has_panic = value;
    }

    /// Record the linker-visible symbol name from `@export("name")`. When
    /// set, the LLVM emitter uses this string verbatim and forces external
    /// linkage; when `None`, the emitter mangles by the function's source
    /// name.
    pub fn set_export_symbol(&mut self, symbol: Option<Symbol>) {
        self.body.export_symbol = symbol;
    }

    /// Record the calling-convention override from `@abi("...")`. When
    /// set, the LLVM emitter dispatches through
    /// `edda_compile::abi_attr::resolve_abi_tag`; when `None`, the
    /// platform default applies.
    pub fn set_abi(&mut self, abi: Option<crate::layout::AbiTag>) {
        self.body.abi = abi;
    }

    /// Record the deterministic module-qualified symbol name. When set,
    /// the LLVM emitter uses it with `linkonce_odr` linkage + a COMDAT so
    /// identical re-lowered copies fold across member objects; when unset,
    /// the body gets a member-unique external symbol.
    pub fn set_qualified_name(&mut self, qualified_name: Symbol) {
        self.body.qualified_name = Some(qualified_name);
    }

    /// Start a new block. The returned [`BlockBuilder`] reserves a fresh
    /// [`BlockId`] (visible via [`BlockBuilder::id`]) so callers can target
    /// the block from another terminator before this one is sealed.
    pub fn block(&mut self) -> BlockBuilder<'_> {
        BlockBuilder::reserve(&mut self.body)
    }

    /// Record the body's entry block.
    pub fn set_entry(&mut self, block: BlockId) {
        debug_assert!(
            block.as_index() < self.body.blocks.len(),
            "BodyBuilder::set_entry given an out-of-range BlockId",
        );
        self.body.entry = block;
    }

    /// Consume the builder and return the finished body.
    pub fn finish(self) -> Body {
        self.body
    }

    /// Crate-internal mutable access to the in-progress [`Body`]. Used by
    /// the typed-HIR -> MIR lowering pass (`crate::lower::*`) to append
    /// statements into pre-allocated blocks and to overwrite their
    /// `Unreachable` placeholder terminators without going through the
    /// consume-on-finish [`BlockBuilder`] API.
    pub(crate) fn body_mut(&mut self) -> &mut Body {
        &mut self.body
    }

    /// Crate-internal read-only access to the in-progress [`Body`]. Used by
    /// the lowering pass to fetch invariants like `return_ty` without
    /// taking a mutable borrow on the whole builder.
    pub(crate) fn body_ref(&self) -> &Body {
        &self.body
    }
}

/// Default mutability inferred from parameter mode. `Mutable` and `Init` writes
/// through the local, so they are reassignable; `Let` and `Take` are
/// single-assignment.
fn param_mode_mutability(mode: ParamMode) -> Mutability {
    match mode {
        ParamMode::Let | ParamMode::Take => Mutability::Imm,
        ParamMode::Mutable | ParamMode::Init => Mutability::Mut,
    }
}
