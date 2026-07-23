//! Expression, statement, and pattern encoders.
//!
//! Each public method on [`Encoder`] dispatches on the variant kind,
//! emits the tag byte from [`super::tags`], and recursively encodes the
//! variant's payload. AST `Path` nodes resolve through the encoder's
//! [`super::QualifiedNameResolver`]; bare `Ident`s (field names, method
//! names, loop labels) bypass the resolver and are written as
//! length-prefixed UTF-8 from the interner.
//!
//! See [`super`] for the surface inventory.

use edda_syntax::ast::{
    Block, CallArg, CallMode, Expr, ExprKind, FStringPart, MatchArm, Pat, PatKind, Stmt, StmtKind,
    StructLitField, StructPatField, VariantPatPayload,
};

use crate::body::encoder::{checked_u32, Encoder};
use crate::body::tags;

#[cfg(test)]
mod tests;

impl<'a> Encoder<'a> {
    /// Encode an [`Expr`] AST node, emitting its [`tags::expr_kind`] tag
    /// followed by the kind-specific payload.
    pub fn write_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Literal(lit) => {
                self.push_byte(tags::expr_kind::LITERAL);
                self.write_literal(lit);
            }
            ExprKind::FString(parts) => {
                self.push_byte(tags::expr_kind::FSTRING);
                self.write_u32_le(checked_u32(parts.len()));
                for part in parts {
                    match part {
                        FStringPart::Text(sym) => {
                            self.push_byte(0x00);
                            let text = self.interner().resolve(*sym).to_string();
                            self.write_length_prefixed_str(&text);
                        }
                        FStringPart::Slot(slot) => {
                            self.push_byte(0x01);
                            self.write_expr(slot);
                        }
                    }
                }
            }
            ExprKind::Path(path) => {
                self.push_byte(tags::expr_kind::PATH);
                let qualified = self.resolver().resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.push_byte(tags::expr_kind::BINARY);
                self.write_bin_op(*op);
                self.write_expr(lhs);
                self.write_expr(rhs);
            }
            ExprKind::Unary { op, expr } => {
                self.push_byte(tags::expr_kind::UNARY);
                self.write_un_op(*op);
                self.write_expr(expr);
            }
            ExprKind::Call { callee, args } => {
                self.push_byte(tags::expr_kind::CALL);
                self.write_expr(callee);
                self.write_call_arg_seq(args);
            }
            ExprKind::MethodCall {
                receiver,
                name,
                args,
            } => {
                self.push_byte(tags::expr_kind::METHOD_CALL);
                self.write_expr(receiver);
                self.write_ident(name);
                self.write_call_arg_seq(args);
            }
            ExprKind::Field { receiver, name } => {
                self.push_byte(tags::expr_kind::FIELD);
                self.write_expr(receiver);
                self.write_ident(name);
            }
            ExprKind::TupleIndex { receiver, index } => {
                self.push_byte(tags::expr_kind::TUPLE_INDEX);
                self.write_expr(receiver);
                self.write_u32_le(*index);
            }
            ExprKind::CompField { receiver, index } => {
                self.push_byte(tags::expr_kind::COMP_FIELD);
                self.write_expr(receiver);
                self.write_expr(index);
            }
            ExprKind::Index { receiver, index } => {
                self.push_byte(tags::expr_kind::INDEX);
                self.write_expr(receiver);
                self.write_expr(index);
            }
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.push_byte(tags::expr_kind::IF);
                self.write_expr(cond);
                self.write_block(then_block);
                self.write_optional_expr(else_branch.as_deref());
            }
            ExprKind::Match { scrutinee, arms } => {
                self.push_byte(tags::expr_kind::MATCH);
                self.write_expr(scrutinee);
                self.write_u32_le(checked_u32(arms.len()));
                for arm in arms {
                    self.write_match_arm(arm);
                }
            }
            ExprKind::Block(block) => {
                self.push_byte(tags::expr_kind::BLOCK);
                self.write_block(block);
            }
            ExprKind::Cast { expr, ty, mode } => {
                self.push_byte(tags::expr_kind::CAST);
                self.write_expr(expr);
                self.write_type(ty);
                self.write_cast_mode(*mode);
            }
            ExprKind::Range { lo, hi, kind } => match (lo, hi) {
                (Some(lo), Some(hi)) => {
                    self.push_byte(tags::expr_kind::RANGE);
                    self.write_expr(lo);
                    self.write_expr(hi);
                    self.write_range_kind(*kind);
                }
                // Open-ended slice-subrange forms have no canonical
                // encoding yet — emit the Error sentinel to keep the
                // byte stream well-formed for downstream hashing.
                _ => {
                    self.push_byte(tags::expr_kind::ERROR);
                }
            },
            ExprKind::Tuple(elems) => {
                self.push_byte(tags::expr_kind::TUPLE);
                self.write_expr_seq(elems);
            }
            ExprKind::Array(elems) => {
                self.push_byte(tags::expr_kind::ARRAY);
                self.write_expr_seq(elems);
            }
            ExprKind::StructLit { path, fields } => {
                self.push_byte(tags::expr_kind::STRUCT_LIT);
                let qualified = self.resolver().resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
                self.write_u32_le(checked_u32(fields.len()));
                for field in fields {
                    self.write_struct_lit_field(field);
                }
            }
            ExprKind::Loop {
                body,
                label,
                decreases,
            } => {
                self.push_byte(tags::expr_kind::LOOP);
                self.write_block(body);
                self.write_optional_ident(label.as_ref());
                // `decreases` field added at BodyVersion(0x04).
                self.write_optional_expr(decreases.as_deref());
            }
            ExprKind::For {
                pat,
                iter,
                body,
                label,
            } => {
                self.push_byte(tags::expr_kind::FOR);
                self.write_pat(pat);
                self.write_expr(iter);
                self.write_block(body);
                self.write_optional_ident(label.as_ref());
            }
            ExprKind::Try(inner) => {
                self.push_byte(tags::expr_kind::TRY);
                self.write_expr(inner);
            }
            ExprKind::Await(inner) => {
                self.push_byte(tags::expr_kind::AWAIT);
                self.write_expr(inner);
            }
            ExprKind::Raise(inner) => {
                self.push_byte(tags::expr_kind::RAISE);
                self.write_expr(inner);
            }
            ExprKind::Panic(inner) => {
                self.push_byte(tags::expr_kind::PANIC);
                self.write_expr(inner);
            }
            ExprKind::Comptime(inner) => {
                self.push_byte(tags::expr_kind::COMPTIME);
                self.write_expr(inner);
            }
            ExprKind::ComptimeBlock(block) => {
                self.push_byte(tags::expr_kind::COMPTIME_BLOCK);
                self.write_block(block);
            }
            ExprKind::Scope { kind, name, body } => {
                self.push_byte(tags::expr_kind::SCOPE);
                let kind_byte = match kind {
                    edda_syntax::ast::ScopeKind::Exec => tags::scope_kind::EXEC,
                    edda_syntax::ast::ScopeKind::Coherence => tags::scope_kind::COHERENCE,
                };
                self.push_byte(kind_byte);
                self.write_optional_ident(name.as_ref());
                self.write_block(body);
            }
            ExprKind::Return(value) => {
                self.push_byte(tags::expr_kind::RETURN);
                self.write_optional_expr(value.as_deref());
            }
            ExprKind::Break { label, value } => {
                self.push_byte(tags::expr_kind::BREAK);
                self.write_optional_ident(label.as_ref());
                self.write_optional_expr(value.as_deref());
            }
            ExprKind::Continue { label } => {
                self.push_byte(tags::expr_kind::CONTINUE);
                self.write_optional_ident(label.as_ref());
            }
            ExprKind::Error => {
                self.push_byte(tags::expr_kind::ERROR);
            }
            ExprKind::EffectRow(row) => {
                self.push_byte(tags::expr_kind::EFFECT_ROW);
                self.write_effect_row(row);
            }
            // Closure literals: canonical-form encoding deferred.
            // Closures inside spec bodies will need a dedicated tag when
            // the rest of the function-type pipeline lands; for now we
            // emit the Error sentinel so codegen still terminates.
            ExprKind::Closure(_) => {
                self.push_byte(tags::expr_kind::ERROR);
            }
            // Handle expressions: encoding deferred until effect-discharge
            // semantics land in a later wave; emit Error sentinel.
            ExprKind::Handle { .. } => {
                self.push_byte(tags::expr_kind::ERROR);
            }
            // Spawn-block: not admissible inside spec bodies per the
            // locked spec-body grammar (`declarations.md` §253 restricts
            // bodies to function / type / let). Canonical encoding is
            // deferred; emit the Error sentinel so the hash terminates.
            // A dedicated tag lands the same wave structured concurrency
            // becomes spec-body admissible (if ever).
            ExprKind::Spawn(_) => {
                self.push_byte(tags::expr_kind::ERROR);
            }
            ExprKind::Forall { bound, iter, body } => {
                self.push_byte(tags::expr_kind::FORALL);
                self.write_ident(bound);
                self.write_expr(iter);
                self.write_expr(body);
            }
            ExprKind::Exists { bound, iter, body } => {
                self.push_byte(tags::expr_kind::EXISTS);
                self.write_ident(bound);
                self.write_expr(iter);
                self.write_expr(body);
            }
        }
    }

    /// Encode a [`Block`]: u32-le statement count, then each [`Stmt`]
    /// in source order, then the optional trailing expression.
    pub fn write_block(&mut self, block: &Block) {
        self.write_u32_le(checked_u32(block.stmts.len()));
        for stmt in &block.stmts {
            self.write_stmt(stmt);
        }
        self.write_optional_expr(block.trailing.as_deref());
    }

    /// Encode a [`Stmt`] AST node, emitting its [`tags::stmt_kind`] tag
    /// followed by the kind-specific payload.
    pub fn write_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                mutability,
                pat,
                ty,
                init,
            } => {
                self.push_byte(tags::stmt_kind::LET);
                self.write_binding_mode(*mutability);
                self.write_pat(pat);
                self.write_optional_type(ty.as_ref());
                self.write_optional_expr_owned(init.as_ref());
            }
            StmtKind::Assign { target, op, rhs } => {
                self.push_byte(tags::stmt_kind::ASSIGN);
                self.write_expr(target);
                self.write_assign_op(*op);
                self.write_expr(rhs);
            }
            StmtKind::Expr(e) => {
                self.push_byte(tags::stmt_kind::EXPR);
                self.write_expr(e);
            }
        }
    }

    /// Encode a [`Pat`] AST node, emitting its [`tags::pat_kind`] tag
    /// followed by the kind-specific payload.
    pub fn write_pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Wildcard => {
                self.push_byte(tags::pat_kind::WILDCARD);
            }
            PatKind::Binding(ident) => {
                self.push_byte(tags::pat_kind::BINDING);
                self.write_ident(ident);
            }
            PatKind::Literal(lit) => {
                self.push_byte(tags::pat_kind::LITERAL);
                self.write_literal(lit);
            }
            PatKind::Tuple(elems) => {
                self.push_byte(tags::pat_kind::TUPLE);
                self.write_u32_le(checked_u32(elems.len()));
                for elem in elems {
                    self.write_pat(elem);
                }
            }
            PatKind::Variant { path, payload } => {
                self.push_byte(tags::pat_kind::VARIANT);
                let qualified = self.resolver().resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
                self.write_variant_pat_payload(payload);
            }
            PatKind::Struct { path, fields, rest } => {
                self.push_byte(tags::pat_kind::STRUCT);
                let qualified = self.resolver().resolve_path(path);
                self.write_length_prefixed_str(qualified.as_str());
                self.write_u32_le(checked_u32(fields.len()));
                for field in fields {
                    self.write_struct_pat_field(field);
                }
                self.push_byte(if *rest { 0x01 } else { 0x00 });
            }
            PatKind::Guard { pat, cond } => {
                self.push_byte(tags::pat_kind::GUARD);
                self.write_pat(pat);
                self.write_expr(cond);
            }
            PatKind::Range { lo, hi, kind } => {
                self.push_byte(tags::pat_kind::RANGE);
                self.write_literal(lo);
                self.write_literal(hi);
                self.write_range_kind(*kind);
            }
            PatKind::AtBinding { name, inner } => {
                self.push_byte(tags::pat_kind::AT_BINDING);
                self.write_ident(name);
                self.write_pat(inner);
            }
            PatKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                self.push_byte(tags::pat_kind::SLICE);
                self.write_u32_le(checked_u32(prefix.len()));
                for elem in prefix {
                    self.write_pat(elem);
                }
                match rest {
                    None => self.push_byte(tags::option_flag::NONE),
                    Some(binding) => {
                        self.push_byte(tags::option_flag::SOME);
                        match binding {
                            None => self.push_byte(tags::option_flag::NONE),
                            Some(name) => {
                                self.push_byte(tags::option_flag::SOME);
                                self.write_ident(name);
                            }
                        }
                    }
                }
                self.write_u32_le(checked_u32(suffix.len()));
                for elem in suffix {
                    self.write_pat(elem);
                }
            }
            PatKind::Error => {
                self.push_byte(tags::pat_kind::ERROR);
            }
        }
    }

    /// Encode a [`MatchArm`]: pattern, optional guard expression,
    /// body expression.
    pub fn write_match_arm(&mut self, arm: &MatchArm) {
        self.write_pat(&arm.pat);
        self.write_optional_expr_owned(arm.guard.as_ref());
        self.write_expr(&arm.body);
    }

    /// Encode a [`StructLitField`]: field name, mode tag, value expression.
    pub fn write_struct_lit_field(&mut self, field: &StructLitField) {
        self.write_ident(&field.name);
        let mode_tag = match field.mode {
            None => tags::call_mode::NONE,
            Some(CallMode::Mutable) => tags::call_mode::INOUT,
            Some(CallMode::Take) => tags::call_mode::SINK,
            Some(CallMode::Init) => tags::call_mode::SET,
        };
        self.push_byte(mode_tag);
        self.write_expr(&field.value);
    }

    /// Encode a [`StructPatField`]: bare field name + sub-pattern.
    pub fn write_struct_pat_field(&mut self, field: &StructPatField) {
        self.write_ident(&field.name);
        self.write_pat(&field.pat);
    }

    /// Encode a [`VariantPatPayload`]: tag byte + payload bytes.
    pub fn write_variant_pat_payload(&mut self, payload: &VariantPatPayload) {
        match payload {
            VariantPatPayload::None => {
                self.push_byte(tags::variant_pat_payload::NONE);
            }
            VariantPatPayload::Tuple(elems) => {
                self.push_byte(tags::variant_pat_payload::TUPLE);
                self.write_u32_le(checked_u32(elems.len()));
                for elem in elems {
                    self.write_pat(elem);
                }
            }
            VariantPatPayload::Struct(fields) => {
                self.push_byte(tags::variant_pat_payload::STRUCT);
                self.write_u32_le(checked_u32(fields.len()));
                for field in fields {
                    self.write_struct_pat_field(field);
                }
            }
        }
    }

    fn write_expr_seq(&mut self, exprs: &[Expr]) {
        self.write_u32_le(checked_u32(exprs.len()));
        for e in exprs {
            self.write_expr(e);
        }
    }

    /// Encode a call-argument sequence: count, then for each argument
    /// the call-mode tag (`tags::call_mode::*`) and the argument
    /// expression. Used by [`ExprKind::Call`] and
    /// [`ExprKind::MethodCall`] so mode keywords participate in the
    /// canonical-form hash (`storage.md` §4).
    fn write_call_arg_seq(&mut self, args: &[CallArg]) {
        self.write_u32_le(checked_u32(args.len()));
        for a in args {
            let mode_tag = match a.mode {
                None => tags::call_mode::NONE,
                Some(CallMode::Mutable) => tags::call_mode::INOUT,
                Some(CallMode::Take) => tags::call_mode::SINK,
                Some(CallMode::Init) => tags::call_mode::SET,
            };
            self.push_byte(mode_tag);
            self.write_expr(&a.expr);
        }
    }

    pub(super) fn write_optional_expr(&mut self, e: Option<&Expr>) {
        match e {
            Some(e) => {
                self.push_byte(tags::option_flag::SOME);
                self.write_expr(e);
            }
            None => {
                self.push_byte(tags::option_flag::NONE);
            }
        }
    }

    fn write_optional_expr_owned(&mut self, e: Option<&Expr>) {
        // Owned-value Option<Expr> (e.g., MatchArm.guard) follows the
        // same wire shape as Option<Box<Expr>>; this helper exists so
        // call sites read clearly at the StmtKind::Let / MatchArm sites.
        self.write_optional_expr(e);
    }

    pub(super) fn write_optional_type(&mut self, ty: Option<&edda_syntax::ast::Type>) {
        match ty {
            Some(t) => {
                self.push_byte(tags::option_flag::SOME);
                self.write_type(t);
            }
            None => {
                self.push_byte(tags::option_flag::NONE);
            }
        }
    }
}
