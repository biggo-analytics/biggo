use std::sync::Arc;

use crate::ast::{
    Arg, Ast, BinaryOp, Date, Expr, ExprId, FieldType, Ident, LambdaParam, MatchArm, Param,
    Pattern, Stmt, StmtKind, TypeExpr, TypeKind, UnaryOp,
};
use crate::diag::Diagnostic;
use crate::intern::{Interner, Symbol};
use crate::lexer::{Lexed, lex};
use crate::scalar;
use crate::span::Span;
use crate::token::{Token, TokenKind};

/// The result of parsing one source file. When there are diagnostics, `ast` holds only the
/// statements that parsed cleanly.
#[derive(Debug)]
pub struct Parsed {
    pub ast: Ast,
    pub diagnostics: Vec<Diagnostic>,
    /// The `//` comments, in source order, for tools that rewrite the source.
    pub comments: Vec<Span>,
    /// The only problem is that the input stops too early, as in `f(1,` or an unclosed block.
    /// An interactive prompt should read another line instead of reporting the error.
    pub incomplete: bool,
}

pub fn parse(source: &str, interner: &mut Interner) -> Parsed {
    if u32::try_from(source.len()).is_err() {
        let diag = Diagnostic::new(Span::new(0, 0), "source file is larger than 4 GiB");
        return Parsed {
            ast: Ast::default(),
            diagnostics: vec![diag],
            comments: Vec::new(),
            incomplete: false,
        };
    }
    let Lexed {
        tokens,
        diags,
        comments,
    } = lex(source);
    let mut parser = Parser {
        src: source,
        tokens,
        pos: 0,
        depth: 0,
        nesting: 0,
        hit_eof: false,
        speculating: false,
        ast: Ast::default(),
        diags,
        interner,
    };
    parser.program();
    parser.diags.sort_by_key(|diag| diag.span.start);
    Parsed {
        ast: parser.ast,
        comments,
        incomplete: parser.hit_eof && parser.diags.len() == 1,
        diagnostics: parser.diags,
    }
}

/// Marks a syntax error whose diagnostic has already been recorded.
struct ParseError;

type PResult<T> = Result<T, ParseError>;

enum Infix {
    Pipe,
    Binary(BinaryOp),
}

/// How deeply expressions and types may nest. The parser and everything that walks the tree
/// recurse once per level, so this bounds their stack use.
const MAX_NESTING: u32 = 256;

const NOT_POWER: u8 = 7;
const NEG_POWER: u8 = 17;

/// The left and right binding powers of an infix operator; a higher power binds tighter.
fn infix(kind: TokenKind) -> Option<(Infix, u8, u8)> {
    let (op, left, right) = match kind {
        TokenKind::Pipe => return Some((Infix::Pipe, 1, 2)),
        TokenKind::Or => (BinaryOp::Or, 3, 4),
        TokenKind::And => (BinaryOp::And, 5, 6),
        TokenKind::EqEq => (BinaryOp::Eq, 9, 10),
        TokenKind::NotEq => (BinaryOp::Ne, 9, 10),
        TokenKind::Lt => (BinaryOp::Lt, 9, 10),
        TokenKind::LtEq => (BinaryOp::Le, 9, 10),
        TokenKind::Gt => (BinaryOp::Gt, 9, 10),
        TokenKind::GtEq => (BinaryOp::Ge, 9, 10),
        // Right-associative: `a ?? b ?? c` is `a ?? (b ?? c)`.
        TokenKind::Coalesce => (BinaryOp::Coalesce, 12, 11),
        TokenKind::Plus => (BinaryOp::Add, 13, 14),
        TokenKind::Minus => (BinaryOp::Sub, 13, 14),
        TokenKind::Star => (BinaryOp::Mul, 15, 16),
        TokenKind::Slash => (BinaryOp::Div, 15, 16),
        TokenKind::Percent => (BinaryOp::Rem, 15, 16),
        _ => return None,
    };
    Some((Infix::Binary(op), left, right))
}

fn starts_expr(kind: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        kind,
        Ident
            | Int
            | Float
            | Str
            | Decimal
            | Date
            | Match
            | True
            | False
            | Null
            | LParen
            | LBracket
            | LBrace
            | If
            | Minus
            | Not
    )
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    /// Number of `(` and `[` open in the current block. A line break can end a statement only
    /// while this is zero.
    depth: u32,
    /// Current recursion depth of `nested`.
    nesting: u32,
    /// Whether an error was reported because the input ended.
    hit_eof: bool,
    /// Set while trying a parse that may be undone, which must not change the tokens.
    speculating: bool,
    ast: Ast,
    diags: Vec<Diagnostic>,
    interner: &'a mut Interner,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    fn peek_second(&self) -> Token {
        self.tokens[(self.pos + 1).min(self.tokens.len() - 1)]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn bump(&mut self) -> Token {
        let tok = self.peek();
        if tok.kind != TokenKind::Eof {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        let found = self.at(kind);
        if found {
            self.bump();
        }
        found
    }

    fn text(&self, span: Span) -> &'a str {
        &self.src[span.range()]
    }

    /// Interns the name written by an identifier token, without the backticks of a quoted one.
    fn name(&mut self, span: Span) -> Symbol {
        let text = self.text(span);
        self.interner.intern(text.trim_matches('`'))
    }

    /// The end of the last consumed token.
    fn prev_end(&self) -> u32 {
        match self.pos.checked_sub(1) {
            Some(prev) => self.tokens[prev].span.end,
            None => 0,
        }
    }

    fn error(&mut self, span: Span, message: impl Into<String>) -> ParseError {
        self.diags.push(Diagnostic::new(span, message));
        ParseError
    }

    /// Reports that the current token is not `what`.
    fn expected(&mut self, what: &str) -> ParseError {
        let tok = self.peek();
        match tok.kind {
            // The lexer has already reported this token.
            TokenKind::Error => ParseError,
            // When the line or file ran out, point at where it stopped, not at whatever follows.
            kind if kind == TokenKind::Eof || (tok.newline_before && self.pos > 0) => {
                let end = self.prev_end();
                let found = if kind == TokenKind::Eof {
                    self.hit_eof = true;
                    "file"
                } else {
                    "line"
                };
                let message = format!("expected {what}, found end of {found}");
                self.error(Span::new(end, end), message)
            }
            _ => {
                let message = format!("expected {what}, found `{}`", self.text(tok.span));
                self.error(tok.span, message)
            }
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            Err(self.expected(what))
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        let tok = self.expect(TokenKind::Ident, what)?;
        Ok(Ident {
            name: self.name(tok.span),
            span: tok.span,
        })
    }

    /// Consumes the token that closes a list. The `>` that closes type arguments may be the
    /// first half of a `>=`, as in `let x: list<int>= []`; the `=` is then left as a token.
    fn eat_close(&mut self, close: TokenKind) -> bool {
        if close == TokenKind::Gt && self.at(TokenKind::GtEq) && !self.speculating {
            let token = &mut self.tokens[self.pos];
            token.kind = TokenKind::Eq;
            token.span.start += 1;
            token.newline_before = false;
            return true;
        }
        self.eat(close)
    }

    /// Parses `item, item, ...` with an optional trailing comma, through the closing token.
    /// The opening token must already be consumed.
    fn comma_list<T>(
        &mut self,
        close: TokenKind,
        close_text: &str,
        item: fn(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        let mut items = Vec::new();
        while !self.eat_close(close) {
            items.push(item(self)?);
            if !self.eat(TokenKind::Comma) {
                if !self.eat_close(close) {
                    return Err(self.expected(&format!("`,` or {close_text}")));
                }
                break;
            }
        }
        Ok(items)
    }

    /// Runs one level of a recursive rule, rejecting input that nests too deeply.
    fn nested<T>(&mut self, rule: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if self.nesting == MAX_NESTING {
            let span = self.peek().span;
            let message = format!("nesting is deeper than {MAX_NESTING} levels");
            return Err(self.error(span, message));
        }
        self.nesting += 1;
        let result = rule(self);
        self.nesting -= 1;
        result
    }

    fn program(&mut self) {
        loop {
            let mut stmts = self.stmts();
            self.ast.stmts.append(&mut stmts);
            if self.at(TokenKind::Eof) {
                return;
            }
            let brace = self.bump();
            self.error(brace.span, "unmatched `}`");
        }
    }

    /// Parses statements up to a `}` or the end of the file. A statement with a syntax error is
    /// reported and dropped, and parsing resumes at the next statement.
    fn stmts(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::Eof | TokenKind::RBrace) {
            let start = self.pos;
            match self.stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(ParseError) => {
                    self.synchronize();
                    if self.pos == start {
                        self.bump();
                    }
                }
            }
        }
        stmts
    }

    /// Skips to where the next statement most likely starts.
    fn synchronize(&mut self) {
        use TokenKind::*;
        // Brackets that the failed statement left open.
        let mut depth = std::mem::take(&mut self.depth);
        loop {
            let tok = self.peek();
            match tok.kind {
                Eof => return,
                // A declaration at the start of a line begins a statement even when a bracket is
                // unclosed; otherwise one missing `)` would swallow the rest of the file.
                Let | Type | Fn | Import if tok.newline_before => return,
                RBrace if depth == 0 => return,
                kind if depth == 0 && tok.newline_before && starts_expr(kind) => return,
                LParen | LBracket | LBrace => depth += 1,
                RParen | RBracket | RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.peek().span.start;
        let kind = match self.peek().kind {
            TokenKind::Let => self.let_stmt()?,
            TokenKind::Type => self.type_stmt()?,
            // `fn (` starts a lambda, which is an expression.
            TokenKind::Fn if self.peek_second().kind != TokenKind::LParen => self.fn_stmt()?,
            TokenKind::Import => {
                self.bump();
                let path = self.expect(TokenKind::Str, "the path of a file, in quotes")?;
                StmtKind::Import {
                    path: self.str_value(path.span)?,
                }
            }
            _ => StmtKind::Expr(self.expr()?),
        };
        let span = Span::new(start, self.prev_end());
        let next = self.peek();
        if !next.newline_before && !matches!(next.kind, TokenKind::Eof | TokenKind::RBrace) {
            return Err(self.expected("end of line"));
        }
        Ok(Stmt { kind, span })
    }

    fn let_stmt(&mut self) -> PResult<StmtKind> {
        self.bump();
        let name = self.ident("a variable name")?;
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.type_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Eq, "`=`")?;
        let value = self.expr()?;
        Ok(StmtKind::Let { name, ty, value })
    }

    fn type_stmt(&mut self) -> PResult<StmtKind> {
        self.bump();
        let name = self.ident("a type name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        let ty = self.type_expr()?;
        Ok(StmtKind::Type { name, ty })
    }

    fn fn_stmt(&mut self) -> PResult<StmtKind> {
        self.bump();
        let name = self.ident("a function name")?;
        self.expect(TokenKind::LParen, "`(`")?;
        let params = self.comma_list(TokenKind::RParen, "`)`", Self::param)?;
        let ret = if self.eat(TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(StmtKind::Fn {
            name,
            params,
            ret,
            body,
        })
    }

    fn param(&mut self) -> PResult<Param> {
        let name = self.ident("a parameter name")?;
        self.expect(TokenKind::Colon, "`:`")?;
        let ty = self.type_expr()?;
        Ok(Param { name, ty })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        self.nested(Self::type_expr_level)
    }

    fn type_expr_level(&mut self) -> PResult<TypeExpr> {
        let start = self.peek().span.start;
        let kind = match self.peek().kind {
            TokenKind::Ident => {
                let name = self.ident("a type")?.name;
                let args = if self.eat(TokenKind::Lt) {
                    self.comma_list(TokenKind::Gt, "`>`", Self::type_expr)?
                } else {
                    Vec::new()
                };
                TypeKind::Named { name, args }
            }
            TokenKind::LBrace => {
                self.bump();
                TypeKind::Record(self.comma_list(TokenKind::RBrace, "`}`", Self::field_type)?)
            }
            TokenKind::Fn => {
                self.bump();
                self.expect(TokenKind::LParen, "`(`")?;
                let params = self.comma_list(TokenKind::RParen, "`)`", Self::type_expr)?;
                let ret = match self.eat(TokenKind::Arrow) {
                    true => Some(Box::new(self.type_expr()?)),
                    false => None,
                };
                // `fn() -> int?` returns a nullable int; the function itself is not nullable.
                let span = Span::new(start, self.prev_end());
                let kind = TypeKind::Fn { params, ret };
                return Ok(TypeExpr { kind, span });
            }
            _ => return Err(self.expected("a type")),
        };
        let ty = TypeExpr {
            kind,
            span: Span::new(start, self.prev_end()),
        };
        if !self.eat(TokenKind::Question) {
            return Ok(ty);
        }
        Ok(TypeExpr {
            kind: TypeKind::Nullable(Box::new(ty)),
            span: Span::new(start, self.prev_end()),
        })
    }

    fn field_type(&mut self) -> PResult<FieldType> {
        let name = self.ident("a field name")?;
        self.expect(TokenKind::Colon, "`:`")?;
        let ty = self.type_expr()?;
        Ok(FieldType { name, ty })
    }

    fn expr(&mut self) -> PResult<ExprId> {
        self.expr_bp(0)
    }

    /// Pratt parser: parses an expression whose infix operators all bind at least as tightly as
    /// `min_power`.
    fn expr_bp(&mut self, min_power: u8) -> PResult<ExprId> {
        self.nested(|parser| parser.expr_level(min_power))
    }

    fn expr_level(&mut self, min_power: u8) -> PResult<ExprId> {
        // Spans run from the first token to the last one consumed, so that they take in the
        // parentheses around an operand: all of `a * (b + c)`, not `a * (b + c`.
        let start = self.peek().span.start;
        let mut lhs = self.prefix()?;
        // Whether `lhs` is a comparison written without parentheses.
        let mut lhs_is_comparison = false;
        loop {
            let tok = self.peek();
            // Tokens that can also begin a statement (`(` and `-`) only continue this expression
            // from the same line, unless an open bracket shows the expression is unfinished.
            let same_line = !tok.newline_before || self.depth > 0;
            match tok.kind {
                TokenKind::LParen if same_line => {
                    lhs = self.call(start, lhs, Vec::new())?;
                    continue;
                }
                TokenKind::LBracket if same_line => {
                    self.bump();
                    self.depth += 1;
                    let index = self.expr()?;
                    self.expect(TokenKind::RBracket, "`]`")?;
                    self.depth -= 1;
                    let span = Span::new(start, self.prev_end());
                    lhs = self.ast.alloc(Expr::Index { base: lhs, index }, span);
                    continue;
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.ident("a field name")?;
                    let span = Span::new(start, name.span.end);
                    lhs = self.ast.alloc(Expr::Field { base: lhs, name }, span);
                    continue;
                }
                TokenKind::Lt if same_line && self.is_path(lhs) => {
                    if let Some(type_args) = self.type_args() {
                        lhs = self.call(start, lhs, type_args)?;
                        continue;
                    }
                }
                _ => {}
            }

            let Some((op, left, right)) = infix(tok.kind) else {
                break;
            };
            if left < min_power || (tok.kind == TokenKind::Minus && !same_line) {
                break;
            }
            self.bump();
            let rhs = self.expr_bp(right)?;
            let span = Span::new(start, self.prev_end());
            let expr = match op {
                Infix::Pipe => {
                    self.check_pipe_stage(rhs)?;
                    lhs_is_comparison = false;
                    Expr::Pipe { lhs, rhs }
                }
                Infix::Binary(op) => {
                    if op.is_comparison() && lhs_is_comparison {
                        let message =
                            "comparison operators cannot be chained; use `and` to combine them";
                        return Err(self.error(tok.span, message));
                    }
                    lhs_is_comparison = op.is_comparison();
                    Expr::Binary { op, lhs, rhs }
                }
            };
            lhs = self.ast.alloc(expr, span);
        }
        Ok(lhs)
    }

    fn check_pipe_stage(&mut self, stage: ExprId) -> PResult<()> {
        // `|>` binds loosest, so in `t |> count() > 1` the stage is `count() > 1`.
        let hint = match self.ast.expr(stage) {
            Expr::Call { .. } => return Ok(()),
            Expr::Binary { .. } => {
                "; put the pipeline in parentheses to use its result in a larger expression"
            }
            _ => "",
        };
        let message = format!("the right side of `|>` must be a function call{hint}");
        Err(self.error(self.ast.span(stage), message))
    }

    fn is_path(&self, expr: ExprId) -> bool {
        matches!(self.ast.expr(expr), Expr::Name(_) | Expr::Field { .. })
    }

    /// Tries to read `<T, ...>` as the type arguments of a call, consuming nothing if it is not.
    /// `f < a > (b)` could also be two comparisons; as in C# and Swift it is a call, because the
    /// `>` is followed by `(`.
    fn type_args(&mut self) -> Option<Vec<TypeExpr>> {
        let (pos, diags) = (self.pos, self.diags.len());
        self.bump();
        self.speculating = true;
        let args = self.comma_list(TokenKind::Gt, "`>`", Self::type_expr);
        self.speculating = false;
        if let Ok(args) = args
            && !args.is_empty()
            && self.at(TokenKind::LParen)
        {
            return Some(args);
        }
        self.pos = pos;
        self.diags.truncate(diags);
        None
    }

    /// Parses the argument list of a call whose callee began at `start`.
    fn call(&mut self, start: u32, callee: ExprId, type_args: Vec<TypeExpr>) -> PResult<ExprId> {
        self.bump();
        self.depth += 1;
        let args = self.comma_list(TokenKind::RParen, "`)`", Self::arg)?;
        self.depth -= 1;
        let span = Span::new(start, self.prev_end());
        let call = Expr::Call {
            callee,
            type_args,
            args,
        };
        Ok(self.ast.alloc(call, span))
    }

    fn arg(&mut self) -> PResult<Arg> {
        let name = if self.at(TokenKind::Ident) && self.peek_second().kind == TokenKind::Eq {
            let name = self.ident("an argument name")?;
            self.bump();
            Some(name)
        } else {
            None
        };
        let value = self.expr()?;
        Ok(Arg { name, value })
    }

    fn prefix(&mut self) -> PResult<ExprId> {
        let tok = self.peek();
        let expr = match tok.kind {
            TokenKind::Int => Expr::Int(self.int_value(tok.span)?),
            TokenKind::Float => Expr::Float(self.float_value(tok.span)?),
            TokenKind::Str => Expr::Str(self.str_value(tok.span)?),
            TokenKind::Decimal => Expr::Decimal(self.decimal_value(tok.span)?),
            TokenKind::Date if self.text(tok.span).contains('T') => {
                Expr::DateTime(self.datetime_value(tok.span)?)
            }
            TokenKind::Date => Expr::Date(self.date_value(tok.span)?),
            TokenKind::True => Expr::Bool(true),
            TokenKind::False => Expr::Bool(false),
            TokenKind::Null => Expr::Null,
            TokenKind::Ident => Expr::Name(self.name(tok.span)),
            TokenKind::Minus => return self.unary(UnaryOp::Neg, NEG_POWER),
            TokenKind::Not => return self.unary(UnaryOp::Not, NOT_POWER),
            TokenKind::LParen => {
                self.bump();
                self.depth += 1;
                let inner = self.expr()?;
                self.expect(TokenKind::RParen, "`)`")?;
                self.depth -= 1;
                return Ok(inner);
            }
            TokenKind::LBracket => {
                self.bump();
                self.depth += 1;
                let items = self.comma_list(TokenKind::RBracket, "`]`", Self::expr)?;
                self.depth -= 1;
                let span = Span::new(tok.span.start, self.prev_end());
                return Ok(self.ast.alloc(Expr::List(items), span));
            }
            TokenKind::LBrace => return self.brace(),
            TokenKind::If => return self.if_expr(),
            TokenKind::Match => return self.match_expr(),
            TokenKind::Fn => return self.lambda(),
            _ => return Err(self.expected("an expression")),
        };
        self.bump();
        Ok(self.ast.alloc(expr, tok.span))
    }

    fn unary(&mut self, op: UnaryOp, power: u8) -> PResult<ExprId> {
        let start = self.bump().span.start;
        let operand = self.expr_bp(power)?;
        let span = Span::new(start, self.prev_end());
        Ok(self.ast.alloc(Expr::Unary { op, operand }, span))
    }

    /// Parses what a `{` starts: a record `{ name: value }`, a map `{ "key": value }`, or a
    /// block. No statement begins with a name or a literal followed by `:`.
    fn brace(&mut self) -> PResult<ExprId> {
        let first = self.peek_second().kind;
        let then = self.tokens[(self.pos + 2).min(self.tokens.len() - 1)].kind;
        if then != TokenKind::Colon {
            return self.block();
        }
        let open = self.peek().span;
        let literal = |kind| {
            use TokenKind::*;
            matches!(kind, Str | Int | Float | Decimal | Date | True | False)
        };
        let expr = if first == TokenKind::Ident {
            self.bump();
            self.depth += 1;
            let fields = self.comma_list(TokenKind::RBrace, "`}`", |parser| {
                let name = parser.ident("a field name")?;
                parser.expect(TokenKind::Colon, "`:`")?;
                Ok((name, parser.expr()?))
            })?;
            Expr::Record(fields)
        } else if literal(first) {
            self.bump();
            self.depth += 1;
            let entries = self.comma_list(TokenKind::RBrace, "`}`", |parser| {
                let key = parser.expr()?;
                parser.expect(TokenKind::Colon, "`:`")?;
                Ok((key, parser.expr()?))
            })?;
            Expr::Map(entries)
        } else {
            return self.block();
        };
        self.depth -= 1;
        Ok(self.ast.alloc(expr, Span::new(open.start, self.prev_end())))
    }

    fn lambda(&mut self) -> PResult<ExprId> {
        let start = self.bump().span.start;
        self.expect(TokenKind::LParen, "`(`")?;
        let params = self.comma_list(TokenKind::RParen, "`)`", |parser| {
            let name = parser.ident("a parameter name")?;
            let ty = match parser.eat(TokenKind::Colon) {
                true => Some(parser.type_expr()?),
                false => None,
            };
            Ok(LambdaParam { name, ty })
        })?;
        let ret = match self.eat(TokenKind::Arrow) {
            true => Some(self.type_expr()?),
            false => None,
        };
        let body = self.block()?;
        let span = Span::new(start, self.prev_end());
        Ok(self.ast.alloc(Expr::Lambda { params, ret, body }, span))
    }

    fn match_expr(&mut self) -> PResult<ExprId> {
        let start = self.bump().span.start;
        let scrutinee = self.expr()?;
        self.expect(TokenKind::LBrace, "`{`")?;
        // Like the statements of a block, arms end at a line break.
        let outer_depth = std::mem::take(&mut self.depth);
        let Ok(arms) = self.match_arms() else {
            // Error recovery skips to the end of what is open: here, the brace of the match.
            self.depth = 1;
            return Err(ParseError);
        };
        self.depth = outer_depth;
        self.bump();
        let span = Span::new(start, self.prev_end());
        Ok(self.ast.alloc(Expr::Match { scrutinee, arms }, span))
    }

    /// Parses the arms of a `match` up to its closing brace.
    fn match_arms(&mut self) -> PResult<Vec<MatchArm>> {
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) {
            let mut patterns = vec![self.pattern()?];
            while self.eat(TokenKind::Bar) {
                patterns.push(self.pattern()?);
            }
            self.expect(TokenKind::FatArrow, "`=>`")?;
            let body = self.expr()?;
            arms.push(MatchArm { patterns, body });
            let next = self.peek();
            if !self.eat(TokenKind::Comma) && next.kind != TokenKind::RBrace && !next.newline_before
            {
                return Err(self.expected("`,` or a new line"));
            }
        }
        Ok(arms)
    }

    /// A pattern of a `match` arm: `_`, or a literal.
    fn pattern(&mut self) -> PResult<Pattern> {
        use TokenKind::*;
        let tok = self.peek();
        match tok.kind {
            Ident if self.text(tok.span) == "_" => {
                self.bump();
                Ok(Pattern::Wildcard(tok.span))
            }
            Int | Float | Decimal | Str | Date | True | False | Null | Minus => {
                Ok(Pattern::Value(self.expr_bp(NEG_POWER)?))
            }
            _ => Err(self.expected("a pattern, which is a literal or `_`")),
        }
    }

    fn block(&mut self) -> PResult<ExprId> {
        let open = self.expect(TokenKind::LBrace, "`{`")?;
        // Line breaks separate statements inside the block even if it sits inside parentheses.
        let outer_depth = std::mem::take(&mut self.depth);
        let stmts = self.stmts();
        self.depth = outer_depth;
        let close = self.expect(TokenKind::RBrace, "`}`")?;
        Ok(self.ast.alloc(Expr::Block(stmts), open.span.to(close.span)))
    }

    fn if_expr(&mut self) -> PResult<ExprId> {
        let start = self.bump().span.start;
        let cond = self.expr()?;
        let then_branch = self.block()?;
        let else_branch = if !self.eat(TokenKind::Else) {
            None
        } else if self.at(TokenKind::If) {
            Some(self.if_expr()?)
        } else {
            Some(self.block()?)
        };
        let span = Span::new(start, self.prev_end());
        let expr = Expr::If {
            cond,
            then_branch,
            else_branch,
        };
        Ok(self.ast.alloc(expr, span))
    }

    fn int_value(&mut self, span: Span) -> PResult<i64> {
        // The lexer guarantees digits and underscores, so overflow is the only way to fail.
        match self.text(span).replace('_', "").parse() {
            Ok(value) => Ok(value),
            Err(_) => Err(self.error(span, "integer literal is too large")),
        }
    }

    fn float_value(&mut self, span: Span) -> PResult<f64> {
        match self.text(span).replace('_', "").parse::<f64>() {
            Ok(value) if value.is_finite() => Ok(value),
            _ => Err(self.error(span, "float literal is out of range")),
        }
    }

    fn str_value(&mut self, span: Span) -> PResult<Arc<str>> {
        let body_start = span.start as usize + 1;
        let body = &self.src[body_start..span.end as usize - 1];
        if !body.contains('\\') {
            return Ok(body.into());
        }
        let mut value = String::with_capacity(body.len());
        let mut chars = body.char_indices();
        while let Some((offset, c)) = chars.next() {
            if c != '\\' {
                value.push(c);
                continue;
            }
            // The lexer only ends a string at an unescaped quote, so a character always follows.
            let Some((_, escape)) = chars.next() else {
                break;
            };
            value.push(match escape {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '0' => '\0',
                '\\' => '\\',
                '"' => '"',
                _ => {
                    let start = body_start + offset;
                    let end = start + 1 + escape.len_utf8();
                    let message = format!("unknown escape sequence `\\{escape}`");
                    return Err(self.error(Span::new(start as u32, end as u32), message));
                }
            });
        }
        Ok(value.into())
    }

    fn decimal_value(&mut self, span: Span) -> PResult<i128> {
        let text = self.text(span).trim_end_matches('d').replace('_', "");
        match scalar::parse_decimal(&text) {
            Some(value) => Ok(value),
            None => {
                let places = scalar::DECIMAL_SCALE;
                let message = format!(
                    "a decimal holds at most {places} digits after the point and 32 before it"
                );
                Err(self.error(span, message))
            }
        }
    }

    fn datetime_value(&mut self, span: Span) -> PResult<i64> {
        let text = self.text(span);
        match scalar::parse_datetime(&text[1..]) {
            Some(micros) => Ok(micros),
            None => {
                let message = format!(
                    "invalid datetime literal `{text}`; expected a real date and time written \
                     as `@YYYY-MM-DDTHH:MM:SS`"
                );
                Err(self.error(span, message))
            }
        }
    }

    fn date_value(&mut self, span: Span) -> PResult<Date> {
        let text = self.text(span);
        match Date::parse(&text[1..]) {
            Some(date) => Ok(date),
            None => {
                let message = format!(
                    "invalid date literal `{text}`; expected a calendar date written as `@YYYY-MM-DD`"
                );
                Err(self.error(span, message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(source: &str) -> String {
        let mut interner = Interner::new();
        let parsed = parse(source, &mut interner);
        assert!(
            parsed.diagnostics.is_empty(),
            "unexpected diagnostics for {source:?}: {:?}",
            parsed.diagnostics
        );
        parsed.ast.dump(&interner).trim_end().to_string()
    }

    fn errors(source: &str) -> Vec<String> {
        let parsed = parse(source, &mut Interner::new());
        parsed.diagnostics.into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(dump("1 + 2 * 3"), "(+ 1 (* 2 3))");
        assert_eq!(dump("(1 + 2) * 3"), "(* (+ 1 2) 3)");
        assert_eq!(dump("a - b - c"), "(- (- a b) c)");
        assert_eq!(dump("a / b % c"), "(% (/ a b) c)");
        assert_eq!(dump("-a * b"), "(* (- a) b)");
        assert_eq!(dump("-f(x).y"), "(- (. (call f x) y))");
    }

    #[test]
    fn logic_and_comparison_precedence() {
        assert_eq!(
            dump("not a == b and c or d"),
            "(or (and (not (== a b)) c) d)"
        );
        assert_eq!(dump("a + 1 >= b * 2"), "(>= (+ a 1) (* b 2))");
        assert_eq!(dump("(a < b) == c"), "(== (< a b) c)");
    }

    #[test]
    fn coalesce_is_right_associative_and_tighter_than_comparison() {
        assert_eq!(dump("a ?? b ?? c + 1 > 2"), "(> (?? a (?? b (+ c 1))) 2)");
    }

    #[test]
    fn pipe_binds_loosest_and_chains_left() {
        assert_eq!(
            dump("a + b |> f() |> g(1)"),
            "(|> (+ a b) (call f) (call g 1))"
        );
        assert_eq!(dump("(t |> count()) > 1"), "(> (|> t (call count)) 1)");
    }

    #[test]
    fn calls_and_named_arguments() {
        assert_eq!(
            dump("f(a, b = 1 + 2, c = g(),)"),
            "(call f a (b: (+ 1 2)) (c: (call g)))"
        );
        assert_eq!(dump("f(a == b)"), "(call f (== a b))");
        assert_eq!(dump("a.b.c(1)(2)"), "(call (call (. (. a b) c) 1) 2)");
    }

    #[test]
    fn generic_call_or_comparison() {
        assert_eq!(dump("f<T>(x)"), "(call f <T> x)");
        assert_eq!(
            dump("read<T, table<{a: int?}>>(x).name"),
            "(. (call read <T, table<{a: int?}>> x) name)"
        );
        assert_eq!(dump("a < b"), "(< a b)");
        assert_eq!(dump("a < b and c > (d)"), "(and (< a b) (> c d))");
        assert_eq!(dump("if a < b { c }"), "(if (< a b) (block c))");
    }

    #[test]
    fn line_breaks_end_statements() {
        assert_eq!(dump("a\n(b)"), "a\nb");
        assert_eq!(dump("a\n-b"), "a\n(- b)");
        assert_eq!(dump("f(a\n-b)"), "(call f (- a b))");
        assert_eq!(dump("f(a\n(b))"), "(call f (call a b))");
        assert_eq!(dump("a +\nb"), "(+ a b)");
        assert_eq!(dump("a\n  |> f()\n  |> g()"), "(|> a (call f) (call g))");
        assert_eq!(dump("f({\n  a\n  (b)\n})"), "(call f (block a b))");
    }

    #[test]
    fn statements() {
        assert_eq!(dump("let x: int? = null"), "(let x: int? null)");
        assert_eq!(
            dump("type T = table<{a: int, b: list<string>?}>"),
            "(type T table<{a: int, b: list<string>?}>)"
        );
        assert_eq!(
            dump("fn add(a: int, b: int) -> int { a + b }"),
            "(fn add(a: int, b: int) -> int (block (+ a b)))"
        );
        assert_eq!(dump("fn noop() {}"), "(fn noop() (block))");
        assert_eq!(
            dump("if a { 1 } else if b { 2 }\nelse { 3 }"),
            "(if a (block 1) (if b (block 2) (block 3)))"
        );
    }

    #[test]
    fn literals() {
        assert_eq!(
            dump("[1_000, 2.5, 2e3, true, null, @2024-02-29]"),
            "(list 1000 2.5 2000.0 true null @2024-02-29)"
        );
        assert_eq!(dump(r#""a\tb\"c\\""#), r#""a\tb\"c\\""#);
        assert_eq!(dump("\"ไทย\""), "\"ไทย\"");
    }

    #[test]
    fn reports_syntax_errors() {
        assert_eq!(
            errors("let x = (1 + 2"),
            ["expected `)`, found end of file"]
        );
        assert_eq!(errors("f(a b)"), ["expected `,` or `)`, found `b`"]);
        assert_eq!(errors("a b"), ["expected end of line, found `b`"]);
        assert_eq!(errors("let = 1"), ["expected a variable name, found `=`"]);
        assert_eq!(
            errors("let x = 1 +"),
            ["expected an expression, found end of file"]
        );
        assert_eq!(errors("fn f(a) {}"), ["expected `:`, found `)`"]);
        assert_eq!(errors("let x: = 1"), ["expected a type, found `=`"]);
        assert_eq!(errors("}"), ["unmatched `}`"]);
        assert_eq!(
            errors("a < b < c"),
            ["comparison operators cannot be chained; use `and` to combine them"]
        );
    }

    #[test]
    fn pipe_stage_must_be_a_call() {
        assert_eq!(
            errors("x |> f"),
            ["the right side of `|>` must be a function call"]
        );
        assert_eq!(
            errors("t |> count() > 1"),
            [
                "the right side of `|>` must be a function call; put the pipeline in parentheses to \
             use its result in a larger expression"
            ]
        );
    }

    #[test]
    fn reports_invalid_literals() {
        assert_eq!(
            errors("99999999999999999999"),
            ["integer literal is too large"]
        );
        assert_eq!(errors("1e999"), ["float literal is out of range"]);
        assert_eq!(errors(r#""\q""#), ["unknown escape sequence `\\q`"]);
        for date in ["@2026-02-30", "@2026-13-01", "@2026-1-1", "@"] {
            let message = format!(
                "invalid date literal `{date}`; expected a calendar date written as `@YYYY-MM-DD`"
            );
            assert_eq!(errors(date), [message]);
        }
    }

    #[test]
    fn unfinished_line_is_reported_where_it_stops() {
        let source = "let t = (1 + 2 // note\nlet u = 3";
        let parsed = parse(source, &mut Interner::new());
        let expected = Diagnostic::new(Span::new(14, 14), "expected `)`, found end of line");
        assert_eq!(parsed.diagnostics, [expected]);
    }

    #[test]
    fn spans_include_the_parentheses_of_operands() {
        let parsed = parse("net * (1 + vat)", &mut Interner::new());
        let StmtKind::Expr(product) = &parsed.ast.stmts[0].kind else {
            panic!("expected an expression statement");
        };
        let Expr::Binary { rhs: sum, .. } = parsed.ast.expr(*product) else {
            panic!("expected a binary expression");
        };
        assert_eq!(parsed.ast.span(*product), Span::new(0, 15));
        assert_eq!(parsed.ast.span(*sum), Span::new(7, 14));
    }

    #[test]
    fn type_arguments_may_end_in_a_comparison_token() {
        assert_eq!(
            dump("let xs: list<int>= [1]"),
            "(let xs: list<int> (list 1))"
        );
        // Trying `a<b>` as type arguments must leave the `>=` whole when it is not one.
        assert_eq!(dump("a < b + 1\nb >= c"), "(< a (+ b 1))\n(>= b c)");
        assert_eq!(
            errors("a < b >= c"),
            ["comparison operators cannot be chained; use `and` to combine them"]
        );
    }

    #[test]
    fn lambdas_and_function_types() {
        assert_eq!(
            dump("let f: fn(int, int) -> int? = fn(a, b: int) -> int? { a + b }"),
            "(let f: fn(int, int) -> int? (lambda (a, b: int) -> int? (block (+ a b))))"
        );
        assert_eq!(
            dump("xs |> map(fn(x) { x * 2 })"),
            "(|> xs (call map (lambda (x) (block (* x 2)))))"
        );
        assert_eq!(dump("fn(x) { x }(1)"), "(call (lambda (x) (block x)) 1)");
        assert_eq!(dump("let g: fn() = h"), "(let g: fn() h)");
    }

    #[test]
    fn match_expressions() {
        assert_eq!(
            dump("match x {\n  1 | 2 => \"low\"\n  -1 => \"neg\",\n  _ => { y }\n}"),
            "(match x (arm 1 2 \"low\") (arm (- 1) \"neg\") (arm _ (block y)))"
        );
        assert_eq!(
            dump("match f(a) { true => 1, false => 2 } + 1"),
            "(+ (match (call f a) (arm true 1) (arm false 2)) 1)"
        );
        assert_eq!(
            errors("match x { 1 => 2 3 => 4 }"),
            ["expected `,` or a new line, found `3`"]
        );
        assert_eq!(
            errors("match x { y => 2 }"),
            ["expected a pattern, which is a literal or `_`, found `y`"]
        );
    }

    #[test]
    fn records_maps_and_indexing() {
        assert_eq!(
            dump("{ name: \"a\", qty: 1 + 2 }"),
            "(record (name: \"a\") (qty: (+ 1 2)))"
        );
        assert_eq!(
            dump("{\n  \"a\": 1,\n  \"b\": 2,\n}"),
            "(map (\"a\" 1) (\"b\" 2))"
        );
        assert_eq!(dump("r.name[0]"), "(index (. r name) 0)");
        assert_eq!(
            dump("m[\"k\"] ?? xs[i + 1]"),
            "(?? (index m \"k\") (index xs (+ i 1)))"
        );
        // A list on its own line is a new statement, not an index.
        assert_eq!(dump("x\n[1]"), "x\n(list 1)");
        // A brace that does not open with `name:` or a literal key is still a block.
        assert_eq!(dump("{ a }"), "(block a)");
        assert_eq!(dump("{}"), "(block)");
    }

    #[test]
    fn imports_decimals_and_datetimes() {
        assert_eq!(
            dump("import \"lib/util.bgo\"\nlet x = 1"),
            "(import \"lib/util.bgo\")\n(let x 1)"
        );
        assert_eq!(dump("[19.99d, 5d, -0.5d]"), "(list 19.99d 5d (- 0.5d))");
        assert_eq!(dump("@2026-01-02T03:04:05"), "@2026-01-02T03:04:05");
        assert_eq!(dump("@2026-01-02T03:04"), "@2026-01-02T03:04:00");
        assert_eq!(dump("@2026-01-02T03:04:05.25"), "@2026-01-02T03:04:05.25");
        // A colon or a dot that no digit follows is not part of the literal.
        assert_eq!(dump("{ @2026-01-02: 1 }"), "(map (@2026-01-02 1))");
        assert_eq!(dump("@2026-01-02T03:04:05.x"), "(. @2026-01-02T03:04:05 x)");
        assert_eq!(
            errors("1.2345678d"),
            ["a decimal holds at most 6 digits after the point and 32 before it"]
        );
        assert_eq!(
            errors("@2026-01-02T25:00"),
            [
                "invalid datetime literal `@2026-01-02T25:00`; expected a real date and time written as `@YYYY-MM-DDTHH:MM:SS`"
            ]
        );
        assert_eq!(
            errors("import lib"),
            ["expected the path of a file, in quotes, found `lib`"]
        );
    }

    #[test]
    fn quoted_names() {
        assert_eq!(
            dump("t |> where(`order type` == \"a\") |> select(`if`, x = `a b`)"),
            "(|> t (call where (== order type \"a\")) (call select if (x: a b)))"
        );
    }

    #[test]
    fn named_arguments_go_anywhere() {
        // Table operations take `name = expression` among their columns in any order; the
        // type checker holds calls of ordinary functions to positional arguments first.
        assert_eq!(
            dump("t |> select(a, b = 1, c)"),
            "(|> t (call select a (b: 1) c))"
        );
    }

    #[test]
    fn limits_nesting() {
        let nested = |levels: usize| format!("{}1{}", "(".repeat(levels), ")".repeat(levels));
        assert_eq!(errors(&nested(200)), [] as [&str; 0]);
        assert_eq!(errors(&nested(300)), ["nesting is deeper than 256 levels"]);
        let generic = format!("let x: {}int{} = 1", "a<".repeat(300), ">".repeat(300));
        assert_eq!(errors(&generic), ["nesting is deeper than 256 levels"]);
    }

    #[test]
    fn incomplete_input_asks_for_more() {
        let incomplete = |source: &str| parse(source, &mut Interner::new()).incomplete;
        assert!(incomplete("f(1,"));
        assert!(incomplete("fn f() {\n  1"));
        assert!(incomplete("let x = 1 +"));
        assert!(incomplete("x |>"));
        // Complete, or broken in a way that more input cannot fix.
        assert!(!incomplete("f(1)"));
        assert!(!incomplete("let = ("));
        assert!(!incomplete("let s = \"abc"));
        assert!(!incomplete("f(1))"));
    }

    #[test]
    fn lexer_errors_are_not_reported_twice() {
        assert_eq!(errors("let x = 5 $ 3"), ["unexpected character '$'"]);
        assert_eq!(errors("let x = \"abc"), ["unterminated string literal"]);
    }

    #[test]
    fn recovers_at_the_next_statement() {
        let source = "let a = (\nlet b = 2\nlet c = 3 3\nf(x >)\nlet d = 4";
        let mut interner = Interner::new();
        let parsed = parse(source, &mut interner);
        let messages: Vec<_> = parsed.diagnostics.iter().map(|d| &d.message).collect();
        assert_eq!(
            messages,
            [
                "expected an expression, found end of line",
                "expected end of line, found `3`",
                "expected an expression, found `)`",
            ]
        );
        assert_eq!(parsed.ast.dump(&interner), "(let b 2)\n(let d 4)\n");
    }
}
