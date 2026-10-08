//! Source spans, tokens, lexer, parser, and AST.

pub mod ast;
mod diag;
mod dump;
mod intern;
mod lexer;
mod parser;
pub mod scalar;
mod span;
mod token;

pub use diag::{Diagnostic, SourceFile};
pub use intern::{Interner, Symbol};
pub use parser::{Parsed, parse};
pub use span::Span;
