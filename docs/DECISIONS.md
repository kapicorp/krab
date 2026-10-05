# Decisions

krab takes over repositories that compile with kapitan 0.36.3 and the
`omegaconf` inventory backend. Parity with that reference is a migration aid:
it decides what a repository sees change when it switches, not which of
kapitan's accidents krab copies.

Input types and outputs that are not implemented yet are status, not
decisions, and are listed in [../README.md](../README.md#compatibility).

## Surfaces

These surfaces stay byte-identical to the reference:

* files under `compiled/`: names, layout, content and mode bits;
* the document `kapitan inventory -t` prints, which is also what a kadet
  component or a jinja2 template reads through `inventory()` and
  `inventory_global()`;
* ref tags in output and ref files on disk;
* the meaning of every key kapitan reads from `.kapitan` and from
  `parameters.kapitan`.

On these surfaces krab copies the reference unless the reference writes
output that is wrong for every reader (D19, D20). A difference on a
byte-identical surface that is not listed below is a bug; report it with the
parity-gap issue template.

Everything else may diverge: diagnostics and their wording, validation of
configuration and input, CLI flags and exit codes, error isolation between
targets, fetching, performance and caching, and security defaults for network
access and secrets. A difference there gets a row when a user could notice
it. The Surface column names output, inventory, refs or `.kapitan` for
the byte-identical surfaces, otherwise diagnostics, validation, CLI, error
isolation, fetch, performance or security.

## Migration

A change that leaves the output of existing repositories as it is, such as a
new warning, needs no opt-in. A change of output needs a row here and either
one release that warns first or an explicit opt-in key.

## Direction

Each row says which way krab departs from the reference:

* stricter: krab fails or refuses where kapitan renders. A repository that
  works with krab also works with kapitan.
* more permissive: kapitan fails where krab renders. The row says that
  kapitan would fail, and krab warns about it, because a repository written
  against krab stops working on kapitan while both are in use.
* different output: both succeed, with different results.
* none: same output and the same success or failure; only the diagnostic
  differs.

## Deviations from the reference

| # | Subject | Reference (what kapitan 0.36.3 does, verified by running it) | krab | Why | Surface | Direction | Migration |
|---|---|---|---|---|---|---|---|
| D1 | Class cycles | Recurses until it runs out of stack | Reported as a diagnostic pointing at the cycle | A render that never terminates cannot be debugged | diagnostics | none | none needed |
| D2 | Unknown YAML tags | PyYAML `safe_load` fails on a tag it has no constructor for: `could not determine a constructor for the tag '!custom'` | `yaml::unknown_tag` at the tag. `!!binary` and tags on mappings and sequences: D22 | Same failure, with the position of the tag | diagnostics | none | none needed |
| D3 | Timestamps (`d: 2020-01-01` unquoted) | Fails the target: `Value 'date' is not a supported primitive type` | Stay strings (`d: '2020-01-01'`) | The reference cannot carry a `datetime` through its own pipeline; the string is the only result that renders | inventory | more permissive | warning that kapitan fails and the value should be quoted: planned, issue tbd |
| D4 | Dependency whose `output_path` exists | Re-clones every git source and adds files that happen to be missing | Not fetched at all | A repository with everything in place compiles offline | fetch | different output | none needed |
| D5 | `force_fetch: true` on a dependency item | Honoured only when neither `--fetch` nor `--force-fetch` is given | Forces that item even under `--fetch` | Otherwise the per-item setting is unreachable in the common invocation | CLI | different output | none needed |
| D6 | Boolean resolvers | Python truthiness | Python truthiness, kept on purpose | `${if:nonempty,…}` must stay true; a stricter mode is a planned opt-in rather than a silent change | output | none | none needed |
| D7 | Two target files with one name (`a/x.yml` and `b/x.yml` without `compose-target-name`) | Renders nothing at all, and says nothing: no targets, no diagnostic, exit code 0 | `inventory::conflicting_targets`, naming both files | An inventory that silently produces no targets cannot be debugged | validation | stricter | none needed |
| D8 | `refs --write` to a path that already holds a ref | Replaces the file | Refuses unless `--force` is given | An encrypted ref may have no other copy, and a mistyped path destroys it | CLI | stricter | none needed |
| D9 | `compile -t <target>` or `inventory -t <target>` while another target fails to render | Renders the whole inventory first and fails: `compile` compiles nothing, and `inventory -t` fails too (a YAML error in another target's file is enough) | Compiles or renders the selected targets; `compile` warns about the others. A selected target that reads a failing target, or the whole global inventory, still fails | One broken target should not block everyone else's work | error isolation | more permissive | none needed; `compile` names the failing targets |
| D10 | `${write:dest,origin}` where a parent of `dest` does not exist | Crashes the render: `dictionary changed size during iteration` | Creates the missing mappings and writes | The write has an obvious meaning, and the crash is an iteration artefact of the reference | output | more permissive | none; krab does not warn yet |
| D11 | OCI credentials (`OCI_USERNAME` / `OCI_PASSWORD`) and the token realm of a bearer challenge | oras sends them as basic auth to whatever realm the registry names, on any host, over plain http with `insecure: true` | Never over http: a registry asking for authentication with `insecure: true` fails. A realm on another origin must be https, and gets credentials only with TLS verification | The realm comes from the registry's response, so anyone who can answer as the registry could collect the credentials | security | stricter | none needed |
| D12 | Digests of an OCI artifact (`source: registry/repo@sha256:...`, index entries, layers) | oras writes whatever manifest and layers the registry returns, checking no digest; an index yields no layers | A digest reference must match the manifest bytes; an index must list exactly one manifest, which must match its entry; every layer must match its `sha256` digest; other algorithms fail | Without the manifest check, a registry answering a pinned digest with another manifest chooses the layers, and they verify against their own digests | security | stricter | none needed |
| D13 | Modules a kadet component imports, across targets | A pool process that compiles a second target keeps them loaded, with the state they built for the first (`inventory()` read at import time, generators registered for that target). Which targets share a process depends on scheduling | The native backend gives each target its own evaluator process; a full compile gives what `compile -t` gives. `--backend python` shares a worker per thread, like the reference | Output must not depend on which targets happened to share a process | output | different output | none needed; the reference's output already varies with scheduling |
| D17 | Vault TLS verification when `skip_verify` is not set | `skip_verify` defaults to true, so no certificate is verified; with `skip_verify: false` a CA file or path is mandatory and the system roots are never used | Without an explicit `skip_verify` (inventory or `VAULT_SKIP_VERIFY`), verifies against the system roots and, when that fails, warns and continues unverified. An explicit `skip_verify: true` skips verification without a warning. The ref file keeps `skip_verify: true` | Every reveal and write otherwise talks to Vault without verification; same reasoning as D11 (#216) | security | stricter | warning release (#216), then a failure naming `VAULT_SKIP_VERIFY=true` |
| D18 | Dot entries in a forced dependency copy | `--force-fetch` copies everything with `copy_tree(clobber_files=True)`, `.git` included; an unforced copy skips names starting with `.` | A forced copy skips dot entries as an unforced one does and reports the skipped names once | The same dependency gives two trees depending on a flag, and a nested `.git` turns the dependency path into an embedded repository for `git add` (#217) | fetch | different output | none needed; the skipped names are reported (#217) |
| D19 | Folded multi-line strings that do not read back | With `yaml-multiline-string-style: folded`, a line that starts with a space and runs past column 80 is folded, and the file loads as a different string | Falls back to double quotes when folding would change the value | An emitter must not write a value that reads back differently (#218) | output | different output | opt-in: only repositories that set `folded` and have such lines see a change, and their output is wrong today (#218) |
| D20 | kadet output whose root is a string, under a YAML or JSON output type | Writes one YAML document per character | Writes one scalar and warns, suggesting `output_type: plain` | The writer treats any iterable as a list of documents; nobody can use the per-character output (#126) | output | different output | warning (#126) |
| D21 | `resolvers.py` that cannot be imported or lacks `pass_resolvers()` | Logs `Couldn't import ...` or `resolvers.py must contain function 'pass_resolvers()'` (the second for any `ImportError`, a missing third-party module included) and renders with the built-in resolvers | A diagnostic with the real exception; every target renders with the built-in resolvers and fails only where it calls a resolver that is missing | Same output as the reference for targets that do not use the file, and a message that names the cause (#121) | diagnostics | none | none needed (#121) |
| D22 | `!!binary`, and non-core tags on mappings and sequences | `!!binary` constructs bytes, rendered as `!!binary \|`. A tag on a mapping or sequence fails (`!custom` has no constructor, `!!set` gives `Value 'set' is not a supported primitive type`), except `!!map`, `!!seq` and `!!omap`, which render (`!!omap` as a list of pairs) | `!!binary` fails with `yaml::unknown_tag`. A tag on a mapping or sequence other than `!!map` and `!!seq` on the matching kind fails with `yaml::unknown_tag` after #219, `!!omap` included; until then it is dropped and the value renders untagged | Nobody writes base64 binaries into an inventory on purpose, and the emitted `!!binary` block breaks most consumers; a dropped tag renders what kapitan refuses (#219) | inventory | stricter for `!!binary`; more permissive for collection tags until #219 | none needed (#219) |
| D23 | Class name whose reclass compatibility fallback leaves no path (`componets.nginx`) | Loads `classes/init.yml` or `<inventory>/classes.yml` without a word | Every fallback hit warns, naming the file used and the paths expected; for this case the warning says a later release makes it an error | A misspelt class merges an unrelated file into the target, and kapitan's own comments describe a relative lookup the code does not do (#214) | inventory | stricter | warning since the release with #214, error in a later release |

## krab extensions

These have no reference behaviour to deviate from, so they carry no D-number.
The `contrib` resolver set (`replace`, `json`, `to_yaml`, `sha256`, `truncate`
and the others) does not exist in kapitan, which fails on each of them with
`Unsupported interpolation type`; krab renders them and warns unless
`.kapitan` sets `inventory.contrib-resolvers: true` (#122). The inventory
daemon, the language server and `krab inventory explain` are krab's own and
change no byte-identical surface.

## Adding one

A new deviation needs a row here before the change merges, with every column
filled and the Reference column taken from running kapitan 0.36.3.
`docs/DESIGN.md` carries the semantics and this file carries the decisions,
so neither repeats the other.
