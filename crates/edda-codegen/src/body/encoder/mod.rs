//! AST encoder — walks AST nodes and produces the deterministic byte
//! sequence that fills [`crate::CanonicalForm::canonical_body`].
//!
//! See [`super`] for the surface inventory.

use edda_intern::Interner;
use edda_syntax::ast::{EffectMember, EffectRow, Ident, ParamMode, Type, TypeKind};

use crate::body::resolver::QualifiedNameResolver;
use crate::body::tags;

#[cfg(test)]
mod tests;

//   given AST input + resolver mapping — calling twice with identical
//   inputs produces identical output
/// Stateful AST encoder. Hand the buffer to the hash kernel via
/// [`Encoder::into_bytes`] once every node has been written.
pub struct Encoder<'a> {
    out: Vec<u8>,
    interner: &'a Interner,
    resolver: &'a dyn QualifiedNameResolver,
}

impl<'a> Encoder<'a> {
    /// Construct a new encoder. The buffer starts empty; call one of the
    /// `write_*` methods to append AST encodings.
    pub fn new(interner: &'a Interner, resolver: &'a dyn QualifiedNameResolver) -> Self {
        Encoder {
            out: Vec::new(),
            interner,
            resolver,
        }
    }

    /// Consume the encoder and return the accumulated bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    /// Borrow the bytes written so far without consuming the encoder.
    pub fn as_bytes(&self) -> &[u8] {
        &self.out
    }

    /// Encode a `Type` AST node, emitting its [`tags::type_kind`] tag
    /// followed by the kind-specific payload.
    pub fn write_type(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Path(path) => {
                self.out.push(tags::type_kind::PATH);
                let qualified = self.resolver.resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
            }
            TypeKind::Tuple(elems) => {
                self.out.push(tags::type_kind::TUPLE);
                self.write_u32_le(checked_u32(elems.len()));
                for elem in elems {
                    self.write_type(elem);
                }
            }
            TypeKind::Slice(inner) => {
                self.out.push(tags::type_kind::SLICE);
                self.write_type(inner);
            }
            TypeKind::Unit => {
                self.out.push(tags::type_kind::UNIT);
            }
            TypeKind::Function {
                params,
                ret,
                effects,
            } => {
                self.out.push(tags::type_kind::FUNCTION);
                self.write_u32_le(checked_u32(params.len()));
                for param in params {
                    // Function-type params carry an optional name and an
                    // optional mode prefix in the surface grammar; for
                    // canonical-form hashing only the inner type is
                    // load-bearing — names and modes affect ABI but not
                    // structural identity here. The full encoding lands
                    // alongside the rest of the function-type pipeline.
                    self.write_type(&param.ty);
                }
                self.write_type(ret);
                self.write_optional_effect_row(effects.as_ref());
            }
            TypeKind::Meta => {
                self.out.push(tags::type_kind::META);
            }
            TypeKind::Comptime(inner) => {
                self.out.push(tags::type_kind::COMPTIME);
                self.write_type(inner);
            }
            TypeKind::Refined { base, pred } => {
                self.out.push(tags::type_kind::REFINED);
                self.write_type(base);
                self.write_expr(pred);
            }
            TypeKind::Error => {
                self.out.push(tags::type_kind::ERROR);
            }
        }
    }

    /// Encode a [`ParamMode`] as a single kind tag byte.
    pub fn write_param_mode(&mut self, mode: ParamMode) {
        let tag = match mode {
            ParamMode::Default => tags::param_mode::DEFAULT,
            ParamMode::Mutable => tags::param_mode::INOUT,
            ParamMode::Take => tags::param_mode::SINK,
            ParamMode::Init => tags::param_mode::SET,
        };
        self.out.push(tag);
    }

    //   the count prefix is u32-le and each member writes its tag plus
    //   its payload
    /// Encode an `EffectRow`: u32-le member count followed by each
    /// [`EffectMember`] in source order.
    ///
    /// Source order is preserved at this layer; canonical-row ordering
    /// (`storage.md` §6) is applied at the `Argument::EffectRow`
    /// argument-kind boundary, not here (deferred).
    pub fn write_effect_row(&mut self, row: &EffectRow) {
        self.write_u32_le(checked_u32(row.members.len()));
        for member in &row.members {
            self.write_effect_member(member);
        }
    }

    /// Encode a single [`EffectMember`].
    pub fn write_effect_member(&mut self, member: &EffectMember) {
        match member {
            EffectMember::Capability(ident) => {
                self.out.push(tags::effect_member::CAPABILITY);
                let name = self.interner.resolve(ident.name);
                self.write_length_prefixed_str(name);
            }
            EffectMember::Named { name, ty } => {
                self.out.push(tags::effect_member::NAMED);
                let label = self.interner.resolve(name.name);
                self.write_length_prefixed_str(label);
                self.write_type(ty);
            }
            EffectMember::Spread(path) => {
                self.out.push(tags::effect_member::SPREAD);
                let qualified = self.resolver.resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
            }
            EffectMember::Graded { kind, bound } => {
                self.out.push(tags::effect_member::GRADED);
                let kind_name = self.interner.resolve(kind.name);
                self.write_length_prefixed_str(kind_name);
                self.write_expr(bound);
            }
        }
    }

    /// Encode `Some(row)` as `0x01` + row, `None` as `0x00`.
    pub(super) fn write_optional_effect_row(&mut self, row: Option<&EffectRow>) {
        match row {
            Some(row) => {
                self.out.push(tags::option_flag::SOME);
                self.write_effect_row(row);
            }
            None => {
                self.out.push(tags::option_flag::NONE);
            }
        }
    }

    pub(super) fn push_byte(&mut self, byte: u8) {
        self.out.push(byte);
    }

    //   byte length (UTF-8 unit count), not the char count
    pub(super) fn write_length_prefixed_str(&mut self, s: &str) {
        self.write_u32_le(checked_u32(s.len()));
        self.out.extend_from_slice(s.as_bytes());
    }

    pub(super) fn write_u32_le(&mut self, value: u32) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    //   — `Ident` is a bare name (field, method, label) that does *not*
    //   pass through [`QualifiedNameResolver`]
    //   from `expect_ident`) writes the literal string `<dummy>` instead
    //   of panicking through `Interner::resolve`. Reaching this branch
    //   indicates a recovery AST leaked into codegen — the cascade is
    //   supposed to short-circuit on parse errors before codegen runs,
    //   so this is defense in depth, not a normal code path.
    pub(super) fn write_ident(&mut self, ident: &Ident) {
        if ident.name == edda_intern::Symbol::DUMMY {
            self.write_length_prefixed_str("<dummy>");
            return;
        }
        let name = self.interner.resolve(ident.name);
        self.write_length_prefixed_str(name);
    }

    pub(super) fn write_optional_ident(&mut self, ident: Option<&Ident>) {
        match ident {
            Some(id) => {
                self.out.push(tags::option_flag::SOME);
                self.write_ident(id);
            }
            None => {
                self.out.push(tags::option_flag::NONE);
            }
        }
    }


    //   construction-time borrow), not `&self` — callers may use the
    //   returned `&str` after a subsequent `&mut self` write
    pub(super) fn interner(&self) -> &'a Interner {
        self.interner
    }

    //   construction-time borrow), not `&self`
    pub(super) fn resolver(&self) -> &'a dyn QualifiedNameResolver {
        self.resolver
    }
}

//   exceeding `u32::MAX` would mean a single AST field is over 4 GiB
pub(super) fn checked_u32(n: usize) -> u32 {
    debug_assert!(
        n <= u32::MAX as usize,
        "edda-codegen: AST encoder length {n} exceeds u32::MAX",
    );
    n as u32
}
