use std::collections::BTreeSet;

use vox_core::{
    diagnostics::{Diagnostic, DiagnosticBag, TextSpan},
    source::{ModuleKind, ModulePath, SurfaceHeader},
};

use crate::frontend::{
    ast::{
        Argument, AssignmentStatement, BinaryOp, BlockExpr, BlockItem, BreakStatement,
        CompilationUnit, CompoundAssignmentOp, CompoundAssignmentStatement, ContinueStatement,
        EconIntrinsic, Expr, ExprKind, ForExpr, ForHeader, FrontendUnit, FunctionDecl,
        GenericParameter, IfBranch, IfExpr, ImportDecl, ImportItem, IntrinsicExpr, LambdaExpr,
        LambdaParameter, LocalValueDecl, Mutability, PanicStatement, ParamDecl, Parameter,
        QualifiedName, RangeExpr, RecordFieldInit, RecordTypeField, ReturnStatement,
        StringLiteral, StringPart, TopLevelItem, TypeKind, TypeSyntax, UnaryOp, UpdatedArg,
        UpdatedIntrinsic, UpdatedPathSegment, ValueDecl, Visibility, WhenArm, WhenExpr,
    },
    lexer::{LexedStringPart, Lexer, Token, TokenKind},
};

pub struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntrinsicName {
    Updated,
    Econ,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, index: 0 }
    }

    pub fn parse_unit(&mut self) -> Result<FrontendUnit, DiagnosticBag> {
        let package_docs = self.take_doc_comments();
        let header = self.parse_header()?;
        self.expect_simple(TokenKind::Semicolon, "expected `;` after file header")?;

        let mut items = Vec::new();
        loop {
            let docs = self.take_doc_comments();
            if self.at(TokenKind::Eof) {
                break;
            }

            if let Some(item) = self.parse_top_level_item(docs.clone(), header.kind)? {
                items.push(item);
                continue;
            }

            if matches!(header.kind, ModuleKind::Package) {
                return self.error_here("package files may contain only top-level declarations");
            }

            if let Some(statement) = self.parse_script_statement()? {
                items.push(TopLevelItem::Statement(statement));
                continue;
            }

            let result = self.parse_expr()?;
            self.expect_simple(
                TokenKind::Eof,
                "unexpected tokens after top-level expression",
            )?;

            let span = TextSpan::new(header.span.start, result.span.end);
            let syntax = CompilationUnit {
                header: header.clone(),
                items,
                result: Some(result),
                span,
            };
            if matches!(header.kind, ModuleKind::Package) {
                self.validate_package_items(&syntax)?;
            }
            return Ok(FrontendUnit::from_syntax_with_docs(syntax, package_docs.clone()));
        }

        let end = self.current().span.end;
        let syntax = CompilationUnit {
            header: header.clone(),
            items,
            result: None,
            span: TextSpan::new(header.span.start, end),
        };
        if matches!(header.kind, ModuleKind::Package) {
            self.validate_package_items(&syntax)?;
        }
        Ok(FrontendUnit::from_syntax_with_docs(syntax, package_docs.clone()))
    }

    fn parse_header(&mut self) -> Result<SurfaceHeader, DiagnosticBag> {
        let start = self.current().span.start;
        let kind = if self.consume(TokenKind::Package) {
            ModuleKind::Package
        } else if self.consume(TokenKind::Evil) {
            self.expect_simple(
                TokenKind::Script,
                "expected `script` after `evil` in file header",
            )?;
            ModuleKind::Script { evil: true }
        } else if self.consume(TokenKind::Script) {
            ModuleKind::Script { evil: false }
        } else {
            return self.error_here("source must start with `package`, `script`, or `evil script`");
        };

        let module = self.parse_module_path()?;
        let end = self.previous().span.end;
        Ok(SurfaceHeader {
            kind,
            module,
            span: TextSpan::new(start, end),
        })
    }

    fn parse_top_level_item(
        &mut self,
        docs: Vec<String>,
        module_kind: ModuleKind,
    ) -> Result<Option<TopLevelItem>, DiagnosticBag> {
        let visibility = self.parse_visibility();
        let visibility = visibility.unwrap_or(Visibility::Private);

        if self.at(TokenKind::Import) {
            let import = self.parse_import_decl(docs, visibility)?;
            return Ok(Some(TopLevelItem::Import(import)));
        }

        if self.at(TokenKind::Param) {
            if visibility != Visibility::Private {
                return self.error_here("`param` does not accept a visibility modifier");
            }
            if !matches!(module_kind, ModuleKind::Script { .. }) {
                return self.error_here("`param` is only valid in scripts");
            }
            let param = self.parse_param_decl(docs)?;
            return Ok(Some(TopLevelItem::Param(param)));
        }

        if self.at(TokenKind::Val) || self.at(TokenKind::Var) {
            if matches!(module_kind, ModuleKind::Package) && self.at(TokenKind::Var) {
                return self.error_here("package top-level values must use `val`");
            }
            let value = self.parse_value_decl(docs, visibility)?;
            return Ok(Some(TopLevelItem::Value(value)));
        }

        if self.at(TokenKind::Evil) || self.at(TokenKind::Fun) {
            let function = self.parse_function_decl(docs, visibility)?;
            return Ok(Some(TopLevelItem::Function(function)));
        }

        if self.index > 0
            && matches!(
                self.tokens[self.index - 1].kind,
                TokenKind::Public | TokenKind::Private
            )
        {
            return self.error_here(
                "visibility modifiers apply only to import, value, and function declarations",
            );
        }

        Ok(None)
    }

    fn parse_script_statement(&mut self) -> Result<Option<BlockItem>, DiagnosticBag> {
        if let Some(statement) = self.try_parse_assignment_statement()? {
            return Ok(Some(statement));
        }

        if self.at(TokenKind::Panic) {
            return Ok(Some(BlockItem::Panic(self.parse_panic_statement()?)));
        }

        if self.at(TokenKind::Return) {
            return self.error_here("`return` may only be used inside a function body");
        }

        if self.at(TokenKind::Break) {
            return self.error_here("`break` may only be used inside a `for` loop");
        }

        if self.at(TokenKind::Continue) {
            return self.error_here("`continue` may only be used inside a `for` loop");
        }

        if self.consume(TokenKind::Semicolon) {
            return Ok(None);
        }

        let checkpoint = self.index;
        let expr = self.parse_expr()?;
        if self.consume(TokenKind::Semicolon) {
            return Ok(Some(BlockItem::Expr(expr)));
        }
        self.index = checkpoint;
        Ok(None)
    }

    fn validate_package_items(&self, unit: &CompilationUnit) -> Result<(), DiagnosticBag> {
        let mut values = BTreeSet::new();
        let mut functions = BTreeSet::new();
        for item in &unit.items {
            match item {
                TopLevelItem::Value(value) => {
                    if matches!(value.mutability, Mutability::Var) {
                        return Err(DiagnosticBag::from(vec![
                            Diagnostic::error("package top-level values must use `val`")
                                .with_span(value.span.clone()),
                        ]));
                    }
                    if !values.insert(value.name.clone()) {
                        return Err(DiagnosticBag::from(vec![
                            Diagnostic::error(format!(
                                "package value `{}` is already declared",
                                value.name
                            ))
                            .with_span(value.span.clone()),
                        ]));
                    }
                    if functions.contains(&value.name) {
                        return Err(DiagnosticBag::from(vec![
                            Diagnostic::error(format!(
                                "package value `{}` conflicts with a function of the same name",
                                value.name
                            ))
                            .with_span(value.span.clone()),
                        ]));
                    }
                }
                TopLevelItem::Function(function) => {
                    if !functions.insert(function.name.clone()) {
                        return Err(DiagnosticBag::from(vec![
                            Diagnostic::error(format!(
                                "package function `{}` has a colliding declaration",
                                function.name
                            ))
                            .with_span(function.span.clone()),
                        ]));
                    }
                    if values.contains(&function.name) {
                        return Err(DiagnosticBag::from(vec![
                            Diagnostic::error(format!(
                                "package function `{}` conflicts with a value of the same name",
                                function.name
                            ))
                            .with_span(function.span.clone()),
                        ]));
                    }
                }
                TopLevelItem::Import(_) => {}
                TopLevelItem::Param(_) | TopLevelItem::Statement(_) => {
                    return Err(DiagnosticBag::from(vec![
                        Diagnostic::error(
                            "package files may contain only imports, values, and functions",
                        )
                        .with_span(unit.span.clone()),
                    ]));
                }
            }
        }
        Ok(())
    }

    fn parse_import_decl(
        &mut self,
        docs: Vec<String>,
        visibility: Visibility,
    ) -> Result<ImportDecl, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Import, "expected `import`")?;
        let module = self.parse_import_module_path()?;

        let alias = if self.consume(TokenKind::As) {
            let (alias_name, _) = self.expect_identifier("expected alias name after `as`")?;
            Some(alias_name)
        } else {
            None
        };

        let items = if self.consume(TokenKind::Dot) {
            self.expect_simple(
                TokenKind::LParen,
                "expected `(` after `.` for selective import list",
            )?;
            let mut items = Vec::new();
            while !self.at(TokenKind::RParen) {
                let item_start = self.current().span.start;
                let (name, _) = self.expect_identifier("expected imported name")?;
                let item_alias = if self.consume(TokenKind::As) {
                    let (alias_name, _) =
                        self.expect_identifier("expected alias name after `as`")?;
                    Some(alias_name)
                } else {
                    None
                };
                items.push(ImportItem {
                    name,
                    alias: item_alias,
                    span: TextSpan::new(item_start, self.previous().span.end),
                });
                if !self.consume(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RParen) {
                    break;
                }
            }
            self.expect_simple(TokenKind::RParen, "expected `)` after selective import list")?;
            Some(items)
        } else {
            None
        };

        self.expect_simple(
            TokenKind::Semicolon,
            "expected `;` after import declaration",
        )?;
        Ok(ImportDecl {
            docs,
            visibility,
            module,
            alias,
            items,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_import_module_path(&mut self) -> Result<QualifiedName, DiagnosticBag> {
        let start = self.current().span.start;
        let (first, _) = self.expect_identifier("expected module path")?;
        let mut segments = vec![first];
        loop {
            if !self.at(TokenKind::Dot) {
                break;
            }
            if self.peek_kind(1) == Some(TokenKind::LParen) {
                break;
            }
            self.consume(TokenKind::Dot);
            let (segment, _) = self.expect_identifier("expected identifier after `.`")?;
            segments.push(segment);
        }
        let end = self.previous().span.end;
        Ok(QualifiedName {
            segments,
            span: TextSpan::new(start, end),
        })
    }

    fn parse_param_decl(&mut self, docs: Vec<String>) -> Result<ParamDecl, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Param, "expected `param`")?;
        let (name, _) = self.expect_identifier("expected parameter name")?;
        self.expect_simple(TokenKind::Colon, "expected `:` after parameter name")?;
        let ty = self.parse_type()?;
        let default = if self.consume(TokenKind::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_simple(
            TokenKind::Semicolon,
            "expected `;` after parameter declaration",
        )?;
        Ok(ParamDecl {
            docs,
            name,
            ty,
            default,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_value_decl(
        &mut self,
        docs: Vec<String>,
        visibility: Visibility,
    ) -> Result<ValueDecl, DiagnosticBag> {
        let start = self.current().span.start;
        let mutability = self.parse_mutability()?;
        let (name, _) = self.expect_identifier("expected binding name")?;
        let ty = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Assign, "expected `=` in value declaration")?;
        let initializer = self.parse_expr()?;
        self.expect_simple(TokenKind::Semicolon, "expected `;` after value declaration")?;
        Ok(ValueDecl {
            docs,
            visibility,
            mutability,
            name,
            ty,
            initializer,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_local_value_decl(&mut self) -> Result<LocalValueDecl, DiagnosticBag> {
        let start = self.current().span.start;
        let mutability = self.parse_mutability()?;
        let (name, _) = self.expect_identifier("expected binding name")?;
        let ty = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Assign, "expected `=` in value declaration")?;
        let initializer = self.parse_expr()?;
        self.expect_simple(TokenKind::Semicolon, "expected `;` after local declaration")?;
        Ok(LocalValueDecl {
            mutability,
            name,
            ty,
            initializer,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_local_value_decl_for_header(
        &mut self,
    ) -> Result<LocalValueDecl, DiagnosticBag> {
        let start = self.current().span.start;
        let mutability = self.parse_mutability()?;
        let (name, _) = self.expect_identifier("expected binding name")?;
        let ty = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Assign, "expected `=` in value declaration")?;
        let initializer = self.parse_expr()?;
        Ok(LocalValueDecl {
            mutability,
            name,
            ty,
            initializer,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_function_decl(
        &mut self,
        docs: Vec<String>,
        visibility: Visibility,
    ) -> Result<FunctionDecl, DiagnosticBag> {
        let start = self.current().span.start;
        let evil = self.consume(TokenKind::Evil);
        self.expect_simple(TokenKind::Fun, "expected `fun`")?;
        let (name, _) = self.expect_identifier("expected function name")?;
        let generic_parameters = if self.at(TokenKind::LBracket) {
            self.parse_generic_parameter_clause()?
        } else {
            Vec::new()
        };
        self.expect_simple(TokenKind::LParen, "expected `(` before parameter list")?;
        let parameters = self.parse_parameter_list()?;
        self.expect_simple(TokenKind::RParen, "expected `)` after parameter list")?;
        let return_type = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if self.consume(TokenKind::Assign) {
            let expr = self.parse_expr()?;
            self.expect_simple(
                TokenKind::Semicolon,
                "expected `;` after expression-bodied function",
            )?;
            expr
        } else {
            self.parse_block_expr_required()?
        };
        let end = self.previous().span.end;

        Ok(FunctionDecl {
            docs,
            visibility,
            evil,
            name,
            generic_parameters,
            parameters,
            return_type,
            span: TextSpan::new(start, end),
            body,
        })
    }

    fn parse_generic_parameter_clause(&mut self) -> Result<Vec<GenericParameter>, DiagnosticBag> {
        self.expect_simple(TokenKind::LBracket, "expected `[`")?;
        let mut parameters = Vec::new();
        while !self.at(TokenKind::RBracket) {
            let start = self.current().span.start;
            let (name, _) = self.expect_identifier("expected generic parameter name")?;
            self.expect_simple(TokenKind::Colon, "expected `:` in generic parameter")?;
            let (bound, bound_span) = self.expect_identifier("expected trait bound name")?;
            parameters.push(GenericParameter {
                name,
                bound,
                span: TextSpan::new(start, bound_span.end),
            });
            if !self.consume(TokenKind::Comma) {
                break;
            }
            if self.at(TokenKind::RBracket) {
                break;
            }
        }
        self.expect_simple(TokenKind::RBracket, "expected `]` after generic parameters")?;
        Ok(parameters)
    }

    fn parse_parameter_list(&mut self) -> Result<Vec<Parameter>, DiagnosticBag> {
        let mut parameters = Vec::new();
        while !self.at(TokenKind::RParen) {
            let start = self.current().span.start;
            let (name, _) = self.expect_identifier("expected parameter name")?;
            self.expect_simple(TokenKind::Colon, "expected `:` after parameter name")?;
            let ty = self.parse_type()?;
            let default = if self.consume(TokenKind::Assign) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            let end = default
                .as_ref()
                .map(|expr| expr.span.end)
                .unwrap_or(ty.span.end);
            parameters.push(Parameter {
                name,
                ty,
                default,
                span: TextSpan::new(start, end),
            });
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        Ok(parameters)
    }

    fn parse_type(&mut self) -> Result<TypeSyntax, DiagnosticBag> {
        let checkpoint = self.index;
        if self.consume(TokenKind::LParen) {
            let start = self.tokens[checkpoint].span.start;
            let mut parameters = Vec::new();
            if !self.at(TokenKind::RParen) {
                loop {
                    parameters.push(self.parse_type()?);
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
            }
            self.expect_simple(TokenKind::RParen, "expected `)` in function type")?;
            if self.consume(TokenKind::Arrow) {
                let result = self.parse_type()?;
                return Ok(TypeSyntax {
                    kind: TypeKind::Function {
                        parameters,
                        result: Box::new(result.clone()),
                    },
                    span: TextSpan::new(start, result.span.end),
                });
            }
            self.index = checkpoint;
        }

        let start = self.current().span.start;
        let base = self.parse_primary_type()?;
        if self.consume(TokenKind::Question) {
            let end = self.previous().span.end;
            Ok(TypeSyntax {
                kind: TypeKind::Nullable(Box::new(base)),
                span: TextSpan::new(start, end),
            })
        } else {
            Ok(base)
        }
    }

    fn parse_primary_type(&mut self) -> Result<TypeSyntax, DiagnosticBag> {
        let start = self.current().span.start;
        if self.consume(TokenKind::Dyn) {
            let name = self.parse_qualified_name()?;
            return Ok(TypeSyntax {
                kind: TypeKind::Dyn(name.clone()),
                span: TextSpan::new(start, name.span.end),
            });
        }

        if self.consume(TokenKind::LBrace) {
            let mut fields = Vec::new();
            while !self.at(TokenKind::RBrace) {
                let field_start = self.current().span.start;
                let (name, _) = self.expect_identifier("expected record field name")?;
                self.expect_simple(TokenKind::Colon, "expected `:` in record type")?;
                let ty = self.parse_type()?;
                fields.push(RecordTypeField {
                    name,
                    span: TextSpan::new(field_start, ty.span.end),
                    ty,
                });
                if !self.consume(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RBrace) {
                    break;
                }
            }
            self.expect_simple(TokenKind::RBrace, "expected `}` after record type")?;
            return Ok(TypeSyntax {
                kind: TypeKind::Record(fields),
                span: TextSpan::new(start, self.previous().span.end),
            });
        }

        if self.consume(TokenKind::LParen) {
            if self.consume(TokenKind::RParen) {
                return Ok(TypeSyntax {
                    kind: TypeKind::Tuple(Vec::new()),
                    span: TextSpan::new(start, self.previous().span.end),
                });
            }

            let first = self.parse_type()?;
            if self.consume(TokenKind::Comma) {
                let mut items = vec![first];
                while !self.at(TokenKind::RParen) {
                    items.push(self.parse_type()?);
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
                self.expect_simple(TokenKind::RParen, "expected `)` after tuple type")?;
                return Ok(TypeSyntax {
                    kind: TypeKind::Tuple(items),
                    span: TextSpan::new(start, self.previous().span.end),
                });
            }

            self.expect_simple(TokenKind::RParen, "expected `)` after grouped type")?;
            return Ok(TypeSyntax {
                kind: TypeKind::Grouped(Box::new(first.clone())),
                span: TextSpan::new(start, self.previous().span.end),
            });
        }

        let name = self.parse_qualified_name()?;
        let end = if self.consume(TokenKind::LBracket) {
            let mut arguments = Vec::new();
            while !self.at(TokenKind::RBracket) {
                arguments.push(self.parse_type()?);
                if !self.consume(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RBracket) {
                    break;
                }
            }
            self.expect_simple(TokenKind::RBracket, "expected `]` after type arguments")?;
            let end = self.previous().span.end;
            return Ok(TypeSyntax {
                kind: TypeKind::Named { name, arguments },
                span: TextSpan::new(start, end),
            });
        } else {
            name.span.end
        };

        Ok(TypeSyntax {
            kind: TypeKind::Named {
                name,
                arguments: Vec::new(),
            },
            span: TextSpan::new(start, end),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let checkpoint = self.index;
        if let Some(lambda) = self.try_parse_lambda()? {
            return Ok(lambda);
        }
        self.index = checkpoint;
        self.parse_coalesce_expr()
    }

    fn try_parse_lambda(&mut self) -> Result<Option<Expr>, DiagnosticBag> {
        let checkpoint = self.index;
        let parameters = if let Some(single) = self.try_parse_single_lambda_parameter() {
            vec![single]
        } else if self.consume(TokenKind::LParen) {
            let start = self.tokens[checkpoint].span.start;
            let mut parameters = Vec::new();
            if !self.at(TokenKind::RParen) {
                loop {
                    let TokenKind::Identifier(name) = self.current().kind.clone() else {
                        self.index = checkpoint;
                        return Ok(None);
                    };
                    let param_start = self.current().span.start;
                    self.index += 1;
                    let ty = if self.consume(TokenKind::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    let end = ty
                        .as_ref()
                        .map(|ty| ty.span.end)
                        .unwrap_or(self.previous().span.end);
                    parameters.push(LambdaParameter {
                        name,
                        ty,
                        span: TextSpan::new(param_start, end),
                    });
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
            }
            self.expect_simple(TokenKind::RParen, "expected `)` after lambda parameters")?;
            let _ = start;
            parameters
        } else {
            return Ok(None);
        };

        if !self.consume(TokenKind::Arrow) {
            self.index = checkpoint;
            return Ok(None);
        }

        let start = self.tokens[checkpoint].span.start;
        let body = if self.at(TokenKind::LBrace) {
            self.parse_block_expr_required()?
        } else {
            self.parse_expr()?
        };

        Ok(Some(Expr {
            kind: ExprKind::Lambda(LambdaExpr {
                parameters,
                body: Box::new(body.clone()),
                span: TextSpan::new(start, body.span.end),
            }),
            span: TextSpan::new(start, body.span.end),
        }))
    }

    fn try_parse_single_lambda_parameter(&mut self) -> Option<LambdaParameter> {
        let token = self.current().clone();
        match &token.kind {
            TokenKind::Identifier(name) if matches!(self.peek_kind(1), Some(TokenKind::Arrow)) => {
                self.index += 1;
                Some(LambdaParameter {
                    name: name.clone(),
                    ty: None,
                    span: token.span,
                })
            }
            _ => None,
        }
    }

    fn parse_coalesce_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let left = self.parse_range_expr()?;
        if self.consume(TokenKind::QuestionColon) {
            let right = self.parse_coalesce_expr()?;
            let span = TextSpan::new(left.span.start, right.span.end);
            Ok(Expr {
                kind: ExprKind::Binary {
                    left: Box::new(left),
                    op: BinaryOp::Coalesce,
                    right: Box::new(right),
                },
                span,
            })
        } else {
            Ok(left)
        }
    }

    fn parse_range_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        if self.consume(TokenKind::DotDot) {
            let start = self.previous().span.start;
            let end = if self.can_start_unary_expr() {
                Some(Box::new(self.parse_or_expr()?))
            } else {
                None
            };
            let span_end = end
                .as_ref()
                .map(|expr| expr.span.end)
                .unwrap_or(self.previous().span.end);
            return Ok(Expr {
                kind: ExprKind::Range(RangeExpr {
                    start: None,
                    end,
                    inclusive_end: false,
                    span: TextSpan::new(start, span_end),
                }),
                span: TextSpan::new(start, span_end),
            });
        }

        if self.consume(TokenKind::DotDotEq) {
            let start = self.previous().span.start;
            let end = self.parse_or_expr()?;
            return Ok(Expr {
                kind: ExprKind::Range(RangeExpr {
                    start: None,
                    end: Some(Box::new(end.clone())),
                    inclusive_end: true,
                    span: TextSpan::new(start, end.span.end),
                }),
                span: TextSpan::new(start, end.span.end),
            });
        }

        let start_expr = self.parse_or_expr()?;
        if self.consume(TokenKind::DotDot) {
            let end = if self.can_start_unary_expr() {
                Some(Box::new(self.parse_or_expr()?))
            } else {
                None
            };
            let span_end = end
                .as_ref()
                .map(|expr| expr.span.end)
                .unwrap_or(self.previous().span.end);
            return Ok(Expr {
                kind: ExprKind::Range(RangeExpr {
                    start: Some(Box::new(start_expr.clone())),
                    end,
                    inclusive_end: false,
                    span: TextSpan::new(start_expr.span.start, span_end),
                }),
                span: TextSpan::new(start_expr.span.start, span_end),
            });
        }

        if self.consume(TokenKind::DotDotEq) {
            let end = self.parse_or_expr()?;
            return Ok(Expr {
                kind: ExprKind::Range(RangeExpr {
                    start: Some(Box::new(start_expr.clone())),
                    end: Some(Box::new(end.clone())),
                    inclusive_end: true,
                    span: TextSpan::new(start_expr.span.start, end.span.end),
                }),
                span: TextSpan::new(start_expr.span.start, end.span.end),
            });
        }

        Ok(start_expr)
    }

    fn parse_or_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(Self::parse_and_expr, &[(TokenKind::PipePipe, BinaryOp::Or)])
    }

    fn parse_and_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(
            Self::parse_equality_expr,
            &[(TokenKind::AmpAmp, BinaryOp::And)],
        )
    }

    fn parse_equality_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(
            Self::parse_comparison_expr,
            &[
                (TokenKind::EqEq, BinaryOp::Equal),
                (TokenKind::BangEq, BinaryOp::NotEqual),
            ],
        )
    }

    fn parse_comparison_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(
            Self::parse_additive_expr,
            &[
                (TokenKind::Less, BinaryOp::Less),
                (TokenKind::LessEq, BinaryOp::LessEqual),
                (TokenKind::Greater, BinaryOp::Greater),
                (TokenKind::GreaterEq, BinaryOp::GreaterEqual),
            ],
        )
    }

    fn parse_additive_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(
            Self::parse_multiplicative_expr,
            &[
                (TokenKind::Plus, BinaryOp::Add),
                (TokenKind::Minus, BinaryOp::Subtract),
            ],
        )
    }

    fn parse_multiplicative_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        self.parse_left_associative(
            Self::parse_unary_expr,
            &[
                (TokenKind::Star, BinaryOp::Multiply),
                (TokenKind::Slash, BinaryOp::Divide),
                (TokenKind::Percent, BinaryOp::Remainder),
            ],
        )
    }

    fn parse_left_associative(
        &mut self,
        next: fn(&mut Self) -> Result<Expr, DiagnosticBag>,
        operators: &[(TokenKind, BinaryOp)],
    ) -> Result<Expr, DiagnosticBag> {
        let mut expr = next(self)?;
        loop {
            let op = operators.iter().find_map(|(kind, op)| {
                if self.at(kind.clone()) {
                    Some(*op)
                } else {
                    None
                }
            });
            let Some(op) = op else {
                break;
            };
            self.index += 1;
            let right = next(self)?;
            let span = TextSpan::new(expr.span.start, right.span.end);
            expr = Expr {
                kind: ExprKind::Binary {
                    left: Box::new(expr),
                    op,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(expr)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        if self.consume(TokenKind::Minus) {
            let start = self.previous().span.start;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Negate,
                    expr: Box::new(expr.clone()),
                },
                span: TextSpan::new(start, expr.span.end),
            });
        }
        if self.consume(TokenKind::Bang) {
            let start = self.previous().span.start;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr.clone()),
                },
                span: TextSpan::new(start, expr.span.end),
            });
        }
        self.parse_postfix_expr()
    }

    fn parse_postfix_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let mut expr = self.parse_primary_expr()?;
        loop {
            if self.consume(TokenKind::LParen) {
                if let Some(intrinsic) = self.intrinsic_name_from_expr(&expr) {
                    expr = self.parse_intrinsic_call(expr.span.start, intrinsic)?;
                    continue;
                }

                let arguments = self.parse_argument_list(TokenKind::RParen)?;
                self.expect_simple(TokenKind::RParen, "expected `)` after argument list")?;
                let end = self.previous().span.end;
                let start = expr.span.start;
                expr = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        arguments,
                    },
                    span: TextSpan::new(start, end),
                };
                continue;
            }
            if self.consume(TokenKind::LBracket) {
                let index = self.parse_expr()?;
                self.expect_simple(TokenKind::RBracket, "expected `]` after index expression")?;
                let end = self.previous().span.end;
                let start = expr.span.start;
                expr = Expr {
                    kind: ExprKind::Index {
                        target: Box::new(expr),
                        index: Box::new(index),
                    },
                    span: TextSpan::new(start, end),
                };
                continue;
            }
            if self.consume(TokenKind::QuestionDot) {
                let start = expr.span.start;
                let (name, span) = self.expect_identifier("expected field name after `?.`")?;
                expr = Expr {
                    kind: ExprKind::SafeField {
                        target: Box::new(expr),
                        name,
                    },
                    span: TextSpan::new(start, span.end),
                };
                continue;
            }
            if self.consume(TokenKind::BangBang) {
                let end = self.previous().span.end;
                let start = expr.span.start;
                expr = Expr {
                    kind: ExprKind::NonNull {
                        target: Box::new(expr),
                    },
                    span: TextSpan::new(start, end),
                };
                continue;
            }
            if self.consume(TokenKind::Dot) {
                if let Some(intrinsic) = self.try_parse_receiver_intrinsic_name() {
                    expr = self.parse_receiver_intrinsic(expr, intrinsic)?;
                    continue;
                }
                if self.consume(TokenKind::LParen) {
                    let callee = self.parse_qualified_name()?;
                    self.expect_simple(
                        TokenKind::RParen,
                        "expected `)` after receiver call target",
                    )?;
                    self.expect_simple(
                        TokenKind::LParen,
                        "expected `(` after receiver call target",
                    )?;
                    let arguments = self.parse_argument_list(TokenKind::RParen)?;
                    self.expect_simple(
                        TokenKind::RParen,
                        "expected `)` after receiver call arguments",
                    )?;
                    let end = self.previous().span.end;
                    let start = expr.span.start;
                    expr = Expr {
                        kind: ExprKind::ReceiverCall {
                            receiver: Box::new(expr),
                            callee,
                            arguments,
                        },
                        span: TextSpan::new(start, end),
                    };
                } else {
                    let start = expr.span.start;
                    let (name, span) = self.expect_identifier("expected field name after `.`")?;
                    expr = Expr {
                        kind: ExprKind::Field {
                            target: Box::new(expr),
                            name,
                        },
                        span: TextSpan::new(start, span.end),
                    };
                }
                continue;
            }
            break;
        }
        Ok(expr)
    }

    fn parse_primary_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        match self.current().kind.clone() {
            TokenKind::Integer(raw) => {
                let span = self.bump().span.clone();
                Ok(Expr {
                    kind: ExprKind::Integer(raw),
                    span,
                })
            }
            TokenKind::Float(raw) => {
                let span = self.bump().span.clone();
                Ok(Expr {
                    kind: ExprKind::Float(raw),
                    span,
                })
            }
            TokenKind::True => {
                let span = self.bump().span.clone();
                Ok(Expr {
                    kind: ExprKind::Bool(true),
                    span,
                })
            }
            TokenKind::False => {
                let span = self.bump().span.clone();
                Ok(Expr {
                    kind: ExprKind::Bool(false),
                    span,
                })
            }
            TokenKind::Null => {
                let span = self.bump().span.clone();
                Ok(Expr {
                    kind: ExprKind::Null,
                    span,
                })
            }
            TokenKind::StringLiteral(raw) => {
                let token = self.bump().clone();
                let literal = self.parse_string_literal(raw, token.span.clone())?;
                Ok(Expr {
                    span: token.span.clone(),
                    kind: ExprKind::String(literal),
                })
            }
            TokenKind::Identifier(_) => self.parse_name_expr(),
            TokenKind::LBracket => self.parse_list_literal(),
            TokenKind::LParen => self.parse_paren_or_tuple_expr(),
            TokenKind::LBrace => self.parse_braced_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::If => self.parse_if_expr(),
            TokenKind::When => self.parse_when_expr(),
            TokenKind::Econ => self.parse_intrinsic_primary_expr(IntrinsicName::Econ),
            _ => self.error_here(self.expected_expression_message()),
        }
    }

    fn parse_string_literal(
        &self,
        raw: crate::frontend::lexer::LexedStringLiteral,
        span: TextSpan,
    ) -> Result<StringLiteral, DiagnosticBag> {
        let mut parts = Vec::new();
        for part in raw.parts {
            match part {
                LexedStringPart::Text(text) => parts.push(StringPart::Text(text)),
                LexedStringPart::InterpolationIdent { name, span } => {
                    let expr = Expr {
                        span: span.clone(),
                        kind: ExprKind::Name(QualifiedName {
                            segments: vec![name],
                            span,
                        }),
                    };
                    parts.push(StringPart::Interpolation(Box::new(expr)));
                }
                LexedStringPart::InterpolationExpr { source, span } => {
                    let tokens = Lexer::new(&source, span.start).lex()?;
                    let mut parser = Parser::new(tokens);
                    let expr = parser.parse_expr()?;
                    parser.expect_simple(
                        TokenKind::Eof,
                        "unexpected tokens in string interpolation",
                    )?;
                    parts.push(StringPart::Interpolation(Box::new(expr)));
                }
            }
        }
        Ok(StringLiteral { parts, span })
    }

    fn parse_list_literal(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::LBracket, "expected `[`")?;
        let mut items = Vec::new();
        while !self.at(TokenKind::RBracket) {
            items.push(self.parse_expr()?);
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        self.expect_simple(TokenKind::RBracket, "expected `]` after list literal")?;
        Ok(Expr {
            kind: ExprKind::List(items),
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_paren_or_tuple_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::LParen, "expected `(`")?;
        if self.consume(TokenKind::RParen) {
            return Ok(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: TextSpan::new(start, self.previous().span.end),
            });
        }

        let first = self.parse_expr()?;
        if self.consume(TokenKind::Comma) {
            let mut items = vec![first];
            while !self.at(TokenKind::RParen) {
                items.push(self.parse_expr()?);
                if !self.consume(TokenKind::Comma) {
                    break;
                }
            }
            self.expect_simple(TokenKind::RParen, "expected `)` after tuple literal")?;
            return Ok(Expr {
                kind: ExprKind::Tuple(items),
                span: TextSpan::new(start, self.previous().span.end),
            });
        }

        self.expect_simple(
            TokenKind::RParen,
            "expected `)` after parenthesized expression",
        )?;
        Ok(first)
    }

    fn parse_braced_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        if let Some(record) = self.try_parse_record_literal()? {
            Ok(record)
        } else {
            self.parse_block_expr_required()
        }
    }

    fn parse_name_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let (name, span) = self.expect_identifier("expected identifier")?;
        Ok(Expr {
            span: span.clone(),
            kind: ExprKind::Name(QualifiedName {
                segments: vec![name],
                span,
            }),
        })
    }

    fn try_parse_record_literal(&mut self) -> Result<Option<Expr>, DiagnosticBag> {
        let checkpoint = self.index;
        let start = self.current().span.start;
        self.expect_simple(TokenKind::LBrace, "expected `{`")?;
        if self.consume(TokenKind::RBrace) {
            return Ok(Some(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: TextSpan::new(start, self.previous().span.end),
            }));
        }

        let Some(first_field) = self.try_parse_record_field()? else {
            self.index = checkpoint;
            return Ok(None);
        };

        if !self.consume(TokenKind::Comma) {
            self.index = checkpoint;
            return Ok(None);
        }

        let mut fields = vec![first_field];
        while !self.at(TokenKind::RBrace) {
            let field = self.parse_record_field()?;
            fields.push(field);
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        self.expect_simple(TokenKind::RBrace, "expected `}` after record literal")?;
        Ok(Some(Expr {
            kind: ExprKind::Record(fields),
            span: TextSpan::new(start, self.previous().span.end),
        }))
    }

    fn parse_record_field(&mut self) -> Result<RecordFieldInit, DiagnosticBag> {
        self.try_parse_record_field()?.ok_or_else(|| {
            vec![Diagnostic::error("expected record field").with_span(self.current().span.clone())]
                .into()
        })
    }

    fn try_parse_record_field(&mut self) -> Result<Option<RecordFieldInit>, DiagnosticBag> {
        let checkpoint = self.index;
        let mut fields = Vec::new();
        let field_start = self.current().span.start;
        let (name, _) = match self.expect_identifier("expected record field name") {
            Ok(name) => name,
            Err(_) => {
                self.index = checkpoint;
                return Ok(None);
            }
        };

        let ty = if self.consume(TokenKind::Colon) {
            let ty = self.parse_type()?;
            if !self.consume(TokenKind::Assign) {
                self.index = checkpoint;
                return Ok(None);
            }
            Some(ty)
        } else if self.consume(TokenKind::Assign) {
            None
        } else {
            self.index = checkpoint;
            return Ok(None);
        };

        let value = self.parse_expr()?;
        fields.push(RecordFieldInit {
            name,
            ty,
            value: value.clone(),
            span: TextSpan::new(field_start, value.span.end),
        });
        Ok(Some(fields.pop().expect("record field should be present")))
    }

    fn parse_block_expr_required(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::LBrace, "expected `{` to start block")?;
        let mut items = Vec::new();
        let mut trailing = None;

        while !self.at(TokenKind::RBrace) {
            self.take_doc_comments();

            if self.at(TokenKind::Val) || self.at(TokenKind::Var) {
                items.push(BlockItem::LocalValue(self.parse_local_value_decl()?));
                continue;
            }

            if let Some(statement) = self.try_parse_assignment_statement()? {
                items.push(statement);
                continue;
            }

            if self.at(TokenKind::Return) {
                items.push(BlockItem::Return(self.parse_return_statement()?));
                continue;
            }

            if self.at(TokenKind::Panic) {
                items.push(BlockItem::Panic(self.parse_panic_statement()?));
                continue;
            }

            if self.at(TokenKind::Break) {
                items.push(BlockItem::Break(self.parse_break_statement()?));
                continue;
            }

            if self.at(TokenKind::Continue) {
                items.push(BlockItem::Continue(self.parse_continue_statement()?));
                continue;
            }

            if self.at(TokenKind::For) || self.at(TokenKind::If) || self.at(TokenKind::When) {
                let expr = self.parse_expr()?;
                items.push(BlockItem::BlockStatement(expr));
                continue;
            }

            if self.consume(TokenKind::Semicolon) {
                continue;
            }

            let expr = self.parse_expr()?;
            if self.consume(TokenKind::Semicolon) {
                items.push(BlockItem::Expr(expr));
            } else {
                trailing = Some(Box::new(expr));
                break;
            }
        }

        self.expect_simple(TokenKind::RBrace, "expected `}` to close block")?;
        let end = self.previous().span.end;
        Ok(Expr {
            kind: ExprKind::Block(BlockExpr {
                items,
                trailing,
                span: TextSpan::new(start, end),
            }),
            span: TextSpan::new(start, end),
        })
    }

    fn try_parse_assignment_statement(&mut self) -> Result<Option<BlockItem>, DiagnosticBag> {
        let TokenKind::Identifier(name) = self.current().kind.clone() else {
            return Ok(None);
        };

        let Some(next) = self.peek_kind(1) else {
            return Ok(None);
        };

        let start = self.current().span.start;
        match next {
            TokenKind::Assign => {
                self.index += 2;
                let value = self.parse_expr()?;
                self.expect_simple(TokenKind::Semicolon, "expected `;` after assignment")?;
                Ok(Some(BlockItem::Assignment(AssignmentStatement {
                    name,
                    span: TextSpan::new(start, self.previous().span.end),
                    value,
                })))
            }
            TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq => {
                self.index += 1;
                let op = match self.bump().kind {
                    TokenKind::PlusEq => CompoundAssignmentOp::Add,
                    TokenKind::MinusEq => CompoundAssignmentOp::Subtract,
                    TokenKind::StarEq => CompoundAssignmentOp::Multiply,
                    TokenKind::SlashEq => CompoundAssignmentOp::Divide,
                    TokenKind::PercentEq => CompoundAssignmentOp::Remainder,
                    _ => unreachable!(),
                };
                let value = self.parse_expr()?;
                self.expect_simple(
                    TokenKind::Semicolon,
                    "expected `;` after compound assignment",
                )?;
                Ok(Some(BlockItem::CompoundAssignment(
                    CompoundAssignmentStatement {
                        name,
                        op,
                        value,
                        span: TextSpan::new(start, self.previous().span.end),
                    },
                )))
            }
            _ => Ok(None),
        }
    }

    fn parse_for_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::For, "expected `for`")?;
        self.expect_simple(TokenKind::LParen, "expected `(` after `for`")?;

        let init = self.parse_for_init()?;
        let header = self.parse_for_header()?;
        let body = self.parse_block_expr_required()?;
        let end = body.span.end;
        let ExprKind::Block(block) = body.kind else {
            unreachable!();
        };
        Ok(Expr {
            kind: ExprKind::For(ForExpr {
                init: init.map(Box::new),
                header,
                body: block,
                span: TextSpan::new(start, end),
            }),
            span: TextSpan::new(start, end),
        })
    }

    fn parse_for_init(&mut self) -> Result<Option<BlockItem>, DiagnosticBag> {
        if self.at(TokenKind::Val) || self.at(TokenKind::Var) {
            let decl = self.parse_local_value_decl_for_header()?;
            self.expect_simple(TokenKind::Semicolon, "expected `;` in `for` header")?;
            return Ok(Some(BlockItem::LocalValue(decl)));
        }

        if let TokenKind::Identifier(_) = self.current().kind {
            let checkpoint = self.index;
            let (name, _) = self.expect_identifier("")?;
            if matches!(
                self.peek_kind(0),
                Some(TokenKind::Assign)
                    | Some(TokenKind::PlusEq)
                    | Some(TokenKind::MinusEq)
                    | Some(TokenKind::StarEq)
                    | Some(TokenKind::SlashEq)
                    | Some(TokenKind::PercentEq)
            ) {
                self.index = checkpoint;
                let stmt = self.try_parse_assignment_for_header(name)?;
                self.expect_simple(TokenKind::Semicolon, "expected `;` in `for` header")?;
                return Ok(Some(stmt));
            }
            self.index = checkpoint;
        }

        let checkpoint = self.index;
        let expr = self.parse_expr()?;
        if self.consume(TokenKind::Semicolon) {
            return Ok(Some(BlockItem::Expr(expr)));
        }
        self.index = checkpoint;
        Ok(None)
    }

    fn parse_for_header(&mut self) -> Result<ForHeader, DiagnosticBag> {
        let expr = self.parse_expr()?;
        if self.consume(TokenKind::In) {
            let pattern = if let ExprKind::Name(ref name) = expr.kind {
                if name.segments.len() != 1 {
                    return self
                        .error_here("expected a single identifier as loop pattern");
                }
                name.segments[0].clone()
            } else {
                return self.error_here("expected an identifier as loop pattern");
            };
            let iterable = self.parse_expr()?;
            self.expect_simple(TokenKind::RParen, "expected `)` after `for` header")?;
            Ok(ForHeader::In {
                pattern,
                iterable: Box::new(iterable),
            })
        } else if self.consume(TokenKind::RParen) {
            Ok(ForHeader::Condition(Box::new(expr)))
        } else {
            self.error_here("expected `in`, `;`, or `)` in `for` header")
        }
    }

    fn try_parse_assignment_for_header(&mut self, name: String) -> Result<BlockItem, DiagnosticBag> {
        let start = self.current().span.start;
        match self.current().kind.clone() {
            TokenKind::Identifier(_) => {}
            _ => unreachable!(),
        }
        self.bump();
        match self.bump().kind {
            TokenKind::Assign => {
                let value = self.parse_expr()?;
                let end = value.span.end;
                Ok(BlockItem::Assignment(AssignmentStatement {
                    name,
                    value,
                    span: TextSpan::new(start, end),
                }))
            }
            TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq => {
                let kind = self.previous().kind.clone();
                let op = match kind {
                    TokenKind::PlusEq => CompoundAssignmentOp::Add,
                    TokenKind::MinusEq => CompoundAssignmentOp::Subtract,
                    TokenKind::StarEq => CompoundAssignmentOp::Multiply,
                    TokenKind::SlashEq => CompoundAssignmentOp::Divide,
                    TokenKind::PercentEq => CompoundAssignmentOp::Remainder,
                    _ => unreachable!(),
                };
                let value = self.parse_expr()?;
                Ok(BlockItem::CompoundAssignment(
                    CompoundAssignmentStatement {
                        name,
                        op,
                        value,
                        span: TextSpan::new(start, self.previous().span.end),
                    },
                ))
            }
            _ => unreachable!(),
        }
    }

    fn parse_return_statement(&mut self) -> Result<ReturnStatement, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Return, "expected `return`")?;
        let value = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_simple(TokenKind::Semicolon, "expected `;` after return statement")?;
        Ok(ReturnStatement {
            value,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_panic_statement(&mut self) -> Result<PanicStatement, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Panic, "expected `panic`")?;
        let expr = self.parse_primary_expr()?;
        let ExprKind::String(message) = expr.kind else {
            return self.error_here("`panic` requires a plain string literal message");
        };
        if message
            .parts
            .iter()
            .any(|part| !matches!(part, StringPart::Text(_)))
        {
            return self.error_here("`panic` requires a plain string literal message");
        }
        self.expect_simple(TokenKind::Semicolon, "expected `;` after panic statement")?;
        Ok(PanicStatement {
            message,
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_break_statement(&mut self) -> Result<BreakStatement, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Break, "expected `break`")?;
        self.expect_simple(TokenKind::Semicolon, "expected `;` after break statement")?;
        Ok(BreakStatement {
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_continue_statement(&mut self) -> Result<ContinueStatement, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Continue, "expected `continue`")?;
        self.expect_simple(TokenKind::Semicolon, "expected `;` after continue statement")?;
        Ok(ContinueStatement {
            span: TextSpan::new(start, self.previous().span.end),
        })
    }

    fn parse_if_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::If, "expected `if`")?;
        let mut branches = Vec::new();

        self.expect_simple(TokenKind::LParen, "expected `(` after `if`")?;
        let condition = self.parse_expr()?;
        self.expect_simple(TokenKind::RParen, "expected `)` after `if` condition")?;
        let body = self.parse_block_expr_required()?;
        let ExprKind::Block(body) = body.kind else {
            unreachable!();
        };
        let mut end = body.span.end;
        branches.push(IfBranch {
            span: TextSpan::new(start, end),
            condition,
            body,
        });

        while self.consume(TokenKind::Else) {
            if self.consume(TokenKind::If) {
                self.expect_simple(TokenKind::LParen, "expected `(` after `else if`")?;
                let condition = self.parse_expr()?;
                self.expect_simple(TokenKind::RParen, "expected `)` after `else if` condition")?;
                let body = self.parse_block_expr_required()?;
                let ExprKind::Block(body) = body.kind else {
                    unreachable!();
                };
                end = body.span.end;
                branches.push(IfBranch {
                    span: TextSpan::new(start, end),
                    condition,
                    body,
                });
            } else {
                let body = self.parse_block_expr_required()?;
                let ExprKind::Block(else_body) = body.kind else {
                    unreachable!();
                };
                end = else_body.span.end;
                return Ok(Expr {
                    kind: ExprKind::If(IfExpr {
                        branches,
                        else_branch: Some(else_body),
                        span: TextSpan::new(start, end),
                    }),
                    span: TextSpan::new(start, end),
                });
            }
        }

        Ok(Expr {
            kind: ExprKind::If(IfExpr {
                branches,
                else_branch: None,
                span: TextSpan::new(start, end),
            }),
            span: TextSpan::new(start, end),
        })
    }

    fn parse_when_expr(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::When, "expected `when`")?;
        self.expect_simple(TokenKind::LParen, "expected `(` after `when`")?;
        let subject = self.parse_expr()?;
        self.expect_simple(TokenKind::RParen, "expected `)` after `when` subject")?;
        self.expect_simple(TokenKind::LBrace, "expected `{` after `when (...)`")?;

        let mut arms = Vec::new();
        let mut else_arm = None;
        while !self.at(TokenKind::RBrace) {
            if self.consume(TokenKind::Else) {
                self.expect_simple(TokenKind::Arrow, "expected `->` after `else`")?;
                let expr = self.parse_expr()?;
                self.expect_simple(TokenKind::Semicolon, "expected `;` after `when` else arm")?;
                else_arm = Some(expr);
                break;
            }

            let arm_start = self.current().span.start;
            self.expect_simple(TokenKind::Is, "expected `is` in `when` arm")?;
            let ty = self.parse_type()?;
            let binding = if self.consume(TokenKind::As) {
                Some(
                    self.expect_identifier("expected binding name after `as`")?
                        .0,
                )
            } else {
                None
            };
            self.expect_simple(TokenKind::Arrow, "expected `->` in `when` arm")?;
            let body = if self.at(TokenKind::LBrace) {
                self.parse_block_expr_required()?
            } else {
                let expr = self.parse_expr()?;
                self.expect_simple(TokenKind::Semicolon, "expected `;` after inline `when` arm")?;
                expr
            };
            arms.push(WhenArm {
                ty,
                binding,
                span: TextSpan::new(arm_start, body.span.end),
                body,
            });
        }

        self.expect_simple(TokenKind::RBrace, "expected `}` after `when` expression")?;
        let end = self.previous().span.end;
        Ok(Expr {
            kind: ExprKind::When(WhenExpr {
                subject: Box::new(subject),
                arms,
                else_arm: else_arm.map(Box::new),
                span: TextSpan::new(start, end),
            }),
            span: TextSpan::new(start, end),
        })
    }

    fn parse_intrinsic_primary_expr(
        &mut self,
        intrinsic: IntrinsicName,
    ) -> Result<Expr, DiagnosticBag> {
        match intrinsic {
            IntrinsicName::Econ => self.parse_econ_intrinsic(),
            IntrinsicName::Updated => self.error_here("`updated` requires a target value"),
        }
    }

    fn parse_intrinsic_call(
        &mut self,
        start: usize,
        intrinsic: IntrinsicName,
    ) -> Result<Expr, DiagnosticBag> {
        match intrinsic {
            IntrinsicName::Updated => {
                let (target, updates) = self.parse_updated_call_arguments(TokenKind::RParen)?;
                self.expect_simple(TokenKind::RParen, "expected `)` after updated arguments")?;
                let end = self.previous().span.end;
                Ok(Expr {
                    kind: ExprKind::Intrinsic(IntrinsicExpr::Updated(UpdatedIntrinsic {
                        target: Box::new(target),
                        updates,
                    })),
                    span: TextSpan::new(start, end),
                })
            }
            IntrinsicName::Econ => self.error_here("`econ` is not called with `(...)`"),
        }
    }

    fn parse_receiver_intrinsic(
        &mut self,
        target: Expr,
        intrinsic: IntrinsicName,
    ) -> Result<Expr, DiagnosticBag> {
        match intrinsic {
            IntrinsicName::Updated => {
                self.expect_simple(TokenKind::LParen, "expected `(` after `.updated`")?;
                let updates = self.parse_updated_argument_list(TokenKind::RParen)?;
                if updates.is_empty() {
                    return self.error_here("`updated` requires at least one field assignment");
                }
                self.expect_simple(TokenKind::RParen, "expected `)` after updated arguments")?;
                let end = self.previous().span.end;
                let start = target.span.start;
                Ok(Expr {
                    kind: ExprKind::Intrinsic(IntrinsicExpr::Updated(UpdatedIntrinsic {
                        target: Box::new(target),
                        updates,
                    })),
                    span: TextSpan::new(start, end),
                })
            }
            IntrinsicName::Econ => self.error_here("`econ` does not support receiver syntax"),
        }
    }

    fn parse_econ_intrinsic(&mut self) -> Result<Expr, DiagnosticBag> {
        let start = self.current().span.start;
        self.expect_simple(TokenKind::Econ, "expected `econ`")?;
        self.expect_simple(TokenKind::LBracket, "expected `[` after `econ`")?;
        let ty = self.parse_type()?;
        self.expect_simple(TokenKind::RBracket, "expected `]` after econ type")?;
        let body = self.parse_block_expr_required()?;
        let ExprKind::Block(body) = body.kind else {
            unreachable!();
        };
        Ok(Expr {
            kind: ExprKind::Intrinsic(IntrinsicExpr::Econ(EconIntrinsic {
                ty,
                body: body.clone(),
            })),
            span: TextSpan::new(start, body.span.end),
        })
    }

    fn parse_argument_list(
        &mut self,
        terminator: TokenKind,
    ) -> Result<Vec<Argument>, DiagnosticBag> {
        let mut arguments = Vec::new();
        while !self.at(terminator.clone()) {
            if let TokenKind::Identifier(name) = self.current().kind.clone() {
                if matches!(self.peek_kind(1), Some(TokenKind::Assign)) {
                    let start = self.current().span.start;
                    self.index += 2;
                    let value = self.parse_expr()?;
                    arguments.push(Argument::Named {
                        name,
                        span: TextSpan::new(start, value.span.end),
                        value,
                    });
                } else {
                    arguments.push(Argument::Positional(self.parse_expr()?));
                }
            } else {
                arguments.push(Argument::Positional(self.parse_expr()?));
            }
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        Ok(arguments)
    }

    fn parse_updated_call_arguments(
        &mut self,
        terminator: TokenKind,
    ) -> Result<(Expr, Vec<UpdatedArg>), DiagnosticBag> {
        let target = self.parse_expr()?;
        if self.at(terminator.clone()) {
            return self.error_here("`updated` requires at least one field assignment");
        }
        self.expect_simple(
            TokenKind::Comma,
            "expected `,` after the value being updated",
        )?;
        let arguments = self.parse_updated_argument_list(terminator)?;
        if arguments.is_empty() {
            return self.error_here("`updated` requires at least one field assignment");
        }
        Ok((target, arguments))
    }

    fn parse_updated_argument_list(
        &mut self,
        terminator: TokenKind,
    ) -> Result<Vec<UpdatedArg>, DiagnosticBag> {
        let mut arguments = Vec::new();
        while !self.at(terminator.clone()) {
            arguments.push(self.parse_updated_argument()?);
            if !self.consume(TokenKind::Comma) {
                break;
            }
        }
        Ok(arguments)
    }

    fn parse_updated_argument(&mut self) -> Result<UpdatedArg, DiagnosticBag> {
        let start = self.current().span.start;
        let path = self.parse_updated_path()?;
        self.expect_simple(TokenKind::Assign, "expected `=` after updated path")?;
        let value = self.parse_expr()?;
        Ok(UpdatedArg {
            path,
            value: value.clone(),
            span: TextSpan::new(start, value.span.end),
        })
    }

    fn parse_updated_path(&mut self) -> Result<Vec<UpdatedPathSegment>, DiagnosticBag> {
        let mut path = vec![self.parse_updated_path_segment("expected updated field path")?];
        while self.consume(TokenKind::Dot) {
            path.push(self.parse_updated_path_segment(
                "expected field name or index after `.` in updated path",
            )?);
        }
        Ok(path)
    }

    fn parse_updated_path_segment(
        &mut self,
        message: &str,
    ) -> Result<UpdatedPathSegment, DiagnosticBag> {
        match self.current().kind.clone() {
            TokenKind::Identifier(name) => {
                self.index += 1;
                Ok(UpdatedPathSegment::Field(name))
            }
            TokenKind::Hash => {
                let hash_span = self.bump().span.clone();
                let TokenKind::Integer(raw) = self.current().kind.clone() else {
                    return Err(vec![
                        Diagnostic::error("expected an integer index after `#`")
                            .with_span(hash_span),
                    ]
                    .into());
                };
                let span = self.bump().span.clone();
                let index =
                    raw.replace('_', "")
                        .parse::<usize>()
                        .map_err(|_| -> DiagnosticBag {
                            vec![
                                Diagnostic::error("updated index is out of range")
                                    .with_span(span.clone()),
                            ]
                            .into()
                        })?;
                Ok(UpdatedPathSegment::Index(index))
            }
            _ => self.error_here(message),
        }
    }

    fn intrinsic_name_from_expr(&self, expr: &Expr) -> Option<IntrinsicName> {
        match &expr.kind {
            ExprKind::Name(QualifiedName { segments, .. })
                if segments.as_slice() == ["updated"] =>
            {
                Some(IntrinsicName::Updated)
            }
            _ => None,
        }
    }

    fn try_parse_receiver_intrinsic_name(&mut self) -> Option<IntrinsicName> {
        let TokenKind::Identifier(name) = self.current().kind.clone() else {
            return None;
        };
        if !matches!(self.peek_kind(1), Some(TokenKind::LParen)) {
            return None;
        }
        let intrinsic = match name.as_str() {
            "updated" => IntrinsicName::Updated,
            _ => return None,
        };
        self.index += 1;
        Some(intrinsic)
    }

    fn parse_visibility(&mut self) -> Option<Visibility> {
        if self.consume(TokenKind::Public) {
            Some(Visibility::Public)
        } else if self.consume(TokenKind::Private) {
            Some(Visibility::Private)
        } else {
            None
        }
    }

    fn parse_mutability(&mut self) -> Result<Mutability, DiagnosticBag> {
        if self.consume(TokenKind::Val) {
            Ok(Mutability::Val)
        } else if self.consume(TokenKind::Var) {
            Ok(Mutability::Var)
        } else {
            self.error_here("expected `val` or `var`")
        }
    }

    fn parse_qualified_name(&mut self) -> Result<QualifiedName, DiagnosticBag> {
        let start = self.current().span.start;
        let (first, _) = self.expect_identifier("expected identifier")?;
        let mut segments = vec![first];
        while self.consume(TokenKind::Dot) {
            let (segment, _) = self.expect_identifier("expected identifier after `.`")?;
            segments.push(segment);
        }
        let end = self.previous().span.end;
        Ok(QualifiedName {
            segments,
            span: TextSpan::new(start, end),
        })
    }

    fn parse_module_path(&mut self) -> Result<ModulePath, DiagnosticBag> {
        let name = self.parse_qualified_name()?;
        ModulePath::parse(&name.to_source_string()).map_err(|diagnostic| vec![diagnostic].into())
    }

    fn take_doc_comments(&mut self) -> Vec<String> {
        let mut docs = Vec::new();
        while let TokenKind::DocComment(text) = self.current().kind.clone() {
            docs.push(text);
            self.index += 1;
        }
        docs
    }

    fn can_start_unary_expr(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Identifier(_)
                | TokenKind::Integer(_)
                | TokenKind::Float(_)
                | TokenKind::StringLiteral(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Null
                | TokenKind::If
                | TokenKind::When
                | TokenKind::Econ
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::Minus
                | TokenKind::Bang
        )
    }

    fn expect_identifier(&mut self, message: &str) -> Result<(String, TextSpan), DiagnosticBag> {
        match self.current().kind.clone() {
            TokenKind::Identifier(name) => {
                let span = self.bump().span.clone();
                Ok((name, span))
            }
            _ => self.error_here(message),
        }
    }

    fn expect_simple(&mut self, expected: TokenKind, message: &str) -> Result<(), DiagnosticBag> {
        if self.at(expected) {
            self.index += 1;
            Ok(())
        } else {
            self.error_here(message)
        }
    }

    fn consume(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn peek_kind(&self, lookahead: usize) -> Option<TokenKind> {
        self.tokens
            .get(self.index + lookahead)
            .map(|token| token.kind.clone())
    }

    fn bump(&mut self) -> &Token {
        self.index += 1;
        &self.tokens[self.index - 1]
    }

    fn expected_expression_message(&self) -> String {
        self.index
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
            .and_then(|token| expression_context(token.kind.clone()))
            .map(|context| format!("expected expression after {context}"))
            .unwrap_or_else(|| "expected expression".to_owned())
    }

    fn error_here<T>(&self, message: impl Into<String>) -> Result<T, DiagnosticBag> {
        Err(vec![Diagnostic::error(message).with_span(self.current().span.clone())].into())
    }
}

fn expression_context(kind: TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::Assign => Some("`=`"),
        TokenKind::Arrow => Some("`->`"),
        TokenKind::Colon => Some("`:`"),
        TokenKind::Comma => Some("`,`"),
        TokenKind::FatArrow => Some("`=>`"),
        TokenKind::LParen => Some("`(`"),
        TokenKind::LBracket => Some("`[`"),
        TokenKind::Minus => Some("`-`"),
        TokenKind::Percent => Some("`%`"),
        TokenKind::Plus => Some("`+`"),
        TokenKind::QuestionColon => Some("`?:`"),
        TokenKind::Return => Some("`return`"),
        TokenKind::Slash => Some("`/`"),
        TokenKind::Star => Some("`*`"),
        _ => None,
    }
}
