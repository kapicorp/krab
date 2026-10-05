# Language server

```
Status: As-built
Verified against: main @ a4fb9c4
Code: crates/krab-lsp/, crates/krab/src/main.rs (the `lsp` command),
      editors/vscode/
```

## Problem

People editing an inventory want to see, while they type, what a parameter
resolves to in each target that includes the file, where that value was
written or overridden, which class file a name refers to, and which targets
fail to render. All of that is already computed by the inventory server
([daemon.md](daemon.md)). The language server (`krab lsp`) translates LSP
requests into daemon calls and renders nothing itself, so the editor shows
exactly what the CLI would compile.

This shapes the design. Values come from the daemon's render of the saved
files; the open buffer is used only to find what is under the cursor. A
file's audience is the set of targets rendered from it (`inventory.deps`),
and hover and definition ask the daemon about each of those targets. The
language server starts the daemon when needed and must survive the daemon
restarting (a `.kapitan` edit, `krab server stop`) without spinning, and it
must exit when its editor goes away so that it does not keep restarting
daemons nobody uses.

The VS Code extension in `editors/vscode` is a thin launcher for
`krab lsp`; this spec covers it only as far as it fixes how the server is
started. The reasoning behind cursor mapping and audiences is in
[DESIGN.md](../DESIGN.md#language-server-krab-lsp); the command line is in
[CLI.md](../CLI.md#krab-lsp).

## Requirements

### Running

```
LSP-1  `krab lsp` MUST speak LSP over stdin and stdout.
       Test: crates/krab/tests/lsp_exit.rs::lsp_exits_when_the_client_closes_stdin
       Since: 627c332

LSP-2  `krab lsp` MUST accept and ignore `--stdio`.
       Test: none
       Since: 1bc6ce9

LSP-3  `krab lsp` MUST refuse to start with `--no-daemon`, KRAB_NO_DAEMON or
       `--raw`, with "the language server needs the inventory daemon; drop
       --no-daemon / --raw".
       Test: none
       Since: 627c332

LSP-4  The inventory, `.kapitan` and the daemon MUST be resolved from the
       working directory as for any other command; file paths shown in hover
       and completion are relative to that directory.
       Test: none
       Since: 627c332

LSP-5  The server MUST advertise exactly: full text document sync, hover,
       definition, and completion with trigger characters `{`, `.` and
       space.
       Test: scripts/lsp-smoke.py (manual, prints the capabilities; the only
       check)
       Since: 627c332

LSP-6  A request for any other method MUST be answered with MethodNotFound
       ("unsupported: <method>"), and parameters that do not deserialise
       with InvalidParams.
       Test: none
       Since: 627c332

LSP-7  The server MUST keep the text of open documents from `didOpen` and
       the last content change of each `didChange`, drop it on `didClose`,
       and use it, or the file on disk when the document is not open, only
       to find what is under the cursor.
       Test: none
       Since: 627c332
```

### Talking to the daemon

```
LSP-8  Every inventory value, target list, diagnostic and class usage MUST
       come from the daemon. The only local inventory work is resolving a
       class name to its file, relative to the file being edited, with the
       engine's `resolve_class_file`.
       Test: none
       Since: 627c332

LSP-9  Requests MUST share one daemon connection. On a transport failure the
       server MUST reconnect, starting a daemon when none answers, and retry
       once; an error answered by the daemon is returned without a retry.
       Test: none
       Since: 627c332

LSP-10 Diagnostics updates MUST long-poll `inventory.wait` (timeout 60 s) on
       a dedicated connection, so a poll never blocks requests.
       Test: none
       Since: 627c332

LSP-11 After failing to connect, or after a failed wait, the diagnostics
       thread MUST wait before reconnecting: 1 s, doubling up to 60 s, reset
       to 1 s after a successful wait. Each new connection MUST start
       waiting from generation 0.
       Test: none
       Since: #159

LSP-12 The server MUST exit when the client closes stdin, or after
       `shutdown` and `exit`, without waiting for the diagnostics thread,
       and the thread MUST NOT publish or reconnect once the server is
       exiting.
       Test: crates/krab/tests/lsp_exit.rs::lsp_exits_when_the_client_closes_stdin
       Since: #159
```

### Cursor mapping

```
LSP-13 A cursor position MUST map to the key path it is on, or to the
       scalar value with the offset inside it, or to an item of the
       `classes:` list, from the document text alone.
       Test: crates/krab-lsp/src/yaml_index.rs::tests::indexes_keys_values_and_classes
       Since: 627c332

LSP-14 A `classes:` item MUST be treated as a class name. Under
       `parameters`, a cursor inside `${name:...}` (a resolver name of
       letters, digits, `_` and `.`, with no nested `${`) MUST be treated as
       a resolver call; a cursor inside any other `${...}` as a reference to
       the parameter path it names, relative references (`${.x}`) resolved
       from the referencing node's parent; anywhere else as that key's
       parameter path. Other top-level keys yield nothing.
       Test: none
       Since: 627c332
```

### Hover

```
LSP-15 Hover on a parameter path or a reference MUST show the value in each
       target rendered from the file: targets are grouped by identical value,
       groups ordered by size, at most 4 shown and the rest counted; each
       group shows the value's type, the targets, the value as YAML (at most
       12 lines), where it was written ("written by kapitan" when it has no
       origin), the interpolation it was resolved from, and how many earlier
       values it overrode. Targets without the path are counted as "Not
       present in N targets". When no target is rendered from the file the
       hover MUST say so ("No target includes this file, so it is never
       rendered.").
       Test: scripts/lsp-smoke.py (manual, prints hover results; the only
       check)
       Since: 627c332

LSP-16 Hover on a class name MUST show the class file relative to the
       working directory, or "not found (no matching file under
       `classes/`)", and the number of targets that include it.
       Test: none
       Since: 627c332

LSP-17 Hover on a resolver call MUST show "Resolver `<name>`".
       Test: none
       Since: 627c332
```

### Definition

```
LSP-18 Definition on a class name MUST return the start of its class file.
       On a parameter path or a reference it MUST return the origin of the
       value and the old and new locations of every override, collected
       over all targets rendered from the file, sorted and without
       duplicates. On a resolver call, or when nothing is found, it returns
       null.
       Test: scripts/lsp-smoke.py (manual, prints definition results; the
       only check)
       Since: 627c332
```

### Completion

```
LSP-19 After `${` with no `:` or `}` between it and the cursor, completion
       MUST offer the keys of `parameters.<typed parent path>` as rendered
       for the first target (by name) rendered from the file, filtered by
       the typed prefix. A mapping is offered as kind Module with detail
       "mapping"; anything else as kind Field with its type, list length or
       the first 40 characters of a string as detail. Without such a target,
       or when the parent is not a mapping, it returns null.
       Test: scripts/lsp-smoke-live.py (manual, prints completions; the only
       check)
       Since: 627c332

LSP-20 In a `classes:` item, including a new `- ` item under a top-level
       `classes:` key, completion MUST offer every class name from
       `inventory.class_usage`, kind Class, with the class file and its
       target count as detail.
       Test: scripts/lsp-smoke-live.py (manual, prints completions; the only
       check)
       Since: 627c332
```

### Diagnostics

```
LSP-21 The server MUST publish diagnostics once after `initialize` and again
       after every wait that reports a new generation.
       Test: scripts/lsp-smoke-live.py (manual, breaks a class on disk and
       prints what is published; the only check)
       Since: 627c332

LSP-22 Each daemon error MUST be published with severity Error and each
       warning with severity Warning, at the first label that has a location,
       or at line 1 of the target's file when none has; a diagnostic with
       neither is dropped. The range runs from the column to the end of the
       line. The message is the diagnostic's message, the primary label's
       text in parentheses unless it is "here", and the help on a new line.
       `code` is the diagnostic code, `source` is "krab (<target>)" or
       "krab", and the other labels with locations become related
       information.
       Test: none
       Since: 627c332

LSP-23 A file that had diagnostics in the previous publish and has none now
       MUST receive an empty publish.
       Test: none
       Since: 627c332

LSP-24 When the daemon cannot be reached, the server MUST tell the user, and
       MUST NOT leave diagnostics it can no longer refresh shown as current
       or report a file as included by no target.
       Test: none
       Since: 627c332
```

### VS Code extension

```
LSP-25 The extension MUST activate on `workspaceContains:**/.kapitan` or
       `onLanguage:yaml` and start one `krab lsp` per workspace folder that
       holds `.kapitan` at its root or in a first-level subdirectory, with
       that directory as working directory, over stdio, for `yaml` files
       below it.
       Test: none
       Since: 627c332

LSP-26 The setting `krab.path` (default `krab`) MUST name the binary
       started, and a non-empty `krab.python` MUST be passed to it as
       KRAB_PYTHON, which the daemon it starts inherits.
       Test: none
       Since: #86

LSP-27 The command `krab.restartServer` MUST stop every client and start
       them again; a change of workspace folders MUST start clients for new
       folders that qualify under LSP-25.
       Test: none
       Since: 627c332

LSP-28 CI MUST package the extension as a `.vsix` on every pull request
       (artifact `krab-vscode`), and the release workflow MUST attach it to
       the release.
       Test: none
       Since: #72
```

## Acceptance criteria

LSP-AC-1. Client goes away (LSP-12, LSP-1). Given `krab lsp` initialised in an
inventory directory, when the client closes stdin, then the process exits
within 10 s. Test:
crates/krab/tests/lsp_exit.rs::lsp_exits_when_the_client_closes_stdin.

LSP-AC-2. Daemon restart mid-session (LSP-11, LSP-21). Given an editor session
with published diagnostics, when `krab server stop` runs, then the
diagnostics thread's wait fails, it reconnects after 1 s, a new daemon
starts, and diagnostics are published again. Test: none (verified by hand in
#159).

LSP-AC-3. Live diagnostics (LSP-10, LSP-21, LSP-22, LSP-23). Given an open
target file, when a class it includes is saved with a missing class name,
then an error is published, and when the class is restored the error is
cleared. Test: scripts/lsp-smoke-live.py, run by
hand against an inventory that has the paths it hard-codes.

LSP-AC-4. Hover across targets (LSP-13, LSP-14, LSP-15). Given a class file
included by several targets that resolve one parameter differently, when
hovering that key, then the hover lists each distinct value once with the
targets that have it and where it was written. Test: scripts/lsp-smoke.py,
run by hand.

## Edge cases

LSP-EC-1. The daemon is unreachable (it fails to start, or its socket path is
too long): each request first tries to start a daemon, which can take the
client's 10 s start-up wait, and then answers as if the file were in no
target; diagnostics stay as last published, and the diagnostics thread keeps trying with back-off,
spawning a daemon on each attempt (open deviation #138; zombie daemons:
daemon.md, #169).

LSP-EC-2. The buffer has unsaved edits that shift lines: the cursor is mapped
on the buffer, but values and diagnostic positions come from the saved
files, so they can point at the wrong line until the file is saved.

LSP-EC-3. A file no target includes: hover says so, definition returns null,
and `${` completion returns null.

LSP-EC-4. More than four distinct values: the hover ends with "…and N more
distinct values".

LSP-EC-5. Requests are handled one at a time on the message loop. The first
request after the daemon was started waits for its initial render, and a
slow daemon call delays every request behind it.

LSP-EC-6. A missed generation bump delays a diagnostics update until the
60 s wait times out (daemon.md, #135).

LSP-EC-7. The configured Python interpreter for resolvers does not exist in
the editor's environment: `krab lsp` exits at start with the
missing-interpreter diagnostic. `krab.python` sets KRAB_PYTHON for it.

LSP-EC-8. `krab.path` names another build than the shell's `krab`: each
build keeps its own daemon (daemon.md, SRV-11), so two daemons render the
same inventory.

LSP-EC-9. A workspace folder is removed: its client keeps running until
`krab.restartServer` or the extension is deactivated.

## Interfaces

Command: `krab lsp [--stdio]`, run in the directory that holds `.kapitan`.

Capabilities sent in the `initialize` response:

```json
{
  "textDocumentSync": 1,
  "hoverProvider": true,
  "definitionProvider": true,
  "completionProvider": { "triggerCharacters": ["{", ".", " "] }
}
```

Requests handled: `textDocument/hover` (Markdown content), `textDocument/definition`
(an array of locations, or null), `textDocument/completion` (an array of items,
or null). Notifications handled: `didOpen`, `didChange`, `didClose`;
`didSave` is accepted and ignored. Notifications sent:
`textDocument/publishDiagnostics` without a version.

Daemon methods used: `inventory.deps`, `inventory.explain`,
`inventory.target`, `inventory.targets`, `inventory.diagnostics`,
`inventory.class_usage`, `inventory.wait` (shapes in daemon.md).

VS Code contributions: settings `krab.path`, `krab.python`,
`krab.trace.server` (`off`, `messages`, `verbose`); command
`krab.restartServer`; language client id `krab`, one output channel per
root named "Kapitan (<directory name>)".

## Out of scope

| Item | Reason | Issue |
|---|---|---|
| Rendering the unsaved buffer | Values come from the daemon's render of saved files | #5 |
| Document symbols, find references, rename | Not built | #6 |
| Code actions | Not built | #7 |
| Semantic tokens for `${...}` and resolver names | Not built | #9 |
| Neovim and Helix client configuration | Only the VS Code client ships | #10 |
| End-to-end test of the VS Code extension | No editor-driven test exists; the smoke scripts drive the server directly | #4 |
| Transports other than stdio | Editors start the server as a child process | none |

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| LSP-24 | #138 | No degraded mode: when the daemon call fails, `publish_diagnostics` returns without publishing, so stale diagnostics stay, nothing tells the user, and hover reports "No target includes this file". The orphaned server and the missing back-off from the same issue are fixed (LSP-11, LSP-12). |