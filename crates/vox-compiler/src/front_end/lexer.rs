use vox_core::diagnostics::{Diagnostic, DiagnosticBag, TextSpan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    DocComment(String),
    Identifier(String),
    Integer(String),
    Float(String),
    StringLiteral(LexedStringLiteral),
    As,
    Dyn,
    Econ,
    Else,
    Evil,
    False,
    For,
    Fun,
    If,
    Import,
    In,
    Is,
    Null,
    Package,
    Panic,
    Param,
    Private,
    Public,
    Return,
    Script,
    True,
    Val,
    Var,
    When,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Colon,
    Semicolon,
    Question,
    Arrow,
    FatArrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    EqEq,
    BangEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    AmpAmp,
    PipePipe,
    QuestionDot,
    QuestionColon,
    BangBang,
    DotDot,
    DotDotEq,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexedStringLiteral {
    pub parts: Vec<LexedStringPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexedStringPart {
    Text(String),
    InterpolationIdent { name: String, span: TextSpan },
    InterpolationExpr { source: String, span: TextSpan },
}

pub struct Lexer<'a> {
    text: &'a str,
    base_offset: usize,
    offset: usize,
    diagnostics: DiagnosticBag,
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a str, base_offset: usize) -> Self {
        Self {
            text,
            base_offset,
            offset: 0,
            diagnostics: DiagnosticBag::default(),
        }
    }

    pub fn lex(mut self) -> Result<Vec<Token>, DiagnosticBag> {
        let mut tokens = Vec::new();

        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.bump_char();
                continue;
            }

            let start = self.absolute_offset();
            match ch {
                '/' => {
                    if self.consume_exact("///") {
                        let comment = self.take_until_line_end();
                        tokens.push(Token {
                            kind: TokenKind::DocComment(comment),
                            span: TextSpan::new(start, self.absolute_offset()),
                        });
                        continue;
                    }
                    if self.consume_exact("//") {
                        self.take_until_line_end();
                        continue;
                    }
                    if self.consume_exact("/*") {
                        self.skip_block_comment(start);
                        continue;
                    }
                    self.bump_char();
                    if self.consume_char('=') {
                        tokens.push(Token {
                            kind: TokenKind::SlashEq,
                            span: TextSpan::new(start, self.absolute_offset()),
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Slash,
                            span: TextSpan::new(start, self.absolute_offset()),
                        });
                    }
                }
                '"' => {
                    let kind = self.lex_string(start);
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '(' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::LParen,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                ')' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::RParen,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '[' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::LBracket,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                ']' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::RBracket,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '{' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::LBrace,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '}' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::RBrace,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                ',' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::Comma,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                ';' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::Semicolon,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                ':' => {
                    self.bump_char();
                    tokens.push(Token {
                        kind: TokenKind::Colon,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '.' => {
                    self.bump_char();
                    let kind = if self.consume_char('.') {
                        if self.consume_char('=') {
                            TokenKind::DotDotEq
                        } else {
                            TokenKind::DotDot
                        }
                    } else {
                        TokenKind::Dot
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '?' => {
                    self.bump_char();
                    let kind = if self.consume_char('.') {
                        TokenKind::QuestionDot
                    } else if self.consume_char(':') {
                        TokenKind::QuestionColon
                    } else {
                        TokenKind::Question
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '+' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::PlusEq
                    } else {
                        TokenKind::Plus
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '-' => {
                    self.bump_char();
                    let kind = if self.consume_char('>') {
                        TokenKind::Arrow
                    } else if self.consume_char('=') {
                        TokenKind::MinusEq
                    } else {
                        TokenKind::Minus
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '*' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::StarEq
                    } else {
                        TokenKind::Star
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '%' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::PercentEq
                    } else {
                        TokenKind::Percent
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '!' => {
                    self.bump_char();
                    let kind = if self.consume_char('!') {
                        TokenKind::BangBang
                    } else if self.consume_char('=') {
                        TokenKind::BangEq
                    } else {
                        TokenKind::Bang
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '=' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::EqEq
                    } else if self.consume_char('>') {
                        TokenKind::FatArrow
                    } else {
                        TokenKind::Assign
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '<' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::LessEq
                    } else {
                        TokenKind::Less
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '>' => {
                    self.bump_char();
                    let kind = if self.consume_char('=') {
                        TokenKind::GreaterEq
                    } else {
                        TokenKind::Greater
                    };
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '&' => {
                    self.bump_char();
                    if self.consume_char('&') {
                        tokens.push(Token {
                            kind: TokenKind::AmpAmp,
                            span: TextSpan::new(start, self.absolute_offset()),
                        });
                    } else {
                        self.error_at(start, "unexpected `&`, expected `&&`");
                    }
                }
                '|' => {
                    self.bump_char();
                    if self.consume_char('|') {
                        tokens.push(Token {
                            kind: TokenKind::PipePipe,
                            span: TextSpan::new(start, self.absolute_offset()),
                        });
                    } else {
                        self.error_at(start, "unexpected `|`, expected `||`");
                    }
                }
                '0'..='9' => {
                    let kind = self.lex_number();
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                '_' | 'a'..='z' | 'A'..='Z' => {
                    let ident = self.lex_identifier();
                    let kind = keyword_kind(&ident).unwrap_or(TokenKind::Identifier(ident));
                    tokens.push(Token {
                        kind,
                        span: TextSpan::new(start, self.absolute_offset()),
                    });
                }
                _ => {
                    self.error_at(start, format!("unexpected character `{ch}`"));
                    self.bump_char();
                }
            }
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: TextSpan::new(self.absolute_offset(), self.absolute_offset()),
        });

        if self.diagnostics.has_errors() {
            Err(self.diagnostics)
        } else {
            Ok(tokens)
        }
    }

    fn lex_string(&mut self, start: usize) -> TokenKind {
        self.bump_char();

        let mut parts = Vec::new();
        let mut text = String::new();

        loop {
            match self.peek_char() {
                Some('"') => {
                    self.bump_char();
                    break;
                }
                Some('\\') => {
                    self.bump_char();
                    match self.parse_escape(start) {
                        Some(ch) => text.push(ch),
                        None => break,
                    }
                }
                Some('$') => {
                    let dollar_start = self.absolute_offset();
                    self.bump_char();
                    match self.peek_char() {
                        Some('{') => {
                            self.bump_char();
                            flush_text(&mut parts, &mut text);
                            let expr_start = self.absolute_offset();
                            match self.take_interpolation_source() {
                                Some(source) => parts.push(LexedStringPart::InterpolationExpr {
                                    source,
                                    span: TextSpan::new(expr_start, self.absolute_offset() - 1),
                                }),
                                None => break,
                            }
                        }
                        Some(ch) if is_identifier_start(ch) => {
                            flush_text(&mut parts, &mut text);
                            let name = self.lex_identifier();
                            parts.push(LexedStringPart::InterpolationIdent {
                                name,
                                span: TextSpan::new(dollar_start, self.absolute_offset()),
                            });
                        }
                        _ => {
                            self.error_at(
                                dollar_start,
                                "invalid string interpolation, expected `$name` or `${expr}`",
                            );
                            break;
                        }
                    }
                }
                Some('\n') | Some('\r') => {
                    self.error_at(start, "string literal cannot contain a newline");
                    break;
                }
                Some(ch) => {
                    self.bump_char();
                    text.push(ch);
                }
                None => {
                    self.error_at(start, "unterminated string literal");
                    break;
                }
            }
        }

        flush_text(&mut parts, &mut text);
        TokenKind::StringLiteral(LexedStringLiteral { parts })
    }

    fn take_interpolation_source(&mut self) -> Option<String> {
        let start = self.offset;
        let mut nested_braces = 0usize;

        loop {
            match self.peek_char() {
                Some('"') => {
                    if self.skip_nested_string().is_none() {
                        return None;
                    }
                }
                Some('/') if self.peek_next_char() == Some('/') => {
                    self.consume_exact("//");
                    self.take_until_line_end();
                }
                Some('/') if self.peek_next_char() == Some('*') => {
                    let comment_start = self.absolute_offset();
                    self.consume_exact("/*");
                    self.skip_block_comment(comment_start);
                    if self.diagnostics.has_errors() {
                        return None;
                    }
                }
                Some('{') => {
                    nested_braces += 1;
                    self.bump_char();
                }
                Some('}') if nested_braces == 0 => {
                    let source = self.text[start..self.offset].to_owned();
                    self.bump_char();
                    return Some(source);
                }
                Some('}') => {
                    nested_braces -= 1;
                    self.bump_char();
                }
                Some(_) => {
                    self.bump_char();
                }
                None => {
                    self.error_at(
                        self.base_offset + start,
                        "unterminated `${...}` interpolation sequence",
                    );
                    return None;
                }
            }
        }
    }

    fn skip_nested_string(&mut self) -> Option<()> {
        self.bump_char();
        loop {
            match self.peek_char() {
                Some('"') => {
                    self.bump_char();
                    return Some(());
                }
                Some('\\') => {
                    self.bump_char();
                    self.parse_escape(self.absolute_offset())?;
                }
                Some('$') if self.peek_next_char() == Some('{') => {
                    self.bump_char();
                    self.bump_char();
                    self.take_interpolation_source()?;
                }
                Some('\n') | Some('\r') => {
                    self.error_at(
                        self.absolute_offset(),
                        "string literal cannot contain a newline",
                    );
                    return None;
                }
                Some(_) => {
                    self.bump_char();
                }
                None => {
                    self.error_at(self.absolute_offset(), "unterminated string literal");
                    return None;
                }
            }
        }
    }

    fn parse_escape(&mut self, span_start: usize) -> Option<char> {
        let Some(ch) = self.bump_char() else {
            self.error_at(span_start, "unterminated escape sequence");
            return None;
        };

        match ch {
            '"' => Some('"'),
            '\\' => Some('\\'),
            '$' => Some('$'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'u' => self.parse_unicode_escape(span_start),
            _ => {
                self.error_at(span_start, format!("unsupported escape sequence `\\{ch}`"));
                None
            }
        }
    }

    fn parse_unicode_escape(&mut self, span_start: usize) -> Option<char> {
        if !self.consume_char('{') {
            self.error_at(span_start, "unicode escape must start with `\\u{`");
            return None;
        }

        let mut digits = String::new();
        while let Some(ch) = self.peek_char() {
            if ch == '}' {
                break;
            }
            if ch.is_ascii_hexdigit() && digits.len() < 6 {
                digits.push(ch);
                self.bump_char();
            } else {
                self.error_at(span_start, "unicode escape must contain 1 to 6 hex digits");
                return None;
            }
        }

        if digits.is_empty() || !self.consume_char('}') {
            self.error_at(span_start, "unicode escape must end with `}`");
            return None;
        }

        let value = match u32::from_str_radix(&digits, 16) {
            Ok(value) => value,
            Err(_) => {
                self.error_at(span_start, "invalid unicode escape");
                return None;
            }
        };

        match char::from_u32(value) {
            Some(ch) => Some(ch),
            None => {
                self.error_at(span_start, "invalid unicode scalar value");
                None
            }
        }
    }

    fn lex_number(&mut self) -> TokenKind {
        let start = self.offset;
        self.take_digit_sequence();
        let mut is_float = false;

        if self.peek_char() == Some('.')
            && self.peek_next_char() != Some('.')
            && matches!(self.peek_nth_char(1), Some('0'..='9'))
        {
            is_float = true;
            self.bump_char();
            self.take_digit_sequence();
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            is_float = true;
            self.bump_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.bump_char();
            }
            if !matches!(self.peek_char(), Some('0'..='9')) {
                self.error_at(
                    self.absolute_offset(),
                    "float exponent must be followed by digits",
                );
            } else {
                self.take_digit_sequence();
            }
        }

        let raw = self.text[start..self.offset].to_owned();
        if is_float {
            TokenKind::Float(raw)
        } else {
            TokenKind::Integer(raw)
        }
    }

    fn take_digit_sequence(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                self.bump_char();
            } else if ch == '_' {
                if matches!(self.peek_next_char(), Some('0'..='9')) {
                    self.bump_char();
                } else {
                    self.error_at(
                        self.absolute_offset(),
                        "numeric separator `_` must be followed by a digit",
                    );
                    self.bump_char();
                }
            } else {
                break;
            }
        }
    }

    fn lex_identifier(&mut self) -> String {
        let start = self.offset;
        self.bump_char();
        while matches!(self.peek_char(), Some(ch) if is_identifier_continue(ch)) {
            self.bump_char();
        }
        self.text[start..self.offset].to_owned()
    }

    fn skip_block_comment(&mut self, start: usize) {
        loop {
            match self.peek_char() {
                Some('*') if self.peek_next_char() == Some('/') => {
                    self.bump_char();
                    self.bump_char();
                    return;
                }
                Some(_) => {
                    self.bump_char();
                }
                None => {
                    self.error_at(start, "unterminated block comment");
                    return;
                }
            }
        }
    }

    fn take_until_line_end(&mut self) -> String {
        let start = self.offset;
        while let Some(ch) = self.peek_char() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            self.bump_char();
        }
        self.text[start..self.offset].trim().to_owned()
    }

    fn consume_exact(&mut self, exact: &str) -> bool {
        if self.text[self.offset..].starts_with(exact) {
            self.offset += exact.len();
            true
        } else {
            false
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.text[self.offset..].chars().next()
    }

    fn peek_next_char(&self) -> Option<char> {
        self.peek_nth_char(1)
    }

    fn peek_nth_char(&self, n: usize) -> Option<char> {
        self.text[self.offset..].chars().nth(n)
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.offset += ch.len_utf8();
        Some(ch)
    }

    fn absolute_offset(&self) -> usize {
        self.base_offset + self.offset
    }

    fn error_at(&mut self, start: usize, message: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(message).with_span(TextSpan::new(start, self.absolute_offset())),
        );
    }
}

fn flush_text(parts: &mut Vec<LexedStringPart>, text: &mut String) {
    if !text.is_empty() {
        parts.push(LexedStringPart::Text(std::mem::take(text)));
    }
}

fn keyword_kind(ident: &str) -> Option<TokenKind> {
    Some(match ident {
        "as" => TokenKind::As,
        "dyn" => TokenKind::Dyn,
        "econ" => TokenKind::Econ,
        "else" => TokenKind::Else,
        "evil" => TokenKind::Evil,
        "false" => TokenKind::False,
        "for" => TokenKind::For,
        "fun" => TokenKind::Fun,
        "if" => TokenKind::If,
        "import" => TokenKind::Import,
        "in" => TokenKind::In,
        "is" => TokenKind::Is,
        "null" => TokenKind::Null,
        "package" => TokenKind::Package,
        "panic" => TokenKind::Panic,
        "param" => TokenKind::Param,
        "private" => TokenKind::Private,
        "public" => TokenKind::Public,
        "return" => TokenKind::Return,
        "script" => TokenKind::Script,
        "true" => TokenKind::True,
        "val" => TokenKind::Val,
        "var" => TokenKind::Var,
        "when" => TokenKind::When,
        _ => return None,
    })
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}
