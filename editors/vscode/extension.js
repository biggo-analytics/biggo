// Starts `biggo lsp` for biggo files, and adds commands that run the file being edited.

const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;
let terminal;

function executable() {
  return vscode.workspace.getConfiguration("biggo").get("path") || "biggo";
}

function startClient() {
  const serverOptions = {
    command: executable(),
    args: ["lsp"],
    transport: TransportKind.stdio,
  };
  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "biggo" }],
  };
  client = new LanguageClient("biggo", "biggo", serverOptions, clientOptions);
  return client.start().catch((error) => {
    vscode.window.showErrorMessage(
      `Cannot start the biggo language server with \`${executable()} lsp\`: ${error.message}. ` +
        "Install biggo, or set `biggo.path` to the executable."
    );
  });
}

async function stopClient() {
  const running = client;
  client = undefined;
  if (running) {
    await running.stop().catch(() => {});
  }
}

// A path or program name as one word of a shell command.
function quoted(word) {
  return /^[\w@%+=:,./-]+$/.test(word) ? word : `"${word.replace(/(["\\$`])/g, "\\$1")}"`;
}

// Runs `biggo <args>` in a terminal that is kept for the next run.
function runInTerminal(args) {
  if (!terminal || terminal.exitStatus !== undefined) {
    terminal = vscode.window.createTerminal("biggo");
  }
  terminal.show(true);
  terminal.sendText([executable(), ...args].map(quoted).join(" "));
}

// Saves the file being edited and runs `biggo <command>` on it.
async function runOnFile(command) {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "biggo") {
    vscode.window.showInformationMessage("Open a biggo file first.");
    return;
  }
  if (editor.document.isUntitled || !(await editor.document.save())) {
    vscode.window.showInformationMessage("Save the file to run it.");
    return;
  }
  runInTerminal([command, editor.document.uri.fsPath]);
}

function activate(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("biggo.runFile", () => runOnFile("run")),
    vscode.commands.registerCommand("biggo.explainFile", () => runOnFile("explain")),
    vscode.commands.registerCommand("biggo.runTests", () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      runInTerminal(folder ? ["test", folder.uri.fsPath] : ["test"]);
    }),
    vscode.commands.registerCommand("biggo.restartServer", async () => {
      await stopClient();
      await startClient();
    }),
    // A new `biggo.path` takes effect at once.
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("biggo.path")) {
        await stopClient();
        await startClient();
      }
    })
  );
  return startClient();
}

function deactivate() {
  return stopClient();
}

module.exports = { activate, deactivate };
