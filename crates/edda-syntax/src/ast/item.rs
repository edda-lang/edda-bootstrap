//! Item AST — top-level declarations within a [`super::File`].
//!
//! Items are functions, type declarations (product or sum), `spec`
//! declarations, imports, and rare `module` overrides. Each item carries
//! its leading outer-doc comments and a visibility modifier.

use edda_intern::Symbol;
use edda_span::Span;

use super::{Attribute, DocLine, EffectRow, Expr, Ident, Path, RefinementClause, Type};
use super::expr::Block;
use super::ty::{ParamMode, ReturnMode};

/// A single top-level declaration.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Item {
    /// Source range covering the item and its leading doc-comments.
    pub span: Span,
    /// Doc-comments attached to this item, in source order. Each line
    /// carries its tier per `01-syntax.md` §3.2.
    pub doc: Vec<DocLine>,
    /// Item-level attributes (`@name(args)`) in source order.
    pub attributes: Vec<Attribute>,
    /// Variant and payload.
    pub kind: ItemKind,
}

/// Stability modifier on a function / type / spec declaration per `01-syntax.md` §3.7.
///
/// `stable` is a load-bearing claim verified by the compiler. Calling a
/// non-stable function from a stable one is a compile error
/// (`stability_callee`). `unstable` is the explicit opposite.
///
/// Per §3.7 / D-19 the `stable` / `unstable` keyword is the sole source
/// of API stability — written as the leading token of a declaration
/// (`stable type T`) or in post-visibility position (`public stable
/// type T`). `@stable` / `@unstable` are not attributes (they reject as
/// `unknown_attribute` in `edda-types`). The `stable` keyword between
/// visibility and `function` is a separate concept — the refinement-
/// stability marker on `FnDecl`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Stability {
    /// `stable` keyword (leading or post-visibility). `since` is always
    /// `None` — the keyword carries no version argument.
    Stable {
        since: Option<Symbol>,
    },
    /// `unstable` keyword. Mirrors [`Stability::Stable`] — the explicit
    /// opt-out of the stable lock. `since` is always `None`.
    Unstable {
        since: Option<Symbol>,
    },
}

/// Every top-level item form admitted by the locked surface.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ItemKind {
    /// `function` declaration (with optional generics, effects, refinements).
    Function(Box<FnDecl>),
    /// `type` declaration — product or sum form.
    TypeDecl(Box<TypeDecl>),
    /// `spec NAME(params) [where ...] { body }` codegen-spec declaration.
    Spec(Box<Spec>),
    /// `spec Path(args)` top-level spec invocation per `comptime.md` §312.
    /// Distinct from a `Spec` declaration: no body, no `where` clauses.
    SpecInvocation(Box<SpecInvocation>),
    /// `let name: Type = expr` module-level constant binding.
    /// At module scope, position implies compile-time evaluation
    /// (`declarations.md` §"Module-level let").
    Let(Box<LetDecl>),
    /// `import path` (also bare-leaf form).
    Import(Import),
    /// `module dot.path` override at the file's top.
    Module(ModuleDecl),
    /// `derive eq, hash, … for Type` closed-vocabulary derive declaration
    /// per `corpus/edda-codex/language/04-specs-comptime.md` §5. Desugars
    /// in codegen to a sequence of `spec std.<path>(Type)` invocations.
    Derive(Box<Derive>),
}

/// Visibility opt-in. Only two states are admitted.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Visibility {
    /// `public` — visible outside the module.
    Public,
    /// No prefix — module-local (the default).
    Module,
}

/// A function declaration `function name<outbound>(params) -> R with {...} where {...} { body }`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FnDecl {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Stability modifier (default: absent — no claim).
    pub stability: Option<Stability>,
    /// Visibility modifier (default: `Module`).
    pub visibility: Visibility,
    /// Refinement-stability marker — the `stable` keyword between
    /// visibility and `function`. Asserts the function produces equal
    /// outputs for equal-by-equality inputs across runs and machines
    /// per `03-verification.md` §7. Orthogonal to [`Self::stability`],
    /// the function's API-stability keyword: in this decl-position the
    /// `stable` keyword names refinement-stability *only*. API
    /// stability is the leading / post-visibility `stable` / `unstable`
    /// keyword (`stable function f`, before `public`).
    pub refinement_stable: bool,
    /// Function name.
    pub name: Ident,
    /// Outbound type parameters from the `<...>` clause between name and
    /// `(`. Each entry is a `comptime <name>: <Type>` declaration per
    /// phase-2-locks Gap 3.
    pub outbound_generics: Vec<GenericParam>,
    /// Inbound-lifted comptime generics from the `comptime <name>: <Type>`
    /// parameter prefix per `comptime.md` §102.
    pub generics: Vec<GenericParam>,
    /// Value parameters in declaration order.
    pub params: Vec<Param>,
    /// Optional return type. Absent means `()`.
    pub return_ty: Option<Type>,
    /// Borrow mode on the return type: `ByValue` (no prefix, the
    /// default), `let` (`-> let T`), or `mutable` (`-> mutable T`). A
    /// non-`ByValue` mode makes the return a borrow tied to a
    /// by-reference parameter; the typechecker enforces that binding.
    pub return_mode: ReturnMode,
    /// Optional effect row from the `with { ... }` clause.
    pub effects: Option<EffectRow>,
    /// `requires` / `ensures` clauses in source order.
    pub refinements: Vec<RefinementClause>,
    /// Function body — either a source `{ ... }` block or an
    /// `extern "symbol"` declaration that binds the function to a
    /// linker-visible symbol with no Edda-side body.
    pub body: FnBody,
}

/// The body of an [`FnDecl`] — a `{ ... }` source block or an
/// `extern "symbol"` declaration that names the linker-visible entry
/// point the function lowers to.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum FnBody {
    /// `{ ... }` source body. Walked by the resolver, lowered to HIR,
    /// type-checked, then lowered to MIR like any other function.
    Block(Block),
    /// `extern "symbol"` declaration — no Edda-side body. The resolver
    /// records the function binding but skips the body walk; the
    /// typechecker registers the signature but skips body inference;
    /// MIR lowering emits a `FuncRef::Extern { name, sig }` at every
    /// call site instead of a `FuncRef::Body(_)`.
    Extern {
        /// Source range covering the `extern "..."` clause.
        span: Span,
        /// Source range covering the string-literal symbol name (used
        /// for diagnostic labels pointing at the symbol).
        name_span: Span,
        /// Interned linker-visible symbol name (the escaped string
        /// payload of the literal — same payload the lexer would store
        /// for any `"..."` literal).
        name: Symbol,
        /// Importing-DLL name from the optional `from "dll"` clause.
        /// `Some` makes the symbol a
        /// PE `.idata` import from the named DLL at link time; `None`
        /// keeps the static (`edda_rt.lib`-style) resolution.
        dll: Option<Symbol>,
    },
}

/// One value parameter on an [`FnDecl`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Param {
    /// Source range covering the parameter.
    pub span: Span,
    /// Parameter name (always present; anonymous params are not admitted).
    pub name: Ident,
    /// Parameter mode (`let` / `mutable` / `take` / `init`).
    pub mode: ParamMode,
    /// Parameter type.
    pub ty: Type,
}

/// A generic parameter on an [`FnDecl`], [`TypeDecl`], or [`Spec`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct GenericParam {
    /// Source range covering the parameter.
    pub span: Span,
    /// `Type` (regular) or `Comptime` (value) parameter.
    pub kind: GenericKind,
    /// Parameter name.
    pub name: Ident,
    /// Type annotation: required for the `comptime <name>: <Type>`
    /// function-parameter form, absent for bare type generics. The carried
    /// type is admitted by the locked type grammar (`Type` meta-type,
    /// primitives like `usize`, etc.).
    pub ty: Option<Type>,
    /// `where <name> admits ...` constraint list. Empty when the clause
    /// is absent. Constraint atoms are operators / constants (phase-2-locks
    /// Gap 9) and member-shape (`name: <fn-type>`) entries (Gap 6).
    pub admits: Vec<AdmitsConstraint>,
}

/// One atom in a generic parameter's `where ... admits ...` constraint
/// list. Operator and constant constraints originate from phase-2-locks
/// Gap 9; member-shape constraints originate from Gap 6.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum AdmitsConstraint {
    /// Operator constraint: `+`, `-`, `*`, `/`, `%`, `<`, `<=`, `>`,
    /// `>=`, `==`, `!=`. Spelled as the source operator token.
    Op {
        /// Source range covering the operator.
        span: Span,
        /// Binary operator the type must admit.
        op: super::BinOp,
    },
    /// Constant constraint: `0`, `1`, etc. Used for identity-element
    /// constraints like `T admits +, 0`.
    Literal {
        /// Source range covering the literal.
        span: Span,
        /// Constant literal payload (current uses: integer constants).
        lit: super::Literal,
    },
    /// Member-shape constraint: `<name>: <fn-type>` (Gap 6). Used on
    /// `comptime A: Module` parameters to declare the structural shape
    /// the module must expose.
    Member {
        /// Source range covering the whole constraint.
        span: Span,
        /// Member name (e.g. `next`).
        name: Ident,
        /// Required member type (typically a function type).
        ty: Type,
    },
}

/// Discriminator for [`GenericParam::kind`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum GenericKind {
    /// A type parameter (no prefix).
    Type,
    /// A `comptime` value parameter.
    Comptime,
}

/// Linearity modifier on a `type` declaration per the codex's
/// first-class type-level consumption discipline.
///
/// `linear` types must be consumed exactly once; `affine` types may
/// be consumed at most once. The compiler tracks usage across modes
/// (`let` / `mutable` / `take` / `init`). Absence of the modifier
/// means the type is freely copyable / droppable.
///
/// Parser admits the keyword between visibility and `type` (e.g.
/// `public affine type AtomicI32 { … }`). Semantic enforcement is
/// downstream of this parser-level surface.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Linearity {
    /// `affine` keyword — values may be dropped at most once.
    Affine,
    /// `linear` keyword — values must be consumed exactly once.
    Linear,
}

/// A `type` declaration — product or sum form.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct TypeDecl {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Stability modifier (default: absent).
    pub stability: Option<Stability>,
    /// Visibility modifier.
    pub visibility: Visibility,
    /// Linearity modifier (`affine` / `linear` keyword between
    /// visibility and `type`). Absent means the type is freely
    /// copyable / droppable.
    pub linearity: Option<Linearity>,
    /// Type name (CamelCase by convention; not enforced here).
    pub name: Ident,
    /// Generic / comptime parameters.
    pub generics: Vec<GenericParam>,
    /// Product vs sum shape.
    pub kind: TypeDeclKind,
}

/// Distinguishes product types (records) from sum types (variants).
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TypeDeclKind {
    /// `type Point { x: f64, y: f64 }` — record / struct.
    Product {
        /// Named fields in declaration order.
        fields: Vec<TypeField>,
    },
    /// `type Color { case red, case rgb(r: u8, g: u8, b: u8) }` — sum.
    Sum {
        /// Variants in declaration order.
        variants: Vec<Variant>,
    },
}

/// A single named field inside a product type or struct-variant payload.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct TypeField {
    /// Source range covering the field.
    pub span: Span,
    /// Field name.
    pub name: Ident,
    /// Field type.
    pub ty: Type,
    /// Optional field-level `where` refinement.
    pub refinement: Option<super::Expr>,
}

/// A single variant of a sum type.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Variant {
    /// Source range covering the variant.
    pub span: Span,
    /// Variant name (snake_case by convention).
    pub name: Ident,
    /// Payload shape: unit, positional tuple, or named struct.
    pub payload: VariantPayload,
}

/// Payload of a sum-type [`Variant`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum VariantPayload {
    /// `case foo` — no payload.
    Unit,
    /// `case foo(T, U)` — positional payload.
    Tuple(Vec<Type>),
    /// `case foo { x: T, y: U }` — named payload.
    Struct(Vec<TypeField>),
}

/// A `spec NAME(<params>) [where <pred>]* { <items> }` codegen-spec declaration.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Spec {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Stability modifier (default: absent).
    pub stability: Option<Stability>,
    /// Visibility modifier (default: `Module`).
    pub visibility: Visibility,
    /// Spec name.
    pub name: Ident,
    /// Generic / comptime parameters (typically `comptime` value params).
    pub generics: Vec<GenericParam>,
    /// `where` clauses constraining acceptable comptime arguments per
    /// `comptime.md` §292.
    pub where_clauses: Vec<RefinementClause>,
    /// Spec body — a sequence of items per `declarations.md` §253.
    pub body: Vec<Item>,
}

/// A top-level `spec Path(<args>)` invocation per `comptime.md` §312.
/// Distinct from a [`Spec`] declaration: no body, no `where` clauses.
/// The `args` are comptime-evaluable expressions validated by
/// `edda-types` against the referenced spec's `generics` per
/// `spec-language.md`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SpecInvocation {
    /// Source range covering the entire invocation.
    pub span: Span,
    /// Path naming the spec being invoked (e.g. `std.option.Option`).
    pub path: Path,
    /// Comptime arguments in declaration order.
    pub args: Vec<Expr>,
}

/// An `import dot.path` declaration (also covers the bare-leaf form
/// `import value` for sibling-file resolution). The optional
/// `alias` is the identifier introduced by an `as <ident>` clause
/// (`import std.core.cmp as ccmp`); when present, it replaces the
/// leaf name in every scope-binding decision. The optional
/// `selection` is the selected-name list introduced by a `.{name,
/// name, ...}` clause (`import std.os.fs.{read, write}`).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Import {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Imported module path.
    pub path: Path,
    /// Optional `as <ident>` alias that overrides the leaf name for scope binding.
    pub alias: Option<Ident>,
    /// Optional `.{name, name, ...}` selected-name list.
    pub selection: Option<Vec<Ident>>,
}

/// A `module dot.path` declaration overriding the file's path-derived
/// module identity. Rare; appears at the file's top before any imports.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ModuleDecl {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Override path.
    pub path: Path,
}

/// A module-level `let name: Type = expr` declaration. Declares a
/// compile-time-evaluated immutable binding visible to every importer.
/// There is no `const` keyword in Edda; at module scope, position implies
/// compile-time evaluation (`declarations.md` §"Module-level let").
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LetDecl {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Visibility modifier (default: `Module`).
    pub visibility: Visibility,
    /// Bound name.
    pub name: Ident,
    /// Declared type (required at module level).
    pub ty: Type,
    /// Initialiser expression (required at module level).
    pub init: Expr,
}

/// A `derive <items> for <Type>` top-level declaration per
/// `corpus/edda-codex/language/04-specs-comptime.md` §5. The parser
/// admits any well-formed identifier list; `edda-resolve` enforces the
/// closed whitelist and reports `derive_unknown` for non-admitted names.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Derive {
    /// Source range covering the entire declaration.
    pub span: Span,
    /// Items to derive, in source order. Each item is a bare identifier
    /// from the locked closed whitelist (`eq`, `ord`, `hash`, `debug`,
    /// `clone`, `properties`, `serialize`, `deserialize`).
    pub items: Vec<Ident>,
    /// Target type path. The desugaring produces one
    /// `spec std.<path>(<target>)` invocation per item.
    pub target: Path,
}
