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
| Understand an editor feature | `crates/krab-lsp/src/backend.rs` and `yaml_index.rs` |

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
* `model.rs` reproduces pydantic normalisation of `parameters.kapitan`.
  `--raw` skips it.
* `emit/` writes PyYAML-, rapidyaml- and Python-`json.dumps`-compatible output.

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

`inputs/` holds native input types, `refs/` the reference backends, `pyenv.rs`
the venv krab builds from `.kapitan`'s `compile.python-requirements`, and
`runner/kapitan/` a small bundled `kapitan` package that serves components'
`kapitan.*` imports so the Python kapitan is not needed. `--backend python`
runs kapitan's own input types instead, and does need it.

### `krab-lsp`

A thin translator that renders nothing itself. `yaml_index.rs` maps a cursor to
a key path, a scalar or a `classes:` entry from buffer content; values always
come from the daemon's render of saved files.

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

### Every error is a `Diagnostic`

Code, message, origins and `help`, so it renders the same through miette,
through `--json` and in the editor.

### The daemon and the local path run the same code

A feature that works only with the daemon, or only without it, is a bug.
`--no-daemon`, `--raw` and a failed server start all fall back to a local
render with identical results.
