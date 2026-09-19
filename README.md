# krab

**krab** is [Kapitan](https://kapitan.dev) rewritten in Rust: a fast `krab`
CLI, an always-on inventory daemon, an incremental `compile`, a language
server for editors, and a library you can build on.

It reads the same `.kapitan`, `inventory/classes`, `inventory/targets`,
`refs/` and `resolvers.py` as the Python implementation and produces the same
output, byte for byte:

* `krab inventory -t <target>` matches kapitan 0.36 (omegaconf backend),
  about 60× faster for a full inventory and 1000× faster for a single target.
* `krab compile` writes the same files as the reference implementation, and
  only for the targets whose inputs actually changed. A no-op compile of a
  160-target production inventory takes a quarter of a second instead of the
  better part of a minute.
* Every value knows where it came from: the file, line and column that wrote
  it, what it overrode, and which `${...}` interpolation produced it. The
  CLI, the daemon and the editor all expose that.

Status: alpha. In place and verified against a production inventory: the
inventory (including a repository's Python `resolvers.py`), the daemon, the
language server, the native compile path for `jinja2`, `kadet`, `copy`,
`remove` and `external` inputs, references (`?{gkms:...}` and friends:
compile, create, reveal, `krab refs`), and dependency fetching (`git`,
`http(s)`, `helm`, `oci`). Not native yet: `jsonnet`, `helm` (as a direct
input type), `kustomize`, `cuelang` and `toml` output; see
[Compatibility](#compatibility).

## Install

Every [release](https://github.com/kapicorp/krab/releases) ships the `krab`
binary for Linux (x86_64 and aarch64, glibc 2.35 or newer) and macOS (Intel
and Apple silicon), a `SHA256SUMS` file, and the VS Code extension as a
`.vsix`:

```sh
version=2.0.0-alpha.4 target=x86_64-unknown-linux-gnu    # or aarch64-unknown-linux-gnu, x86_64-apple-darwin, aarch64-apple-darwin
curl -LO https://github.com/kapicorp/krab/releases/download/v$version/krab-$version-$target.tar.gz
tar xzf krab-$version-$target.tar.gz
install -m 755 krab-$version-$target/krab ~/.local/bin/krab
```

Releases before 2.0.0-alpha.4 named the archive and the binary `kapitan`.

Or build from source:

```sh
git clone https://github.com/kapicorp/krab.git
cd krab
cargo build --release
install -m 755 target/release/krab ~/.local/bin/krab
```

The binary is called `krab`, so it sits next to the Python `kapitan` without
a clash; keep the reference implementation around for the input types that
are not native yet.

Compiling `kadet` components needs a Python 3 on the machine: krab builds
itself a venv with `kadet`, `jinja2` and the packages the repository declares
in `.kapitan` (see [Compiling](#compiling)). The Python `kapitan` is not
required. A repository's `resolvers.py` runs in Python too (see [Python
resolvers](#python-resolvers)). Nothing else needs Python.

## Quick start

Run the commands from the directory holding `.kapitan` and `inventory/`.

```sh
krab inventory targets                    # every target: labels, classes, compile inputs, status
krab inventory -t my.target               # the rendered target, same YAML as kapitan
krab inventory -t my.target -p parameters.cluster --format json
krab inventory explain -t my.target cluster.name   # where the value came from, what it overrode
krab inventory check                      # render everything, report every problem
krab inventory deps inventory/classes/common.yml   # which targets a file affects
krab inventory classes --unused           # class files no target includes
krab compile                              # compile only what changed
krab compile --dry-run                    # what would compile, and why
krab compile --fetch                      # first fetch the dependencies that are missing
krab refs --reveal -f compiled/my/target/manifests/secret.yml
```

Target names are the dotted path of the target file:
`inventory/targets/platform/apps/grafana.yml` is `platform.apps.grafana`.

The first `krab inventory ...` starts a daemon for that inventory in the
background. It renders every target once, watches the files, and re-renders
only the affected targets when something changes, so every later command
answers in milliseconds and always reflects the files on disk. Pass
`--no-daemon` (or set `KRAB_NO_DAEMON=1`) to render locally instead;
results are identical. `krab server status | stop | logs` manages it. Each
build of krab keeps a daemon of its own, so a development build and the
installed release never disturb each other.

Add `--json` to any command for machine-readable output, and
`source <(krab completions bash)` for completion of commands, flags and
target names.

## Documentation

| document | what it covers |
|---|---|
| [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) | installing, the daemon, inspecting an inventory, compiling, editor setup |
| [docs/CLI.md](docs/CLI.md) | every command and flag, environment variables, `.kapitan` keys |
| [docs/DESIGN.md](docs/DESIGN.md) | the data model, the exact merge and interpolation semantics, provenance, the server protocol, how compile decides what is stale |
| [docs/ROADMAP.md](docs/ROADMAP.md) | current status and a pointer to the project board where planned work is tracked |
| [CONTRIBUTING.md](CONTRIBUTING.md) | building, testing, checking parity against the reference implementation |
| [editors/vscode/README.md](editors/vscode/README.md) | the VS Code extension |

## Layout

| path | what |
|---|---|
| `crates/krab-inventory` | the engine: YAML loading with source positions, class resolution, OmegaConf-compatible merge and `${...}` interpolation, resolver registry and the Python resolver bridge, provenance, PyYAML-compatible emitter |
| `crates/krab-server` | the inventory daemon: watches files, re-renders exactly what changed, JSON-RPC over a unix socket; and the client with auto-spawn |
| `crates/krab-compile` | incremental compile: native input types, dependency fetching (git, http, helm, oci), references and their backends, rapidyaml/PyYAML/JSON writers, staleness from a manifest; the kadet evaluator with its bundled `kapitan` package and the Python environment it runs in |
| `crates/krab-lsp` | language server over the daemon: live diagnostics, hover with resolved values and provenance, go to definition, completion |
| `crates/krab` | the `krab` binary |
| `editors/vscode` | VS Code extension that launches `krab lsp` |
| `tests/fixtures` | a small inventory and the reference implementation's output for it; a kadet component exercising the bundled `kapitan` API |
| `vendor/saphyr-parser` | the YAML parser, with two PyYAML-compatibility patches (see `vendor/README.md`) |

## Editing

`krab lsp` speaks the Language Server Protocol over stdio and answers from
the daemon, so everything it shows is the current render:

* **Diagnostics** appear on the class or target line that caused them, a few
  hundred milliseconds after a save, once per affected target.
* **Hover** on a parameter key shows the resolved value in every target that
  includes the file, grouped by value, with where it was written, what it was
  resolved from and how many earlier values it overrode. Hover on a `${...}`
  reference explains the referenced path; hover on a class name shows its
  file and how many targets include it.
* **Go to definition** on a class name opens its file; on a key or reference
  it lists every location that wrote the value across the affected targets.
* **Completion** offers class names in `classes:` lists and parameter paths
  inside `${`.

`editors/vscode` holds a small extension that starts the server for any
workspace folder containing `.kapitan`. It runs `krab` from `PATH`
(`kapitan.path` points it elsewhere) and passes `kapitan.python` on as
`KRAB_PYTHON` for inventories with Python resolvers.

## Compiling

`krab compile` renders the inventory (through the daemon when it is
running), then decides per target whether anything it was built from
changed: the rendered target document, every file and directory the previous
compile read (generator modules, templates, refs, `kgenlib`, helm charts,
...), the inventory of other targets it consulted through
`inventory_global()`, the compiler itself, and the compiled output on disk.
What each compile read is recorded in `compiled/.krab-manifest.json`.
`--explain` and `--dry-run` print the reason per target. Inside a recompiled
target, a `kadet` component whose inputs did not change is not re-run; its
previous output is reused.

Input types, ref embedding, pruning and the YAML/JSON writers are native.
`kadet` components are Python, so evaluating them needs a Python with
`kadet`, `jinja2` and whatever the components import. krab builds it:
declare the components' packages under `compile.python-requirements` in
`.kapitan` (pip specifiers or a requirements file) and the first compile
creates a venv under `~/.cache/krab/python/` with `uv` or `python3 -m
venv`. `$KRAB_PYTHON` names an interpreter to use as it is instead.
The component's `kapitan.*` imports (`inventory()`,
`inventory_global()`, `topics()`, `HelmChart`, `render_jinja2_file`,
`prune_empty`, ...) are served by a small `kapitan` package that ships with
krab, so the Python kapitan is not needed and none of its start-up cost is
paid. The evaluator only runs the component's `main()` and asks the daemon
for other targets when a generator reads them. Charts a component renders
through `HelmChart` are templated by the host, so their files enter the
manifest and identical renders are cached. `--backend python` runs
kapitan's own Python input types instead, for comparison or for input types
that are not native yet; that backend does need the Python kapitan.

Dependencies declared under `parameters.kapitan.dependencies` are fetched
natively before staleness is decided: `git` repositories (a `ref` and a
`subdir`), `http`/`https` files (optionally unpacked), `helm` charts (from a
repository URL or an `oci://` reference, cached per version) and `oci`
artifacts (what `oras push` produces). `--fetch`, or `fetch: true` in
`.kapitan`, fetches whatever is missing; `--force-fetch` refetches and
overwrites everything, for example after a generator or chart version bump.

References are native too. Compile turns `?{type:path}` tags into kapitan's
hashed or embedded form, creates refs that do not exist yet from their
functions (`?{gkms:targets/x/token||random:str}`, `||rsa`,
`||reveal:path|publickey`, ...) with the target's `parameters.kapitan.secrets`,
and `--reveal` decrypts them into the output. `krab refs` writes, reveals,
updates and validates ref files. Backends: `plain`, `base64`, `env`, `gkms`
(Cloud KMS REST API with application-default credentials), `gpg` (the `gpg`
binary), `vaultkv` and `vaulttransit` (Vault's HTTP API); `awskms` and
`azkms` go through the `aws` and `az` command line clients.

## Compatibility

Rendering and compiled output are verified byte for byte against kapitan
0.36.3 with the `omegaconf` inventory backend on the fixture inventory in
`tests/fixtures` and on a 160-target production inventory.

Deliberate differences: class cycles are reported instead of recursing
forever; unknown YAML tags are errors; timestamps stay strings; a dependency
whose output path already exists is not fetched at all, so a repository
with everything in place compiles offline. Not implemented yet: `jsonnet`,
`helm` (as a direct input type; charts rendered by kgenlib inside kadet
work), `kustomize` and `cuelang` inputs, `toml` output, Python-defined
jinja2 filters other than the common ones, and the `write` resolver.
[docs/DESIGN.md](docs/DESIGN.md) lists the semantics in detail.

## Writing a resolver

Resolvers are plain Rust functions registered by name:

```rust
use krab_inventory::resolvers::{Ctx, Registry, ResolverResult, arity, as_str};
use krab_inventory::Value;

fn shout(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("shout", args, 1, 1)?;
    Ok(Value::Str(as_str("shout", args, 0)?.to_uppercase()))
}

let mut registry = Registry::with_builtins();
registry.register("shout", shout);
```

`Ctx` gives access to the node being resolved (`ctx.at`, `ctx.key()`), the
whole tree (`ctx.select("a.b")` returns fully resolved values) and warnings.
The `oc.*`, kapitan and contributed resolver sets in
`crates/krab-inventory/src/resolvers/` are the reference for the API.

## Python resolvers

Kapitan's omegaconf backend let a repository add resolvers in Python: a
`resolvers.py` whose `pass_resolvers()` returns `{name: function}`. krab runs
the same file, found where the reference looked for it
(`<inventory-path>/resolvers.py`, then
`system/omegaconf/resolvers/resolvers.py`), in a pool of Python workers and
registers each function as a resolver. Arguments arrive as Python values;
`_root_`, `_parent_` and `_node_` are passed when the signature names them,
and `OmegaConf.select(_root_, key)` / `OmegaConf.to_container(..., resolve=True)`
work on them: lookups go through krab's evaluator, so Python sees fully
resolved values. The `omegaconf` package is used when installed and stood in
for otherwise.

As in the reference, a Python function replaces a native resolver of the same
name. Settings, all optional, in `.kapitan`:

```yaml
inventory:
  python-resolvers:
    file: system/omegaconf/resolvers/resolvers.py  # explicit path; `python-resolvers: false` disables
    python: /opt/venv/bin/python                    # $KRAB_PYTHON overrides; default: a kapitan PEX on PATH, python3
    prefer-native: true                             # keep krab's Rust resolvers for names both define
    workers: 4                                      # concurrent Python processes (default: CPUs, at most 8)
```

What the file defines is cached by content digest, so no Python process
starts until one of its resolvers is called. The daemon restarts when the
file, or a project module it imports, changes. A missing interpreter is
diagnosed up front, and `krab server status` shows where the known resolvers
came from and which interpreter runs them. Every call is a JSON round trip
to a worker; port a resolver to Rust when that shows in a profile.

## License

Apache-2.0, like upstream Kapitan. See [LICENSE](LICENSE).
