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

use crate::front_end::{
    FrontEndUnit,
    ast::{
        Argument, BinaryOp, BlockExpr, BlockItem, CompilationUnit, CompoundAssignmentOp, Expr,
        ExprKind, FunctionDecl, IntrinsicExpr, LocalValueDecl, Mutability, QualifiedName,
        StringLiteral, StringPart, TopLevelItem, TypeSyntax, UnaryOp, UpdatedPathSegment,
        ValueDecl,
    },
};

pub type MirPassFn = fn(&mut MirModule) -> MirPassReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirPassReport {
    pub name: &'static str,
    pub changed: bool,
    pub summary: String,
}

pub fn lower_and_optimize_mir(
    front_end: &FrontEndUnit,
    optimization: OptimizationLevel,
    rankings: &[vox_core::opt::OptimizationRanking],
    custom_passes: &[MirPassFn],
) -> (MirModule, Vec<String>) {
    let mut module = MirLowerer::new(front_end, optimization, rankings).lower_module();
    let mut summaries = Vec::new();

    for pass in default_passes(optimization)
        .into_iter()
        .chain(custom_passes.iter().copied())
    {
        let report = pass(&mut module);
        if !report.summary.is_empty() {
            summaries.push(report.summary);
        }
    }

    (module, summaries)
}

fn default_passes(optimization: OptimizationLevel) -> Vec<MirPassFn> {
    let mut passes: Vec<MirPassFn> = vec![build_def_use, analyze_lifetimes];
    match optimization {
        OptimizationLevel::NOpt => {}
        OptimizationLevel::IOpt => {
            passes.push(enable_active_value_cache);
        }
        OptimizationLevel::SOpt => {
            passes.push(analyze_projection_demand);
            passes.push(cull_unused_composite_outputs);
            passes.push(mark_copy_on_write);
            passes.push(reuse_value_slots);
            passes.push(seal_module);
        }
    }
    passes
}

struct MirLowerer<'a> {
    front_end: &'a FrontEndUnit,
    optimization: OptimizationLevel,
    rankings: &'a [vox_core::opt::OptimizationRanking],
    next_body: u32,
}

impl<'a> MirLowerer<'a> {
    fn new(
        front_end: &'a FrontEndUnit,
        optimization: OptimizationLevel,
        rankings: &'a [vox_core::opt::OptimizationRanking],
    ) -> Self {
        Self {
            front_end,
            optimization,
            rankings,
            next_body: 0,
        }
    }

    fn lower_module(mut self) -> MirModule {
        let mut bodies = Vec::new();
        if matches!(self.front_end.header.kind, ModuleKind::Script { .. }) {
            bodies.push(self.lower_script_entry(&self.front_end.syntax));
        }

        for item in &self.front_end.syntax.items {
            match item {
                TopLevelItem::Function(function) => bodies.push(self.lower_function(function)),
                TopLevelItem::Value(value)
                    if matches!(self.front_end.header.kind, ModuleKind::Package) =>
                {
                    bodies.push(self.lower_value_initializer(value));
                }
                _ => {}
            }
        }

        MirModule {
            module: self.front_end.header.module.clone(),
            kind: self.front_end.header.kind,
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

        for parameter in &self.front_end.parameters {
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
            BlockItem::For(statement) => {
                let iterable = self.lower_expr(&statement.iterable);
                let iterator = self.emit_op(
                    MirOpKind::Iterator,
                    vec![iterable],
                    Some(statement.span.clone()),
                );
                let item = self.emit_op(
                    MirOpKind::IteratorNext,
                    vec![iterator],
                    Some(statement.span.clone()),
                );
                self.push_scope();
                self.declare_binding(
                    statement.pattern.clone(),
                    MirMutability::Val,
                    None,
                    Some(statement.span.clone()),
                    item,
                    MirVersionSource::Loop,
                );
                self.lower_block_expr(&statement.body);
                self.pop_scope();
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
            BlockItem::Expr(expr) => {
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
                    let args = literal
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            StringPart::Text(_) => None,
                            StringPart::Interpolation(expr) => Some(self.lower_expr(expr)),
                        })
                        .collect();
                    self.emit_op(
                        MirOpKind::Unknown("string_interpolation".to_owned()),
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
                    let mut args = vec![self.lower_expr(&updated.target)];
                    for update in &updated.updates {
                        let value = self.lower_expr(&update.value);
                        args.push(value);
                    }
                    let path = updated
                        .updates
                        .first()
                        .map(|update| update.path.iter().map(mir_path_segment).collect())
                        .unwrap_or_default();
                    self.emit_op(MirOpKind::Updated { path }, args, Some(expr.span.clone()))
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
            ExprKind::Binary { left, op, right }
                if matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce) =>
            {
                let left = self.lower_expr(left);
                let right = self.lower_expr(right);
                self.emit_op(
                    MirOpKind::Unknown(binary_op_name(*op).to_owned()),
                    vec![left, right],
                    Some(expr.span.clone()),
                )
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
            ExprKind::If(if_expr) => {
                let mut args = Vec::new();
                for branch in &if_expr.branches {
                    args.push(self.lower_expr(&branch.condition));
                    self.push_scope();
                    args.push(self.lower_block_expr(&branch.body));
                    self.pop_scope();
                }
                if let Some(else_branch) = &if_expr.else_branch {
                    self.push_scope();
                    args.push(self.lower_block_expr(else_branch));
                    self.pop_scope();
                }
                self.emit_op(
                    MirOpKind::Unknown("if_join".to_owned()),
                    args,
                    Some(expr.span.clone()),
                )
            }
            ExprKind::When(when_expr) => {
                let mut args = vec![self.lower_expr(&when_expr.subject)];
                for arm in &when_expr.arms {
                    self.push_scope();
                    if let Some(binding) = &arm.binding {
                        let subject = args[0];
                        self.declare_binding(
                            binding.clone(),
                            MirMutability::Val,
                            Some(type_syntax_to_vox(&arm.ty)),
                            Some(arm.span.clone()),
                            subject,
                            MirVersionSource::Initializer,
                        );
                    }
                    args.push(self.lower_expr(&arm.body));
                    self.pop_scope();
                }
                if let Some(else_arm) = &when_expr.else_arm {
                    args.push(self.lower_expr(else_arm));
                }
                self.emit_op(
                    MirOpKind::Unknown("when_join".to_owned()),
                    args,
                    Some(expr.span.clone()),
                )
            }
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
            self.emit_op_with_result(
                None,
                MirOpKind::Unknown(format!("assign_missing {name}")),
                vec![value],
                None,
            );
            return;
        };
        if !previous.mutable {
            self.emit_op_with_result(
                None,
                MirOpKind::Unknown(format!("assign_immutable {name}")),
                vec![value],
                None,
            );
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
            MirTerminator::Jump { .. } | MirTerminator::Panic(_) | MirTerminator::Unreachable => {}
        }
        self.current_block().terminator = terminator;
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

fn build_def_use(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        for value in &mut body.values {
            value.uses.clear();
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
                MirTerminator::Branch { condition, .. } => {
                    if let Some(value) = body.values.iter_mut().find(|entry| entry.id == *condition)
                    {
                        value.uses.push(MirUse {
                            block: block.id,
                            op_index: None,
                            kind: MirUseKind::Condition,
                        });
                    }
                }
                MirTerminator::Jump { .. }
                | MirTerminator::Panic(_)
                | MirTerminator::Unreachable => {}
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

fn analyze_lifetimes(module: &mut MirModule) -> MirPassReport {
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

fn enable_active_value_cache(module: &mut MirModule) -> MirPassReport {
    for body in &mut module.bodies {
        body.analyses.active_value_cache_enabled = true;
    }
    MirPassReport {
        name: "active-cache",
        changed: true,
        summary: "active pure value caching enabled for interactive MIR".to_owned(),
    }
}

fn analyze_projection_demand(module: &mut MirModule) -> MirPassReport {
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

fn cull_unused_composite_outputs(module: &mut MirModule) -> MirPassReport {
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

fn mark_copy_on_write(module: &mut MirModule) -> MirPassReport {
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

fn reuse_value_slots(module: &mut MirModule) -> MirPassReport {
    let mut changed = 0;
    for body in &mut module.bodies {
        let mut next_slot = 0_u32;
        let mut reusable = Vec::<u32>::new();
        let mut by_def = body.values.clone();
        by_def.sort_by_key(|value| value.lifetime.first);
        for value in by_def {
            let slot = reusable.pop().unwrap_or_else(|| {
                let slot = next_slot;
                next_slot += 1;
                slot
            });
            body.analyses.value_slots.insert(value.id, slot);
            if let Some(real) = body.values.iter_mut().find(|entry| entry.id == value.id) {
                if real.lifetime.reusable_after_last_use {
                    reusable.push(slot);
                    real.storage = MirStorage::Reuse(value.id);
                    body.analyses.reused_slots += 1;
                    changed += 1;
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

fn seal_module(module: &mut MirModule) -> MirPassReport {
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
