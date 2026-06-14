use std::collections::{BTreeMap, BTreeSet, VecDeque};

use vox_core::{
    diagnostics::{Diagnostic, DiagnosticBag},
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
        ExprKind, ForHeader, FunctionDecl, IfExpr, IntrinsicExpr, LambdaExpr, LocalValueDecl,
        Mutability, QualifiedName, StringLiteral, StringPart, TopLevelItem, TypeSyntax, UnaryOp,
        UpdatedPathSegment, ValueDecl, WhenExpr,
    },
};
use crate::imports::ImportResolution;

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
    import_resolution: ImportResolution,
) -> MirModule {
    MirLowerer::new(frontend, optimization, rankings, import_resolution).lower_module()
}

pub(crate) fn check_return_type_inference(
    frontend: &FrontendUnit,
    module: &MirModule,
) -> DiagnosticBag {
    let mut diagnostics = DiagnosticBag::default();
    let annotated: BTreeMap<&str, bool> = frontend
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Function(f) => Some((f.name.as_str(), f.return_type.is_some())),
            _ => None,
        })
        .collect();

    for body in &module.bodies {
        if !matches!(body.kind, MirBodyKind::Function) {
            continue;
        }
        if body.result_type.is_some() {
            continue;
        }
        if annotated.get(body.name.as_str()).copied().unwrap_or(false) {
            continue;
        }
        let message = format!(
            "function `{}` has no return type annotation and its return type cannot be inferred; add an explicit return type annotation",
            body.name
        );
        let mut diagnostic = Diagnostic::error(message);
        if let Some(span) = &body.span {
            diagnostic = diagnostic.with_span(span.clone());
        }
        diagnostics.push(diagnostic);
    }
    diagnostics
}

struct MirLowerer<'a> {
    frontend: &'a FrontendUnit,
    optimization: OptimizationLevel,
    rankings: &'a [vox_core::opt::OptimizationRanking],
    import_resolution: ImportResolution,
    function_return_types: BTreeMap<String, VoxType>,
    next_body: u32,
    lambda_bodies: Vec<MirBody>,
}

impl<'a> MirLowerer<'a> {
    fn new(
        frontend: &'a FrontendUnit,
        optimization: OptimizationLevel,
        rankings: &'a [vox_core::opt::OptimizationRanking],
        import_resolution: ImportResolution,
    ) -> Self {
        let function_return_types = collect_function_return_types(frontend);
        Self {
            frontend,
            optimization,
            rankings,
            import_resolution,
            function_return_types,
            next_body: 0,
            lambda_bodies: Vec::new(),
        }
    }

    fn lower_module(mut self) -> MirModule {
        let mut function_bodies = VecDeque::new();
        for item in &self.frontend.syntax.items {
            if let TopLevelItem::Function(function) = item {
                function_bodies.push_back(self.lower_function(function));
            }
        }

        let mut bodies = Vec::new();
        if matches!(self.frontend.header.kind, ModuleKind::Script { .. }) {
            bodies.push(self.lower_script_entry(&self.frontend.syntax));
        }

        for item in &self.frontend.syntax.items {
            match item {
                TopLevelItem::Function(_) => {
                    if let Some(body) = function_bodies.pop_front() {
                        bodies.push(body);
                    }
                }
                TopLevelItem::Value(value)
                    if matches!(self.frontend.header.kind, ModuleKind::Package) =>
                {
                    bodies.push(self.lower_value_initializer(value));
                }
                _ => {}
            }
        }

        bodies.append(&mut self.lambda_bodies);

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
            self.import_resolution.clone(),
            self.function_return_types.clone(),
            None,
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
        let (body, lambda_bodies) = body.finish();
        self.lambda_bodies.extend(lambda_bodies);
        body
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
            self.import_resolution.clone(),
            self.function_return_types.clone(),
            function.return_type.as_ref().map(type_syntax_to_vox),
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
        let (finished, lambda_bodies) = body.finish();
        self.lambda_bodies.extend(lambda_bodies);
        if let Some(result_type) = finished.result_type.clone() {
            self.function_return_types
                .insert(function.name.clone(), result_type.clone());
            self.function_return_types.insert(
                format!("{}.{}", self.frontend.header.module.as_str(), function.name),
                result_type,
            );
        }
        finished
    }

    fn lower_value_initializer(&mut self, value: &ValueDecl) -> MirBody {
        let mut body = BodyBuilder::new(
            self.alloc_body_id(),
            format!("init.{}", value.name),
            MirBodyKind::ValueInitializer,
            Purity::Pure,
            self.rank_for(OptimizationSubject::Module),
            Some(value.span.clone()),
            self.import_resolution.clone(),
            self.function_return_types.clone(),
            value.ty.as_ref().map(type_syntax_to_vox),
        );
        let result = body.lower_expr(&value.initializer);
        body.terminate(MirTerminator::Return(result));
        let (finished, lambda_bodies) = body.finish();
        self.lambda_bodies.extend(lambda_bodies);
        finished
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
    import_resolution: ImportResolution,
    function_return_types: BTreeMap<String, VoxType>,
    result_type: Option<VoxType>,
    next_binding: u32,
    next_version: u32,
    next_value: u32,
    next_block: u32,
    loop_stack: Vec<LoopContext>,
    lambda_bodies: Vec<MirBody>,
}

struct LoopContext {
    continue_target: MirBlockId,
    break_target: MirBlockId,
}

impl BodyBuilder {
    fn new(
        body_id: MirBodyId,
        name: String,
        kind: MirBodyKind,
        purity: Purity,
        rank: OptimizationRank,
        span: Option<vox_core::diagnostics::TextSpan>,
        import_resolution: ImportResolution,
        function_return_types: BTreeMap<String, VoxType>,
        result_type: Option<VoxType>,
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
            import_resolution,
            function_return_types,
            result_type,
            next_binding: 0,
            next_version: 0,
            next_value: 0,
            next_block: 1,
            loop_stack: Vec::new(),
            lambda_bodies: Vec::new(),
        }
    }

    fn finish(self) -> (MirBody, Vec<MirBody>) {
        let body = MirBody {
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
            result_type: self.result_type,
            analyses: MirAnalysisSummary::default(),
        };
        (body, self.lambda_bodies)
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
            BlockItem::Break(_) => {
                let target = self
                    .loop_stack
                    .last()
                    .expect("break outside loop")
                    .break_target;
                self.terminate(MirTerminator::Jump {
                    target,
                    args: Vec::new(),
                });
                self.current = self.new_block("after_break");
            }
            BlockItem::Continue(_) => {
                let target = self
                    .loop_stack
                    .last()
                    .expect("continue outside loop")
                    .continue_target;
                self.terminate(MirTerminator::Jump {
                    target,
                    args: Vec::new(),
                });
                self.current = self.new_block("after_continue");
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
                let args = self.lower_arguments(arguments);
                let resolved = self.resolve_callee_label(callee);
                self.emit_op(
                    MirOpKind::Call {
                        callee: resolved,
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
            ExprKind::Lambda(lambda) => self.lower_lambda(lambda, expr),
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
        let result = self.new_value(
            Some(VoxType::Bool),
            MirValueDefinition::BlockParameter(join),
            span.clone(),
        );
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
        if let Some(init) = &for_expr.init {
            self.lower_block_item(init);
        }

        match &for_expr.header {
            ForHeader::In { pattern, iterable } => {
                let iterable = self.lower_expr(iterable);
                let iterator = self.emit_op(MirOpKind::Iterator, vec![iterable], span.clone());
                let header = self.new_block("for_header");
                let body_block = self.new_block("for_body");
                let exit = self.new_block("for_exit");
                self.terminate(MirTerminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                self.current = header;
                let item = self.emit_op(MirOpKind::IteratorNext, vec![iterator], span.clone());
                let is_null = self.emit_op(
                    MirOpKind::TypeTest("Null".to_owned()),
                    vec![item],
                    span.clone(),
                );
                self.terminate(MirTerminator::Branch {
                    condition: is_null,
                    then_target: exit,
                    then_args: Vec::new(),
                    else_target: body_block,
                    else_args: Vec::new(),
                });

                self.current = body_block;
                self.push_scope();
                self.declare_binding(
                    pattern.clone(),
                    MirMutability::Val,
                    None,
                    span.clone(),
                    item,
                    MirVersionSource::Loop,
                );
                self.loop_stack.push(LoopContext {
                    continue_target: header,
                    break_target: exit,
                });
                self.lower_block_expr(&for_expr.body);
                self.loop_stack.pop();
                self.pop_scope();
                self.jump_to_join_if_open(header, Vec::new());
                self.current = exit;
                self.emit_unit()
            }
            ForHeader::Condition(condition) => {
                let header = self.new_block("for_header");
                let body_block = self.new_block("for_body");
                let exit = self.new_block("for_exit");
                self.terminate(MirTerminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                self.current = header;
                let cond = self.lower_expr(condition);
                self.terminate(MirTerminator::Branch {
                    condition: cond,
                    then_target: body_block,
                    then_args: Vec::new(),
                    else_target: exit,
                    else_args: Vec::new(),
                });

                self.current = body_block;
                self.loop_stack.push(LoopContext {
                    continue_target: header,
                    break_target: exit,
                });
                self.lower_block_expr(&for_expr.body);
                self.loop_stack.pop();
                self.jump_to_join_if_open(header, Vec::new());
                self.current = exit;
                self.emit_unit()
            }
        }
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
            self.terminate_with_panic(format!(
                "qualified name `{}` is not available as a MIR value",
                name.to_source_string()
            ));
            self.emit_unit_with_span(span)
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
        self.terminate_with_panic(format!("unknown name `{name}`"));
        self.emit_unit_with_span(span)
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

        if !self.loop_stack.is_empty() {
            let is_outer_binding = self
                .bindings
                .iter()
                .find(|b| b.id == previous.binding)
                .map_or(false, |b| (b.scope_depth as usize) < self.scopes.len() - 1);
            if is_outer_binding {
                self.set_value_version(value, previous.version);
                self.emit_op_with_result(
                    None,
                    MirOpKind::Bind(previous.version),
                    vec![value],
                    None,
                );
                return;
            }
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
        self.emit_unit_with_span(None)
    }

    fn emit_unit_with_span(&mut self, span: Option<vox_core::diagnostics::TextSpan>) -> MirValueId {
        let value = self.new_value(
            Some(VoxType::Tuple(Vec::new())),
            MirValueDefinition::Unit,
            span.clone(),
        );
        self.emit_op_with_result(Some(value), MirOpKind::Unit, Vec::new(), span);
        value
    }

    fn emit_op(
        &mut self,
        kind: MirOpKind,
        args: Vec<MirValueId>,
        span: Option<vox_core::diagnostics::TextSpan>,
    ) -> MirValueId {
        let ty = match &kind {
            MirOpKind::Literal(val) => Some(mir_type_from_literal(val)),
            MirOpKind::Binary(name) => Some(mir_type_from_binary_op(name, &args, self)),
            MirOpKind::Unary(name) => Some(mir_type_from_unary_op(name, &args, self)),
            MirOpKind::Use(_) => args.first().and_then(|arg| self.value_type(*arg).cloned()),
            MirOpKind::TypeRefine(ty) => Some(VoxType::opaque_surface(ty.clone())),
            MirOpKind::Unit => Some(VoxType::Tuple(Vec::new())),
            MirOpKind::NonNull => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .map(mir_non_null_type),
            MirOpKind::TypeTest(_) => Some(VoxType::Bool),
            MirOpKind::Tuple { .. } => Some(VoxType::Tuple(
                args.iter()
                    .map(|arg| {
                        self.value_type(*arg)
                            .cloned()
                            .unwrap_or_else(|| VoxType::opaque_surface("Unknown"))
                    })
                    .collect(),
            )),
            MirOpKind::Record { fields } => Some(VoxType::Record(
                fields
                    .iter()
                    .zip(args.iter())
                    .map(|(name, arg)| vox_core::types::RecordField {
                        name: name.clone(),
                        ty: self
                            .value_type(*arg)
                            .cloned()
                            .unwrap_or_else(|| VoxType::opaque_surface("Unknown")),
                    })
                    .collect(),
            )),
            MirOpKind::List => Some(VoxType::List(Box::new(
                args.first()
                    .and_then(|arg| self.value_type(*arg).cloned())
                    .unwrap_or_else(|| VoxType::opaque_surface("Unknown")),
            ))),
            MirOpKind::StringInterpolate { .. } => Some(VoxType::String),
            MirOpKind::Project(projection) => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .and_then(|ty| mir_projected_type(ty, projection)),
            MirOpKind::SafeProject(field) => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .and_then(|ty| mir_projected_type(ty, &MirProjection::Field(field.clone())))
                .map(|ty| VoxType::Nullable(Box::new(ty))),
            MirOpKind::Index => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .and_then(mir_indexed_type),
            MirOpKind::Updated { .. } => {
                args.first().and_then(|arg| self.value_type(*arg).cloned())
            }
            MirOpKind::Call { callee, .. } => self.function_return_types.get(callee).cloned(),
            MirOpKind::Iterator => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .and_then(mir_iterator_type),
            MirOpKind::IteratorNext => args
                .first()
                .and_then(|arg| self.value_type(*arg))
                .and_then(mir_iterator_next_type),
            _ => None,
        };
        let definition = match kind {
            MirOpKind::Literal(_) => MirValueDefinition::Literal,
            _ => MirValueDefinition::Op,
        };
        let value = self.new_value(ty, definition, span.clone());
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
            MirTerminator::Return(value) => {
                if self.result_type.is_none() {
                    self.result_type = self.value_type(*value).cloned();
                }
                self.add_use(
                    *value,
                    MirUse {
                        block: block_id,
                        op_index: None,
                        kind: MirUseKind::Return,
                    },
                );
            }
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

    fn value_type(&self, id: MirValueId) -> Option<&VoxType> {
        self.values
            .iter()
            .find(|v| v.id == id)
            .and_then(|v| v.ty.as_ref())
    }

    fn set_value_type(&mut self, id: MirValueId, ty: VoxType) {
        if let Some(value) = self.values.iter_mut().find(|v| v.id == id) {
            value.ty = Some(ty);
        }
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
            self.merge_block_parameter_types(target, &args);
            self.terminate(MirTerminator::Jump { target, args });
        }
    }

    fn merge_block_parameter_types(&mut self, target: MirBlockId, args: &[MirValueId]) {
        let params = self
            .blocks
            .iter()
            .find(|block| block.id == target)
            .map(|block| block.parameters.clone())
            .unwrap_or_default();
        for (param, arg) in params.into_iter().zip(args.iter().copied()) {
            let Some(arg_ty) = self.value_type(arg).cloned() else {
                continue;
            };
            let merged = match self.value_type(param).cloned() {
                Some(existing) => merge_mir_types(existing, arg_ty),
                None => Some(arg_ty),
            };
            if let Some(ty) = merged {
                self.set_value_type(param, ty);
            }
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

    fn register_external_value(&mut self, value_id: MirValueId) {
        if !self.values.iter().any(|v| v.id == value_id) {
            self.values.push(MirValue {
                id: value_id,
                ty: None,
                definition: MirValueDefinition::Capture("".to_owned()),
                span: None,
                binding_version: None,
                uses: Vec::new(),
                lifetime: MirLifetime::default(),
                escape: MirEscape::default(),
                demand: MirDemand::Unknown,
                storage: MirStorage::Fresh,
            });
        }
    }

    fn lower_lambda(&mut self, lambda: &LambdaExpr, expr: &Expr) -> MirValueId {
        let param_names: Vec<String> = lambda.parameters.iter().map(|p| p.name.clone()).collect();
        let param_set: BTreeSet<String> = param_names.iter().cloned().collect();

        let mut free_names = BTreeSet::new();
        collect_free_names_for_lambda(&lambda.body, &param_set, &mut free_names);

        let mut captures: Vec<MirValueId> = Vec::new();
        let mut capture_names: Vec<String> = Vec::new();
        for name in &free_names {
            if let Some(binding) = self.resolve_binding(name) {
                captures.push(binding.value);
                capture_names.push(name.clone());
            }
        }

        let body_id = self.alloc_foreign_body_id();

        let mut child = BodyBuilder::new(
            body_id,
            format!("lambda.{}.{}", body_id.0, captures.len()),
            MirBodyKind::Lambda,
            Purity::Pure,
            self.rank,
            Some(lambda.span.clone()),
            self.import_resolution.clone(),
            self.function_return_types.clone(),
            None,
        );

        for (name, value) in capture_names.iter().zip(captures.iter()) {
            child.register_external_value(*value);
            child.declare_binding(
                name.clone(),
                MirMutability::Val,
                None,
                None,
                *value,
                MirVersionSource::Capture,
            );
        }

        for param in &lambda.parameters {
            let value = child.new_value(
                Some(VoxType::opaque_surface("Unknown")),
                MirValueDefinition::Parameter(param.name.clone()),
                Some(param.span.clone()),
            );
            child.declare_binding(
                param.name.clone(),
                MirMutability::Val,
                None,
                Some(param.span.clone()),
                value,
                MirVersionSource::Parameter,
            );
            child.parameters.push(value);
        }

        for name in &capture_names {
            if let Some(binding) = child.resolve_binding(name) {
                child.set_value_version(binding.value, binding.version);
            }
        }

        let result = child.lower_expr(&lambda.body);
        child.terminate(MirTerminator::Return(result));
        let (lambda_body, _nested_lambdas) = child.finish();
        self.lambda_bodies.push(lambda_body);

        self.emit_op(
            MirOpKind::Lambda {
                parameters: param_names,
                captures: captures.clone(),
                body_id,
            },
            captures,
            Some(expr.span.clone()),
        )
    }

    fn alloc_foreign_body_id(&mut self) -> MirBodyId {
        MirBodyId(self.lambda_bodies.len() as u32 + 10000)
    }
}

fn collect_free_names_for_lambda(
    expr: &Expr,
    param_set: &BTreeSet<String>,
    names: &mut BTreeSet<String>,
) {
    match &expr.kind {
        ExprKind::Name(name) => {
            let label = name.to_source_string();
            if !param_set.contains(&label) {
                names.insert(label);
            }
        }
        ExprKind::Call { callee, arguments } => {
            collect_free_names_for_lambda(callee, param_set, names);
            for arg in arguments {
                if let Argument::Positional(expr) = arg {
                    collect_free_names_for_lambda(expr, param_set, names);
                }
            }
        }
        ExprKind::ReceiverCall {
            receiver,
            arguments,
            ..
        } => {
            collect_free_names_for_lambda(receiver, param_set, names);
            for arg in arguments {
                if let Argument::Positional(expr) = arg {
                    collect_free_names_for_lambda(expr, param_set, names);
                }
            }
        }
        ExprKind::Unary { expr: inner, .. } => {
            collect_free_names_for_lambda(inner, param_set, names);
        }
        ExprKind::Binary { left, right, .. } => {
            collect_free_names_for_lambda(left, param_set, names);
            collect_free_names_for_lambda(right, param_set, names);
        }
        ExprKind::Block(block) => {
            let mut block_param_names = BTreeSet::new();
            for item in &block.items {
                match item {
                    BlockItem::LocalValue(local) => {
                        collect_free_names_for_lambda(&local.initializer, param_set, names);
                        block_param_names.insert(local.name.clone());
                    }
                    BlockItem::Assignment(a) => {
                        collect_free_names_for_lambda(&a.value, param_set, names);
                    }
                    BlockItem::CompoundAssignment(ca) => {
                        collect_free_names_for_lambda(&ca.value, param_set, names);
                    }
                    BlockItem::Expr(e) => {
                        collect_free_names_for_lambda(e, param_set, names);
                    }
                    _ => {}
                }
            }
            let inner_set: BTreeSet<String> =
                param_set.union(&block_param_names).cloned().collect();
            if let Some(trailing) = &block.trailing {
                collect_free_names_for_lambda(trailing, &inner_set, names);
            }
        }
        ExprKind::Lambda(inner_lambda) => {
            let mut inner_params: BTreeSet<String> = param_set.clone();
            for p in &inner_lambda.parameters {
                inner_params.insert(p.name.clone());
            }
            collect_free_names_for_lambda(&inner_lambda.body, &inner_params, names);
        }
        ExprKind::Index { target, index } => {
            collect_free_names_for_lambda(target, param_set, names);
            collect_free_names_for_lambda(index, param_set, names);
        }
        ExprKind::Field { target, .. } => {
            collect_free_names_for_lambda(target, param_set, names);
        }
        ExprKind::SafeField { target, .. } => {
            collect_free_names_for_lambda(target, param_set, names);
        }
        ExprKind::If(if_expr) => {
            for branch in &if_expr.branches {
                collect_free_names_for_lambda(&branch.condition, param_set, names);
                collect_free_names_for_lambda_block(&branch.body, param_set, names);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_free_names_for_lambda_block(else_branch, param_set, names);
            }
        }
        ExprKind::Range(range) => {
            if let Some(start) = &range.start {
                collect_free_names_for_lambda(start, param_set, names);
            }
            if let Some(end) = &range.end {
                collect_free_names_for_lambda(end, param_set, names);
            }
        }
        ExprKind::When(when) => {
            collect_free_names_for_lambda(&when.subject, param_set, names);
            for arm in &when.arms {
                collect_free_names_for_lambda(&arm.body, param_set, names);
            }
            if let Some(else_arm) = &when.else_arm {
                collect_free_names_for_lambda(else_arm, param_set, names);
            }
        }
        ExprKind::For(for_expr) => match &for_expr.header {
            ForHeader::In { pattern, iterable } => {
                collect_free_names_for_lambda(iterable, param_set, names);
                let mut for_params = param_set.clone();
                for_params.insert(pattern.clone());
                collect_free_names_for_lambda_block(&for_expr.body, &for_params, names);
            }
            ForHeader::Condition(cond) => {
                collect_free_names_for_lambda(cond, param_set, names);
                collect_free_names_for_lambda_block(&for_expr.body, param_set, names);
            }
        },
        ExprKind::NonNull { target } => {
            collect_free_names_for_lambda(target, param_set, names);
        }
        ExprKind::Intrinsic(intrinsic) => match intrinsic {
            IntrinsicExpr::Updated(updated) => {
                collect_free_names_for_lambda(&updated.target, param_set, names);
                for update in &updated.updates {
                    collect_free_names_for_lambda(&update.value, param_set, names);
                }
            }
            IntrinsicExpr::Econ(econ) => {
                collect_free_names_for_lambda_block(&econ.body, param_set, names);
            }
        },
        _ => {}
    }
}

fn collect_free_names_for_lambda_block(
    block: &BlockExpr,
    param_set: &BTreeSet<String>,
    names: &mut BTreeSet<String>,
) {
    let mut block_param_names = BTreeSet::new();
    for item in &block.items {
        match item {
            BlockItem::LocalValue(local) => {
                collect_free_names_for_lambda(&local.initializer, param_set, names);
                block_param_names.insert(local.name.clone());
            }
            BlockItem::Assignment(a) => {
                collect_free_names_for_lambda(&a.value, param_set, names);
            }
            BlockItem::CompoundAssignment(ca) => {
                collect_free_names_for_lambda(&ca.value, param_set, names);
            }
            BlockItem::Expr(e) => {
                collect_free_names_for_lambda(e, param_set, names);
            }
            BlockItem::Return(r) => {
                if let Some(expr) = &r.value {
                    collect_free_names_for_lambda(expr, param_set, names);
                }
            }
            BlockItem::Panic(_)
            | BlockItem::Break(_)
            | BlockItem::Continue(_)
            | BlockItem::BlockStatement(_) => {}
        }
    }
    let inner_set: BTreeSet<String> = param_set.union(&block_param_names).cloned().collect();
    if let Some(trailing) = &block.trailing {
        collect_free_names_for_lambda(trailing, &inner_set, names);
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

pub(crate) fn propagate_copies(module: &mut MirModule) -> MirPassReport {
    let mut rewrites = 0;
    let mut removed = 0;
    for body in &mut module.bodies {
        let mut rebound_versions: BTreeSet<MirVersionId> = BTreeSet::new();
        {
            let mut version_bind_blocks: BTreeMap<MirVersionId, BTreeSet<MirBlockId>> =
                BTreeMap::new();
            for block in &body.blocks {
                for op in &block.ops {
                    if let MirOpKind::Bind(version) = op.kind {
                        version_bind_blocks
                            .entry(version)
                            .or_default()
                            .insert(block.id);
                    }
                }
            }
            for (version, blocks) in &version_bind_blocks {
                if blocks.len() > 1 {
                    rebound_versions.insert(*version);
                }
            }
        }

        let mut aliases = BTreeMap::<MirValueId, MirValueId>::new();
        for block in &body.blocks {
            for op in &block.ops {
                if let MirOpKind::Use(version) = op.kind {
                    if rebound_versions.contains(&version) {
                        continue;
                    }
                    if let (Some(result), [source]) = (op.result, op.args.as_slice()) {
                        if result != *source {
                            aliases.insert(result, *source);
                        }
                    }
                }
            }
        }

        if aliases.is_empty() {
            continue;
        }

        for block in &mut body.blocks {
            for op in &mut block.ops {
                for arg in &mut op.args {
                    if rewrite_copy_alias(arg, &aliases) {
                        rewrites += 1;
                    }
                }
            }

            match &mut block.terminator {
                MirTerminator::Return(value) => {
                    if rewrite_copy_alias(value, &aliases) {
                        rewrites += 1;
                    }
                }
                MirTerminator::Branch {
                    condition,
                    then_args,
                    else_args,
                    ..
                } => {
                    if rewrite_copy_alias(condition, &aliases) {
                        rewrites += 1;
                    }
                    for arg in then_args.iter_mut().chain(else_args.iter_mut()) {
                        if rewrite_copy_alias(arg, &aliases) {
                            rewrites += 1;
                        }
                    }
                }
                MirTerminator::Jump { args, .. } => {
                    for arg in args {
                        if rewrite_copy_alias(arg, &aliases) {
                            rewrites += 1;
                        }
                    }
                }
                MirTerminator::Panic(_) | MirTerminator::Unreachable => {}
            }
        }

        for version in &mut body.versions {
            if rewrite_copy_alias(&mut version.value, &aliases) {
                rewrites += 1;
            }
        }

        for block in &mut body.blocks {
            let before = block.ops.len();
            block.ops.retain(|op| {
                !(matches!(op.kind, MirOpKind::Use(_))
                    && op
                        .result
                        .is_some_and(|result| aliases.contains_key(&result)))
            });
            removed += before - block.ops.len();
        }

        body.values.retain(|value| !aliases.contains_key(&value.id));
    }

    MirPassReport {
        name: "copy-propagation",
        changed: rewrites > 0 || removed > 0,
        summary: format!(
            "copy propagation rewrote {rewrites} reference(s) and removed {removed} copy op(s)"
        ),
    }
}

fn rewrite_copy_alias(value: &mut MirValueId, aliases: &BTreeMap<MirValueId, MirValueId>) -> bool {
    let original = *value;
    let mut resolved = original;
    let mut seen = BTreeSet::new();
    while let Some(next) = aliases.get(&resolved).copied() {
        if !seen.insert(resolved) {
            break;
        }
        resolved = next;
    }
    *value = resolved;
    resolved != original
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
        MirOpKind::StringInterpolate { text } => Some(format!("string_interpolate:{text:?}")),
        MirOpKind::Project(projection) => Some(format!("project:{projection:?}")),
        MirOpKind::Index => Some("index".to_owned()),
        MirOpKind::Updated { path } => Some(format!("updated:{path:?}")),
        MirOpKind::Lambda { parameters, .. } => Some(format!("lambda:{}", parameters.join(","))),
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

fn collect_function_return_types(frontend: &FrontendUnit) -> BTreeMap<String, VoxType> {
    let mut types = BTreeMap::new();
    for item in &frontend.syntax.items {
        let TopLevelItem::Function(function) = item else {
            continue;
        };
        let Some(return_type) = function.return_type.as_ref().map(type_syntax_to_vox) else {
            continue;
        };
        types.insert(function.name.clone(), return_type.clone());
        types.insert(
            format!("{}.{}", frontend.header.module.as_str(), function.name),
            return_type,
        );
    }
    types
}

fn mir_non_null_type(ty: &VoxType) -> VoxType {
    match ty {
        VoxType::Nullable(inner) => (**inner).clone(),
        other => other.clone(),
    }
}

fn mir_projected_type(ty: &VoxType, projection: &MirProjection) -> Option<VoxType> {
    match (ty, projection) {
        (VoxType::Record(fields), MirProjection::Field(name)) => fields
            .iter()
            .find(|field| field.name == *name)
            .map(|field| field.ty.clone()),
        (VoxType::Tuple(items), MirProjection::Slot(slot)) => items.get(*slot).cloned(),
        _ => None,
    }
}

fn mir_indexed_type(ty: &VoxType) -> Option<VoxType> {
    match ty {
        VoxType::List(item) => Some((**item).clone()),
        VoxType::Tuple(items) => {
            let first = items.first()?.clone();
            if items.iter().all(|item| item == &first) {
                Some(first)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn mir_iterator_type(ty: &VoxType) -> Option<VoxType> {
    let item = match ty {
        VoxType::List(item) => (**item).clone(),
        VoxType::Tuple(items) if items == &[VoxType::Int, VoxType::Int, VoxType::Bool] => {
            VoxType::Int
        }
        VoxType::Tuple(items) => {
            let first = items.first()?.clone();
            if items.iter().all(|item| item == &first) {
                first
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(VoxType::opaque_surface(format!(
        "Iterator<{}>",
        render_type_key(&item)
    )))
}

fn mir_iterator_next_type(ty: &VoxType) -> Option<VoxType> {
    let VoxType::OpaqueSurface(name) = ty else {
        return None;
    };
    let inner = name
        .strip_prefix("Iterator<")
        .and_then(|rest| rest.strip_suffix('>'))?;
    parse_type_key(inner).map(|ty| VoxType::Nullable(Box::new(ty)))
}

fn merge_mir_types(left: VoxType, right: VoxType) -> Option<VoxType> {
    if left == right {
        return Some(left);
    }
    match (left, right) {
        (VoxType::OpaqueSurface(name), ty) | (ty, VoxType::OpaqueSurface(name))
            if name == "Null" =>
        {
            Some(VoxType::Nullable(Box::new(ty)))
        }
        (VoxType::Nullable(left), right) if *left == right => Some(VoxType::Nullable(left)),
        (left, VoxType::Nullable(right)) if left == *right => Some(VoxType::Nullable(right)),
        (VoxType::Nullable(left), VoxType::Nullable(right)) if left == right => {
            Some(VoxType::Nullable(left))
        }
        _ => None,
    }
}

fn render_type_key(ty: &VoxType) -> String {
    match ty {
        VoxType::Int => "Int".to_owned(),
        VoxType::Float => "Float".to_owned(),
        VoxType::Bool => "Bool".to_owned(),
        VoxType::String => "String".to_owned(),
        VoxType::OpaqueSurface(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

fn parse_type_key(raw: &str) -> Option<VoxType> {
    match raw {
        "Int" => Some(VoxType::Int),
        "Float" => Some(VoxType::Float),
        "Bool" => Some(VoxType::Bool),
        "String" => Some(VoxType::String),
        "Null" => Some(VoxType::opaque_surface("Null")),
        _ => None,
    }
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

impl BodyBuilder {
    fn resolve_callee_label(&self, callee: &Expr) -> String {
        match &callee.kind {
            ExprKind::Name(name) => {
                let label = name.to_source_string();
                if self.resolve_binding(&label).is_some() {
                    return label;
                }
                if let Some(resolved) = self.import_resolution.unqualified.get(&label) {
                    return resolved.clone();
                }
                if name.segments.len() > 1
                    && let Some(resolved) = self.resolve_module_prefix(&name.segments)
                {
                    return resolved;
                }
                label
            }
            ExprKind::Field { target, name } => {
                let prefix = callee_label(target);
                if let Some(resolved_prefix) = self.import_resolution.module_aliases.get(&prefix) {
                    return format!("{}.{}", resolved_prefix, name);
                }
                format!("{}.{}", prefix, name)
            }
            _ => callee_label(callee),
        }
    }

    fn resolve_module_prefix(&self, segments: &[String]) -> Option<String> {
        for split in (1..segments.len()).rev() {
            let prefix = segments[..split].join(".");
            if let Some(resolved) = self.import_resolution.module_aliases.get(&prefix) {
                let rest = segments[split..].join(".");
                return Some(format!("{}.{}", resolved, rest));
            }
        }
        None
    }
}

fn mir_type_from_literal(value: &InlineValue) -> VoxType {
    match value {
        InlineValue::Int(_) => VoxType::Int,
        InlineValue::Float(_) => VoxType::Float,
        InlineValue::Bool(_) => VoxType::Bool,
        InlineValue::String(_) => VoxType::String,
        InlineValue::Null => VoxType::opaque_surface("Null"),
        InlineValue::Tuple(_) => VoxType::Tuple(Vec::new()),
        InlineValue::Record(_) => VoxType::Record(Vec::new()),
        InlineValue::Handle(_) => VoxType::opaque_surface("Handle"),
    }
}

fn mir_type_from_binary_op(name: &str, args: &[MirValueId], body: &BodyBuilder) -> VoxType {
    match name {
        "less" | "greater" | "less_equal" | "greater_equal" | "equal" | "not_equal" => {
            VoxType::Bool
        }
        "range" | "range_inclusive" => {
            VoxType::Tuple(vec![VoxType::Int, VoxType::Int, VoxType::Bool])
        }
        _ => {
            if let Some(first) = args.first() {
                body.value_type(*first).cloned().unwrap_or(VoxType::Int)
            } else {
                VoxType::Int
            }
        }
    }
}

fn mir_type_from_unary_op(name: &str, args: &[MirValueId], body: &BodyBuilder) -> VoxType {
    match name {
        "not" => VoxType::Bool,
        _ => {
            if let Some(first) = args.first() {
                body.value_type(*first).cloned().unwrap_or(VoxType::Int)
            } else {
                VoxType::Int
            }
        }
    }
}
