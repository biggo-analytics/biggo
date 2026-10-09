mod build;
mod help;
mod infer;
mod repl;
mod test;

use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use biggo_eval::{Failure, Session, StaticErrors};
use biggo_syntax::{Interner, SourceFile, shown_path};

const USAGE: &str = "\
usage: biggo <command> [args]

commands:
  run <file> [<arg>...]    run a program; `args()` gives it the arguments
  run -e <code> [<arg>...] run a program given on the command line
  repl                     evaluate code interactively
  check <file>             report the syntax and type errors of a program
  explain <file> [<arg>...]  show the query plans of a program without running them
  test [<path>...]         run the tests in the `*_test.bgo` files under the paths
  fmt [--check] <file>...  format programs in place, or list those that need it
  infer <file>             print the row type of a data file, to copy into a program
  help <function>          describe a built-in function

`-` in place of a file is standard input. `fmt -` writes the formatted program to standard
output.
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
    // An executable made by `biggo build` runs its program, and all of its arguments are
    // the program's.
    if let Some(files) = build::embedded() {
        let (name, source) = files[0];
        return run_source(name, source, Path::new(""), false, &files[1..], args);
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
        ["infer", ref rest @ ..] => infer::command(rest),
        ["help", name] => match help::entry(name) {
            Some(entry) => {
                print!("{entry}");
                0
            }
            None => {
                eprintln!("biggo help: {}", help::unknown(name));
                1
            }
        },
        [command @ ("run" | "explain"), "-e", code, ref rest @ ..] => {
            let explain = command == "explain";
            run_source("<command line>", code, Path::new(""), explain, &[], rest)
        }
        [command @ ("run" | "explain"), "-e"] => {
            eprintln!("biggo {command}: expected a program after `-e`\n\n{USAGE}");
            2
        }
        ["run", path, ref rest @ ..] => run(path, false, rest),
        ["explain", path, ref rest @ ..] => run(path, true, rest),
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
        [command @ ("run" | "explain")] => {
            eprintln!("biggo {command}: expected a file\n\n{USAGE}");
            2
        }
        [command @ ("check" | "parse"), ..] => {
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

/// The name that stands for standard input where a file is expected.
const STDIN: &str = "-";

/// The name of a source in messages.
fn shown(path: &str) -> String {
    if path == STDIN {
        "<stdin>".into()
    } else {
        shown_path(path)
    }
}

fn read(path: &str) -> Option<String> {
    let text = match path {
        STDIN => {
            let mut text = String::new();
            io::stdin().read_to_string(&mut text).map(|_| text)
        }
        _ => std::fs::read_to_string(path),
    };
    match text {
        Ok(source) => Some(source),
        Err(err) => {
            eprintln!("biggo: cannot read {}: {err}", shown(path));
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
    match session.check(&shown(path), &source) {
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
    let name = shown(path);
    let file = SourceFile::new(&name, &source);
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
            // Standard input has no file to write back to: the result goes to standard
            // output, which is how an editor formats text that is not saved yet.
            Ok(formatted) if *path == STDIN && !check => print!("{formatted}"),
            Ok(formatted) if formatted == source => {}
            // With `--check` nothing is written: the files that would change are listed.
            Ok(_) if check => {
                println!("{}", shown(path));
                status = 1;
            }
            Ok(formatted) => {
                if let Err(err) = std::fs::write(path, formatted) {
                    eprintln!("biggo: cannot write {}: {err}", shown(path));
                    status = 1;
                }
            }
            Err(diagnostics) => {
                let name = shown(path);
                let file = SourceFile::new(&name, &source);
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

/// Runs the program in a file, which sees `args` as `args()`. With `explain`, its queries
/// print their plans instead of running.
fn run(path: &str, explain: bool, args: &[&str]) -> u8 {
    let Some(source) = read(path) else {
        return 1;
    };
    run_source(&shown(path), &source, base_dir(path), explain, &[], args)
}

/// Runs a program. `bundled` are the files it imports, when they come with it and are not
/// to be read from disk: their paths from `base_dir`, and their sources.
fn run_source(
    name: &str,
    source: &str,
    base_dir: &Path,
    explain: bool,
    bundled: &[(&str, &str)],
    args: &[&str],
) -> u8 {
    let mut session = Session::new(BufWriter::new(io::stdout().lock()));
    session.vm().set_base_dir(base_dir);
    session.vm().set_explain(explain);
    session
        .vm()
        .set_args(args.iter().map(|arg| arg.to_string()));
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
