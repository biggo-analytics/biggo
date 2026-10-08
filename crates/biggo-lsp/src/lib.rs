//! A language server: an editor sends it the text of open files over standard input and gets
//! back errors as the user types, the type of the expression under the cursor, and formatting.
//! It speaks the Language Server Protocol, JSON-RPC messages framed by a `Content-Length` header.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use biggo_eval::Session;
use biggo_syntax::ast::Ast;
use biggo_syntax::{Diagnostic, Interner, Span};
use biggo_types::Type;
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
    /// Analyzes the text of the file at `path`. The files it imports are read from `open`,
    /// the unsaved text of the files the editor has open, before the disk.
    fn new(text: String, path: Option<&Path>, open: &[(PathBuf, String)]) -> Self {
        let parsed = biggo_syntax::parse(&text, &mut Interner::new());
        if !parsed.diagnostics.is_empty() {
            return Self {
                text,
                diagnostics: parsed.diagnostics,
                checked: None,
            };
        }
        let mut session = Session::new(io::sink());
        if let Some(dir) = path.and_then(Path::parent) {
            session.vm().set_base_dir(dir);
        }
        for (path, text) in open {
            session.provide(path, text.clone());
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

    /// Analyzes the new text of a file and tells the editor its errors.
    fn update(&mut self, uri: &str, text: String) -> io::Result<()> {
        let path = file_path(uri);
        let open = self.documents.iter().filter(|(other, _)| *other != uri);
        let open: Vec<(PathBuf, String)> = open
            .filter_map(|(uri, document)| Some((file_path(uri)?, document.text.clone())))
            .collect();
        let document = Document::new(text, path.as_deref(), &open);
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
