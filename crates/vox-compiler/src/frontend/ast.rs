use std::fmt::{self, Write};

use vox_core::{
    diagnostics::TextSpan,
    host::ParameterSpec,
    source::{ModuleKind, SurfaceHeader},
    types::VoxType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    Val,
    Var,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    pub segments: Vec<String>,
    pub span: TextSpan,
}

impl QualifiedName {
    pub fn to_source_string(&self) -> String {
        self.segments.join(".")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceParameter {
    pub name: String,
    pub ty: VoxType,
    pub has_default: bool,
}

impl SurfaceParameter {
    pub fn into_spec(self) -> ParameterSpec {
        ParameterSpec {
            name: self.name,
            ty: self.ty,
            has_default: self.has_default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendUnit {
    pub header: SurfaceHeader,
    pub parameters: Vec<SurfaceParameter>,
    pub syntax: CompilationUnit,
}

impl FrontendUnit {
    pub fn from_syntax(syntax: CompilationUnit) -> Self {
        let parameters = syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Param(param) => Some(SurfaceParameter {
                    name: param.name.clone(),
                    ty: VoxType::opaque_surface(param.ty.to_source_string()),
                    has_default: param.default.is_some(),
                }),
                _ => None,
            })
            .collect();

        Self {
            header: syntax.header.clone(),
            parameters,
            syntax,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationUnit {
    pub header: SurfaceHeader,
    pub items: Vec<TopLevelItem>,
    pub result: Option<Expr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopLevelItem {
    Import(ImportDecl),
    Param(ParamDecl),
    Value(ValueDecl),
    Function(FunctionDecl),
    Statement(BlockItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    pub docs: Vec<String>,
    pub visibility: Visibility,
    pub module: QualifiedName,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDecl {
    pub docs: Vec<String>,
    pub name: String,
    pub ty: TypeSyntax,
    pub default: Option<Expr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueDecl {
    pub docs: Vec<String>,
    pub visibility: Visibility,
    pub mutability: Mutability,
    pub name: String,
    pub ty: Option<TypeSyntax>,
    pub initializer: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalValueDecl {
    pub mutability: Mutability,
    pub name: String,
    pub ty: Option<TypeSyntax>,
    pub initializer: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDecl {
    pub docs: Vec<String>,
    pub visibility: Visibility,
    pub evil: bool,
    pub name: String,
    pub generic_parameters: Vec<GenericParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeSyntax>,
    pub body: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParameter {
    pub name: String,
    pub bound: String,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub ty: TypeSyntax,
    pub default: Option<Expr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSyntax {
    pub kind: TypeKind,
    pub span: TextSpan,
}

impl TypeSyntax {
    pub fn to_source_string(&self) -> String {
        let mut out = String::new();
        self.write_source(&mut out)
            .expect("writing to string should not fail");
        out
    }

    fn write_source(&self, out: &mut String) -> fmt::Result {
        match &self.kind {
            TypeKind::Function { parameters, result } => {
                out.push('(');
                for (index, parameter) in parameters.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    parameter.write_source(out)?;
                }
                out.push(')');
                out.push_str(" -> ");
                result.write_source(out)
            }
            TypeKind::Nullable(inner) => {
                inner.write_source(out)?;
                out.push('?');
                Ok(())
            }
            TypeKind::Named { name, arguments } => {
                out.push_str(&name.to_source_string());
                if !arguments.is_empty() {
                    out.push('[');
                    for (index, argument) in arguments.iter().enumerate() {
                        if index > 0 {
                            out.push_str(", ");
                        }
                        argument.write_source(out)?;
                    }
                    out.push(']');
                }
                Ok(())
            }
            TypeKind::Dyn(name) => {
                out.push_str("dyn ");
                out.push_str(&name.to_source_string());
                Ok(())
            }
            TypeKind::Grouped(inner) => {
                out.push('(');
                inner.write_source(out)?;
                out.push(')');
                Ok(())
            }
            TypeKind::Tuple(items) => {
                out.push('(');
                match items.as_slice() {
                    [] => {}
                    [single] => {
                        single.write_source(out)?;
                        out.push(',');
                    }
                    _ => {
                        for (index, item) in items.iter().enumerate() {
                            if index > 0 {
                                out.push_str(", ");
                            }
                            item.write_source(out)?;
                        }
                    }
                }
                out.push(')');
                Ok(())
            }
            TypeKind::Record(fields) => {
                out.push('{');
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write!(out, "{}: ", field.name)?;
                    field.ty.write_source(out)?;
                }
                out.push('}');
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Function {
        parameters: Vec<TypeSyntax>,
        result: Box<TypeSyntax>,
    },
    Nullable(Box<TypeSyntax>),
    Named {
        name: QualifiedName,
        arguments: Vec<TypeSyntax>,
    },
    Dyn(QualifiedName),
    Grouped(Box<TypeSyntax>),
    Tuple(Vec<TypeSyntax>),
    Record(Vec<RecordTypeField>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordTypeField {
    pub name: String,
    pub ty: TypeSyntax,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    Integer(String),
    Float(String),
    Bool(bool),
    Null,
    String(StringLiteral),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Record(Vec<RecordFieldInit>),
    Name(QualifiedName),
    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
    },
    Intrinsic(IntrinsicExpr),
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Field {
        target: Box<Expr>,
        name: String,
    },
    SafeField {
        target: Box<Expr>,
        name: String,
    },
    NonNull {
        target: Box<Expr>,
    },
    ReceiverCall {
        receiver: Box<Expr>,
        callee: QualifiedName,
        arguments: Vec<Argument>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Range(RangeExpr),
    If(IfExpr),
    When(WhenExpr),
    For(ForExpr),
    Lambda(LambdaExpr),
    Block(BlockExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntrinsicExpr {
    Updated(UpdatedIntrinsic),
    Econ(EconIntrinsic),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedIntrinsic {
    pub target: Box<Expr>,
    pub updates: Vec<UpdatedArg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EconIntrinsic {
    pub ty: TypeSyntax,
    pub body: BlockExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringLiteral {
    pub parts: Vec<StringPart>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringPart {
    Text(String),
    Interpolation(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFieldInit {
    pub name: String,
    pub ty: Option<TypeSyntax>,
    pub value: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argument {
    Positional(Expr),
    Named {
        name: String,
        value: Expr,
        span: TextSpan,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedArg {
    pub path: Vec<UpdatedPathSegment>,
    pub value: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatedPathSegment {
    Field(String),
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    And,
    Or,
    Coalesce,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeExpr {
    pub start: Option<Box<Expr>>,
    pub end: Option<Box<Expr>>,
    pub inclusive_end: bool,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpr {
    pub branches: Vec<IfBranch>,
    pub else_branch: Option<BlockExpr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfBranch {
    pub condition: Expr,
    pub body: BlockExpr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenExpr {
    pub subject: Box<Expr>,
    pub arms: Vec<WhenArm>,
    pub else_arm: Option<Box<Expr>>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenArm {
    pub ty: TypeSyntax,
    pub binding: Option<String>,
    pub body: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaExpr {
    pub parameters: Vec<LambdaParameter>,
    pub body: Box<Expr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaParameter {
    pub name: String,
    pub ty: Option<TypeSyntax>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockExpr {
    pub items: Vec<BlockItem>,
    pub trailing: Option<Box<Expr>>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockItem {
    LocalValue(LocalValueDecl),
    Assignment(AssignmentStatement),
    CompoundAssignment(CompoundAssignmentStatement),
    Return(ReturnStatement),
    Panic(PanicStatement),
    BlockStatement(Expr),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentStatement {
    pub name: String,
    pub value: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundAssignmentOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompoundAssignmentStatement {
    pub name: String,
    pub op: CompoundAssignmentOp,
    pub value: Expr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForExpr {
    pub pattern: String,
    pub iterable: Box<Expr>,
    pub body: BlockExpr,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnStatement {
    pub value: Option<Expr>,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanicStatement {
    pub message: StringLiteral,
    pub span: TextSpan,
}

impl CompilationUnit {
    pub fn module_kind(&self) -> ModuleKind {
        self.header.kind
    }
}
