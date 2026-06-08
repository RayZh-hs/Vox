use std::collections::{BTreeMap, BTreeSet};

use vox_core::{
    host::Purity,
    mir::{
        MirAnalysisSummary, MirBinding, MirBindingId, MirBindingVersion, MirBlock, MirBlockId,
        MirBody, MirBodyId, MirBodyKind, MirCaptureMode, MirDemand, MirEscape, MirLifetime,
        MirModule, MirMutability, MirOp, MirOpKind, MirPathSegment, MirProgramPoint, MirProjection,
        MirStorage, MirTerminator, MirUse, MirUseKind, MirValue, MirValueDefinition, MirValueId,
        MirVersionId, MirVersionSource,
    },
    opt::{OptimizationLevel, OptimizationRank, OptimizationSubject},
    source::ModuleKind,
    types::VoxType,
    value::InlineValue,
};

use crate::frontend::{
    FrontendUnit,
    ast::{
        Argument, BinaryOp, BlockExpr, BlockItem, CompilationUnit, CompoundAssignmentOp, Expr,
        ExprKind, FunctionDecl, IfExpr, IntrinsicExpr, LocalValueDecl, Mutability, QualifiedName,
        StringLiteral, StringPart, TopLevelItem, TypeSyntax, UnaryOp, UpdatedPathSegment,
        ValueDecl, WhenExpr,
    },
};

pub type MirPassFn = fn(&mut MirModule) -> MirPassReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirPassReport {
    pub name: &'static str,
    pub changed: bool,
    pub summary: String,
}

pub(crate) fn lower_mir(
    frontend: &FrontendUnit,
    optimization: OptimizationLevel,
    rankings: &[vox_core::opt::OptimizationRanking],
) -> MirModule {
    MirLowerer::new(frontend, optimization, rankings).lower_module()
}

struct MirLowerer<'a> {
    frontend: &'a FrontendUnit,
    optimization: OptimizationLevel,
    rankings: &'a [vox_core::opt::OptimizationRanking],
    next_body: u32,
}

impl<'a> MirLowerer<'a> {
    fn new(
        frontend: &'a FrontendUnit,
        optimization: OptimizationLevel,
        rankings: &'a [vox_core::opt::OptimizationRanking],
    ) -> Self {
        Self {
            frontend,
            optimization,
            rankings,
            next_body: 0,
        }
    }

    fn lower_module(mut self) -> MirModule {
        let mut bodies = Vec::new();
        if matches!(self.frontend.header.kind, ModuleKind::Script { .. }) {
            bodies.push(self.lower_script_entry(&self.frontend.syntax));
        }

        for item in &self.frontend.syntax.items {
            match item {
                TopLevelItem::Function(function) => bodies.push(self.lower_function(function)),
                TopLevelItem::Value(value)
                    if matches!(self.frontend.header.kind, ModuleKind::Package) =>
                {
                    bodies.push(self.lower_value_initializer(value));
                }
                _ => {}
            }
        }

        MirModule {
            module: self.frontend.header.module.clone(),
            kind: self.frontend.header.kind,
            optimization: self.optimization,
            bodies,
        }
    }

    fn lower_script_entry(&mut self, unit: &CompilationUnit) -> MirBody {
        let purity = match unit.header.kind {
            ModuleKind::Script { evil: true } => Purity::Evil,
            _ => Purity::Pure,
        };
        let mut body = BodyBuilder::new(
            self.alloc_body_id(),
            "script_entry".to_owned(),
            MirBodyKind::ScriptEntry,
            purity,
            self.rank_for(OptimizationSubject::Module),
            Some(unit.span.clone()),
        );

        for parameter in &self.frontend.parameters {
            let value = body.new_value(
                Some(parameter.ty.clone()),
                MirValueDefinition::Parameter(parameter.name.clone()),
                None,
            );
            body.declare_binding(
                parameter.name.clone(),
                MirMutability::Val,
                Some(parameter.ty.clone()),
                None,
                value,
                MirVersionSource::Parameter,
            );
            body.parameters.push(value);
        }

        for item in &unit.items {
            match item {
                TopLevelItem::Import(_) | TopLevelItem::Param(_) | TopLevelItem::Function(_) => {}
                TopLevelItem::Value(value) => body.lower_value_decl(value),
                TopLevelItem::Statement(statement) => body.lower_block_item(statement),
            }
        }

        let result = unit
            .result
            .as_ref()
            .map(|expr| body.lower_expr(expr))
            .unwrap_or_else(|| body.emit_unit());
        body.terminate(MirTerminator::Return(result));
        body.finish()
    }

    fn lower_function(&mut self, function: &FunctionDecl) -> MirBody {
        let mut body = BodyBuilder::new(
            self.alloc_body_id(),
            function.name.clone(),
            MirBodyKind::Function,
            if function.evil {
                Purity::Evil
            } else {
                Purity::Pure
            },
            self.rank_for(OptimizationSubject::Function(function.name.clone())),
            Some(function.span.clone()),
        );

        for parameter in &function.parameters {
            let ty = Some(VoxType::opaque_surface(parameter.ty.to_source_string()));
            let value = body.new_value(
                ty.clone(),
                MirValueDefinition::Parameter(parameter.name.clone()),
                Some(parameter.span.clone()),
            );
            body.declare_binding(
                parameter.name.clone(),
                MirMutability::Val,
                ty,
                Some(parameter.span.clone()),
                value,
                MirVersionSource::Parameter,
            );
            body.parameters.push(value);
        }

        let result = body.lower_expr(&function.body);
        body.terminate(MirTerminator::Return(result));
        body.finish()
    }

    fn lower_value_initializer(&mut self, value: &ValueDecl) -> MirBody {
        let mut body = BodyBuilder::new(
            self.alloc_body_id(),
            format!("init.{}", value.name),
            MirBodyKind::ValueInitializer,
            Purity::Pure,
            self.rank_for(OptimizationSubject::Module),
            Some(value.span.clone()),
        );
        let result = body.lower_expr(&value.initializer);
        body.terminate(MirTerminator::Return(result));
        body.finish()
    }

    fn alloc_body_id(&mut self) -> MirBodyId {
        let id = MirBodyId(self.next_body);
        self.next_body += 1;
        id
    }

    fn rank_for(&self, subject: OptimizationSubject) -> OptimizationRank {
        self.rankings
            .iter()
            .find(|ranking| ranking.subject == subject)
            .map(|ranking| ranking.rank)
            .unwrap_or(OptimizationRank::Baseline)
    }
}

#[derive(Debug, Clone, Copy)]
struct BindingRef {
    binding: MirBindingId,
    version: MirVersionId,
    value: MirValueId,
    mutable: bool,
}

struct BodyBuilder {
    body_id: MirBodyId,
    name: String,
    kind: MirBodyKind,
    span: Option<vox_core::diagnostics::TextSpan>,
    purity: Purity,
    rank: OptimizationRank,
    parameters: Vec<MirValueId>,
    bindings: Vec<MirBinding>,
    versions: Vec<MirBindingVersion>,
    values: Vec<MirValue>,
    blocks: Vec<MirBlock>,
    current: MirBlockId,
    scopes: Vec<BTreeMap<String, BindingRef>>,
    next_binding: u32,
    next_version: u32,
    next_value: u32,
    next_block: u32,
}

impl BodyBuilder {
    fn new(
        body_id: MirBodyId,
        name: String,
        kind: MirBodyKind,
        purity: Purity,
        rank: OptimizationRank,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> Self {
        let entry = MirBlockId(0);
        Self {
            body_id,
            name,
            kind,
            span,
            purity,
            rank,
            parameters: Vec::new(),
            bindings: Vec::new(),
            versions: Vec::new(),
            values: Vec::new(),
            blocks: vec![MirBlock {
                id: entry,
                name: "entry".to_owned(),
                parameters: Vec::new(),
                ops: Vec::new(),
                terminator: MirTerminator::Unreachable,
            }],
            current: entry,
            scopes: vec![BTreeMap::new()],
            next_binding: 0,
            next_version: 0,
            next_value: 0,
            next_block: 1,
        }
    }

    fn finish(self) -> MirBody {
        MirBody {
            id: self.body_id,
            name: self.name,
            kind: self.kind,
            span: self.span,
            purity: self.purity,
            optimization_rank: self.rank,
            parameters: self.parameters,
            captures: Vec::new(),
            bindings: self.bindings,
            versions: self.versions,
            values: self.values,
            blocks: self.blocks,
            result_type: None,
            analyses: MirAnalysisSummary::default(),
        }
    }

    fn lower_value_decl(&mut self, value: &ValueDecl) {
        let init = self.lower_expr(&value.initializer);
        self.declare_binding(
            value.name.clone(),
            mir_mutability(value.mutability),
            value.ty.as_ref().map(type_syntax_to_vox),
            Some(value.span.clone()),
            init,
            MirVersionSource::Initializer,
        );
    }

    fn lower_local_value_decl(&mut self, value: &LocalValueDecl) {
        let init = self.lower_expr(&value.initializer);
        self.declare_binding(
            value.name.clone(),
            mir_mutability(value.mutability),
            value.ty.as_ref().map(type_syntax_to_vox),
            Some(value.span.clone()),
            init,
            MirVersionSource::Initializer,
        );
    }

    fn lower_block_item(&mut self, item: &BlockItem) {
        match item {
            BlockItem::LocalValue(value) => self.lower_local_value_decl(value),
            BlockItem::Assignment(assignment) => {
                let value = self.lower_expr(&assignment.value);
                self.assign_binding(
                    &assignment.name,
                    value,
                    MirVersionSource::Assignment,
                    Some(assignment.span.clone()),
                );
            }
            BlockItem::CompoundAssignment(assignment) => {
                let Some(previous) = self.resolve_binding(&assignment.name) else {
                    self.terminate_with_panic(format!(
                        "assignment requires a previously declared `var`, but `{}` was not found",
                        assignment.name
                    ));
                    return;
                };
                if !previous.mutable {
                    self.terminate_with_panic(format!(
                        "cannot assign to immutable binding `{}`",
                        assignment.name
                    ));
                    return;
                }
                let lhs = self.use_name(&assignment.name, Some(assignment.span.clone()));
                let rhs = self.lower_expr(&assignment.value);
                let value = self.emit_op(
                    MirOpKind::Binary(compound_op_name(assignment.op).to_owned()),
                    vec![lhs, rhs],
                    Some(assignment.span.clone()),
                );
                self.assign_binding(
                    &assignment.name,
                    value,
                    MirVersionSource::CompoundAssignment,
                    Some(assignment.span.clone()),
                );
            }
            BlockItem::Return(statement) => {
                let value = statement
                    .value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr))
                    .unwrap_or_else(|| self.emit_unit());
                self.terminate(MirTerminator::Return(value));
                self.current = self.new_block("after_return");
            }
            BlockItem::Panic(statement) => {
                self.terminate(MirTerminator::Panic(string_literal_text(
                    &statement.message,
                )));
                self.current = self.new_block("after_panic");
            }
            BlockItem::Expr(expr) | BlockItem::BlockStatement(expr) => {
                let value = self.lower_expr(expr);
                self.emit_op_with_result(
                    None,
                    MirOpKind::Drop,
                    vec![value],
                    Some(expr.span.clone()),
                );
            }
        }
    }

    fn lower_expr(&mut self, expr: &Expr) -> MirValueId {
        match &expr.kind {
            ExprKind::Integer(raw) => self.emit_literal(
                InlineValue::Int(raw.replace('_', "").parse::<i64>().unwrap_or(0)),
                Some(expr.span.clone()),
            ),
            ExprKind::Float(raw) => self.emit_literal(
                InlineValue::Float(raw.replace('_', "").parse::<f64>().unwrap_or(0.0)),
                Some(expr.span.clone()),
            ),
            ExprKind::Bool(value) => {
                self.emit_literal(InlineValue::Bool(*value), Some(expr.span.clone()))
            }
            ExprKind::Null => self.emit_literal(InlineValue::Null, Some(expr.span.clone())),
            ExprKind::String(literal) => {
                if literal
                    .parts
                    .iter()
                    .all(|part| matches!(part, StringPart::Text(_)))
                {
                    self.emit_literal(
                        InlineValue::String(string_literal_text(literal)),
                        Some(expr.span.clone()),
                    )
                } else {
                    let mut args = Vec::new();
                    let mut text = vec![String::new()];
                    for part in &literal.parts {
                        match part {
                            StringPart::Text(value) => {
                                text.last_mut()
                                    .expect("interpolation text should have a current segment")
                                    .push_str(value);
                            }
                            StringPart::Interpolation(value) => {
                                args.push(self.lower_expr(value));
                                text.push(String::new());
                            }
                        }
                    }
                    self.emit_op(
                        MirOpKind::StringInterpolate { text },
                        args,
                        Some(expr.span.clone()),
                    )
                }
            }
            ExprKind::List(items) => {
                let args = items.iter().map(|item| self.lower_expr(item)).collect();
                self.emit_op(MirOpKind::List, args, Some(expr.span.clone()))
            }
            ExprKind::Tuple(items) => {
                let args = items.iter().map(|item| self.lower_expr(item)).collect();
                self.emit_op(
                    MirOpKind::Tuple { shape: items.len() },
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Record(fields) => {
                let mut args = Vec::new();
                let mut names = Vec::new();
                for field in fields {
                    names.push(field.name.clone());
                    args.push(self.lower_expr(&field.value));
                }
                self.emit_op(
                    MirOpKind::Record { fields: names },
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Name(name) => self.lower_name(name, Some(expr.span.clone())),
            ExprKind::Call { callee, arguments } => {
                let mut args = Vec::new();
                args.push(self.lower_expr(callee));
                args.extend(self.lower_arguments(arguments));
                self.emit_op(
                    MirOpKind::Call {
                        callee: callee_label(callee),
                        purity: Purity::Pure,
                    },
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Intrinsic(intrinsic) => match intrinsic {
                IntrinsicExpr::Updated(updated) => {
                    let mut current = self.lower_expr(&updated.target);
                    for update in &updated.updates {
                        let value = self.lower_expr(&update.value);
                        let path = update.path.iter().map(mir_path_segment).collect();
                        current = self.emit_op(
                            MirOpKind::Updated { path },
                            vec![current, value],
                            Some(expr.span.clone()),
                        );
                    }
                    current
                }
                IntrinsicExpr::Econ(econ) => {
                    self.push_scope();
                    let value = self.lower_block_expr(&econ.body);
                    self.pop_scope();
                    self.emit_op(
                        MirOpKind::Econ {
                            ty: econ.ty.to_source_string(),
                        },
                        vec![value],
                        Some(expr.span.clone()),
                    )
                }
            },
            ExprKind::Index { target, index } => {
                let target = self.lower_expr(target);
                let index = self.lower_expr(index);
                self.emit_op(
                    MirOpKind::Index,
                    vec![target, index],
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Field { target, name } => {
                let target = self.lower_expr(target);
                self.emit_op(
                    MirOpKind::Project(MirProjection::Field(name.clone())),
                    vec![target],
                    Some(expr.span.clone()),
                )
            }
            ExprKind::SafeField { target, name } => {
                let target = self.lower_expr(target);
                self.emit_op(
                    MirOpKind::SafeProject(name.clone()),
                    vec![target],
                    Some(expr.span.clone()),
                )
            }
            ExprKind::NonNull { target } => {
                let target = self.lower_expr(target);
                self.emit_op(MirOpKind::NonNull, vec![target], Some(expr.span.clone()))
            }
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                let mut args = vec![self.lower_expr(receiver)];
                args.extend(self.lower_arguments(arguments));
                self.emit_op(
                    MirOpKind::Call {
                        callee: callee.to_source_string(),
                        purity: Purity::Pure,
                    },
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Unary { op, expr: inner } => {
                let value = self.lower_expr(inner);
                self.emit_op(
                    MirOpKind::Unary(unary_op_name(*op).to_owned()),
                    vec![value],
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Binary { left, op, right } if matches!(op, BinaryOp::And | BinaryOp::Or) => {
                self.lower_short_circuit_bool(left, *op, right, Some(expr.span.clone()))
            }
            ExprKind::Binary { left, op, right } if matches!(op, BinaryOp::Coalesce) => {
                self.lower_coalesce(left, right, Some(expr.span.clone()))
            }
            ExprKind::Binary { left, op, right } => {
                let left = self.lower_expr(left);
                let right = self.lower_expr(right);
                self.emit_op(
                    MirOpKind::Binary(binary_op_name(*op).to_owned()),
                    vec![left, right],
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Range(range) => {
                let mut args = Vec::new();
                if let Some(start) = &range.start {
                    args.push(self.lower_expr(start));
                }
                if let Some(end) = &range.end {
                    args.push(self.lower_expr(end));
                }
                self.emit_op(
                    MirOpKind::Binary(
                        if range.inclusive_end {
                            "range_inclusive"
                        } else {
                            "range"
                        }
                        .to_owned(),
                    ),
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::If(if_expr) => self.lower_if_expr(if_expr, Some(expr.span.clone())),
            ExprKind::When(when_expr) => self.lower_when_expr(when_expr, Some(expr.span.clone())),
            ExprKind::For(for_expr) => self.lower_for_expr(for_expr, Some(expr.span.clone())),
            ExprKind::Lambda(lambda) => {
                let args = lambda
                    .parameters
                    .iter()
                    .map(|parameter| {
                        self.emit_op(
                            MirOpKind::Unknown(format!("lambda_param {}", parameter.name)),
                            Vec::new(),
                            Some(parameter.span.clone()),
                        )
                    })
                    .collect();
                self.emit_op(
                    MirOpKind::Unknown("lambda".to_owned()),
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::Block(block) => {
                self.push_scope();
                let value = self.lower_block_expr(block);
                self.pop_scope();
                value
            }
        }
    }

    fn lower_block_expr(&mut self, block: &BlockExpr) -> MirValueId {
        for item in &block.items {
            self.lower_block_item(item);
        }
        block
            .trailing
            .as_ref()
            .map(|expr| self.lower_expr(expr))
            .unwrap_or_else(|| self.emit_unit())
    }

    fn lower_if_expr(
        &mut self,
        if_expr: &IfExpr,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let join = self.new_block("if_join");
        let result = self.new_value(None, MirValueDefinition::BlockParameter(join), span.clone());
        self.block_mut(join).parameters.push(result);
        let base_scopes = self.scopes.clone();
        self.lower_if_branch(0, if_expr, join, base_scopes);
        self.current = join;
        result
    }

    fn lower_if_branch(
        &mut self,
        index: usize,
        if_expr: &IfExpr,
        join: MirBlockId,
        base_scopes: Vec<BTreeMap<String, BindingRef>>,
    ) {
        let Some(branch) = if_expr.branches.get(index) else {
            self.scopes = base_scopes.clone();
            let value = if let Some(else_branch) = &if_expr.else_branch {
                self.push_scope();
                let value = self.lower_block_expr(else_branch);
                self.pop_scope();
                value
            } else {
                self.emit_unit()
            };
            self.jump_to_join_if_open(join, vec![value]);
            self.scopes = base_scopes;
            return;
        };

        self.scopes = base_scopes.clone();
        let condition = self.lower_expr(&branch.condition);
        let then_block = self.new_block("if_then");
        let else_block = self.new_block("if_else");
        self.terminate(MirTerminator::Branch {
            condition,
            then_target: then_block,
            then_args: Vec::new(),
            else_target: else_block,
            else_args: Vec::new(),
        });

        self.current = then_block;
        self.scopes = base_scopes.clone();
        self.push_scope();
        let then_value = self.lower_block_expr(&branch.body);
        self.pop_scope();
        self.jump_to_join_if_open(join, vec![then_value]);

        self.current = else_block;
        self.lower_if_branch(index + 1, if_expr, join, base_scopes);
    }

    fn lower_short_circuit_bool(
        &mut self,
        left: &Expr,
        op: BinaryOp,
        right: &Expr,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let left = self.lower_expr(left);
        let join = self.new_block("bool_join");
        let result = self.new_value(None, MirValueDefinition::BlockParameter(join), span.clone());
        self.block_mut(join).parameters.push(result);

        let eval_right = self.new_block("bool_rhs");
        let constant = self.new_block("bool_short");
        let (then_target, else_target) = match op {
            BinaryOp::And => (eval_right, constant),
            BinaryOp::Or => (constant, eval_right),
            _ => unreachable!("only boolean short-circuit operators are lowered here"),
        };
        self.terminate(MirTerminator::Branch {
            condition: left,
            then_target,
            then_args: Vec::new(),
            else_target,
            else_args: Vec::new(),
        });

        self.current = eval_right;
        let right = self.lower_expr(right);
        self.jump_to_join_if_open(join, vec![right]);

        self.current = constant;
        let value = self.emit_literal(InlineValue::Bool(matches!(op, BinaryOp::Or)), span.clone());
        self.jump_to_join_if_open(join, vec![value]);

        self.current = join;
        result
    }

    fn lower_coalesce(
        &mut self,
        left: &Expr,
        right: &Expr,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let left = self.lower_expr(left);
        let is_null = self.emit_op(
            MirOpKind::TypeTest("Null".to_owned()),
            vec![left],
            span.clone(),
        );
        let join = self.new_block("coalesce_join");
        let result = self.new_value(None, MirValueDefinition::BlockParameter(join), span.clone());
        self.block_mut(join).parameters.push(result);

        let eval_right = self.new_block("coalesce_rhs");
        let keep_left = self.new_block("coalesce_left");
        self.terminate(MirTerminator::Branch {
            condition: is_null,
            then_target: eval_right,
            then_args: Vec::new(),
            else_target: keep_left,
            else_args: Vec::new(),
        });

        self.current = eval_right;
        let right = self.lower_expr(right);
        self.jump_to_join_if_open(join, vec![right]);

        self.current = keep_left;
        self.jump_to_join_if_open(join, vec![left]);

        self.current = join;
        result
    }

    fn lower_when_expr(
        &mut self,
        when_expr: &WhenExpr,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let subject = self.lower_expr(&when_expr.subject);
        let join = self.new_block("when_join");
        let result = self.new_value(None, MirValueDefinition::BlockParameter(join), span.clone());
        self.block_mut(join).parameters.push(result);
        let base_scopes = self.scopes.clone();
        self.lower_when_arm(0, subject, when_expr, join, base_scopes);
        self.current = join;
        result
    }

    fn lower_when_arm(
        &mut self,
        index: usize,
        subject: MirValueId,
        when_expr: &WhenExpr,
        join: MirBlockId,
        base_scopes: Vec<BTreeMap<String, BindingRef>>,
    ) {
        let Some(arm) = when_expr.arms.get(index) else {
            self.scopes = base_scopes.clone();
            if let Some(else_arm) = &when_expr.else_arm {
                let value = self.lower_expr(else_arm);
                self.jump_to_join_if_open(join, vec![value]);
            } else {
                self.terminate(MirTerminator::Panic(
                    "when expression did not match any arm".to_owned(),
                ));
            }
            self.scopes = base_scopes;
            return;
        };

        self.scopes = base_scopes.clone();
        let matched = self.emit_op(
            MirOpKind::TypeTest(arm.ty.to_source_string()),
            vec![subject],
            Some(arm.span.clone()),
        );
        let arm_block = self.new_block("when_arm");
        let next_block = self.new_block("when_next");
        self.terminate(MirTerminator::Branch {
            condition: matched,
            then_target: arm_block,
            then_args: Vec::new(),
            else_target: next_block,
            else_args: Vec::new(),
        });

        self.current = arm_block;
        self.scopes = base_scopes.clone();
        self.push_scope();
        if let Some(binding) = &arm.binding {
            let refined = self.emit_op(
                MirOpKind::TypeRefine(arm.ty.to_source_string()),
                vec![subject],
                Some(arm.span.clone()),
            );
            self.declare_binding(
                binding.clone(),
                MirMutability::Val,
                Some(type_syntax_to_vox(&arm.ty)),
                Some(arm.span.clone()),
                refined,
                MirVersionSource::Initializer,
            );
        }
        let value = self.lower_expr(&arm.body);
        self.pop_scope();
        self.jump_to_join_if_open(join, vec![value]);

        self.current = next_block;
        self.lower_when_arm(index + 1, subject, when_expr, join, base_scopes);
    }

    fn lower_for_expr(
        &mut self,
        for_expr: &crate::frontend::ast::ForExpr,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let iterable = self.lower_expr(&for_expr.iterable);
        let iterator = self.emit_op(
            MirOpKind::Iterator,
            vec![iterable],
            span.clone(),
        );
        let header = self.new_block("for_header");
        let body_block = self.new_block("for_body");
        let exit = self.new_block("for_exit");
        self.terminate(MirTerminator::Jump {
            target: header,
            args: Vec::new(),
        });

        self.current = header;
        let item = self.emit_op(
            MirOpKind::IteratorNext,
            vec![iterator],
            span.clone(),
        );
        self.terminate(MirTerminator::Branch {
            condition: item,
            then_target: body_block,
            then_args: Vec::new(),
            else_target: exit,
            else_args: Vec::new(),
        });

        self.current = body_block;
        self.push_scope();
        self.declare_binding(
            for_expr.pattern.clone(),
            MirMutability::Val,
            None,
            span.clone(),
            item,
            MirVersionSource::Loop,
        );
        self.lower_block_expr(&for_expr.body);
        self.pop_scope();
        self.jump_to_join_if_open(header, Vec::new());
        self.current = exit;
        self.emit_unit()
    }

    fn lower_arguments(&mut self, arguments: &[Argument]) -> Vec<MirValueId> {
        arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(expr) => self.lower_expr(expr),
                Argument::Named { value, .. } => self.lower_expr(value),
            })
            .collect()
    }

    fn lower_name(
        &mut self,
        name: &QualifiedName,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        if name.segments.len() == 1 {
            self.use_name(&name.segments[0], span)
        } else {
            self.emit_op(
                MirOpKind::Unknown(format!("qualified_name {}", name.to_source_string())),
                Vec::new(),
                span,
            )
        }
    }

    fn use_name(
        &mut self,
        name: &str,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        if let Some(binding) = self.resolve_binding(name) {
            return self.emit_op(MirOpKind::Use(binding.version), vec![binding.value], span);
        }
        self.emit_op(MirOpKind::Unknown(format!("name {name}")), Vec::new(), span)
    }

    fn declare_binding(
        &mut self,
        name: String,
        mutability: MirMutability,
        ty: Option<VoxType>,
        span: Option<vox_core::diagnostics::TextSpan>,
        value: MirValueId,
        source: MirVersionSource,
    ) -> MirBindingId {
        let binding_id = MirBindingId(self.next_binding);
        self.next_binding += 1;
        let version_id = self.new_version(binding_id, value, source);
        let mutable = matches!(mutability, MirMutability::Var);
        self.bindings.push(MirBinding {
            id: binding_id,
            name: name.clone(),
            mutability,
            scope_depth: self.scopes.len().saturating_sub(1) as u32,
            declared_type: ty,
            span,
            capture: if mutable {
                MirCaptureMode::NonCapturable
            } else {
                MirCaptureMode::Local
            },
            versions: vec![version_id],
        });
        self.set_value_version(value, version_id);
        self.emit_op_with_result(None, MirOpKind::Bind(version_id), vec![value], None);
        self.scopes
            .last_mut()
            .expect("body should always have a scope")
            .insert(
                name,
                BindingRef {
                    binding: binding_id,
                    version: version_id,
                    value,
                    mutable,
                },
            );
        binding_id
    }

    fn assign_binding(
        &mut self,
        name: &str,
        value: MirValueId,
        source: MirVersionSource,
        _span: Option<vox_core::diagnostics::TextSpan>,
    ) {
        let Some(previous) = self.resolve_binding(name) else {
            self.terminate_with_panic(format!(
                "assignment requires a previously declared `var`, but `{name}` was not found"
            ));
            return;
        };
        if !previous.mutable {
            self.terminate_with_panic(format!("cannot assign to immutable binding `{name}`"));
            return;
        }
        let version_id = self.new_version(previous.binding, value, source);
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| binding.id == previous.binding)
        {
            binding.versions.push(version_id);
        }
        self.set_value_version(value, version_id);
        self.emit_op_with_result(None, MirOpKind::Bind(version_id), vec![value], None);
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                *slot = BindingRef {
                    binding: previous.binding,
                    version: version_id,
                    value,
                    mutable: previous.mutable,
                };
                return;
            }
        }
    }

    fn resolve_binding(&self, name: &str) -> Option<BindingRef> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    fn emit_literal(
        &mut self,
        value: InlineValue,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        self.emit_op(MirOpKind::Literal(value), Vec::new(), span)
    }

    fn emit_unit(&mut self) -> MirValueId {
        let value = self.new_value(None, MirValueDefinition::Unit, None);
        self.emit_op_with_result(Some(value), MirOpKind::Unit, Vec::new(), None);
        value
    }

    fn emit_op(
        &mut self,
        kind: MirOpKind,
        args: Vec<MirValueId>,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let definition = match kind {
            MirOpKind::Literal(_) => MirValueDefinition::Literal,
            _ => MirValueDefinition::Op,
        };
        let value = self.new_value(None, definition, span.clone());
        self.emit_op_with_result(Some(value), kind, args, span);
        value
    }

    fn emit_op_with_result(
        &mut self,
        result: Option<MirValueId>,
        kind: MirOpKind,
        args: Vec<MirValueId>,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) {
        let block_id = self.current;
        let op_index = self.current_block().ops.len() as u32;
        for arg in &args {
            self.add_use(
                *arg,
                MirUse {
                    block: block_id,
                    op_index: Some(op_index),
                    kind: MirUseKind::Operand,
                },
            );
        }
        self.current_block().ops.push(MirOp {
            result,
            kind,
            args,
            span,
        });
    }

    fn terminate(&mut self, terminator: MirTerminator) {
        let block_id = self.current;
        match &terminator {
            MirTerminator::Return(value) => self.add_use(
                *value,
                MirUse {
                    block: block_id,
                    op_index: None,
                    kind: MirUseKind::Return,
                },
            ),
            MirTerminator::Branch { condition, .. } => self.add_use(
                *condition,
                MirUse {
                    block: block_id,
                    op_index: None,
                    kind: MirUseKind::Condition,
                },
            ),
            MirTerminator::Jump { args, .. } => {
                for arg in args {
                    self.add_use(
                        *arg,
                        MirUse {
                            block: block_id,
                            op_index: None,
                            kind: MirUseKind::Operand,
                        },
                    );
                }
            }
            MirTerminator::Panic(_) | MirTerminator::Unreachable => {}
        }
        self.current_block().terminator = terminator;
    }

    fn terminate_with_panic(&mut self, message: String) {
        self.terminate(MirTerminator::Panic(message));
        self.current = self.new_block("after_panic");
    }

    fn new_block(&mut self, name: &str) -> MirBlockId {
        let id = MirBlockId(self.next_block);
        self.next_block += 1;
        self.blocks.push(MirBlock {
            id,
            name: name.to_owned(),
            parameters: Vec::new(),
            ops: Vec::new(),
            terminator: MirTerminator::Unreachable,
        });
        id
    }

    fn new_value(
        &mut self,
        ty: Option<VoxType>,
        definition: MirValueDefinition,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let id = MirValueId(self.next_value);
        self.next_value += 1;
        self.values.push(MirValue {
            id,
            ty,
            definition,
            span,
            binding_version: None,
            uses: Vec::new(),
            lifetime: MirLifetime::default(),
            escape: MirEscape::default(),
            demand: MirDemand::Unknown,
            storage: MirStorage::Fresh,
        });
        id
    }

    fn new_version(
        &mut self,
        binding: MirBindingId,
        value: MirValueId,
        source: MirVersionSource,
    ) -> MirVersionId {
        let id = MirVersionId(self.next_version);
        self.next_version += 1;
        self.versions.push(MirBindingVersion {
            id,
            binding,
            value,
            source,
        });
        id
    }

    fn current_block(&mut self) -> &mut MirBlock {
        self.blocks
            .iter_mut()
            .find(|block| block.id == self.current)
            .expect("current block should exist")
    }

    fn block_mut(&mut self, id: MirBlockId) -> &mut MirBlock {
        self.blocks
            .iter_mut()
            .find(|block| block.id == id)
            .expect("block should exist")
    }

    fn jump_to_join_if_open(&mut self, target: MirBlockId, args: Vec<MirValueId>) {
        let should_jump = self
            .blocks
            .iter()
            .find(|block| block.id == self.current)
            .map(|block| {
                matches!(block.terminator, MirTerminator::Unreachable)
                    && !block.name.starts_with("after_return")
                    && !block.name.starts_with("after_panic")
            })
            .unwrap_or(false);
        if should_jump {
            self.terminate(MirTerminator::Jump { target, args });
        }
    }

    fn add_use(&mut self, value: MirValueId, usage: MirUse) {
        if let Some(value) = self.values.iter_mut().find(|entry| entry.id == value) {
            value.uses.push(usage);
        }
    }

    fn set_value_version(&mut self, value: MirValueId, version: MirVersionId) {
        if let Some(value) = self.values.iter_mut().find(|entry| entry.id == value) {
            value.binding_version = Some(version);
        }
    }
}

pub(crate) fn build_def_use(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        for value in &mut body.values {
            value.uses.clear();
            value.escape = MirEscape::default();
        }
        for block in &body.blocks {
            for (index, op) in block.ops.iter().enumerate() {
                for arg in &op.args {
                    if let Some(value) = body.values.iter_mut().find(|value| value.id == *arg) {
                        value.uses.push(MirUse {
                            block: block.id,
                            op_index: Some(index as u32),
                            kind: MirUseKind::Operand,
                        });
                    }
                }
            }
            match &block.terminator {
                MirTerminator::Return(value) => {
                    if let Some(value) = body.values.iter_mut().find(|entry| entry.id == *value) {
                        value.uses.push(MirUse {
                            block: block.id,
                            op_index: None,
                            kind: MirUseKind::Return,
                        });
                        value.escape.returned = true;
                    }
                }
                MirTerminator::Branch {
                    condition,
                    then_args,
                    else_args,
                    ..
                } => {
                    if let Some(value) = body.values.iter_mut().find(|entry| entry.id == *condition)
                    {
                        value.uses.push(MirUse {
                            block: block.id,
                            op_index: None,
                            kind: MirUseKind::Condition,
                        });
                    }
                    for arg in then_args.iter().chain(else_args.iter()) {
                        if let Some(value) = body.values.iter_mut().find(|entry| entry.id == *arg) {
                            value.uses.push(MirUse {
                                block: block.id,
                                op_index: None,
                                kind: MirUseKind::Operand,
                            });
                        }
                    }
                }
                MirTerminator::Jump { args, .. } => {
                    for arg in args {
                        if let Some(value) = body.values.iter_mut().find(|entry| entry.id == *arg) {
                            value.uses.push(MirUse {
                                block: block.id,
                                op_index: None,
                                kind: MirUseKind::Operand,
                            });
                        }
                    }
                }
                MirTerminator::Panic(_) | MirTerminator::Unreachable => {}
            }
        }
        body.analyses.def_use_complete = true;
    }
    MirPassReport {
        name: "def-use",
        changed: true,
        summary: "MIR def-use lists rebuilt".to_owned(),
    }
}

pub(crate) fn fold_constants(module: &mut MirModule) -> MirPassReport {
    let mut folded = 0;
    for body in &mut module.bodies {
        let mut constants = BTreeMap::<MirValueId, InlineValue>::new();
        for block in &mut body.blocks {
            for op in &mut block.ops {
                let folded_value = match &op.kind {
                    MirOpKind::Literal(value) => {
                        if let Some(result) = op.result {
                            constants.insert(result, value.clone());
                        }
                        None
                    }
                    MirOpKind::Unit => Some(InlineValue::Tuple(Vec::new())),
                    MirOpKind::Unary(name) => op
                        .args
                        .first()
                        .and_then(|arg| constants.get(arg))
                        .and_then(|value| fold_unary(name, value)),
                    MirOpKind::Binary(name) => {
                        let [left, right] = op.args.as_slice() else {
                            continue;
                        };
                        constants
                            .get(left)
                            .zip(constants.get(right))
                            .and_then(|(left, right)| fold_binary(name, left, right))
                    }
                    _ => None,
                };

                if let Some(value) = folded_value {
                    op.kind = MirOpKind::Literal(value.clone());
                    op.args.clear();
                    if let Some(result) = op.result {
                        constants.insert(result, value);
                    }
                    folded += 1;
                }
            }
        }
    }

    MirPassReport {
        name: "constant-fold",
        changed: folded > 0,
        summary: format!("constant folding rewrote {folded} op(s)"),
    }
}

pub(crate) fn analyze_lifetimes(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        let mut def_points = BTreeMap::new();
        for block in &body.blocks {
            for (index, op) in block.ops.iter().enumerate() {
                if let Some(result) = op.result {
                    def_points.insert(
                        result,
                        MirProgramPoint {
                            block: block.id,
                            index: index as u32,
                        },
                    );
                }
            }
        }

        for value in &mut body.values {
            let first = def_points.get(&value.id).copied().or(Some(MirProgramPoint {
                block: MirBlockId(0),
                index: 0,
            }));
            let last = value
                .uses
                .iter()
                .filter_map(|usage| {
                    usage.op_index.map(|index| MirProgramPoint {
                        block: usage.block,
                        index,
                    })
                })
                .max()
                .or(first);
            value.lifetime.first = first;
            value.lifetime.last = last;
            value.lifetime.reusable_after_last_use = !value.escape.escapes();
        }
        body.analyses.lifetimes_complete = true;
    }
    MirPassReport {
        name: "lifetime",
        changed: true,
        summary: "MIR lifetimes analyzed".to_owned(),
    }
}

pub(crate) fn enable_active_value_cache(module: &mut MirModule) -> MirPassReport {
    let mut inserted = 0;
    for body in &mut module.bodies {
        for block in &mut body.blocks {
            let mut next_ops = Vec::with_capacity(block.ops.len());
            for op in block.ops.drain(..) {
                let cache_key = op.result.and_then(|result| {
                    pure_cache_key(&op.kind, &op.args)
                        .filter(|_| pure_value_op(&op.kind))
                        .map(|key| (result, key))
                });
                next_ops.push(op);
                if let Some((result, key)) = cache_key {
                    next_ops.push(MirOp {
                        result: None,
                        kind: MirOpKind::CachePut(key),
                        args: vec![result],
                        span: None,
                    });
                    inserted += 1;
                }
            }
            block.ops = next_ops;
        }
        body.analyses.active_value_cache_enabled = true;
    }
    MirPassReport {
        name: "active-cache",
        changed: inserted > 0,
        summary: format!("active pure value caching inserted {inserted} cache op(s)"),
    }
}

pub(crate) fn analyze_projection_demand(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        for value in &mut body.values {
            if value
                .uses
                .iter()
                .any(|usage| matches!(usage.kind, MirUseKind::Return))
            {
                value.demand = MirDemand::Full;
            }
        }
        for block in &body.blocks {
            for op in &block.ops {
                match (&op.kind, op.args.as_slice()) {
                    (MirOpKind::Project(MirProjection::Field(field)), [target]) => {
                        mark_field_demand(&mut body.values, *target, field);
                    }
                    (MirOpKind::Project(MirProjection::Slot(slot)), [target]) => {
                        mark_slot_demand(&mut body.values, *target, *slot);
                    }
                    _ => {}
                }
            }
        }
        body.analyses.demand_complete = true;
    }
    MirPassReport {
        name: "demand",
        changed: true,
        summary: "projection demand analyzed for sealed MIR".to_owned(),
    }
}

pub(crate) fn cull_unused_composite_outputs(module: &mut MirModule) -> MirPassReport {
    let mut culled = 0;
    for body in &mut module.bodies {
        for value in &mut body.values {
            if matches!(value.demand, MirDemand::None) {
                value.storage = MirStorage::Virtual;
                culled += 1;
            }
        }
        for block in &mut body.blocks {
            for op in &mut block.ops {
                if matches!(op.kind, MirOpKind::Tuple { .. } | MirOpKind::Record { .. })
                    && op.result.and_then(|result| {
                        body.values
                            .iter()
                            .find(|value| value.id == result)
                            .map(|value| matches!(value.demand, MirDemand::Projection { .. }))
                    }) == Some(true)
                {
                    body.analyses.culled_values += 1;
                    culled += 1;
                }
            }
        }
    }
    MirPassReport {
        name: "function-culling",
        changed: culled > 0,
        summary: format!("sealed function/composite culling marked {culled} value(s)"),
    }
}

pub(crate) fn mark_copy_on_write(module: &mut MirModule) -> MirPassReport {
    let mut marked = 0;
    for body in &mut module.bodies {
        for block in &body.blocks {
            for op in &block.ops {
                if matches!(op.kind, MirOpKind::Updated { .. }) {
                    if let Some(source) = op.args.first() {
                        if let Some(value) =
                            body.values.iter_mut().find(|value| value.id == *source)
                        {
                            if !value.escape.escapes() {
                                value.storage = MirStorage::CopyOnWrite(*source);
                                body.analyses.copy_on_write_values += 1;
                                marked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    MirPassReport {
        name: "copy-on-write",
        changed: marked > 0,
        summary: format!("copy-on-write marked {marked} value(s)"),
    }
}

pub(crate) fn remove_dead_pure_ops(module: &mut MirModule) -> MirPassReport {
    let mut removed = 0;
    for body in &mut module.bodies {
        let mut body_removed = 0;
        let drop_only_values = body
            .values
            .iter()
            .filter(|value| {
                !value.escape.escapes()
                    && !value.uses.is_empty()
                    && value.uses.iter().all(|usage| {
                        usage.op_index.is_some_and(|index| {
                            body.blocks
                                .iter()
                                .find(|block| block.id == usage.block)
                                .and_then(|block| block.ops.get(index as usize))
                                .is_some_and(|op| matches!(op.kind, MirOpKind::Drop))
                        })
                    })
            })
            .map(|value| value.id)
            .collect::<BTreeSet<_>>();

        for block in &mut body.blocks {
            let before = block.ops.len();
            block.ops.retain(|op| {
                if matches!(op.kind, MirOpKind::Drop) {
                    return false;
                }
                if let Some(result) = op.result {
                    let unused = body
                        .values
                        .iter()
                        .find(|value| value.id == result)
                        .is_none_or(|value| value.uses.is_empty())
                        || drop_only_values.contains(&result);
                    if unused && pure_value_op(&op.kind) {
                        return false;
                    }
                }
                true
            });
            body_removed += before - block.ops.len();
        }
        body.analyses.culled_values += body_removed as u32;
        removed += body_removed;
    }

    MirPassReport {
        name: "dead-pure-op",
        changed: removed > 0,
        summary: format!("sealed dead pure-op cleanup removed {removed} op(s)"),
    }
}

pub(crate) fn reuse_value_slots(module: &mut MirModule) -> MirPassReport {
    let mut changed = 0;
    for body in &mut module.bodies {
        let mut next_slot = 0_u32;
        let mut reusable = Vec::<(u32, MirValueId)>::new();
        let mut active = Vec::<(MirProgramPoint, u32, MirValueId)>::new();
        let mut by_def = body.values.clone();
        by_def.sort_by_key(|value| value.lifetime.first);
        for value in by_def {
            if let Some(first) = value.lifetime.first {
                let mut still_active = Vec::new();
                for (last, slot, source) in active.drain(..) {
                    if last < first {
                        reusable.push((slot, source));
                    } else {
                        still_active.push((last, slot, source));
                    }
                }
                active = still_active;
            }

            let reused_from = reusable.pop();
            let slot = reused_from.map(|(slot, _)| slot).unwrap_or_else(|| {
                let slot = next_slot;
                next_slot += 1;
                slot
            });
            body.analyses.value_slots.insert(value.id, slot);
            if let Some(real) = body.values.iter_mut().find(|entry| entry.id == value.id) {
                if let Some((_, source)) = reused_from {
                    real.storage = MirStorage::Reuse(source);
                    body.analyses.reused_slots += 1;
                    changed += 1;
                }
                if real.lifetime.reusable_after_last_use {
                    if let Some(last) = real.lifetime.last {
                        active.push((last, slot, real.id));
                    }
                }
            }
        }
    }
    MirPassReport {
        name: "slot-reuse",
        changed: changed > 0,
        summary: format!("slot reuse assigned {changed} reusable value(s)"),
    }
}

fn fold_unary(name: &str, value: &InlineValue) -> Option<InlineValue> {
    match (name, value) {
        ("negate", InlineValue::Int(value)) => Some(InlineValue::Int(-value)),
        ("negate", InlineValue::Float(value)) => Some(InlineValue::Float(-value)),
        ("not", InlineValue::Bool(value)) => Some(InlineValue::Bool(!value)),
        _ => None,
    }
}

fn fold_binary(name: &str, left: &InlineValue, right: &InlineValue) -> Option<InlineValue> {
    match (name, left, right) {
        ("add", InlineValue::Int(left), InlineValue::Int(right)) => {
            Some(InlineValue::Int(left + right))
        }
        ("subtract", InlineValue::Int(left), InlineValue::Int(right)) => {
            Some(InlineValue::Int(left - right))
        }
        ("multiply", InlineValue::Int(left), InlineValue::Int(right)) => {
            Some(InlineValue::Int(left * right))
        }
        ("divide", InlineValue::Int(_), InlineValue::Int(0))
        | ("remainder", InlineValue::Int(_), InlineValue::Int(0)) => None,
        ("divide", InlineValue::Int(left), InlineValue::Int(right)) => {
            Some(InlineValue::Int(left / right))
        }
        ("remainder", InlineValue::Int(left), InlineValue::Int(right)) => {
            Some(InlineValue::Int(left % right))
        }
        ("add", InlineValue::Float(left), InlineValue::Float(right)) => {
            Some(InlineValue::Float(left + right))
        }
        ("subtract", InlineValue::Float(left), InlineValue::Float(right)) => {
            Some(InlineValue::Float(left - right))
        }
        ("multiply", InlineValue::Float(left), InlineValue::Float(right)) => {
            Some(InlineValue::Float(left * right))
        }
        ("divide", InlineValue::Float(left), InlineValue::Float(right)) => {
            Some(InlineValue::Float(left / right))
        }
        ("remainder", InlineValue::Float(left), InlineValue::Float(right)) => {
            Some(InlineValue::Float(left % right))
        }
        ("add", InlineValue::String(left), InlineValue::String(right)) => {
            Some(InlineValue::String(format!("{left}{right}")))
        }
        ("less", left, right) => compare_inline(left, right, |ordering| ordering.is_lt()),
        ("less_equal", left, right) => compare_inline(left, right, |ordering| !ordering.is_gt()),
        ("greater", left, right) => compare_inline(left, right, |ordering| ordering.is_gt()),
        ("greater_equal", left, right) => compare_inline(left, right, |ordering| !ordering.is_lt()),
        ("equal", left, right) => Some(InlineValue::Bool(left == right)),
        ("not_equal", left, right) => Some(InlineValue::Bool(left != right)),
        _ => None,
    }
}

fn compare_inline(
    left: &InlineValue,
    right: &InlineValue,
    predicate: impl FnOnce(std::cmp::Ordering) -> bool,
) -> Option<InlineValue> {
    let ordering = match (left, right) {
        (InlineValue::Int(left), InlineValue::Int(right)) => left.cmp(right),
        (InlineValue::String(left), InlineValue::String(right)) => left.cmp(right),
        _ => return None,
    };
    Some(InlineValue::Bool(predicate(ordering)))
}

fn pure_cache_key(kind: &MirOpKind, args: &[MirValueId]) -> Option<String> {
    Some(format!(
        "{}({})",
        pure_op_name(kind)?,
        args.iter()
            .map(|arg| format!("%{}", arg.0))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn pure_op_name(kind: &MirOpKind) -> Option<String> {
    match kind {
        MirOpKind::Literal(value) => Some(format!("literal:{value:?}")),
        MirOpKind::Unit => Some("unit".to_owned()),
        MirOpKind::Unary(name) => Some(format!("unary:{name}")),
        MirOpKind::Binary(name) => Some(format!("binary:{name}")),
        MirOpKind::Tuple { shape } => Some(format!("tuple:{shape}")),
        MirOpKind::Record { fields } => Some(format!("record:{}", fields.join(","))),
        MirOpKind::List => Some("list".to_owned()),
        MirOpKind::StringInterpolate { text } => {
            Some(format!("string_interpolate:{text:?}"))
        }
        MirOpKind::Project(projection) => Some(format!("project:{projection:?}")),
        MirOpKind::Index => Some("index".to_owned()),
        MirOpKind::Updated { path } => Some(format!("updated:{path:?}")),
        MirOpKind::NonNull => Some("non_null".to_owned()),
        MirOpKind::SafeProject(field) => Some(format!("safe_project:{field}")),
        MirOpKind::TypeTest(ty) => Some(format!("type_test:{ty}")),
        MirOpKind::TypeRefine(ty) => Some(format!("type_refine:{ty}")),
        MirOpKind::Use(version) => Some(format!("use:{}", version.0)),
        _ => None,
    }
}

fn pure_value_op(kind: &MirOpKind) -> bool {
    pure_op_name(kind).is_some()
}

pub(crate) fn seal_module(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        body.analyses.sealed = true;
    }
    MirPassReport {
        name: "seal",
        changed: true,
        summary: "sealed MIR compilation complete".to_owned(),
    }
}

fn mark_field_demand(values: &mut [MirValue], target: MirValueId, field: &str) {
    if let Some(value) = values.iter_mut().find(|value| value.id == target) {
        match &mut value.demand {
            MirDemand::Projection { fields, .. } => {
                fields.insert(field.to_owned());
            }
            MirDemand::Unknown | MirDemand::None => {
                let mut fields = BTreeSet::new();
                fields.insert(field.to_owned());
                value.demand = MirDemand::Projection {
                    fields,
                    slots: BTreeSet::new(),
                };
            }
            MirDemand::Full => {}
        }
    }
}

fn mark_slot_demand(values: &mut [MirValue], target: MirValueId, slot: usize) {
    if let Some(value) = values.iter_mut().find(|value| value.id == target) {
        match &mut value.demand {
            MirDemand::Projection { slots, .. } => {
                slots.insert(slot);
            }
            MirDemand::Unknown | MirDemand::None => {
                let mut slots = BTreeSet::new();
                slots.insert(slot);
                value.demand = MirDemand::Projection {
                    fields: BTreeSet::new(),
                    slots,
                };
            }
            MirDemand::Full => {}
        }
    }
}

fn type_syntax_to_vox(ty: &TypeSyntax) -> VoxType {
    VoxType::opaque_surface(ty.to_source_string())
}

fn mir_mutability(mutability: Mutability) -> MirMutability {
    match mutability {
        Mutability::Val => MirMutability::Val,
        Mutability::Var => MirMutability::Var,
    }
}

fn mir_path_segment(segment: &UpdatedPathSegment) -> MirPathSegment {
    match segment {
        UpdatedPathSegment::Field(field) => MirPathSegment::Field(field.clone()),
        UpdatedPathSegment::Index(index) => MirPathSegment::Index(*index),
    }
}

fn unary_op_name(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Negate => "negate",
        UnaryOp::Not => "not",
    }
}

fn binary_op_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Multiply => "multiply",
        BinaryOp::Divide => "divide",
        BinaryOp::Remainder => "remainder",
        BinaryOp::Add => "add",
        BinaryOp::Subtract => "subtract",
        BinaryOp::Less => "less",
        BinaryOp::LessEqual => "less_equal",
        BinaryOp::Greater => "greater",
        BinaryOp::GreaterEqual => "greater_equal",
        BinaryOp::Equal => "equal",
        BinaryOp::NotEqual => "not_equal",
        BinaryOp::And => "and_short_circuit",
        BinaryOp::Or => "or_short_circuit",
        BinaryOp::Coalesce => "coalesce_short_circuit",
    }
}

fn compound_op_name(op: CompoundAssignmentOp) -> &'static str {
    match op {
        CompoundAssignmentOp::Add => "add",
        CompoundAssignmentOp::Subtract => "subtract",
        CompoundAssignmentOp::Multiply => "multiply",
        CompoundAssignmentOp::Divide => "divide",
        CompoundAssignmentOp::Remainder => "remainder",
    }
}

fn string_literal_text(literal: &StringLiteral) -> String {
    let mut out = String::new();
    for part in &literal.parts {
        match part {
            StringPart::Text(text) => out.push_str(text),
            StringPart::Interpolation(_) => out.push_str("{}"),
        }
    }
    out
}

fn callee_label(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Name(name) => name.to_source_string(),
        ExprKind::Field { target, name } => format!("{}.{}", callee_label(target), name),
        _ => "<expr>".to_owned(),
    }
}
