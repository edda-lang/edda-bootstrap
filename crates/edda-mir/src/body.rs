//! `Body`: one MIR function — params, locals, basic blocks, and effect row.

use edda_intern::Symbol;
use edda_span::Span;

use crate::arena::IndexVec;
use crate::block::BasicBlockData;
use crate::effect::{CapabilitySlot, EffectRow};
use crate::ids::{BlockId, LocalId};
use crate::layout::AbiTag;
use crate::ty::{MirType, ParamMode};

/// One MIR function body.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Body {
    /// Function name as it appeared in source.
    pub name: Symbol,
    /// Defining span.
    pub span: Span,
    /// Parameters in declaration order (capabilities first, then user params).
    pub params: Vec<ParamInfo>,
    /// Locals arena.
    pub locals: IndexVec<LocalId, LocalDecl>,
    /// Basic-block arena.
    pub blocks: IndexVec<BlockId, BasicBlockData>,
    /// Entry block.
    pub entry: BlockId,
    /// Return type.
    pub return_ty: MirType,
    /// Effect row (capabilities, possible errors, panic bit).
    pub effect_row: EffectRow,
    /// Capability slots backed by body LOCALS rather than leading
    /// parameters — one per capability value minted inside the body
    /// (`let child = allocator.fork()`).
    /// Unlike `effect_row.capabilities` these slots contribute NO
    /// function parameter; `param_local` is an ordinary local holding
    /// the capability handle. Call-site threading resolves an
    /// `EffectId` against the union of both lists, so ids here continue
    /// the row's numbering and stay body-unique.
    pub local_capabilities: Vec<CapabilitySlot>,
    /// Linker-visible symbol name from an `@abi("name")` symbol override,
    /// if any. When set, the LLVM emitter
    /// uses this string verbatim instead of the mangled function name,
    /// and forces external linkage with no COMDAT.
    pub export_symbol: Option<Symbol>,
    /// Calling-convention override from `@abi("...")`, if any. When
    /// set, the LLVM emitter resolves the tag through
    /// `edda_compile::abi_attr` instead of using the platform default.
    pub abi: Option<AbiTag>,
    /// Deterministic module-qualified symbol name for this body. `Some(s)`
    /// for source functions (`s` is the `<module.path>.<leaf>` mangle the
    /// driver computes from the binding's canonical module path); `None`
    /// for compiler-synthesised bodies (fn-value shims, closures) that
    /// have no source module path. Codegen emits `Some` with
    /// `linkonce_odr` linkage + a COMDAT so identical re-lowered copies
    /// fold across members, and `None` as a member-unique external symbol.
    /// The binary entry and `@abi`-symbol-override bodies override this.
    pub qualified_name: Option<Symbol>,
}

impl Body {
    /// Construct an empty body. `entry` defaults to [`BlockId::DUMMY`] — the
    /// caller is expected to set it once at least one block has been pushed
    /// into `blocks`. `export_symbol` and `abi` default to `None`; callers
    /// set them via [`crate::builder::BodyBuilder::set_export_symbol`] /
    /// [`crate::builder::BodyBuilder::set_abi`] once attribute values flow
    /// in from typecheck.
    pub fn new(name: Symbol, span: Span, return_ty: MirType) -> Self {
        Body {
            name,
            span,
            params: Vec::new(),
            locals: IndexVec::new(),
            blocks: IndexVec::new(),
            entry: BlockId::DUMMY,
            return_ty,
            effect_row: EffectRow::pure(),
            local_capabilities: Vec::new(),
            export_symbol: None,
            abi: None,
            qualified_name: None,
        }
    }
}

/// One parameter of a [`Body`]: declared mode, type, backing local, and span.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ParamInfo {
    /// Parameter mode (Let / Mutable / Take / Set).
    pub mode: ParamMode,
    /// Parameter type.
    pub ty: MirType,
    /// Backing local that receives the argument value.
    pub local: LocalId,
    /// Span of the parameter declaration.
    pub span: Span,
}

/// One entry in a [`Body`]'s locals arena.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LocalDecl {
    /// Type of the local.
    pub ty: MirType,
    /// Whether the local is reassignable.
    pub mutability: Mutability,
    /// Where this local came from (param / temp / user binding / return slot).
    pub source: LocalSource,
    /// Defining span (the binding site for user bindings; lowering site for temps).
    pub span: Span,
}

/// Local mutability.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Mutability {
    /// Single-assignment temporary or `let` binding.
    Imm,
    /// Reassignable (`let mut` / `mutable` / `init` / loop accumulators).
    Mut,
}

/// Provenance of a local — where the lowering pass created it.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum LocalSource {
    /// Parameter local, `i` is the index into `Body::params`.
    Param(u32),
    /// Compiler-introduced temporary.
    Temp,
    /// User-introduced `let` binding (the surface name).
    UserBinding(Symbol),
    /// The body's return slot.
    ReturnSlot,
}
