//! Linear MIR and the typed-HIR -> MIR lowering pass.
//!
//! MIR is the linear, block-structured intermediate form that sits between
//! the typed HIR and the LLVM backend. Parameter modes (let/mutable/take/set)
//! become explicit move/borrow operations here; effect rows lower to capability
//! parameter threading; `?` propagation lowers to early-return on error.
//!
//! `examples/edlang0-compiler/src/lowering/` (the canonical Edda reference)
//! is the worked example for what this pass produces.
//!
//! # Current state
//!
//! As of 2026-05-18 the crate has gone through many rounds of work: data
//! structures + pretty-printer, builder API + structural validation,
//! the typed-HIR -> MIR lowering pass body, short-circuit `&&`/`||`,
//! `StorageLive`/`StorageDead` pairing, refutable patterns in `match`,
//! ADT registration + function-resolution map, layout resolution,
//! dataflow-driven sanity checking, `Call`/`Raise`/`?`-propagation
//! lowering, `StructLit`/`Field`/`Index` rvalue lowering,
//! `Variant`/`Struct` pattern tests, f-string lowering routed through
//! `std.fmt`, wrapping (`+%`/`-%`/`*%`) and checked (`+?`/`-?`/`*?`)
//! arithmetic operators, attribute payload threading, FnPtr lowering,
//! HeapPtr primitive + alloc-family externs, indexed and
//! field-projected assignment LHS, sum-typed returns, and more.
//!
//! The running history for any specific item lives in
//! `git log --oneline crates/edda-mir/`.
//!
//! # Layout
//!
//! Each MIR program is a [`MirProgram`] of:
//! - [`AdtDef`]s (algebraic data types),
//! - [`Body`]s (functions, each holding [`LocalDecl`]s and [`BasicBlockData`]s),
//! - [`Const`]s (interned constants).
//!
//! Each body is a CFG of basic blocks: each block has a flat list of
//! [`Statement`]s and exactly one [`Terminator`].

mod adt;
mod arena;
mod block;
mod body;
mod builder;
mod constant;
mod effect;
mod entry;
mod error;
mod ids;
mod layout;
mod lower;
mod operand;
mod place;
mod pretty;
mod program;
mod rvalue;
mod statement;
mod terminator;
mod ty;
mod validate;

pub use adt::{AdtDef, AdtKind, FieldDef, VariantDef};
pub use arena::{Idx, IndexVec};
pub use block::BasicBlockData;
pub use body::{Body, LocalDecl, LocalSource, Mutability, ParamInfo};
pub use builder::{BlockBuilder, BodyBuilder, ProgramBuilder};
pub use constant::{Const, ConstValue};
pub use effect::{CapabilityKind, EffectRow};
pub use entry::materialize_entry_capabilities;
pub use error::{LoweringError, MirError, ValidationError};
pub use ids::{AdtId, BlockId, BodyId, ConstId, EffectId, FieldIdx, LocalId, VariantIdx};
pub use layout::{AbiTag, AlignBytes, LayoutInfo, LayoutPolicy, ReprKind};
pub use lower::{
    lower, AllocFmtBindings, ConstInput, ExternInput, FmtBindings, FunctionInput, LoweringInput,
    TypeDeclInput,
};
pub use operand::Operand;
pub use place::{Place, Projection};
pub use pretty::{PrettyPrinter, pretty};
pub use program::MirProgram;
pub use rvalue::{BinOp, Rvalue, RvalueKind, UnOp};
pub use statement::{Statement, StatementKind};
pub use terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind, ThreadedCapability};
pub use ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};
pub use validate::validate;
