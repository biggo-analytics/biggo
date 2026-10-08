//! Formats source code in the standard style: two-space indentation, lines of up to 100
//! columns, one pipeline stage per line when a pipeline is broken. Comments are kept.

mod doc;

use biggo_syntax::ast::{
    Arg, Ast, BinaryOp, Expr, ExprId, MatchArm, Pattern, Stmt, StmtKind, TypeExpr, TypeKind,
    UnaryOp,
};
use biggo_syntax::{Diagnostic, Interner, Span};

use doc::{Doc, concat, group, indent, join, render, text};

const WIDTH: usize = 100;

/// Formats `source`. Fails with its syntax errors if it does not parse.
pub fn format(source: &str) -> Result<String, Vec<Diagnostic>> {
    let parsed = biggo_syntax::parse(source, &mut Interner::new());
    if !parsed.diagnostics.is_empty() {
        return Err(parsed.diagnostics);
    }
    let mut formatter = Formatter {
        src: source,
        ast: &parsed.ast,
        comments: &parsed.comments,
        next_comment: 0,
    };
    let file_end = source.len() as u32;
    let body = formatter.stmts(&parsed.ast.stmts, 0, file_end);
    let formatted = render(&body, WIDTH);
    Ok(if formatted.trim().is_empty() {
        String::new()
    } else {
        formatted
    })
}

/// How tightly an expression holds together: an operand whose strength is below what its
/// position needs is put in parentheses.
mod strength {
    pub const PIPE: u8 = 1;
    pub const OR: u8 = 3;
    pub const AND: u8 = 5;
    pub const NOT: u8 = 7;
    pub const COMPARE: u8 = 9;
    pub const COALESCE: u8 = 11;
    pub const ADD: u8 = 13;
    pub const MULTIPLY: u8 = 15;
    pub const NEGATE: u8 = 17;
    pub const PRIMARY: u8 = 19;
}

fn binary_strength(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        Or => strength::OR,
        And => strength::AND,
        Eq | Ne | Lt | Le | Gt | Ge | In | NotIn => strength::COMPARE,
        Coalesce => strength::COALESCE,
        Add | Sub => strength::ADD,
        Mul | Div | Rem => strength::MULTIPLY,
    }
}

struct Formatter<'a> {
    src: &'a str,
    ast: &'a Ast,
    comments: &'a [Span],
    /// The first comment that has not been placed yet.
    next_comment: usize,
}

impl<'a> Formatter<'a> {
    fn text(&self, span: Span) -> &'a str {
        &self.src[span.range()]
    }

    fn has_line_break(&self, from: u32, to: u32) -> bool {
        self.src[from as usize..to as usize].contains('\n')
    }

    fn has_blank_line(&self, from: u32, to: u32) -> bool {
        self.src[from as usize..to as usize].matches('\n').count() >= 2
    }

    fn peek_comment(&self) -> Option<Span> {
        self.comments.get(self.next_comment).copied()
    }

    /// Whether a comment that has not been placed yet starts before `end`.
    fn has_comment_before(&self, end: u32) -> bool {
        self.peek_comment()
            .is_some_and(|comment| comment.start < end)
    }

    /// The comment that follows `after` on the same line, as text for the end of that line.
    fn trailing_comment(&mut self, after: u32) -> Option<Doc> {
        let comment = self.peek_comment()?;
        // Only what closes brackets may stand between: code there has its own claim on it.
        let between = self.src.get(after as usize..comment.start as usize)?;
        if !between
            .chars()
            .all(|c| matches!(c, ' ' | '\t' | ')' | ']' | '}' | ','))
        {
            return None;
        }
        self.next_comment += 1;
        Some(Doc::LineSuffix(format!(
            "  {}",
            self.text(comment).trim_end()
        )))
    }

    /// The comments before `before`, each on a line of its own.
    fn own_line_comments(&mut self, before: u32) -> Vec<Doc> {
        let mut docs = Vec::new();
        while let Some(comment) = self.peek_comment().filter(|comment| comment.start < before) {
            self.next_comment += 1;
            docs.push(text(self.text(comment).trim_end()));
            docs.push(Doc::HardLine);
        }
        docs
    }

    /// Formats the statements of a file or block that spans `start..end`: one per line, with
    /// the comments between them, keeping single blank lines.
    fn stmts(&mut self, stmts: &[Stmt], start: u32, end: u32) -> Doc {
        // Lines are statements and comments; `previous_end` is where the last one stopped.
        let mut lines: Vec<Doc> = Vec::new();
        let mut previous_end = start;
        let mut push = |this: &Self, lines: &mut Vec<Doc>, line: Doc, from: u32, to: u32| {
            if !lines.is_empty() {
                let blank = this.has_blank_line(previous_end, from);
                lines.push(if blank { Doc::BlankLine } else { Doc::HardLine });
            }
            lines.push(line);
            previous_end = to;
        };
        for stmt in stmts {
            while let Some(comment) = self.peek_comment().filter(|c| c.start < stmt.span.start) {
                self.next_comment += 1;
                let line = text(self.text(comment).trim_end());
                push(self, &mut lines, line, comment.start, comment.end);
            }
            let mut line = vec![self.stmt(stmt)];
            // A comment inside the statement that found no place of its own moves out to
            // the end of the statement's last line, where it cannot be lost.
            while let Some(comment) = self.peek_comment().filter(|c| c.start < stmt.span.end) {
                self.next_comment += 1;
                line.push(Doc::LineSuffix(format!(
                    "  {}",
                    self.text(comment).trim_end()
                )));
            }
            line.extend(self.trailing_comment(stmt.span.end));
            push(
                self,
                &mut lines,
                concat(line),
                stmt.span.start,
                stmt.span.end,
            );
        }
        while let Some(comment) = self.peek_comment().filter(|c| c.start < end) {
            self.next_comment += 1;
            let line = text(self.text(comment).trim_end());
            push(self, &mut lines, line, comment.start, comment.end);
        }
        concat(lines)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Doc {
        match &stmt.kind {
            StmtKind::Let { name, ty, value } => {
                let mut parts = vec![text(format!("let {}", self.text(name.span)))];
                if let Some(ty) = ty {
                    parts.push(text(": "));
                    parts.push(self.type_expr(ty));
                }
                parts.push(text(" = "));
                parts.push(self.expr(*value, 0));
                concat(parts)
            }
            StmtKind::Type { name, ty } => concat([
                text(format!("type {} = ", self.text(name.span))),
                self.type_expr(ty),
            ]),
            StmtKind::Fn {
                name,
                params,
                ret,
                body,
            } => {
                let mut docs = Vec::with_capacity(params.len());
                for param in params {
                    let mut parts = vec![
                        text(format!("{}: ", self.text(param.name.span))),
                        self.type_expr(&param.ty),
                    ];
                    if let Some(default) = param.default {
                        parts.push(text(" = "));
                        parts.push(self.expr(default, 0));
                    }
                    docs.push(concat(parts));
                }
                let params = docs;
                let mut parts = vec![
                    text(format!("fn {}", self.text(name.span))),
                    self.list("(", params, ")", false),
                ];
                if let Some(ret) = ret {
                    parts.push(text(" -> "));
                    parts.push(self.type_expr(ret));
                }
                parts.push(text(" "));
                parts.push(self.expr(*body, 0));
                concat(parts)
            }
            // The path is kept as written, after one space.
            StmtKind::Import { .. } => {
                let path = self.text(stmt.span)["import".len()..].trim();
                text(format!("import {path}"))
            }
            StmtKind::Expr(expr) => self.expr(*expr, 0),
        }
    }

    /// A bracketed, comma-separated list: on one line if it fits, otherwise one item per line
    /// with a trailing comma. With `padded`, the one-line form has spaces inside the brackets.
    fn list(&self, open: &str, items: Vec<Doc>, close: &str, padded: bool) -> Doc {
        if items.is_empty() {
            return text(format!("{open}{close}"));
        }
        let edge = || if padded { Doc::Line } else { Doc::SoftLine };
        group(concat([
            text(open),
            indent(concat([
                edge(),
                join(items, || concat([text(","), Doc::Line])),
            ])),
            Doc::IfBroken(","),
            edge(),
            text(close),
        ]))
    }

    /// Formats the items of a list that closes at `close`, placing the comments written
    /// among them. A comment forces the list onto several lines.
    fn items(
        &mut self,
        spans: &[(u32, u32)],
        close: u32,
        mut item: impl FnMut(&mut Self, usize) -> Doc,
    ) -> Vec<Doc> {
        let mut docs = Vec::with_capacity(spans.len());
        for (index, &(start, end)) in spans.iter().enumerate() {
            let mut parts = self.own_line_comments(start);
            parts.push(item(self, index));
            // A comment after the comma belongs to the item before it. One after the bracket
            // that closes the list belongs to what the list is part of.
            let line_end = self.src[end as usize..]
                .find('\n')
                .map_or(self.src.len(), |n| end as usize + n);
            let next_start = spans.get(index + 1).map_or(u32::MAX, |next| next.0);
            if let Some(comment) = self.peek_comment()
                && (comment.start as usize) < line_end
                && comment.start < next_start.min(close)
            {
                self.next_comment += 1;
                parts.push(Doc::LineSuffix(format!(
                    "  {}",
                    self.text(comment).trim_end()
                )));
            }
            docs.push(concat(parts));
        }
        docs
    }

    fn type_expr(&mut self, ty: &TypeExpr) -> Doc {
        match &ty.kind {
            TypeKind::Named { args, .. } => {
                // The name is the first word of the type.
                let whole = self.text(ty.span);
                let name_len = whole.find('<').unwrap_or(whole.len());
                let name = text(whole[..name_len].trim_end());
                if args.is_empty() {
                    return name;
                }
                let args = args.iter().map(|arg| self.type_expr(arg)).collect();
                concat([name, self.list("<", args, ">", false)])
            }
            TypeKind::Record(fields) => {
                let spans: Vec<(u32, u32)> = fields
                    .iter()
                    .map(|field| (field.name.span.start, field.ty.span.end))
                    .collect();
                let fields = self.items(&spans, ty.span.end - 1, |this, index| {
                    let field = &fields[index];
                    concat([
                        text(format!("{}: ", this.text(field.name.span))),
                        this.type_expr(&field.ty),
                    ])
                });
                self.list("{", fields, "}", true)
            }
            TypeKind::Nullable(inner) => concat([self.type_expr(inner), text("?")]),
            TypeKind::Fn { params, ret } => {
                let params = params.iter().map(|param| self.type_expr(param)).collect();
                let mut parts = vec![text("fn"), self.list("(", params, ")", false)];
                if let Some(ret) = ret {
                    parts.push(text(" -> "));
                    parts.push(self.type_expr(ret));
                }
                concat(parts)
            }
        }
    }

    fn strength(&self, id: ExprId) -> u8 {
        match self.ast.expr(id) {
            Expr::Pipe { .. } => strength::PIPE,
            Expr::Binary { op, .. } => binary_strength(*op),
            Expr::Unary {
                op: UnaryOp::Not, ..
            } => strength::NOT,
            Expr::Unary {
                op: UnaryOp::Neg, ..
            } => strength::NEGATE,
            _ => strength::PRIMARY,
        }
    }

    /// Formats an expression for a position that needs at least `needed` strength.
    fn expr(&mut self, id: ExprId, needed: u8) -> Doc {
        let own = self.strength(id);
        // Parentheses that the author put around an operand stay, even where the grammar
        // does not need them: `not (a > b)` reads better than `not a > b`.
        let compound = own < strength::PRIMARY || matches!(self.ast.expr(id), Expr::If { .. });
        let kept = needed > 0 && compound && self.is_parenthesized(id);
        let doc = self.bare_expr(id);
        if own < needed || kept {
            concat([text("("), doc, text(")")])
        } else {
            doc
        }
    }

    /// Whether the expression is directly inside parentheses in the source.
    fn is_parenthesized(&self, id: ExprId) -> bool {
        let span = self.ast.span(id);
        let before = self.src[..span.start as usize].trim_end();
        let after = self.src[span.end as usize..].trim_start();
        before.ends_with('(') && after.starts_with(')')
    }

    fn bare_expr(&mut self, id: ExprId) -> Doc {
        let ast = self.ast;
        let span = ast.span(id);
        match ast.expr(id) {
            // Literals and names are kept exactly as written.
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Null
            | Expr::Date(_)
            | Expr::DateTime(_)
            | Expr::Decimal(_)
            | Expr::Name(_) => text(self.text(span)),
            Expr::Record(fields) => {
                let spans: Vec<(u32, u32)> = fields
                    .iter()
                    .map(|(name, value)| (name.span.start, self.extent(*value).1))
                    .collect();
                let fields = self.items(&spans, span.end - 1, |this, index| {
                    let (name, value) = &fields[index];
                    concat([
                        text(format!("{}: ", this.text(name.span))),
                        this.expr(*value, 0),
                    ])
                });
                self.list("{", fields, "}", true)
            }
            Expr::Map(entries) => {
                let spans: Vec<(u32, u32)> = entries
                    .iter()
                    .map(|(key, value)| (self.extent(*key).0, self.extent(*value).1))
                    .collect();
                let entries = self.items(&spans, span.end - 1, |this, index| {
                    let (key, value) = &entries[index];
                    concat([this.expr(*key, 0), text(": "), this.expr(*value, 0)])
                });
                self.list("{", entries, "}", true)
            }
            Expr::Index { base, index } => concat([
                self.expr(*base, strength::PRIMARY),
                text("["),
                self.expr(*index, 0),
                text("]"),
            ]),
            Expr::Lambda { params, ret, body } => {
                let params = params.iter().map(|param| {
                    let name = text(self.text(param.name.span));
                    match &param.ty {
                        Some(ty) => concat([name, text(": "), self.type_expr(ty)]),
                        None => name,
                    }
                });
                let params: Vec<Doc> = params.collect();
                let mut parts = vec![text("fn"), self.list("(", params, ")", false)];
                if let Some(ret) = ret {
                    parts.push(text(" -> "));
                    parts.push(self.type_expr(ret));
                }
                parts.push(text(" "));
                parts.push(self.expr(*body, 0));
                concat(parts)
            }
            Expr::Match { scrutinee, arms } => self.match_expr(span, *scrutinee, arms),
            Expr::List(items) => {
                let spans: Vec<_> = items.iter().map(|item| self.extent(*item)).collect();
                let items = self.items(&spans, span.end - 1, |this, index| {
                    this.expr(items[index], 0)
                });
                self.list("[", items, "]", false)
            }
            Expr::Unary { op, operand } => match op {
                UnaryOp::Neg => concat([text("-"), self.expr(*operand, strength::NEGATE)]),
                UnaryOp::Not => concat([text("not "), self.expr(*operand, strength::NOT)]),
            },
            Expr::Binary { op, lhs, rhs } => {
                let own = binary_strength(*op);
                // Operators group to the left, `??` to the right, and comparisons not at all.
                let (left, right) = match op {
                    BinaryOp::Coalesce => (own + 1, own),
                    op if op.is_comparison() => (own + 1, own + 1),
                    _ => (own, own + 1),
                };
                concat([
                    self.expr(*lhs, left),
                    text(format!(" {} ", op.symbol())),
                    self.expr(*rhs, right),
                ])
            }
            Expr::Pipe { .. } => self.pipeline(id),
            Expr::Call {
                callee,
                type_args,
                args,
            } => {
                let mut parts = vec![self.expr(*callee, strength::PRIMARY)];
                if !type_args.is_empty() {
                    let types = type_args.iter().map(|ty| self.type_expr(ty)).collect();
                    parts.push(self.list("<", types, ">", false));
                }
                if let Some(hugged) = self.hugged_args(args) {
                    parts.push(hugged);
                    return concat(parts);
                }
                let spans: Vec<(u32, u32)> = args
                    .iter()
                    .map(|arg| {
                        let (start, end) = self.extent(arg.value);
                        (arg.name.map_or(start, |name| name.span.start), end)
                    })
                    .collect();
                let args = self.items(&spans, span.end - 1, |this, index| {
                    let arg = &args[index];
                    let value = this.expr(arg.value, 0);
                    match arg.name {
                        Some(name) => concat([text(format!("{} = ", this.text(name.span))), value]),
                        None => value,
                    }
                });
                parts.push(self.list("(", args, ")", false));
                concat(parts)
            }
            Expr::Field { base, name } => concat([
                self.expr(*base, strength::PRIMARY),
                text(format!(".{}", self.text(name.span))),
            ]),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let mut parts = vec![
                    text("if "),
                    self.expr(*cond, 0),
                    text(" "),
                    self.expr(*then_branch, 0),
                ];
                if let Some(else_branch) = else_branch {
                    parts.push(text(" else "));
                    parts.push(self.expr(*else_branch, 0));
                }
                concat(parts)
            }
            Expr::Block(stmts) => self.block(span, stmts),
        }
    }

    /// The arguments of a call that ends in a function written over several lines, as in
    /// `each(items, fn(item) {`: the function opens on the line of the call and its body
    /// follows, instead of every argument taking a line. The arguments before the function
    /// must be short enough to stay on that line.
    fn hugged_args(&mut self, args: &[Arg]) -> Option<Doc> {
        const LEADING_WIDTH: u32 = 40;
        let ast = self.ast;
        let (last, leading) = args.split_last()?;
        let Expr::Lambda { body, .. } = ast.expr(last.value) else {
            return None;
        };
        let Expr::Block(stmts) = ast.expr(*body) else {
            return None;
        };
        let (function, body) = (ast.span(last.value), ast.span(*body));
        let several_lines = stmts.len() > 1 || self.has_line_break(body.start, body.end);
        let start = |arg: &Arg| match arg.name {
            Some(name) => name.span.start,
            None => self.extent(arg.value).0,
        };
        let first = leading.first().map_or(function.start, start);
        let fits = function.start - first <= LEADING_WIDTH
            && !self.has_line_break(first, function.start)
            && !self.has_comment_before(function.start);
        if last.name.is_some() || !several_lines || !fits || self.is_parenthesized(last.value) {
            return None;
        }
        let mut parts = vec![text("(")];
        for arg in leading {
            if let Some(name) = arg.name {
                parts.push(text(format!("{} = ", self.text(name.span))));
            }
            parts.push(self.expr(arg.value, 0));
            parts.push(text(", "));
        }
        parts.push(self.expr(last.value, 0));
        parts.push(text(")"));
        Some(concat(parts))
    }

    /// A `match`: its arms one per line, with the comments written among them.
    fn match_expr(&mut self, span: Span, scrutinee: ExprId, arms: &[MatchArm]) -> Doc {
        let ast = self.ast;
        let head = concat([text("match "), self.expr(scrutinee, 0), text(" {")]);
        let mut lines = Vec::new();
        for arm in arms {
            let start = match &arm.patterns[0] {
                Pattern::Wildcard(span) => span.start,
                Pattern::Value(value) => ast.span(*value).start,
            };
            lines.push(Doc::HardLine);
            lines.extend(self.own_line_comments(start));
            let patterns = arm.patterns.iter().map(|pattern| match pattern {
                Pattern::Wildcard(_) => text("_"),
                Pattern::Value(value) => self.expr(*value, 0),
            });
            let patterns: Vec<Doc> = patterns.collect();
            lines.push(join(patterns, || text(" | ")));
            lines.push(text(" => "));
            lines.push(self.expr(arm.body, 0));
            lines.extend(self.trailing_comment(self.extent(arm.body).1));
        }
        // Comments after the last arm stay inside the braces.
        let last = self.own_line_comments(span.end - 1);
        if !last.is_empty() {
            lines.push(Doc::HardLine);
            lines.extend(last);
            // Each comment brought its own line break; the closing brace needs only one.
            lines.pop();
        }
        concat([head, indent(concat(lines)), Doc::HardLine, text("}")])
    }

    /// Where an expression starts and ends in the source, including any parentheses that the
    /// tree does not record. Comments are placed by these positions.
    fn extent(&self, id: ExprId) -> (u32, u32) {
        let span = self.ast.span(id);
        let before = &self.src[..span.start as usize];
        let after = &self.src[span.end as usize..];
        let opened = before.len() - before.trim_end_matches(['(', ' ']).len();
        let closed = after.len() - after.trim_start_matches([')', ' ']).len();
        (span.start - opened as u32, span.end + closed as u32)
    }

    fn block(&mut self, span: Span, stmts: &[Stmt]) -> Doc {
        let (inner_start, inner_end) = (span.start + 1, span.end - 1);
        let has_comments = self.has_comment_before(inner_end);
        if stmts.is_empty() && !has_comments {
            return text("{}");
        }
        // A block that holds one expression and was written on one line may stay on one.
        let one_liner = matches!(
            stmts,
            [Stmt {
                kind: StmtKind::Expr(_),
                ..
            }]
        ) && !has_comments
            && !self.has_line_break(span.start, span.end);
        if one_liner {
            let body = self.stmt(&stmts[0]);
            return group(concat([
                text("{"),
                indent(concat([Doc::Line, body])),
                Doc::Line,
                text("}"),
            ]));
        }
        let body = self.stmts(stmts, inner_start, inner_end);
        concat([
            text("{"),
            indent(concat([Doc::HardLine, body])),
            Doc::HardLine,
            text("}"),
        ])
    }

    /// A pipeline: on one line, or its source followed by one stage per line. It is broken
    /// if it does not fit, and also if its author broke it.
    fn pipeline(&mut self, id: ExprId) -> Doc {
        let ast = self.ast;
        let mut stages = Vec::new();
        let mut source = id;
        while let Expr::Pipe { lhs, rhs } = ast.expr(source) {
            stages.push(*rhs);
            source = *lhs;
        }
        stages.reverse();
        let source_end = ast.span(source).end;
        let written_broken = self.has_line_break(source_end, ast.span(stages[0]).start);

        let mut parts = vec![self.expr(source, strength::PIPE)];
        parts.extend(self.trailing_comment(source_end));
        let mut rest = Vec::new();
        for stage in stages {
            let stage_start = ast.span(stage).start;
            rest.push(Doc::Line);
            rest.extend(self.own_line_comments(stage_start));
            rest.push(text("|> "));
            rest.push(self.expr(stage, strength::PRIMARY));
            rest.extend(self.trailing_comment(ast.span(stage).end));
        }
        if written_broken {
            rest.push(Doc::BreakParent);
        }
        parts.push(indent(concat(rest)));
        group(concat(parts))
    }
}
