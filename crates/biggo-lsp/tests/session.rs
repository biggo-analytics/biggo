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
    assert_eq!(replies[0]["result"]["capabilities"]["hoverProvider"], true);
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
