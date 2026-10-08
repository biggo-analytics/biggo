//! The editor support and the reference documentation repeat facts about the language; these
//! tests fail when they drift apart from it.

use std::fs;

fn project_file(path: &str) -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    fs::read_to_string(format!("{root}{path}")).unwrap()
}

/// The reference lists every built-in function, written as a call or as a name.
#[test]
fn the_reference_documents_every_builtin_function() {
    let reference = project_file("docs/06-builtins.md");
    for name in biggo_types::builtin_names() {
        let listed = [
            format!("`{name}("),
            format!("`{name}<"),
            format!("`{name}`"),
        ];
        assert!(
            listed.iter().any(|form| reference.contains(form)),
            "`{name}` is missing from docs/06-builtins.md"
        );
    }
}

/// The grammar highlights built-in functions by name, so it has to list them all.
#[test]
fn the_grammar_lists_every_builtin_function() {
    let grammar = project_file("editors/vscode/syntaxes/biggo.tmLanguage.json");
    let names = biggo_types::builtin_names();
    assert!(names.len() > 90, "only {} built-in functions", names.len());
    for name in names {
        let listed = [
            format!("({name}|"),
            format!("|{name}|"),
            format!("|{name})"),
        ];
        assert!(
            listed.iter().any(|form| grammar.contains(form)),
            "`{name}` is missing from the grammar"
        );
    }
}
