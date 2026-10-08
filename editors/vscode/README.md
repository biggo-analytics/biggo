# biggo for VS Code

Editor support for biggo programs (`.bgo` files):

- syntax highlighting, bracket matching, and `//` comment toggling
- errors as you type, from the same checker that `biggo check` runs
- the type of the expression under the cursor on hover — for a table, its columns
- **Format Document**, with the formatter behind `biggo fmt`
- commands: **biggo: Run File**, **biggo: Explain Query Plans of File**, **biggo: Run Tests**,
  **biggo: Restart Language Server**

Highlighting comes from a grammar in this extension. Everything else comes from the language
server built into the `biggo` executable (`biggo lsp`), so the editor and the command line
always agree.

## Install

The extension needs `biggo` itself. Build it from the root of this repository and put it on
your `PATH`:

```sh
cargo build --release
cp target/release/biggo ~/.local/bin/     # or anywhere on PATH
```

Then package and install the extension:

```sh
cd editors/vscode
npm install
npx vsce package --allow-missing-repository --skip-license
code --install-extension biggo-0.1.0.vsix
```

To try it without installing, open this folder in VS Code and press `F5`: a second window
opens with the extension loaded.

## Settings

| Setting      | Default | Meaning                                                        |
| ------------ | ------- | -------------------------------------------------------------- |
| `biggo.path` | `biggo` | The executable to run: a full path, or a name found on `PATH`. |

biggo files use two-space indentation, which the extension sets for the language.

## Other editors

Any editor that speaks the Language Server Protocol can use `biggo lsp`; it talks over
standard input and output. The server sends diagnostics and answers hover and formatting
requests, and wants the whole text of a file on every change (full document sync).

Neovim 0.11 or later:

```lua
vim.filetype.add({ extension = { bgo = "biggo" } })
vim.lsp.config("biggo", {
  cmd = { "biggo", "lsp" },
  filetypes = { "biggo" },
  root_markers = { ".git" },
})
vim.lsp.enable("biggo")
```

Helix, in `~/.config/helix/languages.toml`:

```toml
[language-server.biggo]
command = "biggo"
args = ["lsp"]

[[language]]
name = "biggo"
scope = "source.biggo"
file-types = ["bgo"]
comment-token = "//"
indent = { tab-width = 2, unit = "  " }
language-servers = ["biggo"]
```

## Working on the grammar

`syntaxes/biggo.tmLanguage.json` lists the built-in functions by name. A test in the
compiler (`cargo test -p biggo-types`) fails if a built-in is missing from it.

`test/tokens.js` runs the grammar through the engine VS Code uses and prints the scopes of
each token, so a change can be checked without opening an editor:

```sh
npm install --no-save vscode-textmate vscode-oniguruma
node test/tokens.js ../../testdata/sales.bgo         # every token and its scopes
node test/tokens.js ../../testdata/sales.bgo plain   # the words that got no scope
```
