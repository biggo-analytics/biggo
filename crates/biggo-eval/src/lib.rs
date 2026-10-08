//! Runs checked programs: a bytecode compiler, the virtual machine, and a `Session` that takes
//! source text all the way from parsing to its result.

mod bytecode;
mod compile;
mod lists;
mod scalars;
mod value;
mod vm;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use biggo_syntax::ast::StmtKind;
use biggo_syntax::{Diagnostic, Interner, SourceFile, Span};
use biggo_types::hir::{Builtin, Expr, ExprKind, Program};
use biggo_types::{Checked, Checker, Type};

pub use value::{Closure, Decimal, Key, Map, Record, Repr, Value};
pub use vm::{Module, RuntimeError, Vm};

/// The syntax and type errors of one source text.
#[derive(Debug)]
pub struct StaticErrors {
    pub name: String,
    pub source: String,
    pub diagnostics: Vec<Diagnostic>,
    /// Set when the errors are in an imported file: where the source that the session was
    /// given imports it, directly or through other files.
    pub import: Option<Span>,
}

impl StaticErrors {
    /// Formats every error with its source line, a blank line between them.
    pub fn render(&self) -> String {
        let file = SourceFile::new(&self.name, &self.source);
        let rendered: Vec<String> = self.diagnostics.iter().map(|d| file.render(d)).collect();
        rendered.join("\n")
    }
}

/// Why a source text did not run to its end.
#[derive(Debug)]
pub enum Failure {
    Static(StaticErrors),
    Runtime(Box<RuntimeError>),
}

impl Failure {
    pub fn render(&self) -> String {
        match self {
            Failure::Static(errors) => errors.render(),
            Failure::Runtime(error) => error.render(),
        }
    }

    /// Set when the failure is that the program's output could not be written.
    pub fn io(&self) -> Option<io::ErrorKind> {
        match self {
            Failure::Static(_) => None,
            Failure::Runtime(error) => error.io,
        }
    }
}

/// A sequence of source texts that are checked and run one after another, each seeing the
/// top-level definitions of those before it: the entries of a REPL, or a file and the files
/// it imports. A session either only checks its sources or runs every one of them.
pub struct Session<W> {
    interner: Interner,
    checker: Checker,
    vm: Vm<W>,
    /// The files imported so far. A file is loaded once, however often it is imported.
    imported: HashSet<PathBuf>,
    /// The files whose own imports are being loaded, outermost first.
    importing: Vec<PathBuf>,
    /// Sources that stand in for files, by path.
    provided: HashMap<PathBuf, String>,
}

/// One source text on its way through the session.
#[derive(Clone, Copy)]
struct Source<'s> {
    name: &'s str,
    text: &'s str,
    /// The directory that its relative paths start from.
    dir: &'s Path,
    /// Where the session's own source imports this one, if it is an imported file.
    import: Option<Span>,
}

impl Source<'_> {
    fn errors(&self, diagnostics: Vec<Diagnostic>) -> Failure {
        Failure::Static(StaticErrors {
            name: self.name.to_string(),
            source: self.text.to_string(),
            diagnostics,
            import: self.import,
        })
    }
}

/// Removes the `.` and `..` steps of a path without looking at the file system, so that one
/// file has one path however the imports that lead to it are written.
pub fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normal.components().next_back() {
                Some(Component::Normal(_)) => {
                    normal.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => normal.push(".."),
            },
            other => normal.push(other),
        }
    }
    normal
}

impl<W: Write> Session<W> {
    pub fn new(out: W) -> Self {
        Self {
            interner: Interner::new(),
            checker: Checker::new(),
            vm: Vm::new(out),
            imported: HashSet::new(),
            importing: Vec::new(),
            provided: HashMap::new(),
        }
    }

    pub fn vm(&mut self) -> &mut Vm<W> {
        &mut self.vm
    }

    /// Makes `source` the content of the file at `path` for imports, whatever is on disk.
    pub fn provide(&mut self, path: &Path, source: String) {
        self.provided.insert(normalize(path), source);
    }

    /// The files the session has imported, in no particular order.
    pub fn imported(&self) -> impl Iterator<Item = &Path> {
        self.imported.iter().map(PathBuf::as_path)
    }

    /// Whether `source` only fails to parse because it stops too early, so that an
    /// interactive prompt should read another line.
    pub fn incomplete(&mut self, source: &str) -> bool {
        biggo_syntax::parse(source, &mut self.interner).incomplete
    }

    /// Parses and type-checks `source` and the files it imports. If they pass, their
    /// definitions join the session.
    pub fn check(&mut self, name: &str, source: &str) -> Result<Checked, StaticErrors> {
        let dir = self.vm.base_dir().to_path_buf();
        let source = Source {
            name,
            text: source,
            dir: &dir,
            import: None,
        };
        match self.prepare(source, false) {
            Ok(checked) => Ok(checked),
            Err(Failure::Static(errors)) => Err(errors),
            Err(Failure::Runtime(_)) => unreachable!("nothing runs while checking"),
        }
    }

    /// Checks and runs `source`, after the files it imports, and returns the value of its
    /// last statement. With `echo`, that value is also printed the way a REPL shows a result.
    pub fn run(&mut self, name: &str, source: &str, echo: bool) -> Result<Value, Failure> {
        let dir = self.vm.base_dir().to_path_buf();
        let source = Source {
            name,
            text: source,
            dir: &dir,
            import: None,
        };
        let checked = self.prepare(source, true)?;
        self.execute(source, checked.program, echo)
    }

    /// Parses `source`, loads the files it imports, and checks it. With `run`, the imported
    /// files are run as well, as they must be before `source` itself runs.
    fn prepare(&mut self, source: Source, run: bool) -> Result<Checked, Failure> {
        let parsed = biggo_syntax::parse(source.text, &mut self.interner);
        if !parsed.diagnostics.is_empty() {
            return Err(source.errors(parsed.diagnostics));
        }
        for stmt in &parsed.ast.stmts {
            if let StmtKind::Import { path } = &stmt.kind {
                self.import(source, path, stmt.span, run)?;
            }
        }
        let checked = self.checker.check(&parsed.ast, &mut self.interner);
        checked.map_err(|diagnostics| source.errors(diagnostics))
    }

    /// Loads the file that `from` imports as `target`, unless the session has it already.
    fn import(&mut self, from: Source, target: &str, span: Span, run: bool) -> Result<(), Failure> {
        let path = normalize(&from.dir.join(target));
        if self.imported.contains(&path) {
            return Ok(());
        }
        let fail = |message: String| Err(from.errors(vec![Diagnostic::new(span, message)]));
        if self.importing.contains(&path) {
            return fail(format!(
                "`{target}` is in the middle of being imported; \
                 files cannot import each other in a circle"
            ));
        }
        let text = match self.provided.get(&path) {
            Some(text) => text.clone(),
            None => match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return fail(format!("there is no file `{}`", path.display()));
                }
                Err(err) => return fail(format!("cannot read `{}`: {err}", path.display())),
            },
        };
        let name = path.display().to_string();
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        let source = Source {
            name: &name,
            text: &text,
            dir: &dir,
            import: from.import.or(Some(span)),
        };
        self.importing.push(path.clone());
        let loaded = self.prepare(source, run).and_then(|checked| match run {
            true => self.execute(source, checked.program, false).map(drop),
            false => Ok(()),
        });
        self.importing.pop();
        loaded?;
        self.imported.insert(path);
        Ok(())
    }

    fn execute(
        &mut self,
        source: Source,
        mut program: Program,
        echo: bool,
    ) -> Result<Value, Failure> {
        if echo
            && program.result != Type::Unit
            && let ExprKind::Block(_, value) = &mut program.main.body.kind
            && let Some(result) = value.take()
        {
            let span = result.span;
            let kind = ExprKind::Builtin(Builtin::Echo, vec![*result]);
            *value = Some(Box::new(Expr::new(kind, Type::Unit, span)));
        }
        let module = Rc::new(Module {
            name: source.name.to_string(),
            source: source.text.to_string(),
            dir: source.dir.to_path_buf(),
        });
        self.vm.run(&program, &module).map_err(Failure::Runtime)
    }
}
