use serde_json::{Value, json};

fn frame(message: Value) -> String {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// Sends `messages` to the server and returns everything it writes back.
fn exchange(messages: Vec<Value>) -> Vec<Value> {
    let input: String = messages.into_iter().map(frame).collect();
    let mut output = Vec::new();
    biggo_lsp::serve(input.as_bytes(), &mut output).unwrap();
    let mut rest = String::from_utf8(output).unwrap();
    let mut replies = Vec::new();
    while let Some(header_end) = rest.find("\r\n\r\n") {
        let length: usize = rest["Content-Length: ".len()..header_end].parse().unwrap();
        let body: String = rest
            .drain(..header_end + 4 + length)
            .skip(header_end + 4)
            .collect();
        replies.push(serde_json::from_str(&body).unwrap());
    }
    replies
}

fn open(text: &str) -> Value {
    let document =
        json!({ "uri": "file:///demo.bgo", "languageId": "biggo", "version": 1, "text": text });
    json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": document } })
}

fn request(id: u32, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The kinds of names, as the protocol numbers them.
const FUNCTION: u64 = 3;
const FIELD: u64 = 5;
const VARIABLE: u64 = 6;
const KEYWORD: u64 = 14;
const TYPE: u64 = 22;

/// The start of the programs that completion is tried on.
const SALES: &str = "type Sale = { region: string, units: int, price: float }\n\
                     let sales = read_csv<Sale>(\"sales.csv\")\n";

/// Asks for the names that can be written at `offset` of the file that was opened.
fn complete(id: u32, text: &str, offset: usize) -> Value {
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let character = before
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .encode_utf16()
        .count();
    let params = json!({
        "textDocument": { "uri": "file:///demo.bgo" },
        "position": { "line": line, "character": character },
    });
    request(id, "textDocument/completion", params)
}

/// The names that the server offers where `$` stands in `text`.
fn completions(text: &str) -> Vec<Value> {
    let cursor = text.find('$').unwrap();
    let text = text.replace('$', "");
    let replies = exchange(vec![open(&text), complete(1, &text, cursor)]);
    replies[1]["result"].as_array().unwrap().clone()
}

/// The names of one kind among `items`, in the order the server gives them.
fn labels(items: &[Value], kind: u64) -> Vec<&str> {
    let of_kind = items.iter().filter(|item| item["kind"] == kind);
    of_kind
        .map(|item| item["label"].as_str().unwrap())
        .collect()
}

/// The columns or fields among `items`, each as `name: type`.
fn fields(items: &[Value]) -> Vec<String> {
    let fields = items.iter().filter(|item| item["kind"] == FIELD);
    let text = |value: &Value| value.as_str().unwrap().to_string();
    fields
        .map(|item| format!("{}: {}", text(&item["label"]), text(&item["detail"])))
        .collect()
}

#[test]
fn reports_errors_as_the_text_changes() {
    let change = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": "file:///demo.bgo", "version": 2 },
            "contentChanges": [{ "text": "let ยอด = 1\nlet y = ยอด + 2\n" }],
        },
    });
    let replies = exchange(vec![
        request(1, "initialize", json!({})),
        open("let ยอด = 1\nlet y = ยอด + \"a\"\n"),
        change,
        request(2, "shutdown", Value::Null),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    assert_eq!(replies.len(), 4);
    let capabilities = &replies[0]["result"]["capabilities"];
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(
        capabilities["completionProvider"],
        json!({ "triggerCharacters": ["."] })
    );
    let diagnostics = &replies[1]["params"]["diagnostics"];
    assert_eq!(
        diagnostics[0]["message"],
        "cannot apply `+` to int and string"
    );
    // Columns count UTF-16 units: the Thai name is three of them, though nine bytes.
    let range =
        json!({ "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 17 } });
    assert_eq!(diagnostics[0]["range"], range);
    assert_eq!(replies[2]["params"]["diagnostics"], json!([]));
    assert_eq!(
        replies[3],
        json!({ "jsonrpc": "2.0", "id": 2, "result": null })
    );
}

#[test]
fn hover_shows_the_type_under_the_cursor() {
    let text = "type T = { region: string, qty: int? }\nlet t = read_csv<T>(\"t.csv\")\nlet big = t |> where(qty > 1)\nlet n = 1 + 2.5\n";
    let at = |id, line, character| {
        let params = json!({
            "textDocument": { "uri": "file:///demo.bgo" },
            "position": { "line": line, "character": character },
        });
        request(id, "textDocument/hover", params)
    };
    let replies = exchange(vec![
        open(text),
        at(1, 3, 9),
        at(2, 3, 12),
        at(3, 2, 10),
        at(4, 2, 22),
        at(5, 0, 2),
    ]);
    let hover = |index: usize| {
        replies[index]["result"]["contents"]["value"]
            .as_str()
            .unwrap_or("none")
    };
    assert_eq!(hover(1), "```\nint\n```");
    assert_eq!(hover(2), "```\nfloat\n```");
    assert_eq!(hover(3), "```\ntable<{region: string, qty: int?}>\n```");
    assert_eq!(hover(4), "```\nint?\n```");
    assert_eq!(hover(5), "none");
}

#[test]
fn formats_a_document() {
    let params = json!({ "textDocument": { "uri": "file:///demo.bgo" }, "options": {} });
    let replies = exchange(vec![
        open("let   x=1\nlet y = [ 1,2 ]"),
        request(1, "textDocument/formatting", params.clone()),
        open("let x = 1\n"),
        request(2, "textDocument/formatting", params.clone()),
        open("let x = (\n"),
        request(3, "textDocument/formatting", params),
        request(4, "workspace/unknown", Value::Null),
    ]);
    let edit = &replies[1]["result"][0];
    assert_eq!(edit["newText"], "let x = 1\nlet y = [1, 2]\n");
    let whole =
        json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 15 } });
    assert_eq!(edit["range"], whole);
    // Already formatted: nothing to change. Syntax errors: nothing to offer.
    assert_eq!(replies[3]["result"], json!([]));
    assert_eq!(replies[5]["result"], Value::Null);
    assert_eq!(replies[6]["error"]["code"], -32601);
}

#[test]
fn imports_use_the_open_text_of_a_file() {
    let did_open = |uri: &str, text: &str| {
        let document = json!({ "uri": uri, "languageId": "biggo", "version": 1, "text": text });
        json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": document } })
    };
    let replies = exchange(vec![
        request(1, "initialize", json!({})),
        // The imported file exists only in the editor, not on disk.
        did_open("file:///work/my%20lib/rates.bgo", "let rate = 0.07\n"),
        did_open(
            "file:///work/main.bgo",
            "let before = 1\nimport \"my lib/rates.bgo\"\n",
        ),
        did_open(
            "file:///work/main.bgo",
            "import \"my lib/rates.bgo\"\nlet tax = 100 * rate\n",
        ),
        did_open("file:///work/my%20lib/rates.bgo", "let rate = \n"),
        did_open("file:///work/my%20lib/rates.bgo", "let rate: int = 0.07\n"),
        did_open(
            "file:///work/main.bgo",
            "let x = 1\n\nimport \"my lib/rates.bgo\"\n",
        ),
        did_open("file:///work/other.bgo", "import \"nowhere.bgo\"\n"),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    let diagnostics = |reply: usize| replies[reply]["params"]["diagnostics"].clone();
    assert_eq!(diagnostics(1), json!([]));
    assert_eq!(
        diagnostics(2)[0]["message"],
        "imports come before everything else in a file"
    );
    assert_eq!(diagnostics(3), json!([]));
    assert_eq!(diagnostics(5)[0]["message"], "expected int, found float");
    // An error in an imported file shows at the import, with the name of the file.
    assert_eq!(
        diagnostics(6)[0]["message"],
        "`/work/my lib/rates.bgo` has errors: expected int, found float"
    );
    assert_eq!(diagnostics(6)[0]["range"]["start"]["line"], 2);
    assert_eq!(
        diagnostics(7)[0]["message"],
        "there is no file `/work/nowhere.bgo`"
    );
}

#[test]
fn completion_offers_the_columns_of_the_table_in_the_pipeline() {
    let columns = ["region: string", "units: int", "price: float"];
    // Right after the bracket, closed or not, inside a name, and with the table as an argument.
    for stage in [
        "sales |> where($",
        "sales |> where($)\nprint(sales)\n",
        "let big = sales |> where(pr$",
        "let big = sales |> where(units > 10 and pr$ice < 2.5)\n",
        "where(sales, $",
    ] {
        let items = completions(&format!("{SALES}{stage}"));
        assert_eq!(fields(&items), columns, "{stage}");
        // A condition can use more than columns.
        assert_eq!(labels(&items, VARIABLE), ["sales"], "{stage}");
        assert!(labels(&items, FUNCTION).contains(&"is_null"), "{stage}");
    }
}

#[test]
fn completion_offers_columns_in_later_and_nested_arguments() {
    let columns = ["region: string", "units: int", "price: float"];
    for stage in [
        "sales |> sort(desc($))",
        "sales |> sort(region, desc($",
        "sales |> group(region) |> agg(total = sum($))",
        "sales |> group(region) |> agg(total = sum(units), top = max($",
    ] {
        let items = completions(&format!("{SALES}{stage}"));
        assert_eq!(fields(&items), columns, "{stage}");
    }
    // Where only the name of a column can stand, nothing else is offered.
    for stage in ["sales |> select(region, $)", "sales |> select(region, un$"] {
        let items = completions(&format!("{SALES}{stage}"));
        assert_eq!(fields(&items), columns, "{stage}");
        assert_eq!(items.len(), 3, "{stage}");
    }
}

#[test]
fn completion_offers_a_column_that_an_earlier_stage_added() {
    let derived = "let big = sales |> where(units > 10) |> derive(amount = units * price)\n\
                   print(big |> group(region) |> agg(total = sum($)) |> sort(desc(total)))\n";
    let items = completions(&format!("{SALES}{derived}"));
    let columns = [
        "region: string",
        "units: int",
        "price: float",
        "amount: float",
    ];
    assert_eq!(fields(&items), columns);
    // In the same pipeline, and in the same `derive`.
    for stage in [
        "sales |> derive(amount = units * price) |> where(am$",
        "sales |> derive(amount = units * price, tax = am$",
    ] {
        let items = completions(&format!("{SALES}{stage}"));
        assert_eq!(fields(&items), columns, "{stage}");
    }
    // After `agg`, the columns are those of the group and the aggregates.
    let aggregated = "sales\n  |> group(region)\n  \
                      |> agg(total = sum(units), mean = mean(price))\n  |> sort(desc($";
    let items = completions(&format!("{SALES}{aggregated}"));
    assert_eq!(
        fields(&items),
        ["region: string", "total: int", "mean: float"]
    );
    // A name that needs backticks is offered with them, and found by what is typed of it.
    let renamed = "sales |> select(`unit price` = price) |> where($";
    let items = completions(&format!("{SALES}{renamed}"));
    assert_eq!(fields(&items), ["`unit price`: float"]);
    assert_eq!(items[0]["filterText"], "unit price");
}

#[test]
fn completion_offers_the_columns_of_a_join() {
    let tables = "type Manager = { region: string, manager: string }\n\
                  let managers = read_csv<Manager>(\"managers.csv\")\n";
    let joined = "sales |> join(managers, on = region, how = \"left\") |> where($";
    let items = completions(&format!("{SALES}{tables}{joined}"));
    // The right side of a left join can be missing.
    let columns = [
        "region: string",
        "units: int",
        "price: float",
        "manager: string?",
    ];
    assert_eq!(fields(&items), columns);
    // A key is a column of both tables.
    let items = completions(&format!("{SALES}{tables}sales |> join(managers, on = $"));
    assert_eq!(fields(&items), ["region: string"]);
    let key = "sales |> join(managers, left_on = region, right_on = $";
    let items = completions(&format!("{SALES}{tables}{key}"));
    assert_eq!(fields(&items), ["region: string", "manager: string"]);
}

#[test]
fn completion_works_on_a_half_typed_last_line() {
    let lines = "let big = sales |> where(units > 10)\n\
                 let rich = big |> derive(amount = units * price)\n";
    let items = completions(&format!("{SALES}{lines}print(rich |> select(region, am$"));
    let columns = [
        "region: string",
        "units: int",
        "price: float",
        "amount: float",
    ];
    assert_eq!(fields(&items), columns);
    // A line further up that does not parse takes only its own names away.
    let broken = "let big = sales |> where(units >\nlet small = sales |> where(units < 3)\n";
    let items = completions(&format!("{SALES}{broken}print(small |> sort(desc($"));
    assert_eq!(fields(&items), columns[..3]);
    assert_eq!(labels(&items, VARIABLE), ["small", "sales"]);
}

#[test]
fn completion_offers_the_fields_of_a_record() {
    let columns = ["region: string", "units: int", "price: float"];
    for access in [
        "let rows = to_rows(sales)\nprint(rows[0].$",
        "let first = to_rows(sales)[0]\nprint(first.re$gion)\n",
        // The parameter of a function gets its type from the list it is called with.
        "each(to_rows(sales), fn(row) {\n  print(row.$",
        "print(map(to_rows(sales), fn(row) { row.$ }))\n",
        "to_rows(sales) |> filter(fn(row) { row.units > 1 and row.$",
    ] {
        let items = completions(&format!("{SALES}{access}"));
        assert_eq!(fields(&items), columns, "{access}");
        assert_eq!(items.len(), 3, "{access}");
    }
    let nested = "let order = { id: 7, buyer: { name: \"Ann\", city: \"Bangkok\" } }\n";
    let items = completions(&format!("{nested}print(order.buyer.$"));
    assert_eq!(fields(&items), ["name: string", "city: string"]);
    // Only a record has fields.
    assert!(completions(&format!("{SALES}print(sales.$")).is_empty());
}

#[test]
fn completion_outside_a_table_operation_offers_what_is_in_scope() {
    let program = "let limit = 3\n\
                   fn double(x: int) -> int { x * 2 }\n\
                   fn twice(n: int) -> int {\n  let extra = 1\n  if extra > $";
    let items = completions(&format!("{SALES}{program}"));
    assert!(fields(&items).is_empty());
    // The nearest first: the variables of the block, the parameter, then those of the file.
    assert_eq!(labels(&items, VARIABLE), ["extra", "n", "limit", "sales"]);
    let functions = labels(&items, FUNCTION);
    assert_eq!(functions[..2], ["twice", "double"]);
    assert!(functions.contains(&"print") && functions.contains(&"where"));
    let double = items.iter().find(|item| item["label"] == "double");
    assert_eq!(double.unwrap()["detail"], "fn(x: int) -> int");
    let keywords = [
        "let", "fn", "type", "import", "if", "else", "match", "and", "or", "not", "in", "true",
        "false", "null",
    ];
    assert_eq!(labels(&items, KEYWORD), keywords);
    assert_eq!(labels(&items, TYPE)[..2], ["Sale", "int"]);
    // The editor is told to keep this order.
    let order: Vec<&str> = items
        .iter()
        .filter_map(|item| item["sortText"].as_str())
        .collect();
    assert!(order.len() == items.len() && order.is_sorted());
    // A variable is not in scope before its `let`, nor in its own value.
    let items = completions(&format!("{SALES}let first = $\nlet second = 2\n"));
    assert_eq!(labels(&items, VARIABLE), ["sales"]);
    // A function sees the parameters of the function it is written in.
    let nested = "fn scale(factor: int) -> list<int> {\n  map([1, 2], fn(item) { item * $ })\n}\n";
    let items = completions(nested);
    assert_eq!(labels(&items, VARIABLE), ["item", "factor"]);
}

#[test]
fn completion_offers_functions_where_one_is_called() {
    let program = "fn top(t: table<Sale>) -> table<Sale> { t |> take(3) }\n";
    for call in ["sales |> $", "sales |> to$", "print(to$(sales))\n"] {
        let items = completions(&format!("{SALES}{program}{call}"));
        let functions = labels(&items, FUNCTION);
        assert_eq!(functions.len(), items.len(), "{call}");
        assert_eq!(functions[0], "top", "{call}");
        assert!(functions.contains(&"to_rows"), "{call}");
    }
}

#[test]
fn completion_offers_types_where_a_type_is_written() {
    for place in [
        "let total: $",
        "fn area(shape: Sa$",
        "fn first(t: table<Sale>) -> $",
        "fn top(t: table<Sa$",
        "type Pair = { left: Sale, right: $",
        "let other = read_csv<$>(\"other.csv\")\n",
    ] {
        let items = completions(&format!("{SALES}{place}"));
        let types = labels(&items, TYPE);
        assert_eq!(types.len(), items.len(), "{place}");
        assert_eq!(types[..3], ["Sale", "int", "float"], "{place}");
        let sale = "{region: string, units: int, price: float}";
        assert_eq!(items[0]["detail"], sale, "{place}");
    }
    // Before its `(`, the row type of a file reads as the right side of a comparison.
    let items = completions(&format!("{SALES}let other = read_csv<Sa$"));
    assert!(labels(&items, TYPE).contains(&"Sale"));
}

/// The keywords and the built-in types are lists of the server's own.
#[test]
fn completion_offers_the_keywords_and_types_of_the_language() {
    let items = completions("$");
    let mut messages = Vec::new();
    for keyword in labels(&items, KEYWORD) {
        messages.push(open(&format!("let {keyword} = 1\n")));
    }
    for name in labels(&items, TYPE) {
        messages.push(open(&format!("type {name} = bool\n")));
    }
    let replies = exchange(messages);
    assert_eq!(replies.len(), 14 + 11);
    for reply in &replies[..14] {
        let message = reply["params"]["diagnostics"][0]["message"].as_str();
        assert!(message.unwrap().starts_with("expected a variable name"));
    }
    for reply in &replies[14..] {
        let message = reply["params"]["diagnostics"][0]["message"].as_str();
        assert!(message.unwrap().ends_with("is a built-in type"));
    }
}

#[test]
fn completion_offers_the_names_of_an_imported_file() {
    let did_open = |uri: &str, text: &str| {
        let document = json!({ "uri": uri, "languageId": "biggo", "version": 1, "text": text });
        json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": document } })
    };
    let params = json!({
        "textDocument": { "uri": "file:///work/main.bgo" },
        "position": { "line": 1, "character": 10 },
    });
    let replies = exchange(vec![
        // The imported file exists only in the editor, not on disk.
        did_open("file:///work/rates.bgo", "let rate = 0.07\n"),
        did_open("file:///work/main.bgo", "import \"rates.bgo\"\nlet tax = "),
        request(1, "textDocument/completion", params),
    ]);
    let items = replies[2]["result"].as_array().unwrap();
    assert_eq!(labels(items, VARIABLE), ["rate"]);
}

#[test]
fn completion_offers_nothing_where_no_name_fits() {
    for place in [
        "print(sales |> where(region == \"no$\"))\n",
        "print(sales) // the sa$\n",
        "print(sales |> take(1$))\n",
        "let to$",
        "fn total(am$",
        "print(sales) $",
    ] {
        let items = completions(&format!("{SALES}{place}"));
        assert!(items.is_empty(), "{place}");
    }
    // A file that is not open, and a position past the end of the text.
    let elsewhere = json!({
        "textDocument": { "uri": "file:///other.bgo" },
        "position": { "line": 0, "character": 0 },
    });
    let past = json!({
        "textDocument": { "uri": "file:///demo.bgo" },
        "position": { "line": 9, "character": 9 },
    });
    let replies = exchange(vec![
        open("let x = 1\nprint(x)"),
        request(1, "textDocument/completion", elsewhere),
        request(2, "textDocument/completion", past),
    ]);
    assert_eq!(replies[1]["result"], json!([]));
    assert_eq!(replies[2]["result"], json!([]));
}

/// Whatever the text and wherever the cursor, the server answers with a list.
#[test]
fn completion_answers_at_every_position() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/");
    for file in ["sales.bgo", "core.bgo", "errors/syntax.bgo"] {
        let program = std::fs::read_to_string(format!("{root}{file}")).unwrap();
        let mut messages = Vec::new();
        let cuts = (0..=program.len()).filter(|cut| program.is_char_boundary(*cut));
        for (id, cut) in cuts.enumerate() {
            // The whole file with the cursor here, then only the text typed up to here.
            messages.push(open(&program));
            messages.push(complete(id as u32, &program, cut));
            messages.push(open(&program[..cut]));
            messages.push(complete(id as u32, &program[..cut], cut));
        }
        let asked = messages.len() / 2;
        let replies = exchange(messages);
        let answers = replies.iter().filter(|reply| reply.get("id").is_some());
        assert_eq!(answers.clone().count(), asked, "{file}");
        assert!(answers.clone().all(|reply| reply["result"].is_array()));
    }
}
