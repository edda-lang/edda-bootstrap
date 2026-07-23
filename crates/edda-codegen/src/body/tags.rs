//! Locked one-byte kind tags for the AST node-kind taxonomy at
//! [`BodyVersion(0x01)`].
//!
//! Per `storage.md` §4, every AST node is serialised as a kind tag
//! followed by length-prefixed children. The taxonomy is split into
//! 22 families (`storage.md` §4); each family has its own one-byte
//! tag space and the families never share tags with each other (they
//! are not pooled into a single 256-value space).
//!
//! Pre-v0.1 the lowest-free-tag policy permits reordering. Post-v0.1
//! the values are immutable; retired variants keep their tag reserved;
//! any change to an existing variant's child shape requires bumping the
//! [`BodyVersion`] byte at the canonical-form layer and triggers full
//! codegen-tier rebuild.
//!
//! This module owns *only* the family tag tables. The encoder
//! ([`super::encoder`]) is responsible for emitting the chosen tag
//! alongside the variant's payload.

/// Tags for `edda_syntax::ast::TypeKind` (9 variants).
pub mod type_kind {
    pub const PATH: u8 = 0x00;
    pub const TUPLE: u8 = 0x01;
    pub const SLICE: u8 = 0x02;
    pub const UNIT: u8 = 0x03;
    pub const FUNCTION: u8 = 0x04;
    pub const META: u8 = 0x05;
    pub const COMPTIME: u8 = 0x06;
    pub const REFINED: u8 = 0x07;
    pub const ERROR: u8 = 0x08;
}

/// Tags for `edda_syntax::ast::FnBody` (2 variants).
pub mod fn_body {
    pub const BLOCK: u8 = 0x00;
    pub const EXTERN: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::ParamMode` (4 variants).
pub mod param_mode {
    pub const DEFAULT: u8 = 0x00;
    pub const INOUT: u8 = 0x01;
    pub const SINK: u8 = 0x02;
    pub const SET: u8 = 0x03;
}

/// Tags for `edda_syntax::ast::CallArg.mode` (4 states: bare + 3 keywords).
pub mod call_mode {
    pub const NONE: u8 = 0x00;
    pub const INOUT: u8 = 0x01;
    pub const SINK: u8 = 0x02;
    pub const SET: u8 = 0x03;
}

/// Tags for `edda_syntax::ast::EffectMember` (4 variants).
pub mod effect_member {
    pub const CAPABILITY: u8 = 0x00;
    pub const NAMED: u8 = 0x01;
    pub const SPREAD: u8 = 0x02;
    /// `kind(<bound>)` graded pure-effect entry per
    /// `02-modes-effects-refinements.md` §5.
    pub const GRADED: u8 = 0x03;
}

//            discriminator — every Scope artifact emitted before
//            `scope(coherence)` landed was exec; new artifacts emit
//            the byte explicitly
/// Tags for `edda_syntax::ast::ScopeKind` (2 variants).
pub mod scope_kind {
    pub const EXEC: u8 = 0x00;
    pub const COHERENCE: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::Literal` (6 variants).
pub mod literal {
    pub const INT: u8 = 0x00;
    pub const FLOAT: u8 = 0x01;
    pub const STR: u8 = 0x02;
    pub const FSTRING: u8 = 0x03;
    pub const BOOL: u8 = 0x04;
    pub const UNIT: u8 = 0x05;
}

/// Tags for `edda_syntax::token::IntBase` (4 variants). The base is
/// part of the canonical form because two source forms with different
/// bases (`16` vs `0x10`) are different ASTs even when the value is
/// equal; including the base matches the printer's round-trip rule.
pub mod int_base {
    pub const DEC: u8 = 0x00;
    pub const HEX: u8 = 0x01;
    pub const BIN: u8 = 0x02;
    pub const OCT: u8 = 0x03;
}

/// Tags for `edda_syntax::ast::BinOp` (28 variants).
pub mod bin_op {
    pub const ADD: u8 = 0x00;
    pub const SUB: u8 = 0x01;
    pub const MUL: u8 = 0x02;
    pub const DIV: u8 = 0x03;
    pub const MOD: u8 = 0x04;
    pub const EQ: u8 = 0x05;
    pub const NE: u8 = 0x06;
    pub const LT: u8 = 0x07;
    pub const LE: u8 = 0x08;
    pub const GT: u8 = 0x09;
    pub const GE: u8 = 0x0a;
    pub const AND: u8 = 0x0b;
    pub const OR: u8 = 0x0c;
    pub const BIT_AND: u8 = 0x0d;
    pub const BIT_OR: u8 = 0x0e;
    pub const BIT_XOR: u8 = 0x0f;
    pub const SHL: u8 = 0x10;
    pub const SHR: u8 = 0x11;
    /// Wrapping integer addition `+%` per `spec-sweep-locks.md` S1.
    pub const WRAP_ADD: u8 = 0x12;
    /// Wrapping integer subtraction `-%`.
    pub const WRAP_SUB: u8 = 0x13;
    /// Wrapping integer multiplication `*%`.
    pub const WRAP_MUL: u8 = 0x14;
    /// Checked integer addition `+?` (raises `err: Overflow`) per `spec-sweep-locks.md` S1.
    pub const CHECK_ADD: u8 = 0x15;
    /// Checked integer subtraction `-?`.
    pub const CHECK_SUB: u8 = 0x16;
    /// Checked integer multiplication `*?`.
    pub const CHECK_MUL: u8 = 0x17;
    /// Checked integer modulo `%?` (raises `err: Overflow` on `INT_MIN % -1`).
    pub const CHECK_MOD: u8 = 0x18;
    /// Saturating integer addition `+|` per CLAUDE.md §"Numeric operators".
    pub const SAT_ADD: u8 = 0x19;
    /// Saturating integer subtraction `-|`.
    pub const SAT_SUB: u8 = 0x1a;
    /// Saturating integer multiplication `*|`.
    pub const SAT_MUL: u8 = 0x1b;
}

/// Tags for `edda_syntax::ast::UnOp` (3 variants).
pub mod un_op {
    pub const NEG: u8 = 0x00;
    pub const NOT: u8 = 0x01;
    pub const BIT_NOT: u8 = 0x02;
}

/// Tags for `edda_syntax::ast::RangeKind` (2 variants).
pub mod range_kind {
    pub const HALF_OPEN: u8 = 0x00;
    pub const CLOSED: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::CastMode` (4 variants). Encoded as a
/// trailing byte after the cast-target type so older readers that
/// stop at the target type encounter the version-bump checksum
/// mismatch rather than silently misinterpret the mode byte.
pub mod cast_mode {
    pub const TRAP: u8 = 0x00;
    pub const WRAPPING: u8 = 0x01;
    pub const SATURATING: u8 = 0x02;
    pub const CHECKED: u8 = 0x03;
}

/// Tags for `edda_syntax::ast::AssignOp` (11 variants).
pub mod assign_op {
    pub const PLAIN: u8 = 0x00;
    pub const ADD: u8 = 0x01;
    pub const SUB: u8 = 0x02;
    pub const MUL: u8 = 0x03;
    pub const DIV: u8 = 0x04;
    pub const MOD: u8 = 0x05;
    pub const BIT_AND: u8 = 0x06;
    pub const BIT_OR: u8 = 0x07;
    pub const BIT_XOR: u8 = 0x08;
    pub const SHL: u8 = 0x09;
    pub const SHR: u8 = 0x0a;
}

/// Tags for `edda_syntax::ast::BindingMode` (3 variants).
pub mod binding_mode {
    pub const IMMUTABLE: u8 = 0x00;
    pub const MUTABLE: u8 = 0x01;
    pub const UNINIT: u8 = 0x02;
}

/// Tags for `edda_syntax::ast::StmtKind` (3 variants).
pub mod stmt_kind {
    pub const LET: u8 = 0x00;
    pub const ASSIGN: u8 = 0x01;
    pub const EXPR: u8 = 0x02;
}

/// Tags for `edda_syntax::ast::PatKind` (11 variants).
pub mod pat_kind {
    pub const WILDCARD: u8 = 0x00;
    pub const BINDING: u8 = 0x01;
    pub const LITERAL: u8 = 0x02;
    pub const TUPLE: u8 = 0x03;
    pub const VARIANT: u8 = 0x04;
    pub const STRUCT: u8 = 0x05;
    pub const GUARD: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    /// `lo..<hi` / `lo..=hi` literal range pattern. Layout after the
    /// tag: lower-bound literal, upper-bound literal, then a
    /// `RangeKind` tag byte. Added at `BodyVersion(0x0a)`.
    pub const RANGE: u8 = 0x08;
    /// `name @ subpattern`. Layout after the tag: bound ident, then the
    /// nested sub-pattern. Added at `BodyVersion(0x0a)`.
    pub const AT_BINDING: u8 = 0x09;
    /// `[p, ..]` / `[head, ..tail]` / `[..init, last]` / `[]` slice
    /// pattern. Layout after the tag: a u32-le prefix count then each
    /// prefix pattern; a rest frame (an `option_flag` presence byte,
    /// and when present a second `option_flag` + ident for the `..name`
    /// binding); a u32-le suffix count then each suffix pattern. Added
    /// at `BodyVersion(0x0a)`.
    pub const SLICE: u8 = 0x0a;
}

/// Tags for `edda_syntax::ast::VariantPatPayload` (3 variants).
pub mod variant_pat_payload {
    pub const NONE: u8 = 0x00;
    pub const TUPLE: u8 = 0x01;
    pub const STRUCT: u8 = 0x02;
}

/// Tags for `edda_syntax::ast::ExprKind` (33 encoded variants).
pub mod expr_kind {
    pub const LITERAL: u8 = 0x00;
    pub const PATH: u8 = 0x01;
    pub const BINARY: u8 = 0x02;
    pub const UNARY: u8 = 0x03;
    pub const CALL: u8 = 0x04;
    pub const METHOD_CALL: u8 = 0x05;
    pub const FIELD: u8 = 0x06;
    pub const INDEX: u8 = 0x07;
    pub const IF: u8 = 0x08;
    pub const MATCH: u8 = 0x09;
    pub const BLOCK: u8 = 0x0a;
    pub const CAST: u8 = 0x0b;
    pub const RANGE: u8 = 0x0c;
    pub const TUPLE: u8 = 0x0d;
    pub const STRUCT_LIT: u8 = 0x0e;
    pub const LOOP: u8 = 0x0f;
    pub const FOR: u8 = 0x10;
    pub const TRY: u8 = 0x11;
    pub const AWAIT: u8 = 0x12;
    pub const RAISE: u8 = 0x13;
    pub const PANIC: u8 = 0x14;
    pub const COMPTIME: u8 = 0x15;
    pub const COMPTIME_BLOCK: u8 = 0x16;
    /// `scope(exec) [<name>] { <block> }`. Layout after the tag: optional
    /// ident (binder name, `None` for the binder-free legacy form) then
    /// the body block. The optional-ident frame is the standard
    /// `OPT_NONE` / `OPT_SOME + ident` shape.
    pub const SCOPE: u8 = 0x17;
    pub const RETURN: u8 = 0x18;
    pub const BREAK: u8 = 0x19;
    pub const CONTINUE: u8 = 0x1a;
    pub const ERROR: u8 = 0x1b;
    pub const EFFECT_ROW: u8 = 0x1c;
    /// `<receiver>.<u32 index>` — tuple positional-field access. Layout
    /// after the tag: receiver expression, then u32-le index.
    pub const TUPLE_INDEX: u8 = 0x1d;
    /// `forall <bound> in <iter>: <body>` bounded universal quantifier.
    /// Layout after the tag: bound ident, iter expression, body
    /// expression. Per V1.0 refinement-fragment widening.
    pub const FORALL: u8 = 0x1e;
    /// `exists <bound> in <iter>: <body>` bounded existential quantifier.
    /// Same layout as [`FORALL`].
    pub const EXISTS: u8 = 0x1f;
    /// `<receiver>.(<index>)` — comptime-indexed field access (D-22).
    /// Layout after the tag: receiver expression, then the index
    /// expression. Appended at `0x20` (no existing tag reordered) with a
    /// `BodyVersion` bump per the wire-format lock.
    pub const COMP_FIELD: u8 = 0x20;
    /// `f"...{expr}..."` interpolated string.
    /// Layout after the tag: a u32-le part count, then for each part a
    /// 1-byte discriminator (`0x00` Text / `0x01` Slot) followed by a
    /// length-prefixed string (Text) or a nested expression (Slot).
    /// Appended at `0x21` (no existing tag reordered) with a
    /// `BodyVersion` bump per the wire-format lock.
    pub const FSTRING: u8 = 0x21;
    /// `[e1, ..., en]` array / slice literal, including the empty form
    /// `[]`. Layout after the tag: an
    /// expression sequence (u32-le count, then each element expression),
    /// identical to [`TUPLE`]'s payload. Appended at `0x22` (no existing
    /// tag reordered) with a `BodyVersion` bump per the wire-format lock.
    pub const ARRAY: u8 = 0x22;
}

/// Presence flag for the `Option<T>` shape: `0x00` = None, `0x01` = Some.
/// Used everywhere the AST holds an optional child (`Break.value`,
/// `Return`, `If.else_branch`, function-type `effects`, etc.).
pub mod option_flag {
    pub const NONE: u8 = 0x00;
    pub const SOME: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::Visibility` (2 variants).
pub mod visibility {
    pub const MODULE: u8 = 0x00;
    pub const PUBLIC: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::GenericKind` (2 variants).
pub mod generic_kind {
    pub const TYPE: u8 = 0x00;
    pub const COMPTIME: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::TypeDeclKind` (2 variants).
pub mod type_decl_kind {
    pub const PRODUCT: u8 = 0x00;
    pub const SUM: u8 = 0x01;
}

/// Tags for `edda_syntax::ast::VariantPayload` (3 variants). Distinct
/// from [`variant_pat_payload`] — type-decl payloads and pattern
/// payloads occupy separate families even though both share the
/// `Unit / Tuple / Struct` shape, so neither can shift independently
/// without bumping the body version byte.
pub mod variant_payload {
    pub const UNIT: u8 = 0x00;
    pub const TUPLE: u8 = 0x01;
    pub const STRUCT: u8 = 0x02;
}

/// Tags for `edda_syntax::ast::RefinementKind` (4 variants).
pub mod refinement_kind {
    pub const WHERE: u8 = 0x00;
    pub const REQUIRES: u8 = 0x01;
    pub const ENSURES: u8 = 0x02;
    /// `decreases` clause — termination measure. Added at
    /// `BodyVersion(0x04)`.
    pub const DECREASES: u8 = 0x03;
}

/// Tags for `edda_syntax::ast::ItemKind` (8 variants).
pub mod item_kind {
    pub const FUNCTION: u8 = 0x00;
    pub const TYPE_DECL: u8 = 0x01;
    pub const SPEC: u8 = 0x02;
    pub const IMPORT: u8 = 0x03;
    pub const MODULE: u8 = 0x04;
    pub const LET: u8 = 0x05;
    pub const SPEC_INVOCATION: u8 = 0x06;
    /// `derive` top-level form. Added at `BodyVersion(0x04)`.
    pub const DERIVE: u8 = 0x07;
}
