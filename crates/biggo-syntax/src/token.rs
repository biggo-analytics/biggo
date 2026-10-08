use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Int,
    Float,
    Str,
    /// `19.99d`
    Decimal,
    /// `@2026-01-01` or `@2026-01-01T10:30:00`
    Date,

    Let,
    Type,
    Fn,
    Match,
    Import,
    If,
    Else,
    And,
    Or,
    Not,
    True,
    False,
    Null,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Eq,
    Arrow,
    FatArrow,
    Bar,
    Question,

    Pipe,
    Coalesce,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,

    Eof,
    /// Malformed input that the lexer has already reported.
    Error,
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    /// Whether a line break separates this token from the previous one.
    pub newline_before: bool,
    pub span: Span,
}
