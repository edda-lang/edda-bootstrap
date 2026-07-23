//! Type, effect-row, and pattern clone-and-rewrite arms for the
//! substitution [`Walker`].
//!
//! Split out from `walk/mod.rs` for file-size reasons. These methods
//! deep-clone the type / effect-row / pattern AST shapes, recursing
//! through the walker's shared `ty` / `expr` helpers and rewriting type
//! reference path heads (`StructLit.path`, `Pat::Variant.path`,
//! `Pat::Struct.path`) through [`super::Walker`]'s `rewrite_path_as_type`.

use edda_syntax::ast::{
    CallArg, EffectMember, EffectRow, FnTypeParam, MatchArm, Pat, PatKind, StructLitField,
    StructPatField, TypeKind, VariantPatPayload,
};

use super::Walker;

impl<'a> Walker<'a> {
    pub(super) fn ty_kind(&self, k: &TypeKind) -> TypeKind {
        match k {
            TypeKind::Path(p) => TypeKind::Path(p.clone()),
            TypeKind::Tuple(ts) => TypeKind::Tuple(ts.iter().map(|t| self.ty(t)).collect()),
            TypeKind::Slice(t) => TypeKind::Slice(Box::new(self.ty(t))),
            TypeKind::Unit => TypeKind::Unit,
            TypeKind::Function {
                params,
                ret,
                effects,
            } => TypeKind::Function {
                params: params
                    .iter()
                    .map(|p| FnTypeParam {
                        span: p.span,
                        name: p.name,
                        mode: p.mode,
                        ty: self.ty(&p.ty),
                    })
                    .collect(),
                ret: Box::new(self.ty(ret)),
                effects: effects.as_ref().map(|e| self.effect_row(e)),
            },
            TypeKind::Meta => TypeKind::Meta,
            TypeKind::Comptime(t) => TypeKind::Comptime(Box::new(self.ty(t))),
            TypeKind::Refined { base, pred } => TypeKind::Refined {
                base: Box::new(self.ty(base)),
                pred: self.expr(pred),
            },
            TypeKind::Error => TypeKind::Error,
        }
    }

    pub(in crate::substitution) fn effect_row(&self, r: &EffectRow) -> EffectRow {
        EffectRow {
            span: r.span,
            members: r.members.iter().map(|m| self.effect_member(m)).collect(),
        }
    }

    fn effect_member(&self, m: &EffectMember) -> EffectMember {
        match m {
            EffectMember::Capability(i) => EffectMember::Capability(*i),
            EffectMember::Named { name, ty } => EffectMember::Named {
                name: *name,
                ty: self.ty(ty),
            },
            EffectMember::Spread(p) => EffectMember::Spread(p.clone()),
            EffectMember::Graded { kind, bound } => EffectMember::Graded {
                kind: *kind,
                bound: Box::new(self.expr(bound)),
            },
        }
    }

    pub(super) fn pat(&self, p: &Pat) -> Pat {
        Pat {
            span: p.span,
            kind: self.pat_kind(&p.kind),
        }
    }

    fn pat_kind(&self, k: &PatKind) -> PatKind {
        match k {
            PatKind::Wildcard => PatKind::Wildcard,
            PatKind::Binding(i) => PatKind::Binding(*i),
            PatKind::Literal(l) => PatKind::Literal(*l),
            PatKind::Tuple(ps) => PatKind::Tuple(ps.iter().map(|p| self.pat(p)).collect()),
            PatKind::Variant { path, payload } => PatKind::Variant {
                path: self.rewrite_path_as_type(path),
                payload: self.variant_pat_payload(payload),
            },
            PatKind::Struct { path, fields, rest } => PatKind::Struct {
                path: self.rewrite_path_as_type(path),
                fields: fields.iter().map(|f| self.struct_pat_field(f)).collect(),
                rest: *rest,
            },
            PatKind::Guard { pat, cond } => PatKind::Guard {
                pat: Box::new(self.pat(pat)),
                cond: self.expr(cond),
            },
            PatKind::Range { lo, hi, kind } => PatKind::Range {
                lo: *lo,
                hi: *hi,
                kind: *kind,
            },
            PatKind::AtBinding { name, inner } => PatKind::AtBinding {
                name: *name,
                inner: Box::new(self.pat(inner)),
            },
            PatKind::Slice {
                prefix,
                rest,
                suffix,
            } => PatKind::Slice {
                prefix: prefix.iter().map(|p| self.pat(p)).collect(),
                rest: *rest,
                suffix: suffix.iter().map(|p| self.pat(p)).collect(),
            },
            PatKind::Error => PatKind::Error,
        }
    }

    fn variant_pat_payload(&self, p: &VariantPatPayload) -> VariantPatPayload {
        match p {
            VariantPatPayload::None => VariantPatPayload::None,
            VariantPatPayload::Tuple(ps) => {
                VariantPatPayload::Tuple(ps.iter().map(|p| self.pat(p)).collect())
            }
            VariantPatPayload::Struct(fs) => VariantPatPayload::Struct(
                fs.iter().map(|f| self.struct_pat_field(f)).collect(),
            ),
        }
    }

    fn struct_pat_field(&self, f: &StructPatField) -> StructPatField {
        StructPatField {
            span: f.span,
            name: f.name,
            pat: self.pat(&f.pat),
        }
    }

    pub(super) fn match_arm(&self, a: &MatchArm) -> MatchArm {
        MatchArm {
            span: a.span,
            pat: self.pat(&a.pat),
            guard: a.guard.as_ref().map(|g| self.expr(g)),
            body: self.expr(&a.body),
        }
    }

    pub(super) fn struct_lit_field(&self, f: &StructLitField) -> StructLitField {
        StructLitField {
            span: f.span,
            name: f.name,
            mode: f.mode,
            value: self.expr(&f.value),
        }
    }

    pub(super) fn call_arg(&self, a: &CallArg) -> CallArg {
        CallArg {
            span: a.span,
            mode: a.mode,
            name: a.name.clone(),
            expr: self.expr(&a.expr),
        }
    }
}
