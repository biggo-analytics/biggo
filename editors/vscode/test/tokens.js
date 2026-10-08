// Tokenizes a biggo file with the grammar of this extension, the way VS Code does, and
// prints each token with its scopes. It is how the grammar is checked without an editor:
//
//   npm install --no-save vscode-textmate vscode-oniguruma
//   node test/tokens.js ../../testdata/sales.bgo         # every token and its scopes
//   node test/tokens.js ../../testdata/sales.bgo plain   # only the words left unscoped
const fs = require("fs");
const path = require("path");
const oniguruma = require("vscode-oniguruma");
const textmate = require("vscode-textmate");

const grammarPath = path.join(__dirname, "../syntaxes/biggo.tmLanguage.json");
const [sourcePath, mode = "all"] = process.argv.slice(2);
if (!sourcePath) {
  console.error("usage: node test/tokens.js <file.bgo> [plain]");
  process.exit(2);
}
const wasm = fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm")).buffer;
const registry = new textmate.Registry({
  onigLib: oniguruma.loadWASM(wasm).then(() => ({
    createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
    createOnigString: (text) => new oniguruma.OnigString(text),
  })),
  loadGrammar: async () => textmate.parseRawGrammar(fs.readFileSync(grammarPath, "utf8"), grammarPath),
});

registry.loadGrammar("source.biggo").then((grammar) => {
  let state = textmate.INITIAL;
  const unscoped = new Map();
  for (const line of fs.readFileSync(sourcePath, "utf8").split("\n")) {
    const result = grammar.tokenizeLine(line, state);
    state = result.ruleStack;
    for (const token of result.tokens) {
      const text = line.slice(token.startIndex, token.endIndex);
      const scope = token.scopes[token.scopes.length - 1];
      if (mode === "all") {
        if (text.trim()) console.log(`${text.padEnd(28)} ${token.scopes.slice(1).join(" ")}`);
      } else if (scope === "source.biggo") {
        for (const word of text.split(/[\s()\[\]{},.:]+/).filter(Boolean)) {
          unscoped.set(word, (unscoped.get(word) || 0) + 1);
        }
      }
    }
  }
  if (mode !== "all") console.log([...unscoped.keys()].join(" "));
});
