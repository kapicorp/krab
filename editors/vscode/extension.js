// Starts `krab lsp` for every workspace folder that holds a `.kapitan`
// file and connects it to YAML documents under that folder.
const path = require("path");
const fs = require("fs");
const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let clients = new Map();

function kapitanRoot(folder) {
  const root = folder.uri.fsPath;
  if (fs.existsSync(path.join(root, ".kapitan"))) return root;
  // Common layout: the kapitan project is a subdirectory (e.g. kapitan/).
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    if (entry.isDirectory() && fs.existsSync(path.join(root, entry.name, ".kapitan"))) {
      return path.join(root, entry.name);
    }
  }
  return null;
}

async function startClient(root) {
  if (clients.has(root)) return;
  const config = vscode.workspace.getConfiguration("krab");
  // The language server starts the inventory daemon with this environment,
  // and the extension host's PATH rarely holds a Python with omegaconf.
  const env = { ...process.env };
  const python = config.get("python", "");
  if (python) env.KRAB_PYTHON = python;
  const serverOptions = {
    command: config.get("path", "krab"),
    args: ["lsp"],
    options: { cwd: root, env },
    transport: TransportKind.stdio,
  };
  const clientOptions = {
    documentSelector: [
      { scheme: "file", language: "yaml", pattern: `${root}/**/*.{yml,yaml}` },
    ],
    outputChannelName: `Kapitan (${path.basename(root)})`,
    workspaceFolder: vscode.workspace.getWorkspaceFolder(vscode.Uri.file(root)),
  };
  const client = new LanguageClient("krab", "krab", serverOptions, clientOptions);
  clients.set(root, client);
  try {
    await client.start();
  } catch (err) {
    // Forget it, or restart would skip this root forever.
    clients.delete(root);
    throw err;
  }
}

async function stopAll() {
  // stop() throws on a client that is not running.
  for (const client of clients.values()) await client.stop().catch(() => {});
  clients.clear();
}

async function startAll() {
  for (const folder of vscode.workspace.workspaceFolders || []) {
    const root = kapitanRoot(folder);
    if (root) await startClient(root);
  }
}

exports.activate = async function activate(context) {
  context.subscriptions.push(vscode.commands.registerCommand("krab.restartServer", async () => {
    await stopAll();
    await startAll();
  }));
  context.subscriptions.push(vscode.workspace.onDidChangeWorkspaceFolders(startAll));
  await startAll();
};

exports.deactivate = stopAll;
