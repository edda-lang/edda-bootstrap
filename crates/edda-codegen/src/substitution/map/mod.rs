//! Comptime-parameter binding table.
//!
//! [`SubstitutionMap`] is the lookup structure consumed by the
//! AST substitution walker. Construction ([`SubstitutionMap::bind`])
//! position-wise matches each `GenericParam` to its `Argument`,
//! validates kind compatibility, and rejects the argument kinds the
//! walker does not yet implement (`EffectRow`, `UserDefined`). The
//! `Function` kind is admitted: a
//! `comptime f: function(...)` parameter binds a function reference and
//! the walker rewrites in-body `f(..)` calls through the same Type-style
//! path-rewrite machinery.
//!
//! The lookup key is the [`Symbol`] handle of the generic-parameter
//! name. Two `Ident`s referencing the same interned identifier carry
//! equal `Symbol`s by the interner contract, so the walker can match
//! a path's head segment against the bindings without re-resolving
//! through the interner.

mod mangle_names;

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{GenericKind, GenericParam, Item, ItemKind, TypeKind};
use smol_str::SmolStr;

use crate::argument::{Argument, ArgumentTuple};
use crate::error::CodegenError;

use self::mangle_names::{ast_mangled_name, post_subst_mangled_name};

//   parameter, in declaration order; lookup is by `Symbol` equality on
//   the parameter name
//   a `GenericKind::Comptime` parameter is bound to `Argument::Type`
//   (when its annotation is the `Type` meta-type) or `Argument::Primitive`
//   (otherwise). EffectRow / UserDefined are rejected at bind time.
/// Position-validated binding table for one spec invocation.
#[derive(Clone, Debug)]
pub struct SubstitutionMap {
    bindings: Box<[Binding]>,
}

//   spec's AST was lexed with
//   the argument tag (Type vs Primitive vs …) rather than on the spec
//   parameter's `GenericKind`, so the parameter kind is not stored here
/// One entry of a [`SubstitutionMap`].
#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) name: Symbol,
    pub(crate) value: Argument,
}

impl SubstitutionMap {
    /// The empty table — admissible for spec invocations whose spec
    /// takes no comptime arguments.
    pub fn empty() -> Self {
        Self {
            bindings: Box::new([]),
        }
    }

    //   binding's kind matches its position's `GenericParam.kind`, and
    //   no binding's value is `Argument::EffectRow` or
    //   `Argument::UserDefined`
    /// Bind a spec's declared generics to an argument tuple, validating
    /// arity and per-position kind compatibility.
    ///
    /// `spec_qualified` is the spec's resolved qualified name; it is
    /// attached to every error produced so the caller can render a
    /// useful diagnostic without re-threading the spec identity.
    ///
    /// Supports `Type`, `Primitive`, and `Function` argument
    /// kinds; the `EffectRow` and `UserDefined` kinds are rejected with
    /// [`CodegenError::MonomorphUnsupportedArgument`].
    pub fn bind(
        spec_qualified: &str,
        generics: &[GenericParam],
        args: &ArgumentTuple,
        interner: &Interner,
    ) -> Result<Self, CodegenError> {
        if generics.len() != args.args().len() {
            return Err(CodegenError::MonomorphArityMismatch {
                spec_qualified: SmolStr::new(spec_qualified),
                expected: generics.len(),
                found: args.args().len(),
            });
        }
        let mut bindings = Vec::with_capacity(generics.len());
        for (position, (gp, arg)) in generics.iter().zip(args.args().iter()).enumerate() {
            check_kind(spec_qualified, gp, arg, position, interner)?;
            check_supported(spec_qualified, gp, arg, position, interner)?;
            bindings.push(Binding {
                name: gp.name.name,
                value: arg.clone(),
            });
        }
        Ok(Self {
            bindings: bindings.into_boxed_slice(),
        })
    }

    /// Number of bound generic parameters.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// `true` if no generic parameters are bound.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Look up a binding by parameter-name symbol.
    pub(crate) fn lookup(&self, name: Symbol) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.name == name)
    }

    /// Augment the binding table with renames that re-qualify bare
    /// references to the spec's parent-module siblings. When a spec body
    /// references a sibling type/function bare (`with {err: AllocError}`
    /// in `std.alloc.Box`'s body, where `AllocError` is the parent
    /// `std.alloc` module's `type AllocError`), monomorphisation hoists
    /// the body into a new sibling module (`std.alloc.Box_Expr`) where
    /// the bare reference no longer resolves. Rewriting `AllocError` to
    /// `alloc.AllocError` plus a synthetic `import std.alloc` brings the
    /// reference back into scope.
    pub fn with_parent_siblings(
        mut self,
        parent_leaf: &str,
        sibling_names: &[Symbol],
        interner: &Interner,
    ) -> Self {
        if parent_leaf.is_empty() || sibling_names.is_empty() {
            return self;
        }
        let mut additions = Vec::with_capacity(sibling_names.len());
        for &name in sibling_names {
            if name == Symbol::DUMMY {
                continue;
            }
            let text = interner.resolve(name);
            let qualified = format!("{parent_leaf}.{text}");
            additions.push(Binding {
                name,
                value: Argument::Type(SmolStr::new(qualified)),
            });
        }
        if !additions.is_empty() {
            let mut all: Vec<Binding> = self.bindings.into_vec();
            all.extend(additions);
            self.bindings = all.into_boxed_slice();
        }
        self
    }

    /// Augment the binding table with `pre_mangled → post_mangled` renames
    /// for every `SpecInvocation` in `body`. After
    /// [`SubstitutionMap::bind`] swaps `comptime T: Type` parameters,
    /// each nested invocation's mangled name (`Option_V`) changes to its
    /// substituted form (`Option_f64`). Body references to the
    /// pre-substitution name must follow — the resolver registers the
    /// post-substitution name as a `BindingKind::SpecInvocation` in the
    /// generated module's scope, so the rename routes single-segment
    /// path heads through the existing `Argument::Type` rewrite path.
    pub fn with_sibling_renames(mut self, body: &[Item], interner: &Interner) -> Self {
        let mut additions = Vec::new();
        for item in body {
            let ItemKind::SpecInvocation(si) = &item.kind else { continue };
            let Some(pre) = ast_mangled_name(si, interner) else { continue };
            let post = post_subst_mangled_name(si, &self, interner);
            let Some(post) = post else { continue };
            if pre == post {
                continue;
            }
            additions.push(Binding {
                name: interner.intern(&pre),
                value: Argument::Type(SmolStr::new(post)),
            });
        }
        if !additions.is_empty() {
            let mut all: Vec<Binding> = self.bindings.into_vec();
            all.extend(additions);
            self.bindings = all.into_boxed_slice();
        }
        self
    }
}

//   {Type ↔ Type, Comptime[Type] ↔ Type, Comptime[function] ↔ Function, Comptime ↔ Primitive/EffectRow/UserDefined} matrix
/// Kind-compatibility check between a declared generic parameter and
/// its supplied argument. Does not reject yet-unsupported argument
/// kinds — that is [`check_supported`]'s responsibility.
fn check_kind(
    spec_qualified: &str,
    gp: &GenericParam,
    arg: &Argument,
    position: usize,
    interner: &Interner,
) -> Result<(), CodegenError> {
    let comptime_type_param = gp.kind == GenericKind::Comptime
        && matches!(gp.ty.as_ref().map(|t| &t.kind), Some(TypeKind::Meta));
    // A `comptime f: function(...) -> ...` parameter (the function-arg
    // form) accepts a function-reference argument. The annotation, not the
    // `comptime` keyword alone, gates this — a numeric comptime param
    // must not bind a function.
    let comptime_fn_param = gp.kind == GenericKind::Comptime
        && matches!(
            gp.ty.as_ref().map(|t| &t.kind),
            Some(TypeKind::Function { .. })
        );
    let ok = matches!(
        (gp.kind, arg),
        (GenericKind::Type, Argument::Type(_))
            | (GenericKind::Comptime, Argument::Primitive(_))
            | (GenericKind::Comptime, Argument::EffectRow(_))
            | (GenericKind::Comptime, Argument::UserDefined(_))
    ) || (comptime_type_param && matches!(arg, Argument::Type(_)))
        || (comptime_fn_param && matches!(arg, Argument::Function(_)));
    if ok {
        return Ok(());
    }
    Err(CodegenError::MonomorphKindMismatch {
        spec_qualified: SmolStr::new(spec_qualified),
        generic_name: SmolStr::new(interner.resolve(gp.name.name)),
        position,
        generic_kind: match gp.kind {
            GenericKind::Type => "type",
            GenericKind::Comptime => "comptime",
        },
        argument_kind_tag: arg.kind_tag(),
    })
}

//   and `Argument::Function` (the function-arg kind, which the walker
//   substitutes through the same path-rewrite machinery as `Type`);
//   returns `Err` for `Argument::EffectRow` and `Argument::UserDefined`
//   (not yet implemented)
/// Reject argument kinds the substitution walker does not yet implement.
fn check_supported(
    spec_qualified: &str,
    gp: &GenericParam,
    arg: &Argument,
    position: usize,
    interner: &Interner,
) -> Result<(), CodegenError> {
    match arg {
        Argument::EffectRow(_) | Argument::UserDefined(_) => {
            Err(CodegenError::MonomorphUnsupportedArgument {
                spec_qualified: SmolStr::new(spec_qualified),
                generic_name: SmolStr::new(interner.resolve(gp.name.name)),
                position,
                argument_kind_tag: arg.kind_tag(),
            })
        }
        Argument::Type(_) | Argument::Primitive(_) | Argument::Function(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests;
