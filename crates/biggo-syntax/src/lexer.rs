use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diags: Vec<Diagnostic>,
    /// The `//` comments, in source order.
    pub comments: Vec<Span>,
}

/// Splits `source` into tokens, ending with `Eof`. Malformed input becomes an `Error` token with
/// a matching diagnostic. `source` must be shorter than 4 GiB so that offsets fit in a `Span`.
pub fn lex(source: &str) -> Lexed {
    let mut lexer = Lexer {
        src: source,
        pos: 0,
        tokens: Vec::new(),
        diags: Vec::new(),
        comments: Vec::new(),
    };
    if source.starts_with('\u{feff}') {
        lexer.pos = '\u{feff}'.len_utf8();
    }
    loop {
        let newline_before = lexer.skip_trivia();
        let start = lexer.pos;
        let kind = lexer.next_kind();
        lexer.tokens.push(Token {
            kind,
            newline_before,
            span: Span::new(start as u32, lexer.pos as u32),
        });
        if kind == TokenKind::Eof {
            return Lexed {
                tokens: lexer.tokens,
                diags: lexer.diags,
                comments: lexer.comments,
            };
        }
    }
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
    comments: Vec<Span>,
}

impl Lexer<'_> {
    fn byte(&self, offset: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + offset).copied()
    }

    fn char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn error(&mut self, start: usize, message: impl Into<String>) {
        let span = Span::new(start as u32, self.pos as u32);
        self.diags.push(Diagnostic::new(span, message));
    }

    /// Skips whitespace and `//` comments; returns whether a line break was skipped.
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        loop {
            match self.byte(0) {
                Some(b'\n') => {
                    newline = true;
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | b'\r') => self.pos += 1,
                Some(b'/') if self.byte(1) == Some(b'/') => {
                    let start = self.pos;
                    while !matches!(self.byte(0), None | Some(b'\n' | b'\r')) {
                        self.pos += 1;
                    }
                    self.comments.push(Span::new(start as u32, self.pos as u32));
                }
                _ => return newline,
            }
        }
    }

    fn next_kind(&mut self) -> TokenKind {
        use TokenKind::*;
        let start = self.pos;
        let Some(c) = self.char() else {
            return Eof;
        };
        match c {
            '(' => self.take(1, LParen),
            ')' => self.take(1, RParen),
            '{' => self.take(1, LBrace),
            '}' => self.take(1, RBrace),
            '[' => self.take(1, LBracket),
            ']' => self.take(1, RBracket),
            ',' => self.take(1, Comma),
            ':' => self.take(1, Colon),
            '.' if self.byte(1) == Some(b'.') && self.byte(2) == Some(b'.') => {
                self.take(3, Ellipsis)
            }
            '.' => self.take(1, Dot),
            '+' => self.take(1, Plus),
            '*' => self.take(1, Star),
            '/' => self.take(1, Slash),
            '%' => self.take(1, Percent),
            '-' => self.either(b'>', Arrow, Minus),
            '=' if self.byte(1) == Some(b'>') => self.take(2, FatArrow),
            '=' => self.either(b'=', EqEq, Eq),
            '<' => self.either(b'=', LtEq, Lt),
            '>' => self.either(b'=', GtEq, Gt),
            '?' => self.either(b'?', Coalesce, Question),
            '|' => self.either(b'>', Pipe, Bar),
            '!' if self.byte(1) == Some(b'=') => self.take(2, NotEq),
            '"' => self.string(),
            // A raw string: `r"\d+"` is a backslash, a `d`, and a plus.
            'r' if self.byte(1) == Some(b'"') => self.raw_string(),
            '`' => self.quoted_name(),
            '@' => self.date(),
            '0'..='9' => self.number(),
            _ if c == '_' || unicode_ident::is_xid_start(c) => self.word(),
            _ => {
                self.pos += c.len_utf8();
                self.error(start, format!("unexpected character {c:?}"));
                Error
            }
        }
    }

    fn take(&mut self, len: usize, kind: TokenKind) -> TokenKind {
        self.pos += len;
        kind
    }

    /// The two-byte token if `second` comes next, otherwise the one-byte token.
    fn either(&mut self, second: u8, two: TokenKind, one: TokenKind) -> TokenKind {
        if self.byte(1) == Some(second) {
            self.take(2, two)
        } else {
            self.take(1, one)
        }
    }

    fn word(&mut self) -> TokenKind {
        use TokenKind::*;
        let start = self.pos;
        self.skip_ident();
        match &self.src[start..self.pos] {
            "let" => Let,
            "type" => Type,
            "fn" => Fn,
            "if" => If,
            "else" => Else,
            "and" => And,
            "or" => Or,
            "not" => Not,
            "in" => In,
            "true" => True,
            "false" => False,
            "null" => Null,
            "match" => Match,
            "import" => Import,
            _ => Ident,
        }
    }

    fn skip_ident(&mut self) {
        while let Some(c) = self.char()
            && unicode_ident::is_xid_continue(c)
        {
            self.pos += c.len_utf8();
        }
    }

    fn digits(&mut self) {
        while matches!(self.byte(0), Some(b'0'..=b'9' | b'_')) {
            self.pos += 1;
        }
    }

    fn number(&mut self) -> TokenKind {
        let start = self.pos;
        let mut kind = TokenKind::Int;
        self.digits();
        // A dot only starts a fraction when a digit follows, so `1.abs` stays a field access.
        if self.byte(0) == Some(b'.') && self.byte(1).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            self.digits();
            kind = TokenKind::Float;
        }
        if matches!(self.byte(0), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.byte(1), Some(b'+' | b'-')));
            if self.byte(1 + sign).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1 + sign;
                self.digits();
                kind = TokenKind::Float;
            }
        }
        // A `d` right after the digits makes the number a decimal: `19.99d`.
        if kind != TokenKind::Float || !self.src[start..self.pos].contains(['e', 'E']) {
            let after = self.src[self.pos..].chars().nth(1);
            if self.byte(0) == Some(b'd') && !after.is_some_and(unicode_ident::is_xid_continue) {
                self.pos += 1;
                return TokenKind::Decimal;
            }
        }
        if self.char().is_some_and(unicode_ident::is_xid_continue) {
            self.skip_ident();
            self.error(start, "invalid number literal");
            return TokenKind::Error;
        }
        kind
    }

    /// Finds the closing quote; the parser decodes the escapes.
    fn string(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.byte(0) {
                Some(b'"') => {
                    self.pos += 1;
                    return TokenKind::Str;
                }
                Some(b'\\') if !matches!(self.byte(1), None | Some(b'\n')) => self.pos += 2,
                None | Some(b'\n') => {
                    self.error(start, "unterminated string literal");
                    return TokenKind::Error;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// A string in which a backslash stands for itself, so it ends at the first quote.
    fn raw_string(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos += 2;
        loop {
            match self.byte(0) {
                Some(b'"') => {
                    self.pos += 1;
                    return TokenKind::Str;
                }
                None | Some(b'\n') => {
                    self.error(start, "unterminated string literal");
                    return TokenKind::Error;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// A name in backticks, which may contain spaces or be a keyword: `` `order type` ``.
    fn quoted_name(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos += 1;
        while !matches!(self.byte(0), None | Some(b'\n' | b'`')) {
            self.pos += 1;
        }
        if self.byte(0) != Some(b'`') {
            self.error(start, "unterminated quoted name");
            return TokenKind::Error;
        }
        self.pos += 1;
        if self.pos - start == 2 {
            self.error(start, "a quoted name cannot be empty");
            return TokenKind::Error;
        }
        TokenKind::Ident
    }

    /// Takes everything that could belong to a date literal; the parser validates it.
    fn date(&mut self) -> TokenKind {
        self.pos += 1;
        let mut in_time = false;
        loop {
            match self.byte(0) {
                Some(b) if b.is_ascii_alphanumeric() || b == b'-' => {}
                // A colon or a point belongs to a time of day only before a digit: a date
                // may be the key of a map, `{ @2026-01-01: 1 }`, or have a field read.
                Some(b':') if self.byte(1).is_some_and(|b| b.is_ascii_digit()) => in_time = true,
                Some(b'.') if in_time && self.byte(1).is_some_and(|b| b.is_ascii_digit()) => {}
                _ => break,
            }
            self.pos += 1;
        }
        TokenKind::Date
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        let lexed = lex(source);
        assert!(
            lexed.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            lexed.diags
        );
        lexed.tokens.iter().map(|t| t.kind).collect()
    }

    fn errors(source: &str) -> Vec<String> {
        lex(source).diags.into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn quoted_names_and_comments() {
        assert_eq!(kinds("`order type` + `if`"), [Ident, Plus, Ident, Eof]);
        assert_eq!(errors("`abc"), ["unterminated quoted name"]);
        assert_eq!(errors("``"), ["a quoted name cannot be empty"]);
        let lexed = lex("a // one\n// two\nb");
        let comments: Vec<_> = lexed.comments.iter().map(|span| span.range()).collect();
        assert_eq!(comments, [2..8, 9..15]);
    }

    #[test]
    fn operators_and_punctuation() {
        assert_eq!(
            kinds("|> ?? ? -> - == = != <= < >= > ( ) { } [ ] , : . + * / %"),
            [
                Pipe, Coalesce, Question, Arrow, Minus, EqEq, Eq, NotEq, LtEq, Lt, GtEq, Gt,
                LParen, RParen, LBrace, RBrace, LBracket, RBracket, Comma, Colon, Dot, Plus, Star,
                Slash, Percent, Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("1 1_000 1.5 2e9 1.5e-3 7.abs"),
            [Int, Int, Float, Float, Float, Int, Dot, Ident, Eof]
        );
    }

    #[test]
    fn decimals_and_match_tokens() {
        assert_eq!(
            kinds("19.99d 5d 1.5 x => y | z match import"),
            [
                Decimal, Decimal, Float, Ident, FatArrow, Ident, Bar, Ident, Match, Import, Eof
            ]
        );
        assert_eq!(errors("1e3d"), ["invalid number literal"]);
        assert_eq!(errors("5days"), ["invalid number literal"]);
    }

    #[test]
    fn keywords_and_identifiers() {
        assert_eq!(
            kinds("let lets _x if else and or not true false null fn type"),
            [
                Let, Ident, Ident, If, Else, And, Or, Not, True, False, Null, Fn, Type, Eof
            ]
        );
    }

    #[test]
    fn identifiers_may_be_unicode() {
        let Lexed { tokens, diags, .. } = lex("ยอดขาย ร้าน2 größe");
        assert!(diags.is_empty());
        let spans: Vec<_> = tokens.iter().map(|t| (t.kind, t.span.range())).collect();
        assert_eq!(
            spans,
            [
                (Ident, 0..18),
                (Ident, 19..32),
                (Ident, 33..40),
                (Eof, 40..40)
            ]
        );
    }

    #[test]
    fn strings_and_dates() {
        assert_eq!(
            kinds(r#""a \" b" @2026-01-01 "ไทย""#),
            [Str, Date, Str, Eof]
        );
    }

    #[test]
    fn comments_and_line_breaks() {
        let tokens = lex("a // note |> x\n  |> b c\n").tokens;
        let flags: Vec<_> = tokens.iter().map(|t| (t.kind, t.newline_before)).collect();
        assert_eq!(
            flags,
            [
                (Ident, false),
                (Pipe, true),
                (Ident, false),
                (Ident, false),
                (Eof, true)
            ]
        );
    }

    #[test]
    fn skips_byte_order_mark() {
        assert_eq!(kinds("\u{feff}let"), [Let, Eof]);
    }

    #[test]
    fn reports_malformed_input() {
        assert_eq!(errors("\"abc\nlet"), ["unterminated string literal"]);
        assert_eq!(errors("\"abc\\"), ["unterminated string literal"]);
        assert_eq!(errors("12abc"), ["invalid number literal"]);
        assert_eq!(errors("a $ b"), ["unexpected character '$'"]);
        assert_eq!(errors("a ; b"), ["unexpected character ';'"]);
    }
}
