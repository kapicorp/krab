# Design

## Goals

1. **Compatibility first.** An existing inventory renders byte-identically to
   kapitan 0.36 (omegaconf backend). This is verified against a 160-target
   production inventory: identical JSON for every target and identical
   `kapitan inventory -t` YAML text.
2. **Fast.** Render one target without touching the others; render everything
   in parallel with class closures shared across targets.
3. **Explainable.** Every value knows the file, line and column it came from,
   what it overrode, and which interpolation produced it.
4. **Always on.** A daemon keeps the rendered inventory in memory, watches the
   files, and re-renders only what changed. The CLI works identically with or
   without it.
5. **Library.** Everything the CLI does is a function call in
   `krab-inventory`; the CLI and the server are thin.

## Data model

`Value` is a JSON-like tree (`Null | Bool | Int | Float | Str | List | Map`,
maps keep insertion order, keys are strings). Every `Node { value, origin }`
carries an `Origin { file, line, col }`; files are interned in `Sources`.
Synthetic nodes (metadata, resolver results) have `Origin::SYNTHETIC`.

Equality and stringification follow Python (`1 == 1.0 == True`, `str(None) ==
"None"`, float `repr`), because that is what the reference produces when an
interpolation is embedded in a string.

## Loading

`yaml.rs` parses with `saphyr-parser` (events with positions) and applies
PyYAML `safe_load` scalar rules (YAML 1.1: `yes`/`no`/`on`/`off` are booleans,
`0755` is octal, `1e5` and `1.5e3` are strings, `1:30` is 90). `<<` merge keys
and anchors work. Deviations from PyYAML: timestamps stay strings (the
reference cannot hold `datetime` values anyway); unknown tags are errors.

A class or target file is a `ClassDoc { classes, parameters, applications,
exports }`; `null` sections are empty, unknown top-level keys are ignored.

## Target names

A target is named after its file (`targets/prod/app.yml` is `app`), and
`compose-target-name` (or the older `compile.compose-node-name`) names it after
the path instead (`prod.app`). Off by default, as in the reference. The name is
what `_kapitan_.name.full` / `_reclass_.name.full` report, and what the compiled
directory follows: `compiled/app/` against `compiled/prod/app/`. `name.path`
(`prod/app`) and `name.short` (`app`) do not depend on the setting.

`TargetSpec::dotted_path` is the path spelling whether or not it is the name, so
`-t prod.app` selects the target in both modes.

Two files that end up with one name are an `inventory::conflicting_targets`
diagnostic naming both. The reference renders nothing at all in that case, and
says nothing (`docs/DECISIONS.md`, D7).

## Class resolution

`Inventory::resolve_class_file` mirrors the reference exactly, including its
two "reclass compatibility" fallbacks that drop the first two name components.
Relative names (`.foo`) resolve against the including class' directory.

For each file the loader builds a `ClassClosure`: its classes' closures merged
in order, then its own parameters. Merging is associative, so closures are
memoised per class file and shared between targets. A target is
`initial_parameters` (kapitan defaults + `_kapitan_`/`_reclass_` metadata)
merged with its file's closure.

Cycles are detected and reported (the reference recurses forever).

## Merge semantics (`merge.rs`)

`OmegaConf.unsafe_merge(dest, src, list_merge_mode=EXTEND_UNIQUE)`:

* map ← map: recurse per key, new keys appended;
* list ← list: append items of `src` not already in `dest` (Python `==`);
* `${…}` string ← container: the placeholder is evaluated against the tree
  merged so far. If it yields a container, that container is copied in and
  `src` merged into the copy. Otherwise `src` replaces the string;
* anything else: `src` replaces `dest`.

Every override, list append and dereference is recorded as a `MergeEvent`
with both origins (opt-out with `track_provenance = false`).

## Interpolation (`interp/`)

`parse.rs` is a hand-written port of OmegaConf's ANTLR grammar (lexer modes
and all): node paths `${a.b[0]}`, relative paths `${.x}` / `${..x}`, resolvers
`${name:arg, 'quoted ${nested}', [list], {k: v}}`, typing of unquoted
primitives (`3` is an int, `1-2` a string, `null`, `true`, `inf`), escaping.

`eval.rs` reproduces `OmegaConf.resolve()` applied twice plus
`to_container(resolve=True)`: three passes, each visiting nodes in order,
evaluating every string containing `${` and writing the result back. A node
interpolation aliasing a container resolves that container in place first and
then copies it. Resolver results are written back verbatim, so a resolver that
returns a string with `${` (e.g. `default`, `relpath`, `oc.dict.values`) is
evaluated on the next pass — exactly as in the reference. Cycles and references
to an enclosing container are errors with the full chain of locations.

After the passes, `${escape:x}` markers become literal `${x}`.

## Resolvers (`resolvers/`)

`Registry` maps names to `Fn(&mut Ctx, &[Value]) -> Result<Value>`. `Ctx`
exposes the node path, the parent key, the root, `select(key)` (resolved
lookup, relative when the key starts with `.`), `decode`, and warnings.
Three sets ship: `oc.*`, kapitan's built-ins (`key`, `parentkey`, `escape`,
`if`/`ifelse`/`and`/`or`/`not`/`equal`, `merge`, `dict`, `list`, `yaml`,
`add`, `default`, …) and `contrib` (`replace`, `json`, `to_yaml`, `sha256`,
`truncate`, `pluck`, `select_fields`, `filter_keys`, `join`, …).

Boolean resolvers use Python truthiness on purpose (`${if:nonempty,…}` is
true); a stricter mode is a planned opt-in.

A fourth set comes from the user's `resolvers.py` (`resolvers/python.rs`,
`runner/resolver_runner.py`), the file kapitan's omegaconf backend imported.
It runs in Python workers speaking newline-delimited JSON (`python.rs`, shared
with the compile runners). Each `pass_resolvers()` entry is registered as a
resolver that ships its arguments as JSON and, while the function runs,
answers `_root_` / `_parent_` lookups by calling `Ctx::select` on the
evaluator: Python sees fully resolved values, and a nested resolution failure
surfaces as the evaluator's own diagnostic rather than a traceback. The worker
patches `OmegaConf.select` / `to_container` / `is_config` to accept those
proxies, or installs a stand-in `omegaconf` module when the package is
missing. The import result (names, special arguments, project modules loaded)
is cached by file digest under `~/.cache/krab/resolvers`, so workers start
on first use only (at most `workers`, default CPUs capped at 8). Python wins
over same-named native resolvers unless `prefer-native` is set, because the
native `contrib` set is a port of one such file and cannot follow its edits.
The registry records the files it depends on; the daemon exits when one
changes and the next request starts a fresh one.

`write` (mutating the tree from a resolver) is not supported and reports why.

## Kapitan model (`model.rs`)

The reference validates `parameters.kapitan` with pydantic models, which fills
defaults into every `compile` and `dependencies` entry, orders fields, forces
helm's `output_type` to `auto`, and rejects unknown fields. `normalize()` does
the same; `--raw` skips it.

## Output (`emit/yaml.rs`)

A port of PyYAML's emitter for the value model: `analyze_scalar`, style
selection (plain / single / double quoted), 80-column folding of plain and
quoted scalars, ASCII-only output, sorted keys, and both indentation styles
(kapitan's `PrettyDumper` and the stock indentless sequences used by the
`yaml`/`to_yaml` resolvers).

## Diagnostics

Every error is a `Diagnostic { code, message, target, path, labels, help }`.
Labels carry origins that resolve to `file:line:col`. The CLI renders them with
miette (source snippets) or as JSON lines (`--json`).

## Server (`krab-server`)

One daemon per inventory directory, started on demand by the CLI (or with
`krab server start`), exiting after 30 minutes without requests.

* **State**: the `Inventory` (with its file and class-closure caches), the
  rendered targets, the failed targets with their diagnostics, and an index
  from every relevant path to the targets it matters to. "Relevant" means the
  files a target was rendered from *and* every path probed while resolving
  its class names, so creating `classes/common/init.yml` next to
  `classes/common.yml` invalidates exactly the targets that include `common`.
* **Watching**: `notify` (debounced 150 ms) on the inventory directory, plus
  the real directories of symlinked files. Any event on a path re-renders the
  indexed targets (prefix match for directories and vanished paths), retries
  every failed target, and picks up new or deleted target files. Atomic
  editor saves (write temp + rename) therefore cost one target render.
* **Protocol**: JSON-RPC 2.0, newline delimited, over
  `$XDG_RUNTIME_DIR/krab/<hash of inventory path>-<hash of build>.sock`
  (see `protocol.rs` for the method list). `inventory.wait` is a long poll on
  the generation counter; `krab inventory watch` is a thin client of it.
  The socket is bound before the initial render; `inventory.*` requests wait
  for the render, `server.*` ones answer at once (`ready: false`).
* **Parity**: the CLI uses the server when it can and renders locally
  otherwise (`--no-daemon`, `--raw`, or a server that failed to start); the
  same library code runs in both, so results are identical. Each build owns
  its socket, so two builds never fight over one daemon, and a client checks
  the build of the server that answers after it started one.
* **Secrets** are never revealed server-side; the inventory holds references
  only.

## Language server (`krab-lsp`)

A thin translator from LSP to the daemon's JSON-RPC, so the editor never
renders anything itself:

* `yaml_index.rs` maps a cursor position to a key path, a scalar (with the
  `${...}` expression under the cursor) or a `classes:` item, by walking the
  saphyr events of the buffer with their spans. Unsaved buffer content wins
  over disk for this; values always come from the daemon's render of the
  saved files.
* A file's audience is `inventory.deps` (the targets rendered from it);
  hover and definition run `inventory.explain` for each and group targets by
  value. Class names resolve through the same `resolve_class_file` the engine
  uses, relative to the file being edited.
* Diagnostics: a thread long-polls `inventory.wait` and republishes
  `inventory.diagnostics` after every generation, attaching each to the first
  label's location (or the target file's first line when there is none) and
  clearing files that became clean.

## Compile (`krab-compile`)

Principle: *exact invalidation or nothing*. A target is recompiled when, and
only when, one of these changed since its last compile:

1. its rendered document (`doc_digest`, computed by the inventory);
2. any path the previous compile read. The Python worker records file reads
   (`open`), directory listings (`os.scandir`/`listdir`), modules loaded via
   the import machinery (including kadet components and `kgenlib`, with
   `.pyc` mapped back to source), and the search-path probes that decide
   which input files are used;
3. the documents of other targets it read through `inventory_global()`
   (recorded per target name, or `*` when it iterated everything);
4. compile settings and the compiler identity (krab version, evaluator
   digest, Python-side kadet and Python versions);
5. the compiled output itself (a tree digest, so manual edits or a `git
   checkout` are noticed).

Everything is stored in `compiled/.krab-manifest.json`. `--explain` and
`--dry-run` print the reason per target.

Within a stale target, kadet items are reused rather than evaluated when
their own inputs did not change. The evaluator hands a component its target
document through a recording view: each `parameters.<key>` it reads is
noted (iteration, Box methods and writes count as reading all of it), the
way `inventory_global()` records other targets. The manifest keeps, per
kadet item, the digest of the item definition, the digests of the document
parts it read, its file and target dependencies and the files it wrote. When
all of those still match, the previous output files are copied into the new
compile tree; `compiled ... (0.14s, 2 kadet items reused)` says so. On
grid, a change to a parameter no generator reads recompiles a chart-heavy
cluster target in 0.14 s instead of 10 s. `--force` disables reuse.

Execution: stale targets are compiled on a thread pool (one per CPU), each
into a private temporary tree that then replaces `compiled/<target path>`
while leaving nested targets' directories alone. Full runs remove output
directories that belong to no target.

### Native input types (`krab-compile/src/inputs`, `output.rs`, `refs/`)

* `jinja2`: minijinja with Jinja2's environment (strict undefined,
  `trim_blocks`, `lstrip_blocks`, no auto-escaping), Python-style rendering of
  `True`/`False`/`None`, kapitan's filters, and `inventory_global` as a lazy
  object that fetches targets on access and records which ones were read.
* `copy`, `remove`, `external`: straightforward ports.
* `kadet`: a Python evaluator (`runner/kadet_runner.py`) imports the component
  and calls `main()`; its output comes back as JSON. It records files read,
  directories listed, modules imported and targets read from the global
  inventory. With an inventory server running it fetches targets over the
  socket on demand; without one it loads a snapshot file. The component's
  `kapitan.*` imports resolve to the package next to the evaluator
  (`runner/kapitan`): the component API (`inventory()`, `inventory_global()`,
  `topics()`, `load_from_search_paths`, `BaseObj`...), `HelmChart`,
  `utils.render_jinja2_file`/`prune_empty`, `resources.inventory()`, the
  error classes, and a `cached` module that presents krab's compile settings
  under the old `cached.args` names. It is built on `kapitan.runtime`, which
  the evaluator configures with the documents, the settings and the
  recorder; nothing in it reads a parsed command line or keeps process-wide
  state, and importing any other `kapitan.*` module is an error rather than
  a fall-through to an installed kapitan. The interpreter therefore only
  needs `kadet` (and `jinja2` for templates), not the Python kapitan.
  `HelmChart`
  renders inside a component go through the host: mid-evaluation the runner
  sends a `helm` request on its stdout (`worker.rs` answers host requests
  between the eval request and its reply) and `inputs/helm.rs` builds the
  `helm template` arguments the way kapitan's `render_chart` does, hashes
  the chart directory so every chart file becomes a dependency of the
  target, runs helm, caches the output by content under
  `$XDG_CACHE_HOME/krab/helm-render` and parses it with the inventory's
  loader. kapitan's kadet output cache is off because a hit would hide what
  a component reads.
* Output: `prune_empty`, output-type resolution, ref embedding
  (`?{type:base64(json(ref)):embedded}`) or hashing, then rapidyaml-compatible
  YAML (`emit/ryml.rs`, verified byte for byte on ~7000 compiled files),
  PyYAML fallback for control characters, and Python-compatible JSON.
  Multiline strings default to double quotes, matching a quirk of the
  reference where the compile flag is shadowed by the inventory one.
* References (`refs/`): a port of `kapitan/refs`. `RefController` loads ref
  files (cached), compiles tags (`?{type:path:hash}`, embedded payloads,
  `plain` inlined, `env` always hashed), creates missing refs from their
  functions (`random`, `sha256`, `rsa`, `ed25519`, `publickey`, `reveal`,
  `basicauth`, `base64`) with the target's `parameters.kapitan.secrets`, and
  reveals them. Mappings are compiled in passes so a `||reveal:` tag can
  depend on a sibling key's ref, as in kapitan. Backends: `plain`, `base64`,
  `env` in process; `gkms` over the Cloud KMS REST API with
  application-default credentials (`authorized_user` refresh, service
  account JWT, metadata server, `gcloud` fallback); `gpg` through the `gpg`
  binary with python-gnupg's flags; Vault KV/transit over the HTTP API with
  token/approle/userpass/ldap/github auth; `awskms`/`azkms` through their
  CLIs. Ref files are written with `yaml.safe_dump`'s layout so kapitan and
  krab can read each other's. kapitan's `mock` key is honoured for tests.

### Dependency fetching (`krab-compile/src/fetch.rs`)

`parameters.kapitan.dependencies` is fetched before staleness is decided,
so the files it produces are ordinary inputs: the inputs that read them
record them and the manifest tracks them. The daemon is asked for just that
path of each candidate target. Items are deduplicated by source and
destination across targets, grouped by source (git, http) or chart
identity (helm), and the groups fetched in parallel; each source is
fetched once per run into a temporary directory and copied to every
destination. `git` and `helm` are driven as subprocesses (the reference
does the same through GitPython and `helm pull`); http(s) uses `ureq`,
with tar, gzip and zip unpacking by content type or magic bytes, as
kapitan's `unpack_downloaded_file` does. Copying follows kapitan's
`safe_copy_tree` (never overwrite, skip dot-entries) or, when forced,
`copy_tree` (overwrite everything). Versioned helm charts are cached under
`$XDG_CACHE_HOME/krab/charts` because a published chart version is
immutable; `--force-fetch` pulls again.

`type: oci` (`oci.rs`) speaks the registry distribution API the way oras
does: the manifest (an index is followed to its first manifest) lists the
layers, each is downloaded to the path in its
`org.opencontainers.image.title` annotation (the digest when there is
none), verified against its `sha256` digest, and, as in kapitan's
`_extract_tar_blobs`, layers that are tar archives (gzipped or not) are
extracted into the artifact root and removed. Authentication answers one
`WWW-Authenticate` challenge: a bearer token from the realm (Docker Hub,
ghcr.io, Artifact Registry) with `OCI_USERNAME` / `OCI_PASSWORD` as basic
credentials when set, or basic authentication directly. `insecure` selects
plain http and `tls_verify` disables verification or names a CA bundle
(parsed with ureq's PEM reader). Conflicting connection settings for one
source are an error and `media_type` filters are unioned, as in the
reference.

Two deliberate differences from the reference: a dependency whose output
path already exists is not fetched (kapitan re-clones every git source and
adds files that happen to be missing), which keeps `fetch: true`
repositories offline once populated; and `force_fetch: true` on an item
forces that item even when `--fetch` is given (kapitan only honours it
when neither flag is set).

Known limits: `jsonnet`, `helm`, `kustomize`, `cuelang` inputs, `toml`
output. `--backend python` runs kapitan's Python input types instead.

## Testing

`tests/fixtures/inventory` is a small inventory exercising class resolution,
list merging, merge-time dereferencing, every shipped resolver, YAML 1.1
scalars and emitter quirks; `tests/fixtures/expected/*.yaml` is the reference
implementation's output for it (regenerate with `generate_expected.py`).
`crates/krab-inventory/tests/fixture.rs` renders it and compares byte for
byte. The production inventory this was developed against renders identically
for all 160 targets.
