//! A language server: an editor sends it the text of open files over standard input and gets
//! back errors as the user types, the type of the expression under the cursor, the names that
//! can be written at the cursor, and formatting. It speaks the Language Server Protocol, JSON-RPC
//! messages framed by a `Content-Length` header.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use biggo_eval::Session;
use biggo_plan::Name;
use biggo_syntax::ast::Ast;
use biggo_syntax::{Diagnostic, Interner, Span, Token, TokenKind};
use biggo_types::{NameKind, Names, Place, Type};
use serde_json::{Value, json};

/// What is known about one open file.
struct Document {
    text: String,
    diagnostics: Vec<Diagnostic>,
    /// The tree and the type of each of its expressions, when the file has no errors.
    checked: Option<(Ast, Vec<Option<Type>>)>,
}

/// The path of the file that a `file:` URI names.
fn file_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.as_bytes();
    while let [first, tail @ ..] = rest {
        // `%41` stands for the byte 0x41.
        let escaped = match (first, tail) {
            (b'%', [high, low, ..]) => std::str::from_utf8(&[*high, *low])
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 16).ok()),
            _ => None,
        };
        match escaped {
            Some(byte) => {
                bytes.push(byte);
                rest = &tail[2..];
            }
            None => {
                bytes.push(*first);
                rest = tail;
            }
        }
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

impl Document {
    /// Analyzes the text of a file in `session`, which finds the files it imports.
    fn new(text: String, mut session: Session<io::Sink>) -> Self {
        let parsed = biggo_syntax::parse(&text, &mut Interner::new());
        if !parsed.diagnostics.is_empty() {
            return Self {
                text,
                diagnostics: parsed.diagnostics,
                checked: None,
            };
        }
        let (diagnostics, checked) = match session.check("", &text) {
            Ok(checked) => (Vec::new(), Some((parsed.ast, checked.types))),
            // Errors in an imported file show where this file imports it.
            Err(errors) => match errors.import {
                Some(import) => {
                    let first = errors.diagnostics.first();
                    let first = first.map_or("", |diagnostic| &diagnostic.message);
                    let message = format!("`{}` has errors: {first}", errors.name);
                    (vec![Diagnostic::new(import, message)], None)
                }
                None => (errors.diagnostics, None),
            },
        };
        Self {
            text,
            diagnostics,
            checked,
        }
    }

    /// Converts a byte offset to a line and a column counted in UTF-16 units, as the
    /// protocol counts them.
    fn position(&self, offset: u32) -> Value {
        let before = &self.text[..(offset as usize).min(self.text.len())];
        let line = before.matches('\n').count();
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        let character: usize = before[line_start..].chars().map(char::len_utf16).sum();
        json!({ "line": line, "character": character })
    }

    fn range(&self, span: Span) -> Value {
        json!({ "start": self.position(span.start), "end": self.position(span.end) })
    }

    /// The inverse of `position`.
    fn offset(&self, position: &Value) -> Option<u32> {
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let mut line_start = 0;
        for _ in 0..line {
            line_start += self.text[line_start..].find('\n')? + 1;
        }
        let mut units = 0;
        for (index, c) in self.text[line_start..].char_indices() {
            if units >= character || c == '\n' {
                return Some((line_start + index) as u32);
            }
            units += c.len_utf16();
        }
        Some(self.text.len() as u32)
    }

    /// The smallest expression that contains `offset`, with its type.
    fn type_at(&self, offset: u32) -> Option<(Span, &Type)> {
        let (ast, types) = self.checked.as_ref()?;
        let candidates = ast.expr_ids().filter_map(|id| {
            let span = ast.span(id);
            let ty = types[id.index()].as_ref()?;
            (span.start <= offset && offset <= span.end).then_some((span, ty))
        });
        candidates.min_by_key(|(span, _)| span.end - span.start)
    }
}

/// The name that stands at the cursor in the text that is analyzed for a completion.
const WANTED: &str = "wanted_here";

const KEYWORDS: [&str; 14] = [
    "let", "fn", "type", "import", "if", "else", "match", "and", "or", "not", "in", "true",
    "false", "null",
];

/// The types that the language has built in.
const TYPES: [&str; 11] = [
    "int", "float", "bool", "string", "date", "datetime", "duration", "decimal", "list", "map",
    "table",
];

/// Whether a statement that begins with this keyword still waits for the rest: a `let` for
/// its `=`, an `if`, a `match`, or a `fn` for its block.
fn owes(kind: TokenKind) -> bool {
    use TokenKind::*;
    matches!(kind, Let | If | Match | Fn)
}

/// Follows `token` to what is open after it: the brackets that are not closed yet, and the
/// keywords of the statements that still wait for their rest. Returns what the token leaves
/// behind for good without its closing, innermost first.
fn follow(open: &mut Vec<TokenKind>, token: &Token) -> Vec<TokenKind> {
    use TokenKind::*;
    let mut left = Vec::new();
    if token.newline_before {
        let over = |kind: TokenKind| match token.kind {
            // A declaration begins a statement whatever is open, as it does for the parser.
            Let | Type | Import => kind != LBrace,
            LBrace => false,
            // A statement that its line leaves unfinished never gets its rest.
            _ => owes(kind),
        };
        while let Some(kind) = open.pop_if(|kind| over(*kind)) {
            left.push(kind);
        }
    }
    match token.kind {
        LParen | LBracket | Let | If | Match | Fn => open.push(token.kind),
        LBrace => {
            // The block that an `if`, a `match`, or a `fn` waited for.
            while let Some(If | Match | Fn) = open.last() {
                open.pop();
            }
            open.push(LBrace);
        }
        Eq => {
            // A `fn` between a `let` and its `=` is in the type of the variable.
            let last = open.iter().rposition(|kind| !owes(*kind) || *kind == Let);
            if let Some(last) = last.filter(|last| open[*last] == Let) {
                open.truncate(last);
            }
        }
        RParen | RBracket | RBrace => {
            let opener = match token.kind {
                RParen => LParen,
                RBracket => LBracket,
                _ => LBrace,
            };
            if let Some(opened) = open.iter().rposition(|kind| *kind == opener) {
                left.extend(open.drain(opened + 1..).rev());
                open.pop();
            }
        }
        _ => {}
    }
    left
}

/// A copy of `text` that can be analyzed while the statement at `cursor` is still being
/// written, with the offset of the cursor in it. A name stands in for the one being written
/// there, or where none is written yet, and what is open at the cursor and never closed is
/// closed: brackets, a `let` without its value, an `if`, `match`, or `fn` without its block.
/// With `in_type`, a `<` just before the cursor is taken to open the argument of a type, as
/// in `table<`, and is closed too; there is no copy if there is no such `<`.
fn whole_at(text: &str, cursor: u32, in_type: bool) -> Option<(String, u32)> {
    use TokenKind::*;
    // The offsets of tokens are 32 bits wide.
    u32::try_from(text.len()).ok()?;
    let lexed = biggo_syntax::lex(text);
    let tokens = &lexed.tokens;
    // A name that is being written may spell a keyword so far.
    let word = |token: &Token| token.kind == Ident || KEYWORDS.contains(&&text[token.span.range()]);
    let touches = |token: &Token| token.span.start <= cursor && cursor <= token.span.end;
    let written = tokens
        .iter()
        .position(|token| word(token) && touches(token));
    // The tokens on either side of the name being written, or of the empty place for one.
    let (before, name, after) = match written {
        Some(index) => (&tokens[..index], tokens[index].span, &tokens[index + 1..]),
        None => {
            let ended = |token: &Token| token.span.end <= cursor && token.kind != Eof;
            let split = tokens.partition_point(ended);
            let empty = Span::new(cursor, cursor);
            (&tokens[..split], empty, &tokens[split..])
        }
    };
    let (start, end) = (name.start as usize, name.end as usize);
    let previous = before.last();
    if in_type && previous.is_none_or(|token| token.kind != Lt) {
        return None;
    }

    let mut open = Vec::new();
    for token in before {
        follow(&mut open, token);
    }
    let after_previous = previous.map_or(0, |token| token.span.end as usize);
    let stand_in = Token {
        kind: Ident,
        newline_before: text[after_previous..start].contains('\n'),
        span: name,
    };
    follow(&mut open, &stand_in);

    let mut whole = String::from(&text[..start]);
    whole.push_str(WANTED);
    // A stage of a pipeline is a call.
    let called = |token: &Token| token.kind == LParen && !token.newline_before;
    if previous.is_some_and(|token| token.kind == Pipe) && !after.first().is_some_and(called) {
        whole.push_str("()");
    }
    if in_type {
        whole.push('>');
    }
    let closer = |kind| match kind {
        LParen => ")",
        LBracket => "]",
        LBrace => "}",
        Let => " = 0",
        _ => " {}",
    };
    // What is added goes on the line of the cursor, before a comment that ends the line.
    let line = end + text[end..].find('\n').unwrap_or(text.len() - end);
    let mut comments = lexed.comments.iter().map(|comment| comment.start as usize);
    let line_end = comments
        .find(|comment| (end..line).contains(comment))
        .unwrap_or(line);
    let mut copied = end;
    // How many of the open things were opened before the cursor.
    let mut ours = open.len();
    for token in after {
        let later = open.len() - ours;
        let left = follow(&mut open, token);
        // What the token opens itself came after the cursor.
        let opens = matches!(
            token.kind,
            LParen | LBracket | LBrace | Let | If | Match | Fn
        );
        ours = ours.min(open.len() - usize::from(opens));
        // What a later token leaves unclosed is closed just before it.
        let upto = (token.span.start as usize).clamp(copied, line_end);
        whole.push_str(&text[copied..upto]);
        whole.extend(left.into_iter().skip(later).map(closer));
        copied = upto;
    }
    // A block that is never closed ends with the file, so that the statements after the
    // cursor stay in it. Everything else ends with the line.
    let blocks = open[..ours]
        .iter()
        .take_while(|kind| **kind == LBrace)
        .count();
    whole.push_str(&text[copied..line_end]);
    whole.extend(open[blocks..ours].iter().rev().copied().map(closer));
    whole.push_str(&text[line_end..]);
    whole.push_str(&"\n}".repeat(blocks));
    Some((whole, name.start + 1))
}

/// The names to offer at a place: those the checker found there, then the built-in functions,
/// the keywords, and the built-in types where the place takes them.
fn items(found: &Names) -> Vec<Value> {
    // The numbers that the protocol gives the kinds of names.
    const FUNCTION: u8 = 3;
    const FIELD: u8 = 5;
    const VARIABLE: u8 = 6;
    const KEYWORD: u8 = 14;
    const STRUCT: u8 = 22;
    let mut items = Vec::new();
    for named in &found.names {
        let kind = match named.kind {
            NameKind::Column | NameKind::Field => FIELD,
            NameKind::Variable => VARIABLE,
            NameKind::Function => FUNCTION,
            NameKind::Type => STRUCT,
        };
        // A name that is not a plain word is written in backticks.
        let written = Name(&named.name).to_string();
        let mut item = json!({ "label": written, "kind": kind });
        if written != *named.name {
            item["filterText"] = json!(&*named.name);
        }
        // The type of something that has an error says nothing.
        if !named.ty.is_error() {
            item["detail"] = json!(named.ty.to_string());
        }
        items.push(item);
    }
    let fixed = |kind: u8| move |name: &str| json!({ "label": name, "kind": kind });
    if matches!(found.place, Place::Expr | Place::Callee) {
        let builtins = biggo_types::builtin_names().into_iter();
        items.extend(builtins.map(fixed(FUNCTION)));
    }
    if found.place == Place::Expr {
        items.extend(KEYWORDS.into_iter().map(fixed(KEYWORD)));
    }
    if matches!(found.place, Place::Expr | Place::Type) {
        items.extend(TYPES.into_iter().map(fixed(STRUCT)));
    }
    // The editor lists the names in this order, the nearest first, not by the alphabet.
    for (index, item) in items.iter_mut().enumerate() {
        item["sortText"] = json!(format!("{index:04}"));
    }
    items
}

fn read_message(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if input.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message without Content-Length",
        ));
    };
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = message.to_string();
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}

struct Server<W> {
    output: W,
    documents: HashMap<String, Document>,
}

impl<W: Write> Server<W> {
    fn respond(&mut self, id: &Value, result: Value) -> io::Result<()> {
        write_message(
            &mut self.output,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        )
    }

    /// A session for the file `uri`. The files it imports are read from the unsaved text of
    /// the files the editor has open, before the disk.
    fn session(&self, uri: &str) -> Session<io::Sink> {
        let mut session = Session::new(io::sink());
        if let Some(dir) = file_path(uri).as_deref().and_then(Path::parent) {
            session.vm().set_base_dir(dir);
        }
        for (other, document) in &self.documents {
            if let Some(path) = file_path(other).filter(|_| other != uri) {
                session.provide(&path, document.text.clone());
            }
        }
        session
    }

    /// Analyzes the new text of a file and tells the editor its errors.
    fn update(&mut self, uri: &str, text: String) -> io::Result<()> {
        let document = Document::new(text, self.session(uri));
        let diagnostics: Vec<Value> = document
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "range": document.range(diagnostic.span),
                    "severity": 1,
                    "source": "biggo",
                    "message": diagnostic.message,
                })
            })
            .collect();
        self.documents.insert(uri.to_string(), document);
        let params = json!({ "uri": uri, "diagnostics": diagnostics });
        let method = "textDocument/publishDiagnostics";
        write_message(
            &mut self.output,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }

    fn hover(&self, params: &Value) -> Value {
        let found = || {
            let uri = params.pointer("/textDocument/uri")?.as_str()?;
            let document = self.documents.get(uri)?;
            let offset = document.offset(params.get("position")?)?;
            let (span, ty) = document.type_at(offset)?;
            Some(json!({
                "contents": { "kind": "markdown", "value": format!("```\n{ty}\n```") },
                "range": document.range(span),
            }))
        };
        found().unwrap_or(Value::Null)
    }

    /// The names that can be written at the cursor; none where the text around it cannot be
    /// analyzed. The editor keeps those that match what is typed so far.
    fn completion(&self, params: &Value) -> Value {
        let found = || {
            let uri = params.pointer("/textDocument/uri")?.as_str()?;
            let document = self.documents.get(uri)?;
            let cursor = document.offset(params.get("position")?)?;
            // A `<` before the cursor compares two values, or else opens the argument of a
            // type.
            [false, true].into_iter().find_map(|in_type| {
                let (text, cursor) = whole_at(&document.text, cursor, in_type)?;
                self.session(uri).names_at(&text, cursor)
            })
        };
        json!(found().map_or(Vec::new(), |found| items(&found)))
    }

    /// The edit that replaces a file with its formatted text; none if it has syntax errors.
    fn formatting(&self, params: &Value) -> Value {
        let edits = || {
            let uri = params.pointer("/textDocument/uri")?.as_str()?;
            let document = self.documents.get(uri)?;
            let formatted = biggo_fmt::format(&document.text).ok()?;
            if formatted == document.text {
                return Some(json!([]));
            }
            let whole = Span::new(0, document.text.len() as u32);
            Some(json!([{ "range": document.range(whole), "newText": formatted }]))
        };
        edits().unwrap_or(Value::Null)
    }

    /// Handles one message. Returns false when the editor says to exit.
    fn handle(&mut self, message: &Value) -> io::Result<bool> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").unwrap_or(&Value::Null);
        let text_of = |pointer: &str| {
            let text = params.pointer(pointer).and_then(Value::as_str);
            text.map(str::to_string)
        };
        match (method, message.get("id")) {
            ("initialize", Some(id)) => {
                let capabilities = json!({
                    // The editor sends the whole text on every change.
                    "textDocumentSync": 1,
                    "hoverProvider": true,
                    "completionProvider": { "triggerCharacters": ["."] },
                    "documentFormattingProvider": true,
                });
                let info = json!({ "name": "biggo", "version": env!("CARGO_PKG_VERSION") });
                self.respond(
                    id,
                    json!({ "capabilities": capabilities, "serverInfo": info }),
                )?;
            }
            ("textDocument/didOpen", _) => {
                if let (Some(uri), Some(text)) =
                    (text_of("/textDocument/uri"), text_of("/textDocument/text"))
                {
                    self.update(&uri, text)?;
                }
            }
            ("textDocument/didChange", _) => {
                let text = text_of("/contentChanges/0/text");
                if let (Some(uri), Some(text)) = (text_of("/textDocument/uri"), text) {
                    self.update(&uri, text)?;
                }
            }
            ("textDocument/didClose", _) => {
                if let Some(uri) = text_of("/textDocument/uri") {
                    self.documents.remove(&uri);
                }
            }
            ("textDocument/hover", Some(id)) => {
                let result = self.hover(params);
                self.respond(id, result)?;
            }
            ("textDocument/completion", Some(id)) => {
                let result = self.completion(params);
                self.respond(id, result)?;
            }
            ("textDocument/formatting", Some(id)) => {
                let result = self.formatting(params);
                self.respond(id, result)?;
            }
            ("shutdown", Some(id)) => self.respond(id, Value::Null)?,
            ("exit", _) => return Ok(false),
            // A request this server does not know must still get an answer.
            (_, Some(id)) => {
                let error =
                    json!({ "code": -32601, "message": format!("unknown method `{method}`") });
                write_message(
                    &mut self.output,
                    &json!({ "jsonrpc": "2.0", "id": id, "error": error }),
                )?;
            }
            _ => {}
        }
        Ok(true)
    }
}

/// Serves an editor until it says to exit or closes the input.
pub fn serve(mut input: impl BufRead, output: impl Write) -> io::Result<()> {
    let mut server = Server {
        output,
        documents: HashMap::new(),
    };
    while let Some(message) = read_message(&mut input)? {
        if !server.handle(&message)? {
            break;
        }
    }
    Ok(())
}
