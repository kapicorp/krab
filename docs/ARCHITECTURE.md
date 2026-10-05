# Architecture

A guided tour of the workspace for people about to change it. `DESIGN.md` is
the reference for what the semantics are; this file is about where things live
and which constraints hold them together.

## Bird's eye view

krab reads a Kapitan inventory (`.kapitan`, `inventory/classes`,
`inventory/targets`, `refs/`, `resolvers.py`) and produces two things: rendered
target documents, and compiled output. Both must be byte-identical to
kapitan 0.36.3 with the omegaconf inventory backend.

Five crates, layered:

```
krab            binary: parses flags, formats output
krab-lsp        language server            krab-compile   incremental compile
        \                                  /
         krab-server   daemon: in-memory inventory, file watching, JSON-RPC
                              |
                       krab-inventory   the engine, and the library entry point
```

`krab-inventory` stays free of daemon, compile and CLI concerns. Everything the
CLI prints is computed in `krab-inventory` or `krab-compile`; the CLI only
formats.

## Entry points

| You want to | Start at |
|---|---|
| Follow a command end to end | `crates/krab/src/main.rs`, then the `cmd_*.rs` for that subcommand |
| Understand rendering | `crates/krab-inventory/src/lib.rs`, then `inventory.rs` |
| Understand the daemon | `crates/krab-server/src/lib.rs`, then `state.rs` |
| Understand incremental compile | `crates/krab-compile/src/engine.rs` and `manifest.rs` |
| Understand an editor feature | `crates/krab-lsp/src/server.rs`, then `yaml_index.rs` and `backend.rs` |
| Add something | [Extending](#extending) |

## Code map

### `krab-inventory`

The engine pipeline, in the order a value travels through it:

* `yaml.rs` parses via the vendored `saphyr-parser`, applying PyYAML
  `safe_load` YAML 1.1 scalar rules.
* `classfile.rs` shapes a single file.
* `inventory.rs` resolves class names, including the reference's two
  reclass-compat fallbacks, and memoises a `ClassClosure` per class file.
* `merge.rs` applies OmegaConf `unsafe_merge(EXTEND_UNIQUE)`.
* `interp/` parses and evaluates `${...}` against the resolver `Registry`. The
  parser follows OmegaConf's ANTLR grammar token for token.
* `resolvers/` holds the resolver sets: `mod.rs` the `Registry` and
  `with_builtins`, `oc.rs` OmegaConf's `oc.*`, `builtin.rs` kapitan's own,
  `contrib.rs` the contributed helpers, and `python.rs` the functions of a
  user `resolvers.py`, run by `runner/resolver_runner.py`.
* `model.rs` reproduces pydantic normalisation of `parameters.kapitan`.
  `--raw` skips it.
* `emit/` writes PyYAML-, rapidyaml- and Python-`json.dumps`-compatible output.

`dotkapitan.rs` parses `.kapitan`. Its getters (`compile_str`,
`compile_bool`, ...) take the key as a string; the consumers in
`crates/krab/src/cmd_*.rs` name the keys.

`python.rs` is the shared Python bridge: interpreter choice, the embedded
worker script, newline-delimited JSON. Both the Python resolvers and
`krab-compile`'s kadet and kapitan runners go through it.

### `krab-server`

One daemon per inventory directory *and* build.

* `state.rs` keeps an index from every path a target was rendered from, plus
  every path probed while resolving its class names, to the affected targets.
  A new `classes/common/init.yml` therefore invalidates exactly the right
  targets.
* `watch.rs` debounces `notify` events 150 ms.
* `protocol.rs` is JSON-RPC 2.0, one document per line, over a socket named by
  hashes of the inventory path and the build.
* `rpc.rs` accepts connections; `Server::dispatch` has one match arm per
  method.
* `client.rs` connects, spawns a server when there is none, and refuses one
  whose build or `PROTOCOL_VERSION` differs.

The socket binds *before* the initial render, so a second starter loses cheaply
and clients wait on readiness instead of a timeout. Secrets are never revealed
server-side.

### `krab-compile`

> [!IMPORTANT]
> Exact invalidation or nothing. Approximating staleness here is worse than
> not caching at all.

A target recompiles only when one of these changed:

* its rendered document digest;
* any path the previous compile read: file opens, directory listings,
  imported modules and search-path probes, all recorded by the Python worker;
* the documents of targets it read via `inventory_global()`;
* the compiler identity;
* the compiled output tree digest.

All of it lives in `compiled/.krab-manifest.json` next to the output, so the
cache travels with the output. `--explain` and `--dry-run` print the reason per
target. Within a stale target, kadet items whose own recorded inputs are
unchanged are reused rather than re-evaluated.

A compile goes from `engine.rs` (plan, staleness, fetch, one job per target)
to `native.rs`, whose `NativeCompiler::compile_item` dispatches on the item's
`input_type` string to `inputs/`. kadet, jinja2 and helm output goes through
the `Writer` in `output.rs`, which compiles or reveals refs and, for objects,
picks the `OutputType` and writes the file; copy, remove and external work on
files directly.

* `inputs/` holds the native input types; `inputs/mod.rs` has `Item` and
  `Reads`.
* `refs/` holds the reference backends, with `refs/mod.rs` dispatching on
  `RefType`.
* `fetch.rs` fetches `parameters.kapitan.dependencies` (git, http, helm) and
  `oci.rs` pulls OCI artifacts. The type names are also listed in
  `dependency_fields` in krab-inventory's `model.rs`, which runs first.
* `pyenv.rs` builds the venv from `.kapitan`'s
  `compile.python-requirements`.
* The Python runners live in `runner/`: `kadet_runner.py` evaluates kadet
  components (`inputs/kadet.rs`), and `kapitan_runner.py` runs kapitan's own
  input types for `--backend python` (`python.rs`), which needs the Python
  kapitan.
* `runner/kapitan/` is a small bundled `kapitan` package that serves
  components' `kapitan.*` imports, so the native backend does not need the
  Python kapitan. Every file in it is listed in `KADET_RUNNER_FILES`
  (`inputs/kadet.rs`).

### `krab-lsp`

A thin translator that renders nothing itself. `server.rs` holds the
message loop, the advertised capabilities and every handler. `yaml_index.rs`
maps a cursor to a key path, a scalar or a `classes:` entry from buffer
content; values always come from the daemon's render of saved files, through
`backend.rs`.

### `vendor/saphyr-parser`

The upstream crate plus two PyYAML-compatibility patches, described in
`vendor/README.md`. Keep the diff against upstream minimal.

## Cross-cutting invariants

Breaking one of these is usually quiet: the build still passes and a
user-visible surface goes wrong somewhere else.

### Provenance is never optional

`Value` is a JSON-like tree whose every `Node` carries an
`Origin { file, line, col }` interned in `Sources`, and `merge.rs` records
every override, list append and merge-time dereference as a `MergeEvent`.
`explain`, hover and definition all read that data, so a change that drops
origins breaks three surfaces at once.

### Equality and stringification follow Python

`1 == 1.0 == True`, `str(None) == "None"`, float `repr`. Those values end up
inside interpolated strings that have to match the reference byte for byte.

### Inventory errors are `Diagnostic`s

Code, message, origins and `help`, so it renders the same through miette,
through `--json` and in the editor. This holds for `krab-inventory`,
`krab-server` and the CLI. `krab-compile` returns `Result<_, String>`, so a
compile error is a plain message without a code. The codes are listed in
[diagnostics.md](diagnostics.md).

### The daemon and the local path run the same code

A feature that works only with the daemon, or only without it, is a bug.
`--no-daemon`, `--raw` and a failed server start all fall back to a local
render with identical results.

## Extending

Each list names every place a change of that kind touches. Paths are
relative to the repository root. Every change also edits the spec for its
area under [specs/](specs/README.md), and a deliberate difference from
kapitan gets a row in [DECISIONS.md](DECISIONS.md).

### A shipped resolver

1. The function and its `r.register("name", f)` in
   `crates/krab-inventory/src/resolvers/contrib.rs` (kapitan's own resolvers
   go in `builtin.rs`, OmegaConf's in `oc.rs`).
2. A call in a class under `tests/fixtures/inventory` and the regenerated
   `tests/fixtures/expected` ([tests/fixtures/README.md](../tests/fixtures/README.md)).
   The reference knows a contributed resolver only through
   `tests/fixtures/inventory/resolvers.py`, so the Python version goes there
   too. A resolver the fixture does not call goes into `UNCALLED_RESOLVERS`
   in `crates/krab-inventory/tests/fixture.rs`, which fails otherwise.
3. A same-named function in a user's `resolvers.py` replaces the native one
   unless `prefer-native` is set (`resolvers/python.rs`).

### An input type

1. `crates/krab-compile/src/inputs/<x>.rs` and its `pub mod` in
   `inputs/mod.rs`.
2. An arm in the `input_type` match of `NativeCompiler::compile_item` in
   `crates/krab-compile/src/native.rs`. A missing arm is not a compile
   error; it falls through to "not supported by the native compiler yet".
3. Every file and directory the input reads is recorded with `Reads::file`
   or `Reads::dir` (`inputs/mod.rs`). A read that is not recorded makes
   incremental compile keep stale output, and no test catches it.
4. The item's defaults in `compile_fields` in
   `crates/krab-inventory/src/model.rs`; a name new to kapitan also goes
   into the expected list in the `unknown input_type` message in
   `normalize`.
5. A golden test: `crates/krab/tests/helm_input.rs` with `tests/fixtures/helm`
   and `tests/fixtures/helm-expected` is the template, and
   [tests/fixtures/README.md](../tests/fixtures/README.md) says how the
   expected output is produced.
6. The open deviation OUT-3 in
   [specs/inputs-and-output.md](specs/inputs-and-output.md#open-deviations)
   and the native input list in `README.md`.

### An output type

`OutputType` in `crates/krab-compile/src/output.rs`: the variant, its name
in `parse`, and the arms in `ext` and `Writer::to_file`. The last two match
exhaustively, so the compiler finds them. `toml` already parses and fails in
its arm (open deviation OUT-29).

### A dependency type

1. The `Kind` variant and `Kind::name` in `crates/krab-compile/src/fetch.rs`.
2. The arm in the `type` match of `dependencies`, and the type list in its
   error message.
3. The arm in `fetch_group` and a `fetch_<x>` function (OCI lives in
   `oci.rs`).
4. The type's fields in `dependency_fields` in
   `crates/krab-inventory/src/model.rs`. Rendering runs the model before
   fetch, so without it every target that declares the type fails with
   "unknown dependency type". Add an item to
   `every_kind_is_accepted_by_the_inventory_model` in `fetch.rs`; its match
   stops compiling until the new variant is there.
5. A test against the `serve` mock in the `fetch.rs` tests.

### An RPC method

1. Parameter and result types in `crates/krab-server/src/protocol.rs`, and
   the method in its module doc list.
2. An arm in `Server::dispatch` in `crates/krab-server/src/rpc.rs`.
3. The caller in `crates/krab/src/cmd_*.rs`: `app.client()` returns a
   client, or `None` to render locally. Both branches compute the result
   through one library function; `class_usage` in `cmd_inventory.rs` and
   `Inventory::class_usage` show the shape.
4. A wrapper in `crates/krab-lsp/src/backend.rs` when the editor needs it.
5. The method in the protocol paragraph of [CLI.md](CLI.md#krab-server).
   `every_rpc_method_is_documented` in `crates/krab/tests/docs_consistency.rs`
   compares CLI.md and the `protocol.rs` list with `rpc.rs`.
6. A change existing clients cannot read bumps `PROTOCOL_VERSION` in
   `protocol.rs`; `client.rs` refuses a server with another one.

### An LSP capability

1. The field in `ServerCapabilities` in `run_stdio`,
   `crates/krab-lsp/src/server.rs`.
2. An arm in `handle_request` on the `lsp_types` `METHOD` constant, and the
   handler in `server.rs`. Cursor mapping comes from `yaml_index.rs`, values
   from the daemon through `backend.rs`.
3. The feature lists in the `crates/krab-lsp/src/lib.rs` module doc and in
   the `krab lsp` section of [CLI.md](CLI.md#krab-lsp). The VS Code
   extension (`editors/vscode`) is a generic client and needs no change.

### A subcommand or a flag

A subcommand is a variant of `Command` and an arm in `run`, both in
`crates/krab/src/main.rs`, plus a new `cmd_<x>.rs` with its `mod` line. A
flag is a field in the subcommand's args struct (`CompileArgs`,
`InventoryArgs`, `RefsArgs`, ...) and its use. Both get an entry in
[CLI.md](CLI.md); `crates/krab/tests/cli_docs.rs` fails when a command or
long flag is missing there, and shell completions follow from clap. A flag
that mirrors a `.kapitan` key follows the next list as well.

### A `.kapitan` key

1. Read it where it is used, through a getter of `DotKapitan` in
   `crates/krab-inventory/src/dotkapitan.rs` (`compile_str`,
   `compile_bool`, `compile_int`, `compile_strings`, `inventory_str`,
   `refs_str`): compile settings in `run` in `crates/krab/src/cmd_compile.rs`,
   refs settings in `cmd_refs.rs`. Inventory settings are parsed in
   `DotKapitan::load`.
2. A key that changes compiled output reaches `NativeOptions`
   (`crates/krab-compile/src/native.rs`), which `CompileOptions::config_digest`
   in `engine.rs` hashes through its `Debug` output, or is added to
   `config_digest` by hand. Otherwise a changed value does not recompile
   anything, and no test catches it.
3. A row in the `.kapitan` table of [CLI.md](CLI.md#kapitan).

### A diagnostic code

An `area::name` literal at the error site, through `Error::new`,
`Diagnostic::error` or `Diagnostic::warning`
(`crates/krab-inventory/src/error.rs`), and a row in
[diagnostics.md](diagnostics.md). `every_diagnostic_code_is_documented` in
`crates/krab/tests/docs_consistency.rs` fails without the row. Errors in
`krab-compile` are strings, so a coded compile error first needs that error
path changed.

### A ref backend

There is no single seam yet;
[exec-plans/144-secrets-backend-contract.md](exec-plans/144-secrets-backend-contract.md)
introduces one with a contract test suite, and a new backend should wait for
it or follow it. Today a backend touches `RefType` and `RefController` in
`crates/krab-compile/src/refs/mod.rs` (exhaustive matches the compiler
finds, plus `is_kms`, `is_vault` and `_ =>` arms it does not),
`crates/krab/src/cmd_refs.rs`, and the `secrets` fields in
`crates/krab-inventory/src/model.rs`.
