//! Pretty-printing of bodies, basic blocks, statements, and terminators.

use crate::body::{Body, LocalSource, Mutability, ParamInfo};
use crate::effect::EffectRow;
use crate::ids::BodyId;
use crate::program::MirProgram;
use crate::statement::{Statement, StatementKind};
use crate::terminator::{CallArg, FuncRef, Terminator, TerminatorKind};

use super::PrettyPrinter;

impl PrettyPrinter<'_> {
    /// Render a [`Body`] in full: signature line, locals, blocks.
    pub(crate) fn print_body(&mut self, id: BodyId, body: &Body, program: &MirProgram) {
        self.write_line(&self.format_body_header(id, body));
        self.with_indent(|p| {
            p.print_effect_row(&body.effect_row);
            p.print_locals(body);
            p.write_line("");
            for (block_id, block) in body.blocks.iter_enumerated() {
                let header = format!("{}:", Self::format_block(block_id));
                p.write_line(&header);
                p.with_indent(|p| {
                    for stmt in &block.stmts {
                        p.print_statement(stmt, program);
                    }
                    p.print_terminator(&block.terminator, program);
                });
            }
        });
        self.write_line("}");
    }

    /// Build the `fn body0 name(...) -> Ty {` opening line. When
    /// `@export` / `@abi` are set on the body, their values appear as
    /// `export "<sym>"` / `abi <tag>` annotations before the entry
    /// block reference.
    fn format_body_header(&self, id: BodyId, body: &Body) -> String {
        let mut s = format!("fn body{} {}(", id.as_u32(), self.resolve(body.name));
        for (i, param) in body.params.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&self.format_param_decl(param));
        }
        s.push_str(") -> ");
        s.push_str(&self.format_type(&body.return_ty.kind));
        if let Some(sym) = body.export_symbol {
            s.push_str(&format!(" export \"{}\"", self.resolve(sym)));
        }
        if let Some(abi) = &body.abi {
            s.push_str(&format!(" abi {}", self.format_abi_tag(abi)));
        }
        s.push_str(" entry=");
        s.push_str(&Self::format_block(body.entry));
        s.push_str(" {");
        s
    }

    /// Render an [`crate::layout::AbiTag`] as the short string the body /
    /// ADT header annotations print.
    fn format_abi_tag(&self, abi: &crate::layout::AbiTag) -> String {
        use crate::layout::AbiTag;
        match abi {
            AbiTag::Edda => "edda".to_string(),
            AbiTag::C => "c".to_string(),
            AbiTag::System => "system".to_string(),
            AbiTag::Named(sym) => format!("\"{}\"", self.resolve(*sym)),
        }
    }

    /// Render one `ParamInfo` as `_N: mode Ty`.
    fn format_param_decl(&self, param: &ParamInfo) -> String {
        format!(
            "{}: {}",
            Self::format_local(param.local),
            self.format_param(param.mode, &param.ty),
        )
    }

    /// Emit the `// uses: ...` comment summarising the effect row (skipped
    /// when the row is pure).
    fn print_effect_row(&mut self, row: &EffectRow) {
        if row.capabilities.is_empty() && row.errors.is_empty() && !row.has_panic {
            return;
        }
        let mut s = String::from("// uses: ");
        let mut first = true;
        for slot in &row.capabilities {
            if !first {
                s.push_str(", ");
            }
            first = false;
            s.push_str(&format!(
                "{} @ {} (eff{})",
                self.format_capability(&slot.ty),
                Self::format_local(slot.param_local),
                slot.id.as_u32(),
            ));
        }
        for err in &row.errors {
            if !first {
                s.push_str(", ");
            }
            first = false;
            s.push_str(&format!("err: adt{}", err.as_u32()));
        }
        if row.has_panic {
            if !first {
                s.push_str(", ");
            }
            s.push_str("panic");
        }
        self.write_line(&s);
    }

    /// Emit one `let _N: Ty;` line per local.
    fn print_locals(&mut self, body: &Body) {
        for (id, decl) in body.locals.iter_enumerated() {
            let line = format!(
                "let {}{}: {}; // {}",
                mutability_prefix(decl.mutability),
                Self::format_local(id),
                self.format_type(&decl.ty.kind),
                self.format_local_source(&decl.source),
            );
            self.write_line(&line);
        }
    }

    /// Short tag for a local's provenance, for use in trailing comments.
    fn format_local_source(&self, source: &LocalSource) -> String {
        match source {
            LocalSource::Param(i) => format!("param {}", i),
            LocalSource::Temp => "temp".to_string(),
            LocalSource::UserBinding(sym) => format!("binding {}", self.resolve(*sym)),
            LocalSource::ReturnSlot => "return slot".to_string(),
        }
    }

    /// Emit one statement line.
    fn print_statement(&mut self, stmt: &Statement, program: &MirProgram) {
        let line = match &stmt.kind {
            StatementKind::Assign { place, rvalue } => format!(
                "{} = {};",
                self.format_place(place),
                self.format_rvalue(rvalue, program),
            ),
            StatementKind::StorageLive(local) => {
                format!("storage_live({});", Self::format_local(*local))
            }
            StatementKind::StorageDead(local) => {
                format!("storage_dead({});", Self::format_local(*local))
            }
            StatementKind::SetInit(local) => {
                format!("set_init({});", Self::format_local(*local))
            }
            StatementKind::Drop(local) => format!("drop({});", Self::format_local(*local)),
            StatementKind::Nop => "nop;".to_string(),
        };
        self.write_line(&line);
    }

    /// Emit one terminator line (the closing instruction of a basic block).
    fn print_terminator(&mut self, term: &Terminator, program: &MirProgram) {
        match &term.kind {
            TerminatorKind::Return(op) => {
                let s = format!("return {};", self.format_operand(op, program));
                self.write_line(&s);
            }
            TerminatorKind::Goto(target) => {
                let s = format!("goto -> {};", Self::format_block(*target));
                self.write_line(&s);
            }
            TerminatorKind::SwitchBool {
                cond,
                true_bb,
                false_bb,
            } => {
                let s = format!(
                    "switch_bool({}) -> [true: {}, false: {}];",
                    self.format_operand(cond, program),
                    Self::format_block(*true_bb),
                    Self::format_block(*false_bb),
                );
                self.write_line(&s);
            }
            TerminatorKind::SwitchTag {
                subject,
                adt,
                arms,
                otherwise,
            } => {
                let s = self.format_switch_tag(subject, *adt, arms, *otherwise, program);
                self.write_line(&s);
            }
            TerminatorKind::Call {
                func,
                args,
                capabilities,
                destination,
                target,
                on_error,
            } => {
                let line = self.format_call(
                    func,
                    args,
                    capabilities,
                    destination,
                    *target,
                    *on_error,
                    program,
                );
                self.write_line(&line);
            }
            TerminatorKind::Raise { err_adt, value } => {
                let s = format!(
                    "raise(adt{}, {});",
                    err_adt.as_u32(),
                    self.format_operand(value, program),
                );
                self.write_line(&s);
            }
            TerminatorKind::Panic { msg } => {
                let s = format!("panic({});", self.format_operand(msg, program));
                self.write_line(&s);
            }
            TerminatorKind::Unreachable => self.write_line("unreachable;"),
            TerminatorKind::Spawn {
                child,
                args,
                group_local,
                dest,
                target,
            } => {
                let s = self.format_spawn(*child, args, *group_local, *dest, *target, program);
                self.write_line(&s);
            }
            TerminatorKind::Await { task, dest, target } => {
                let s = format!(
                    "{} = await({}) -> {};",
                    Self::format_local(*dest),
                    self.format_operand(task, program),
                    Self::format_block(*target),
                );
                self.write_line(&s);
            }
        }
    }

    /// Format a `Spawn` terminator on one line.
    fn format_spawn(
        &self,
        child: BodyId,
        args: &[crate::operand::Operand],
        group_local: crate::ids::LocalId,
        dest: crate::ids::LocalId,
        target: crate::ids::BlockId,
        program: &MirProgram,
    ) -> String {
        let mut s = format!(
            "{} = spawn body{}(",
            Self::format_local(dest),
            child.as_u32(),
        );
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&self.format_operand(arg, program));
        }
        s.push_str(&format!(
            ") | group: {} -> {};",
            Self::format_local(group_local),
            Self::format_block(target),
        ));
        s
    }

    /// Format a `switch_tag` terminator on one line.
    fn format_switch_tag(
        &self,
        subject: &crate::operand::Operand,
        adt: crate::ids::AdtId,
        arms: &[(crate::ids::VariantIdx, crate::ids::BlockId)],
        otherwise: crate::ids::BlockId,
        program: &MirProgram,
    ) -> String {
        let mut s = format!(
            "switch_tag({}, adt{}) -> [",
            self.format_operand(subject, program),
            adt.as_u32(),
        );
        for (i, (variant, target)) in arms.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!(
                "v{}: {}",
                variant.as_u32(),
                Self::format_block(*target),
            ));
        }
        if !arms.is_empty() {
            s.push_str(", ");
        }
        s.push_str(&format!("otherwise: {}", Self::format_block(otherwise)));
        s.push_str("];");
        s
    }

    /// Format a `Call` terminator on one line. A capability with a
    /// positional value pairing renders as `eff<N>=arg<J>`; an
    /// accounting-only slot renders as the bare `eff<N>`.
    #[allow(clippy::too_many_arguments)]
    fn format_call(
        &self,
        func: &FuncRef,
        args: &[CallArg],
        capabilities: &[crate::terminator::ThreadedCapability],
        destination: &crate::place::Place,
        target: crate::ids::BlockId,
        on_error: Option<crate::ids::BlockId>,
        program: &MirProgram,
    ) -> String {
        let mut s = format!("{} = call ", self.format_place(destination));
        s.push_str(&self.format_func_ref(func, program));
        s.push('(');
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(arg.mode.as_str());
            s.push(' ');
            s.push_str(&self.format_operand(&arg.operand, program));
        }
        if !capabilities.is_empty() {
            s.push_str(" | caps: ");
            for (i, cap) in capabilities.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format!("eff{}", cap.id.as_u32()));
                if let Some(j) = cap.value_arg {
                    s.push_str(&format!("=arg{j}"));
                }
            }
        }
        s.push(')');
        s.push_str(&format!(" -> [ok: {}", Self::format_block(target)));
        if let Some(err_bb) = on_error {
            s.push_str(&format!(", err: {}", Self::format_block(err_bb)));
        }
        s.push_str("];");
        s
    }

    /// Render the callee reference of a `Call`.
    fn format_func_ref(&self, func: &FuncRef, program: &MirProgram) -> String {
        match func {
            FuncRef::Body(id) => format!("body{}", id.as_u32()),
            FuncRef::Extern { name, sig } => {
                format!("extern \"{}\" {}", self.resolve(*name), self.format_fn_sig(sig))
            }
            FuncRef::Indirect { callee, sig } => {
                // Same operand-rendering rules as elsewhere — `Copy` /
                // `Move` / `Const` shapes round-trip through the rvalue
                // printer.
                format!(
                    "indirect {} {}",
                    self.format_operand(callee, program),
                    self.format_fn_sig(sig),
                )
            }
        }
    }
}

/// Prefix used in `let` lines to expose mutability.
fn mutability_prefix(m: Mutability) -> &'static str {
    match m {
        Mutability::Imm => "",
        Mutability::Mut => "mut ",
    }
}

