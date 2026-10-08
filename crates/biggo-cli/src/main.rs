mod build;
mod repl;
mod test;

use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;

use biggo_eval::{Failure, Session, StaticErrors};
use biggo_syntax::{Interner, SourceFile};

const USAGE: &str = "\
usage: biggo <command> [args]

commands:
  run <file>               run a program
  repl                     evaluate code interactively
  check <file>             report the syntax and type errors of a program
  explain <file>           show the query plans of a program without running them
  test [<path>...]         run the tests in the `*_test.bgo` files under the paths
  fmt [--check] <file>...  format programs in place, or list those that need it
  build <file> [-o <out>]  make a standalone executable of a program
  lsp                      serve an editor over the Language Server Protocol
  parse <file>             print the syntax tree of a program
  version                  print the version
";

/// Stack for the thread that does the work. The parser and the type checker recurse along
/// the nesting of a program; this leaves room for programs far deeper than people write.
const STACK_SIZE: usize = 256 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, || command(&args))
            .expect("cannot start the main thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    ExitCode::from(code)
}

fn command(args: &[&str]) -> u8 {
    // An executable made by `biggo build` runs its program, whatever its arguments.
    if let Some(files) = build::embedded() {
        let (name, source) = files[0];
        return run_source(name, source, Path::new(""), false, &files[1..]);
    }
    match *args {
        ["version" | "--version" | "-V"] => {
            println!("biggo {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["help" | "--help" | "-h"] => {
            print!("{USAGE}");
            0
        }
        ["run", path] => run(path, false),
        ["explain", path] => run(path, true),
        ["repl"] => repl::repl(),
        ["check", path] => check(path),
        ["test", ref paths @ ..] => test::test(paths),
        ["parse", path] => parse(path),
        ["fmt", "--check", ref paths @ ..] if !paths.is_empty() => fmt(paths, true),
        ["fmt", ref paths @ ..] if !paths.is_empty() => fmt(paths, false),
        ["fmt", ..] => {
            eprintln!("biggo fmt: expected at least one file\n\n{USAGE}");
            2
        }
        ["build", path] => {
            let name = Path::new(path)
                .file_stem()
                .map(|stem| stem.to_string_lossy());
            build(path, &name.unwrap_or("program".into()))
        }
        ["build", path, "-o", output] | ["build", "-o", output, path] => build(path, output),
        ["build", ..] => {
            eprintln!("biggo build: expected a file and optionally `-o <output>`\n\n{USAGE}");
            2
        }
        ["lsp"] => match biggo_lsp::serve(io::stdin().lock(), io::stdout().lock()) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("biggo lsp: {err}");
                1
            }
        },
        [command @ ("run" | "explain" | "check" | "parse"), ..] => {
            eprintln!("biggo {command}: expected exactly one file\n\n{USAGE}");
            2
        }
        ["repl", ..] => {
            eprintln!("biggo repl: takes no arguments\n\n{USAGE}");
            2
        }
        [command, ..] => {
            eprintln!("biggo: unknown command `{command}`\n\n{USAGE}");
            2
        }
        [] => {
            eprint!("{USAGE}");
            2
        }
    }
}

fn read(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(err) => {
            eprintln!("biggo: cannot read {path}: {err}");
            None
        }
    }
}

fn report(errors: &StaticErrors) {
    eprintln!("{}", errors.render());
    let count = errors.diagnostics.len();
    let plural = if count == 1 { "" } else { "s" };
    eprintln!("{count} error{plural} in {}", errors.name);
}

fn check(path: &str) -> u8 {
    let Some(source) = read(path) else {
        return 1;
    };
    let mut session = Session::new(io::sink());
    session.vm().set_base_dir(base_dir(path));
    match session.check(path, &source) {
        Ok(_) => 0,
        Err(errors) => {
            report(&errors);
            1
        }
    }
}

fn parse(path: &str) -> u8 {
    let Some(source) = read(path) else {
        return 1;
    };
    let mut interner = Interner::new();
    let parsed = biggo_syntax::parse(&source, &mut interner);
    if parsed.diagnostics.is_empty() {
        print!("{}", parsed.ast.dump(&interner));
        return 0;
    }
    let file = SourceFile::new(path, &source);
    for diag in &parsed.diagnostics {
        eprintln!("{}", file.render(diag));
    }
    1
}

fn fmt(paths: &[&str], check: bool) -> u8 {
    let mut status = 0;
    for path in paths {
        let Some(source) = read(path) else {
            status = 1;
            continue;
        };
        match biggo_fmt::format(&source) {
            Ok(formatted) if formatted == source => {}
            // With `--check` nothing is written: the files that would change are listed.
            Ok(_) if check => {
                println!("{path}");
                status = 1;
            }
            Ok(formatted) => {
                if let Err(err) = std::fs::write(path, formatted) {
                    eprintln!("biggo: cannot write {path}: {err}");
                    status = 1;
                }
            }
            Err(diagnostics) => {
                let file = SourceFile::new(path, &source);
                for diag in &diagnostics {
                    eprintln!("{}", file.render(diag));
                }
                status = 1;
            }
        }
    }
    status
}

fn build(path: &str, output: &str) -> u8 {
    match build::build(path, output) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("biggo: {message}");
            1
        }
    }
}

/// The directory of a program. File paths in a program are relative to the program, wherever
/// it is run from.
fn base_dir(path: &str) -> &Path {
    Path::new(path).parent().unwrap_or(Path::new(""))
}

/// Runs the program in a file. With `explain`, its queries print their plans instead of
/// running.
fn run(path: &str, explain: bool) -> u8 {
    let Some(source) = read(path) else {
        return 1;
    };
    run_source(path, &source, base_dir(path), explain, &[])
}

/// Runs a program. `bundled` are the files it imports, when they come with it and are not
/// to be read from disk: their paths from `base_dir`, and their sources.
fn run_source(
    name: &str,
    source: &str,
    base_dir: &Path,
    explain: bool,
    bundled: &[(&str, &str)],
) -> u8 {
    let mut session = Session::new(BufWriter::new(io::stdout().lock()));
    session.vm().set_base_dir(base_dir);
    session.vm().set_explain(explain);
    for (path, source) in bundled {
        session.provide(&base_dir.join(path), source.to_string());
    }
    let result = session.run(name, source, false);
    let flushed = session.vm().output().flush();
    match result {
        Ok(_) if flushed.is_ok() => 0,
        // The reader of our output has gone away, as in `biggo run x.bgo | head`.
        Ok(_) => 1,
        Err(failure) if failure.io() == Some(io::ErrorKind::BrokenPipe) => 1,
        Err(Failure::Static(errors)) => {
            report(&errors);
            1
        }
        Err(failure) => {
            eprint!("{}", failure.render());
            1
        }
    }
}
