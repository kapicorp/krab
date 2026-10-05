---
name: krab
description: How to operate krab, the Rust Kapitan (github.com/kapicorp/krab, binary `krab`): render and inspect an inventory through its daemon, explain where a value came from, find what a file affects, run Python resolvers from resolvers.py, compile incrementally, run the language server, verify parity against the Python `kapitan`, and rebuild after engine changes. Use whenever krab, kapitan2 or the Rust kapitan is mentioned, when working in a krab checkout, or when inspecting or changing a Kapitan inventory and a faster or more informative tool than `kapitan` helps.
---

# krab

`krab` is the from-scratch Rust Kapitan. It reads the same `.kapitan`,
`inventory/targets`, `inventory/classes` and `resolvers.py` and produces the
same inventory and compiled output as kapitan 0.36.3, only faster and with
provenance. Run it from the directory holding `.kapitan`.

- Source: `github.com/kapicorp/krab` (cargo workspace). Design in
  `docs/DESIGN.md`, flags in `docs/CLI.md`, open work on the GitHub project
  board (linked from `docs/ROADMAP.md`).
- Binary: `krab` (releases before 2.0.0-alpha.4 shipped it as `kapitan`),
  from a release archive or `cargo build --release`. A development build is
  `target/release/krab` in its checkout; every build keeps its own daemon,
  so a release and a development build can run side by side.
- Reference: the Python `kapitan`. Keep using it for anything krab does not
  do yet (see Compile).

## Mental model

The first `krab inventory ...` starts a **daemon** for that inventory and
binary (one per inventory directory *and* build) that renders every target
once, then watches the inventory files and re-renders only the affected
targets. Every command asks the daemon, so results reflect the files on disk
right now. `--no-daemon` renders locally with the same code and identical
results. The daemon binds its socket before rendering and holds requests
until the first render is done; it exits after 30 minutes idle. A rebuilt or
newer binary gets a socket of its own and never touches another build's
daemon; `krab server stop` stops every build's daemon for the inventory.

Target names are dotted paths of the target file:
`inventory/targets/team/app/prod.yml` is `team.app.prod`.

## Inspect the inventory

```bash
krab inventory targets                         # table: labels, classes, inputs, status
krab inventory targets -q                      # names only
krab inventory targets -l env=prod       # label selection (also on show/compile)
krab inventory -t team.app.prod        # rendered target, same YAML as kapitan
krab inventory -t team.app.prod -p parameters.cluster --format json
krab inventory -l env=prod -p parameters.cluster.name   # one value per target
krab inventory -t team.app.prod -F     # flattened dotted keys, easy to grep
krab inventory classes -t team.app.prod        # class files a target includes, in order
krab inventory classes                                  # every class file with how many targets include it
krab inventory classes --unused                         # dead classes
krab inventory export --out /tmp/inv --format json   # one file per target
```

`--json` on any command turns output and diagnostics into JSON.

## Explain a value

```bash
krab inventory explain -t team.app.prod cluster.name
krab inventory explain -t team.app.prod kapitan.compile[0].name
```

Shows the value, its type, the file:line:col that wrote it, the `${...}`
expression it was resolved from, and the history of earlier values it
overrode (oldest first, each with its location). Paths are relative to
`parameters`. Use this instead of grepping classes to answer "why is this
value X" or "which class overrides this".

## What depends on a file

```bash
krab inventory deps inventory/classes/common.yml       # targets rendered from these files
krab inventory check                                    # render everything, pretty diagnostics
krab inventory check --json                             # one JSON diagnostic per line
krab inventory watch                                    # live: what re-renders as files change, and failures
```

Diagnostics carry a stable `code` (e.g. `inventory::class_not_found`), the
target, the parameter path and source locations.

## Python resolvers (resolvers.py)

krab runs the repository's `resolvers.py` in a pool of Python workers, as
the omegaconf backend did, configured in `.kapitan`:

```yaml
inventory:
  python-resolvers:
    file: system/omegaconf/resolvers/resolvers.py
    python: /opt/venv/bin/python    # $KRAB_PYTHON overrides this; default: a kapitan PEX on PATH, python3
    prefer-native: true             # keep krab's Rust ports for names both define (faster)
    workers: 4
```

- `KRAB_PYTHON` is the per-machine override of the shared `python:` key.
  Where that interpreter does not exist, set `KRAB_PYTHON` (a venv or pixi
  python that imports omegaconf), or drop the key locally. A missing interpreter is diagnosed up front, naming the
  key.
- Keep `prefer-native: true`. With `false`, `json`, `to_yaml`, `pluck` and
  friends run in Python and their `_root_` lookups call Python again; each
  nested call needs a further worker process, which is correct but slow.
- The daemon restarts itself when `.kapitan`, `resolvers.py` or a module it
  imports changes. The unknown-resolver help ends with where the known resolvers came
  from (`18 Python resolvers from … via python3 (2 kept native)`), and
  `krab server status` prints the same under `resolvers`; if a resolver that
  exists in the file is reported unknown, that line says which build and
  interpreter answered.

## Compile

```bash
krab compile                    # only targets whose inputs changed
krab compile --dry-run          # what would compile, and why (which file changed)
krab compile --explain          # same, while compiling
krab compile -t a.b -t c.d      # selected targets
krab compile -l env=prod
krab compile --force            # everything, regardless
krab compile --reveal           # decrypt refs into the output instead of embedding them
krab compile --backend python   # kapitan's own Python input types in a worker (slow, complete)
krab compile --fetch            # first fetch parameters.kapitan.dependencies whose output is missing
krab compile --force-fetch      # refetch every dependency, overwriting (updates generators, charts)
krab compile --no-fetch         # ignore `fetch: true` in .kapitan
krab refs --reveal -f compiled/prod/manifests/secret.yml   # write, reveal, update, validate refs
```

Dependencies (git, http(s), helm, oci) are fetched natively before
staleness is decided. With `fetch: true` in `.kapitan`, a missing
chart directory (`system/sources/charts/<name>/<version>` after a version
bump) or generator checkout is fetched on the next compile; whatever exists
is left alone. `--dry-run` shows `would fetch ...`, `--explain` shows
`not fetched ... [already present]`. Versioned charts are cached under
`~/.cache/krab/charts`.

Staleness comes from `compiled/.krab-manifest.json`: per target, the
rendered document digest, every file the inputs read (templates, helm chart
files, kadet modules and their imports, copied files, refs), the other
targets read through the global inventory, and the output tree digest.
Editing a template or a kadet module therefore recompiles exactly its
readers. Do not commit the manifest unless asked.

`kadet` components run in a venv krab builds from
`compile.python-requirements` in `.kapitan` (under `~/.cache/krab/python/`),
against krab's own bundled `kapitan` package; Rust does the rest. The input
and output types that are not native yet are the open deviations in
`docs/specs/inputs-and-output.md`. For those use `--backend python` or the
reference `kapitan`.

## Parity check (after any engine change)

```bash
cd path/to/inventory/repo
krab compile --force && git status --short compiled   # must print nothing
```

For the inventory alone: `krab inventory -t X` must match
`kapitan inventory -t X` byte for byte. Fixture tests
(`cargo test --release`) cover the engine without a real inventory, including
`resolvers.py` through the Python bridge; the corpus test needs
`KRAB_CORPUS` and `KRAB_COMPILED` (see `docs/DESIGN.md`, Testing).

Reference scripts run with the Python kapitan's interpreter (for a PEX:
`PEX_INTERPRETER=1 kapitan script.py`) from the inventory repository.
Always revert test edits to the inventory repository.

## Daemon

```bash
krab server status         # every build's daemon for this inventory: version, pid, binary, resolvers, starting/ready
krab server status --json  # the same as a list
krab server stop           # stops them all
krab server logs           # the log, shared by all builds
krab server run            # foreground, for debugging (exits at once if this build's daemon runs)
```

Socket `$XDG_RUNTIME_DIR/krab/<inventory>-<build>.sock` (fallback
`/tmp/krab-<uid>/`), log `~/.local/state/krab/server-<inventory>.log`.
JSON-RPC 2.0, newline delimited; the methods are listed in `docs/CLI.md`
(`krab server`), their shapes in `crates/krab-server/src/protocol.rs`. Use them directly
from scripts when the CLI shape does not fit. `server.*` answer while the
initial render runs (`ready: false`); `inventory.*` wait for it.

Troubleshooting:

- `inventory server unavailable (...); rendering locally`: the daemon did
  not come up in 10 s; the message carries the socket and the log tail.
- Results disagree with `--no-daemon`, or a resolver from `resolvers.py` is
  "unsupported": `krab server status` shows which build and interpreter
  serve the inventory; `krab server stop`, then rerun.
- `no Python resolver worker became free in 60s`: a Python resolver is
  stuck (or the interpreter hangs on start); check `server logs`.

## Editor

`krab lsp` is a language server over the daemon (hover = value + origin +
overrides across the targets that include the file, go to definition,
completion of classes and `${` paths, live diagnostics). The VS Code
extension is `editors/vscode` (installed as `kapicorp.krab`). Point
`krab.path` at the same binary the shell uses and set `krab.python`
(passed on as `KRAB_PYTHON`) when the inventory has Python resolvers: the
extension host's `python3` cannot import omegaconf. Smoke test without an
editor (the scripts run `krab` from `PATH`):

```bash
python3 scripts/lsp-smoke.py path/to/inventory/repo inventory/targets/team/app/prod.yml 1:10 20:24
python3 scripts/lsp-smoke-live.py path/to/inventory/repo    # completion + diagnostics (edits and restores a class)
```

Server-side problems show in the "Kapitan (<repository directory>)" output channel, whose log
is under `~/.vscode-server/data/logs/*/exthost*/output_logging_*/`.

## Developing krab

```bash
git worktree add ../krab-<name> -b <branch> origin/main   # one worktree per branch
cargo build --release          # target/release/krab; a rebuilt binary uses a fresh socket, nothing to stop
cargo fmt --all && cargo clippy --all-targets --release && cargo test --release
```

CI denies warnings, so run clippy before pushing; on a machine with little
memory, limit the build with `-j`. Commit messages are
`area: what changed` (`inventory: ...`, `server: ...`, `vscode: ...`);
issues follow the dated-observation-then-bullets shape of the existing ones.
To release: bump `version` in the workspace `Cargo.toml` (and
`editors/vscode/package.json` when the extension changed), merge, tag `main`
`vX.Y.Z-alpha.N` and push the tag; `release.yml` publishes the pre-release.

Crates: `krab-inventory` (engine: loader, class resolution, OmegaConf
merge and interpolation, resolvers, the Python resolver bridge, provenance,
emitters), `krab-server` (daemon + client), `krab-compile` (manifest,
native inputs, refs, Python runners), `krab-lsp`, `krab` (CLI). Test
commands and conventions are in `AGENTS.md`; where to add a resolver, an
input type, an RPC method and the like is the Extending section of
`docs/ARCHITECTURE.md`.
`vendor/saphyr-parser` carries two PyYAML-compatibility patches.

Gotchas: build failures leave the old binary in place, so confirm
`Finished` before trusting a test; `cargo fmt` reformats code, so patch with
exact strings after formatting, not before; never subclass python-box in the
kadet runner (attribute access recurses); a Python resolver's `_root_`
lookup may re-enter the resolver pool, so a nested acquire must never wait
for the pool.
