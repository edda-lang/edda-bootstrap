//! Typed cross-module context — [`TyCx`].
//!
//! After `edda-resolve` produces a [`ResolvedPackage`], the typechecker
//! lowers every item-level declaration (function signatures, type
//! definitions) once and stores them in a [`TyCx`]. Subsequent
//! inference of expression-level forms (calls, field access, struct
//! literals, multi-segment Path references) consults the [`TyCx`] by
//! [`BindingId`] to look up the relevant signature or layout.
//!
//! The [`TyCx`] is the typing-layer analogue of `edda-resolve`'s
//! [`ResolvedPackage`]: name resolution gives a [`BindingId`]; the
//! [`TyCx`] translates that [`BindingId`] into a typed surface.
//!
//! # What does *not* live here
//!
//! Param and Local [`BindingId`]s do **not** appear in [`TyCx`].
//! Their types are inferred at use-site through the symbol-keyed
//! `TyEnv` (the standard lexical-scope environment from
//! `inference-rules.md §1a.2`). Only items that need cross-module
//! lookup — function signatures and `type` declarations — live in
//! [`TyCx`].

use ahash::AHashMap;
use edda_intern::Symbol;
use edda_resolve::BindingId;
use edda_span::Span;
use edda_syntax::ast::{Expr, Linearity};

use crate::sig::FnSig;
use crate::ty::TyId;

/// Typed cross-module context.
///
/// Stores:
/// - Function signatures keyed by [`BindingId`] — populated by
///   walking the resolved package's AST once at build time.
/// - Product- / sum-type layouts keyed by [`BindingId`] — same.
/// - Module-level `let` constant types and folded initialiser values
///   keyed by [`BindingId`] — same; the recorded [`TyId`] is the
///   annotated declared type from the AST (`declarations.md`
///   §"Module-level let" requires annotations at module scope), and
///   the paired [`ConstInit`] is the constant-folded initialiser
///   that MIR lowering inlines at every reference site.
///
/// Local and Param [`BindingId`]s do **not** appear here; their
/// types are tracked at use-site through `TyEnv`.
#[derive(Default, Debug)]
pub struct TyCx {
    binding_sigs: AHashMap<BindingId, FnSig>,
    type_decls: AHashMap<BindingId, TypeDeclInfo>,
    binding_consts: AHashMap<BindingId, (TyId, ConstInit)>,
}

/// Constant-folded value of a module-level `let` declaration's
/// initialiser. Stored alongside the declared [`TyId`] in
/// [`TyCx::insert_const`] so MIR lowering can substitute a
/// `ConstValue` at every reference site instead of erroring out with
/// `lowering: unknown binding`.
///
/// Only literal initialisers and `-literal` are supported so far; complex
/// initialisers (arithmetic, calls, struct literals) lift to
/// [`ConstInit::Unsupported`] — the typechecker still records the
/// declared type so cross-module type inference keeps working, but
/// references will fail at MIR lowering with the same UnknownBinding
/// diagnostic users hit before this fold existed.
#[derive(Copy, Clone, Debug)]
pub enum ConstInit {
    /// Integer literal (with optional unary negation). Stored as
    /// signed `i128`; codegen narrows to the destination width.
    Int(i128),
    /// Float literal (with optional unary negation). Stored as
    /// IEEE-754 bits of the parsed `f64` so NaN payloads stay
    /// deterministic across the pipeline.
    Float(u64),
    /// `true` / `false` literal.
    Bool(bool),
    /// String literal (interned).
    Str(Symbol),
    /// Initialiser shape not yet supported by the literal folder.
    /// Reference sites surface `lowering: unknown binding` at MIR
    /// lowering — the same diagnostic that fires for every reference
    /// to a missing binding — so the user gets a precise span.
    Unsupported,
}

impl TyCx {
    /// Construct an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the signature of a Function binding.
    pub fn insert_sig(&mut self, id: BindingId, sig: FnSig) {
        self.binding_sigs.insert(id, sig);
    }

    /// Record the field / variant layout of a TypeDecl binding.
    pub fn insert_type_decl(&mut self, id: BindingId, info: TypeDeclInfo) {
        self.type_decls.insert(id, info);
    }

    /// Record the declared type and constant-folded initialiser of a
    /// module-level `let` constant binding. The initialiser is the
    /// AST `LetDecl.init` reduced to a flat [`ConstInit`] — see that
    /// type's docs for the supported initialiser shapes.
    pub fn insert_const(&mut self, id: BindingId, ty: TyId, init: ConstInit) {
        self.binding_consts.insert(id, (ty, init));
    }

    /// Look up a function signature by its [`BindingId`].
    pub fn sig(&self, id: BindingId) -> Option<&FnSig> {
        self.binding_sigs.get(&id)
    }

    /// Look up a type-decl layout by its [`BindingId`].
    pub fn type_decl(&self, id: BindingId) -> Option<&TypeDeclInfo> {
        self.type_decls.get(&id)
    }

    /// Look up a module-level `let` constant's declared type by its
    /// [`BindingId`].
    pub fn const_ty(&self, id: BindingId) -> Option<TyId> {
        self.binding_consts.get(&id).map(|(ty, _)| *ty)
    }

    /// Look up a module-level `let` constant's constant-folded
    /// initialiser by its [`BindingId`]. Returns `None` when no const
    /// is recorded; returns `Some(ConstInit::Unsupported)` when the
    /// initialiser was too complex for the current folder.
    pub fn const_init(&self, id: BindingId) -> Option<ConstInit> {
        self.binding_consts.get(&id).map(|(_, init)| *init)
    }

    /// Number of Function bindings recorded.
    pub fn sig_count(&self) -> usize {
        self.binding_sigs.len()
    }

    /// Number of TypeDecl bindings recorded.
    pub fn type_decl_count(&self) -> usize {
        self.type_decls.len()
    }

    /// Number of module-level `let` constant bindings recorded.
    pub fn const_count(&self) -> usize {
        self.binding_consts.len()
    }

    /// Iterate every recorded `(BindingId, &FnSig)` pair in arbitrary
    /// order. Used by method-call resolution (the typechecker's
    /// `synth_method_call`) to find the free function whose first
    /// parameter type matches the method-call receiver.
    pub fn iter_sigs(&self) -> impl Iterator<Item = (BindingId, &FnSig)> {
        self.binding_sigs.iter().map(|(id, sig)| (*id, sig))
    }

    /// Iterate every recorded `(BindingId, &TypeDeclInfo)` pair in
    /// arbitrary order. Used by the refine-integration `Schema` builder
    /// to walk every product type's field list into a
    /// `edda_refine::RecordSchema`.
    pub fn iter_type_decls(&self) -> impl Iterator<Item = (BindingId, &TypeDeclInfo)> {
        self.type_decls.iter().map(|(id, info)| (*id, info))
    }

    /// Iterate every recorded module-level `let` constant as a
    /// `(BindingId, TyId, ConstInit)` triple in arbitrary order.
    /// Consumed by the driver's MIR-lowering input builder to pre-
    /// intern each constant's value into the program so path
    /// references emit `Operand::Const(id)`.
    pub fn iter_consts(&self) -> impl Iterator<Item = (BindingId, TyId, ConstInit)> + '_ {
        self.binding_consts
            .iter()
            .map(|(id, (ty, init))| (*id, *ty, *init))
    }
}

/// Typed layout of one user-declared `type` declaration.
#[derive(Clone, Debug)]
pub struct TypeDeclInfo {
    /// Source span of the declaration.
    pub span: Span,
    /// Linearity modifier — `None` for freely-copyable types, `Some`
    /// for `linear` / `affine` types that the §4 function-exit
    /// relaxation must refuse to silently drop.
    pub linearity: Option<Linearity>,
    /// Product vs sum shape.
    pub kind: TypeDeclShape,
}

/// Discriminator for [`TypeDeclInfo::kind`].
#[derive(Clone, Debug)]
pub enum TypeDeclShape {
    /// Product type (record).
    Product {
        /// Fields in source order.
        fields: Box<[FieldInfo]>,
    },
    /// Sum type (variants).
    Sum {
        /// Variants in source order.
        variants: Box<[VariantInfo]>,
    },
}

impl TypeDeclInfo {
    /// Look up a field by name. Returns `None` for sum types or when
    /// the field is not declared.
    pub fn field(&self, name: Symbol) -> Option<&FieldInfo> {
        match &self.kind {
            TypeDeclShape::Product { fields } => fields.iter().find(|f| f.name == name),
            TypeDeclShape::Sum { .. } => None,
        }
    }

    /// Borrow the field list (empty for sum types).
    pub fn fields(&self) -> &[FieldInfo] {
        match &self.kind {
            TypeDeclShape::Product { fields } => fields,
            TypeDeclShape::Sum { .. } => &[],
        }
    }

    /// Borrow the variant list (empty for product types).
    pub fn variants(&self) -> &[VariantInfo] {
        match &self.kind {
            TypeDeclShape::Sum { variants } => variants,
            TypeDeclShape::Product { .. } => &[],
        }
    }

    /// Look up a variant by name. Returns `None` for product types
    /// or when the variant is not declared.
    pub fn variant(&self, name: Symbol) -> Option<&VariantInfo> {
        match &self.kind {
            TypeDeclShape::Sum { variants } => variants.iter().find(|v| v.name == name),
            TypeDeclShape::Product { .. } => None,
        }
    }
}

/// One named field inside a product type or struct-payload variant.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    /// Source span of the field declaration.
    pub span: Span,
    /// Field name.
    pub name: Symbol,
    /// Declared type.
    pub ty: TyId,
    /// Inline `where`-clause predicate on this field's own type, if any.
    pub refinement: Option<Expr>,
}

/// One variant of a sum type.
#[derive(Clone, Debug)]
pub struct VariantInfo {
    /// Source span of the variant declaration.
    pub span: Span,
    /// Variant name.
    pub name: Symbol,
    /// Payload shape.
    pub payload: VariantPayloadInfo,
}

/// Discriminator for [`VariantInfo::payload`].
#[derive(Clone, Debug)]
pub enum VariantPayloadInfo {
    /// `case foo` — no payload.
    Unit,
    /// `case foo(T, U)` — positional payload.
    Tuple {
        /// Element types in source order.
        elems: Box<[TyId]>,
    },
    /// `case foo { x: T, y: U }` — named payload.
    Struct {
        /// Named fields in source order.
        fields: Box<[FieldInfo]>,
    },
}


#[cfg(test)]
#[path = "cx_tests.rs"]
mod tests;
