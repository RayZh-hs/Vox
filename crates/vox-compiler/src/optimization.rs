use vox_core::{
    opt::{OptimizationLevel, OptimizationRank, OptimizationRanking, OptimizationSubject},
    source::ModuleKind,
};

use crate::front_end::{
    FrontEndUnit,
    ast::{
        Argument, AssignmentStatement, BlockExpr, BlockItem, CompilationUnit,
        CompoundAssignmentStatement, Expr, ExprKind, ForStatement, FunctionDecl, IfExpr,
        IntrinsicExpr, RangeExpr, RecordFieldInit, ReturnStatement, StringLiteral, StringPart,
        TopLevelItem, ValueDecl, WhenExpr,
    },
};

pub fn derive_rankings(
    front_end: &FrontEndUnit,
    level: OptimizationLevel,
) -> Vec<OptimizationRanking> {
    let mut rankings = Vec::new();
    rankings.push(OptimizationRanking {
        subject: OptimizationSubject::Module,
        rank: rank_module(&front_end.syntax, level),
    });

    for function in front_end.syntax.items.iter().filter_map(|item| match item {
        TopLevelItem::Function(function) => Some(function),
        _ => None,
    }) {
        rankings.push(OptimizationRanking {
            subject: OptimizationSubject::Function(function.name.clone()),
            rank: rank_function(function, level),
        });
    }

    rankings
}

fn rank_module(unit: &CompilationUnit, level: OptimizationLevel) -> OptimizationRank {
    let mut features = RankFeatures::default();
    for item in &unit.items {
        if let TopLevelItem::Value(value) = item {
            visit_value(value, &mut features);
        }
    }
    if let Some(result) = &unit.result {
        visit_expr(result, &mut features);
    }

    let evil = matches!(unit.header.kind, ModuleKind::Script { evil: true });
    rank_from_features(level, evil, features)
}

fn rank_function(function: &FunctionDecl, level: OptimizationLevel) -> OptimizationRank {
    let mut features = RankFeatures::default();
    visit_expr(&function.body, &mut features);
    rank_from_features(level, function.evil, features)
}

fn rank_from_features(
    level: OptimizationLevel,
    evil: bool,
    features: RankFeatures,
) -> OptimizationRank {
    match level {
        OptimizationLevel::NOpt => OptimizationRank::Baseline,
        OptimizationLevel::IOpt => OptimizationRank::Interactive,
        OptimizationLevel::SOpt => {
            if evil {
                return OptimizationRank::SealedOwnership;
            }

            if features.has_composite_producer {
                return OptimizationRank::SealedMaterialization;
            }

            if features.has_projection {
                return OptimizationRank::SealedDemand;
            }

            OptimizationRank::SealedOwnership
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RankFeatures {
    has_projection: bool,
    has_composite_producer: bool,
}

fn visit_value(value: &ValueDecl, features: &mut RankFeatures) {
    visit_expr(&value.initializer, features);
}

fn visit_expr(expr: &Expr, features: &mut RankFeatures) {
    match &expr.kind {
        ExprKind::Integer(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Name(_) => {}
        ExprKind::String(literal) => visit_string_literal(literal, features),
        ExprKind::List(items) | ExprKind::Tuple(items) => {
            if matches!(expr.kind, ExprKind::Tuple(_)) {
                features.has_composite_producer = true;
            }
            for item in items {
                visit_expr(item, features);
            }
        }
        ExprKind::Record(fields) => {
            features.has_composite_producer = true;
            for field in fields {
                visit_record_field(field, features);
            }
        }
        ExprKind::Call { callee, arguments } => {
            visit_expr(callee, features);
            for argument in arguments {
                visit_argument(argument, features);
            }
        }
        ExprKind::Intrinsic(intrinsic) => visit_intrinsic(intrinsic, features),
        ExprKind::Index { target, index } => {
            visit_expr(target, features);
            visit_expr(index, features);
        }
        ExprKind::Field { target, .. } | ExprKind::SafeField { target, .. } => {
            features.has_projection = true;
            visit_expr(target, features);
        }
        ExprKind::NonNull { target } => visit_expr(target, features),
        ExprKind::ReceiverCall {
            receiver,
            arguments,
            ..
        } => {
            visit_expr(receiver, features);
            for argument in arguments {
                visit_argument(argument, features);
            }
        }
        ExprKind::Unary { expr, .. } => visit_expr(expr, features),
        ExprKind::Binary { left, right, .. } => {
            visit_expr(left, features);
            visit_expr(right, features);
        }
        ExprKind::Range(range) => visit_range(range, features),
        ExprKind::If(if_expr) => visit_if(if_expr, features),
        ExprKind::When(when_expr) => visit_when(when_expr, features),
        ExprKind::Lambda(lambda) => visit_expr(&lambda.body, features),
        ExprKind::Block(block) => visit_block(block, features),
    }
}

fn visit_intrinsic(intrinsic: &IntrinsicExpr, features: &mut RankFeatures) {
    match intrinsic {
        IntrinsicExpr::Updated(updated) => {
            features.has_projection = true;
            features.has_composite_producer = true;
            visit_expr(&updated.target, features);
            for update in &updated.updates {
                visit_expr(&update.value, features);
            }
        }
        IntrinsicExpr::Econ(econ) => visit_block(&econ.body, features),
    }
}

fn visit_string_literal(literal: &StringLiteral, features: &mut RankFeatures) {
    for part in &literal.parts {
        if let StringPart::Interpolation(expr) = part {
            visit_expr(expr, features);
        }
    }
}

fn visit_record_field(field: &RecordFieldInit, features: &mut RankFeatures) {
    visit_expr(&field.value, features);
}

fn visit_argument(argument: &Argument, features: &mut RankFeatures) {
    match argument {
        Argument::Positional(expr) => visit_expr(expr, features),
        Argument::Named { value, .. } => visit_expr(value, features),
    }
}

fn visit_range(range: &RangeExpr, features: &mut RankFeatures) {
    if let Some(start) = &range.start {
        visit_expr(start, features);
    }
    if let Some(end) = &range.end {
        visit_expr(end, features);
    }
}

fn visit_if(if_expr: &IfExpr, features: &mut RankFeatures) {
    for branch in &if_expr.branches {
        visit_expr(&branch.condition, features);
        visit_block(&branch.body, features);
    }
    if let Some(else_branch) = &if_expr.else_branch {
        visit_block(else_branch, features);
    }
}

fn visit_when(when_expr: &WhenExpr, features: &mut RankFeatures) {
    visit_expr(&when_expr.subject, features);
    for arm in &when_expr.arms {
        visit_expr(&arm.body, features);
    }
    if let Some(else_arm) = &when_expr.else_arm {
        visit_expr(else_arm, features);
    }
}

fn visit_block(block: &BlockExpr, features: &mut RankFeatures) {
    for item in &block.items {
        match item {
            BlockItem::LocalValue(value) => visit_expr(&value.initializer, features),
            BlockItem::Assignment(assignment) => visit_assignment(assignment, features),
            BlockItem::CompoundAssignment(assignment) => {
                visit_compound_assignment(assignment, features)
            }
            BlockItem::For(statement) => visit_for(statement, features),
            BlockItem::Return(statement) => visit_return(statement, features),
            BlockItem::Panic(statement) => visit_string_literal(&statement.message, features),
            BlockItem::Expr(expr) => visit_expr(expr, features),
        }
    }
    if let Some(trailing) = &block.trailing {
        visit_expr(trailing, features);
    }
}

fn visit_assignment(assignment: &AssignmentStatement, features: &mut RankFeatures) {
    visit_expr(&assignment.value, features);
}

fn visit_compound_assignment(
    assignment: &CompoundAssignmentStatement,
    features: &mut RankFeatures,
) {
    visit_expr(&assignment.value, features);
}

fn visit_for(statement: &ForStatement, features: &mut RankFeatures) {
    visit_expr(&statement.iterable, features);
    visit_block(&statement.body, features);
}

fn visit_return(statement: &ReturnStatement, features: &mut RankFeatures) {
    if let Some(value) = &statement.value {
        visit_expr(value, features);
    }
}
