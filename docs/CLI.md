# CLI reference

The binary is `krab`. Run it from the directory holding `.kapitan`.
`krab <command> --help` is always current; this page adds the context.

## Global options

Accepted before or after the subcommand.

| flag | meaning |
|---|---|
| `--inventory-path <DIR>` | inventory directory. Default: `inventory-path` from `.kapitan`, else `./inventory`. Env: `KRAB_INVENTORY_PATH` |
| `--json` | results and diagnostics as JSON (see [JSON output](#json-output)) |
| `--raw` | skip kapitan's typed normalisation of `parameters.kapitan` (pydantic model defaults and ordering). Implies local rendering |
| `--no-daemon` | render in-process instead of talking to (or starting) the daemon. Env: `KRAB_NO_DAEMON=1` |
| `-V, --version` | version |

Exit code is 0 on success and 1 when a command fails or any target it rendered
or compiled has errors. Diagnostics go to stderr (or, with `--json`, to stdout
as one object per line). Log verbosity follows `RUST_LOG` (default `warn`;
`info` for `server run`).

## `krab inventory` (alias `i`)

Show the rendered inventory. Without a subcommand it prints target documents.

| flag | meaning |
|---|---|
| `-t, --target-name <TARGET>` | one target. Without it, every target (or every target matching `-l`) is printed as a multi-document stream |
| `-p, --pattern <PATH>` | dotted path inside the target document, e.g. `parameters.kapitan.compile`. With several targets, one value per target |
| `-l, --labels k=v ...` | only targets whose `parameters.kapitan.labels` match (repeatable, all must match) |
| `-F, --flat` | flatten nested keys into dotted keys, one per line |
| `--format yaml\|json` | output format (default `yaml`) |
| `-i, --indent <N>` | YAML indentation (default: `inventory.indent` from `.kapitan`, else 2) |

Target names are the dotted path of the target file under `targets/`
without the extension: `targets/a/b/c.yml` is `a.b.c`.

### `inventory targets`

List targets. Default output is a table with name, labels, class count,
compile input types and status (ok, or the first error). `-q, --quiet`
prints names only; `-l` filters by label; `--json` gives one object per
target with labels, classes, compile inputs and diagnostics.

### `inventory classes`

`-t <TARGET>`: the target's classes in include order (what
`krab inventory -t X` would show under `classes`, but as the flat,
resolved list). Without `-t`: every class file and how many targets include
it. `--unused`: only class files no target includes.

### `inventory explain -t <TARGET> <PATH>`

Where a value came from and what it overrode. `PATH` is relative to
`parameters` (`cluster.name`, `kapitan.compile[0].name`). Prints the final
value, its origin (file:line:col), each overridden earlier value with its
origin, and for interpolated values the expression, the referenced paths and
how each resolved. Merge-time dereferences and list appends are shown as
well.

### `inventory check`

Render every target and report every problem, with source snippets. Exit
code 1 when any target fails. `--json` prints one diagnostic per line.

### `inventory export --out <DIR> [--format yaml|json]`

Write every target to `<DIR>/<target name>.<ext>`.

### `inventory deps <FILE>...`

Which targets are rendered from the given files (class files, target files
or directories). Paths are relative to the working directory. Nothing is
printed for a file no target uses.

### `inventory watch`

Long-running. Prints a header (inventory, target count, errors, daemon
pid), then one line per change on disk: the files that changed, the targets
re-rendered from them and which of those failed, followed by the new
diagnostics. `--json` prints one change object per line. Ctrl-C to stop; the
daemon keeps running.

## `krab compile` (alias `c`)

Compile the targets whose inputs changed.

| flag | meaning |
|---|---|
| `-t, --targets <T>...` | targets to consider (default: all) |
| `-l, --labels k=v ...` | targets whose labels match |
| `--force` | recompile even when nothing changed |
| `--dry-run` | print which targets would compile and why, compile nothing |
| `--explain` | compile, and print per target why it was compiled or skipped (and per dependency why it was or was not fetched) |
| `--fetch` | fetch `parameters.kapitan.dependencies` (git, http/https, helm, oci) whose output path is missing before compiling (default: `compile.fetch` from `.kapitan`) |
| `--no-fetch` | do not fetch even when `.kapitan` says `fetch: true` |
| `--force-fetch` | fetch every dependency again and overwrite what exists (default: `compile.force-fetch` from `.kapitan`) |
| `-p, --parallelism <N>` | worker processes (default: number of CPUs) |
| `--output-path <DIR>` | where `compiled/` lives (default: `compile.output-path` from `.kapitan`, else `.`) |
| `--reveal` | reveal refs in the output instead of compiling them (default: `compile.reveal` from `.kapitan`) |
| `--embed-refs` | embed the ref files' contents in the output instead of writing hashed tags (default: `compile.embed-refs` from `.kapitan`) |
| `--python <PATH>` | Python used to evaluate kadet components, as it is (with `--backend python`, one with kapitan installed). Default: `$KRAB_PYTHON`, else the venv krab builds from `compile.python-requirements` |
| `--flag <FLAG>` | extra flag passed through to kapitan's compile in the Python backend (e.g. `--indent 4`) |
| `--backend native\|python` | `native` (default): input types run in Rust, Python only evaluates kadet `main()`. `python`: kapitan's own input types in worker processes |

Staleness is decided per target from `compiled/.krab-manifest.json`: the
rendered document digest, every path the previous compile read, the other
targets consulted through the global inventory, the compiler identity and
the output tree digest. A full run (no `-t`/`-l`) also removes output
directories that belong to no target. Reasons printed by `--explain` and
`--dry-run` name the specific changed path or target.

Dependencies (`parameters.kapitan.dependencies`) are fetched before
staleness is decided, so the fetched files count as inputs like any other
read. `type: git` clones with `git` and copies the repository or its
`subdir` at `ref` (the remote's default branch when unset; `submodules:
true` initialises submodules); `type: http`/`https` downloads the file and
saves it, or with `unpack: true` extracts a tar, tar.gz/tgz or zip archive
into `output_path`; `type: helm` runs `helm pull --untar` for `chart_name`
at `version` from `source` (a repository URL or `oci://` reference) and
keeps versioned charts under `$XDG_CACHE_HOME/krab/charts` so a chart
seen once is never pulled again. `output_path` is relative to
`--output-path`. As in kapitan, nothing that exists is overwritten unless
forced; unlike kapitan, a dependency whose output path already exists is
not fetched at all, so a repository with everything in place compiles
offline. Without `--fetch` only items marked `force_fetch: true` are
fetched (and overwritten). `--dry-run` lists what would be fetched.
`type: oci` pulls an artifact (what `oras push` produces) from `source`, a
bare `registry/repository:tag` or `@digest` reference, with the registry
distribution API: layers are saved under their title annotation, tar blobs
are extracted, and the artifact or its `subpath` is copied; `media_type`
keeps only matching layers, `insecure: true` uses plain http, `tls_verify`
is a boolean or a CA bundle path, and credentials come from `OCI_USERNAME`
/ `OCI_PASSWORD`.

References in the output are compiled the way kapitan does: an existing ref
becomes `?{type:path:hash}` (or its embedded payload with `--embed-refs`),
`plain` refs are inlined, and a missing ref whose tag carries functions
(`?{gkms:targets/x/token||random:str}`) is created under the refs path with
the target's `parameters.kapitan.secrets` (KMS key, GPG recipients, Vault
settings). Created ref files are recorded as reads, so they enter the
manifest. `--reveal` decrypts refs into the output instead, and makes the
`reveal_maybe` jinja2 filter reveal.

`.kapitan` keys used: `compile.search-paths`, `compile.output-path`,
`compile.indent`, `compile.fetch`, `compile.force-fetch`, `compile.refs-path`,
`compile.embed-refs`, `compile.reveal`, `compile.python-requirements`,
`inventory.multiline-string-style`, `inventory.python-resolvers`.

`compile.python-requirements` (a list of pip specifiers, or the path of a
requirements file) names what kadet components import besides `kadet` and
`jinja2`. krab installs them into a venv of its own under
`$XDG_CACHE_HOME/krab/python/<digest>` on the first compile (with `uv`
when on PATH, else `python3 -m venv` and pip) and evaluates components
there; a changed list is a new environment. `--python` / `KRAB_PYTHON`
bypass it.

## `krab refs`

Write, reveal, update and validate references (`?{type:path}` tags), with
kapitan's flags. Ref files live under `--refs-path` (default: `refs.refs-path`
from `.kapitan`, else `./refs`). Types: `plain`, `base64`, `env`, `gkms`,
`gpg`, `awskms`, `azkms`, `vaultkv`, `vaulttransit`.

| flag | meaning |
|---|---|
| `-w, --write <type:path>` | write a ref from `--file` (`-` reads stdin); `--base64` encodes the content first, `--binary` accepts non-text content |
| `-r, --reveal` | reveal the tags in `--file` (a file, or a directory whose YAML files are concatenated; `-` reads stdin), the ref in `--ref-file`, or the string given as `--tag` |
| `--update <type:path>` | re-encrypt a ref for new `--recipients` (gpg) or a new `--key` (gkms, awskms, azkms) |
| `--update-targets` | re-encrypt every ref under `<refs-path>/<target>/...` with what that target's `parameters.kapitan.secrets` declares |
| `--validate-targets` | report refs whose recipients or key differ from their target's; exit code 1 when any do |
| `-t, --target-name <T>` | take recipients, keys and Vault settings from that target's `parameters.kapitan.secrets` |
| `-R, --recipients <R>...` | GPG recipients (names or fingerprints) |
| `-K, --key <KEY>` | KMS key |
| `--vault-auth`, `--vault-mount`, `--vault-path`, `--vault-key` | Vault settings for `vaultkv`/`vaulttransit` writes |
| `--refs-path <DIR>` | where ref files live |

```sh
krab refs --write gkms:targets/prod/db-password -f password.txt -t prod
krab refs --reveal -f compiled/prod/manifests/secret.yml
krab refs --reveal --tag '?{gkms:targets/prod/db-password}'
krab refs --validate-targets
```

Credentials: `gkms` uses application-default credentials
(`GOOGLE_APPLICATION_CREDENTIALS`, the gcloud ADC file, the GCE metadata
server, or `gcloud auth application-default print-access-token`); `gpg` runs
the `gpg` binary against the current keyring; Vault reads `VAULT_ADDR`,
`VAULT_TOKEN` (or `~/.vault-token`), `VAULT_USERNAME`/`VAULT_PASSWORD`,
`VAULT_ROLE_ID`/`VAULT_SECRET_ID` and the `VAULT_*` TLS variables, unless
the inventory's `vault_params` set them; `awskms` and `azkms` call the `aws`
and `az` command line clients. `env` refs read `KAPITAN_VAR_<name>` at
reveal time and fall back to the stored value.

## `krab server`

The daemon is started automatically by the commands above; these manage it.

| command | meaning |
|---|---|
| `server status` | every server running for this inventory (any build): version, pid, binary, socket, log, resolver sources, whether it is still rendering, targets rendered and failing, generation, uptime and idle timeout. `--json` prints them as a list |
| `server stop` | stop them all |
| `server logs [-n, --lines <N>]` | print the last `N` lines of its log file (default 50) |
| `server start` | start one detached (no-op when one runs) |
| `server run [--idle-timeout <SECS>]` | run in the foreground; this is what `start` launches. Default idle timeout 1800 s |

One daemon per inventory directory and build. Socket:
`$XDG_RUNTIME_DIR/krab/<inventory>-<build>.sock` (fallback
`/tmp/krab-<uid>/`), where `<inventory>` hashes the canonical inventory
path and `<build>` the binary's version, size and mtime. Two builds pointed
at the same inventory (the shell's `krab` and a development build the
editor was pointed at) each
keep their own daemon rather than restart each other's; a rebuilt binary gets
a fresh socket and the previous daemon idles out. Log, shared by all builds:
`$XDG_STATE_HOME/krab/server-<inventory>.log` (fallback
`~/.local/state/krab/`).

The daemon binds its socket before the initial render and holds `inventory.*`
requests until that render is done, so a client's first call waits on the
socket rather than on a start-up timeout; `server status` says `starting`
meanwhile. A second starter sees the socket at once and exits.

The protocol is JSON-RPC 2.0, newline delimited: `server.info`,
`server.shutdown`, `inventory.targets`, `inventory.target`, `inventory.all`,
`inventory.classes`, `inventory.explain`, `inventory.deps`,
`inventory.diagnostics` and `inventory.wait` (a long poll on the generation
counter). Parameter and result shapes are in
`crates/krab-server/src/protocol.rs`. After starting a daemon the client
checks that the server answering on its socket is the same build, and
refuses to use one that is not.

## `krab lsp`

Run the language server over stdio. Editors pass `--stdio`; it is accepted
and ignored. The working directory must be the repository root (where
`.kapitan` is) or the extension must start it there. Features: diagnostics,
hover, go to definition, completion (see
[GETTING-STARTED.md](GETTING-STARTED.md#5-editor)).

## `krab completions <bash|zsh|fish|elvish|powershell>`

Print the completion script: `source <(krab completions bash)`. The script
registers whatever name the command was invoked as, so a development build
linked as `~/.local/bin/krab-dev -> .../target/release/krab` completes as
`krab-dev` (run `krab-dev completions bash`). Target names are completed
from the daemon.

## Environment variables

| variable | effect |
|---|---|
| `KRAB_INVENTORY_PATH` | same as `--inventory-path` |
| `KRAB_NO_DAEMON` | same as `--no-daemon` |
| `KRAB_PYTHON` | same as `--python` for compile (that interpreter as it is, instead of the venv krab builds); for Python resolvers it overrides `inventory.python-resolvers.python` in `.kapitan` (the per-machine override of a shared setting) |
| `KRAB_PYTHON_REQUIREMENTS` | specifiers added to the venv krab builds, for this machine only, one per line: `kadet==0.3.1`, `kadet @ file:///home/me/kadet`, or `-e /home/me/kadet` for an editable checkout. A named `kadet` or `jinja2` replaces krab's baseline entry |
| `RUST_LOG` | log filter (`krab_server=debug`, ...) |

The `KAPITAN_*` spellings of the first four (`KAPITAN_PYTHON`, ...) still
work in this release and print a note; they go away in the next.
| `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | where the socket, log and worker cache live |

The `oc.env` resolver reads the environment of the process that renders,
which is the daemon when one is used. Restart it (`server stop`) after
changing variables your inventory reads.

## `.kapitan`

Read from the working directory. Recognised keys, in the sections kapitan
itself uses:

| key | section(s) | use |
|---|---|---|
| `inventory-path` | `compile`, `inventory`, `global` | inventory directory |
| `compose-node-name` / `compose-target-name` | `compile`, `inventory`, `global` | dotted target names from the directory layout |
| `inventory-backend` | `global` | informational; only `omegaconf` semantics are implemented |
| `indent` | `inventory` | YAML indentation for `krab inventory` |
| `search-paths`, `output-path`, `indent`, `fetch`, `force-fetch` | `compile` | as for krab compile |
| `refs-path`, `embed-refs`, `reveal` | `compile` | where ref files live, embed them, reveal them |
| `python-requirements` | `compile` | packages kadet components import; installed into krab's own venv |
| `refs-path` | `refs` | where `krab refs` looks for ref files |
| `multiline-string-style` | `inventory` | multiline string style for compiled YAML |

## JSON output

`--json` changes every command's output to JSON on stdout:

* `inventory -t X --json` and `--format json`: the target document.
* `inventory targets --json`, `classes --json`, `deps --json`: arrays.
* `inventory explain --json`: the explanation object (value, origin,
  overrides, interpolation steps).
* `check --json`, `compile --json`, `watch --json`: one JSON object per
  line. A diagnostic is
  `{severity, code, message, target, path, labels: [{location: {file, line, col}, text}], help}`.
* `compile --json`: one report with `outcomes` (per target: name, status,
  reason, warnings), `fetched` dependencies (type, source, output path,
  declaring target, status `fetched`/`would_fetch`/`skipped`/`failed`,
  reason), `removed` output directories, `elapsed_ms`, the manifest path
  and the engine identity.
