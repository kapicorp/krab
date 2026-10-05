# Decisions

Rendering and compiled output are byte-identical to kapitan 0.36.3 with the
`omegaconf` inventory backend. Everywhere krab behaves differently, it is
listed here with the reason. A difference that is not in this list is a bug;
report it with the parity-gap issue template.

Input types and outputs that are not implemented yet are status, not
decisions, and are listed in [../README.md](../README.md#compatibility).

## Deviations from the reference

| # | Subject | Reference | krab | Why |
|---|---|---|---|---|
| D1 | Class cycles | Recurses until it runs out of stack | Reported as a diagnostic pointing at the cycle | A render that never terminates cannot be debugged |
| D2 | Unknown YAML tags | PyYAML `safe_load` constructs what it knows and carries the rest | An error | A tag krab does not implement would otherwise render as something else without saying so |
| D3 | Timestamps | PyYAML resolves them to `datetime` | Stay strings | The reference cannot hold `datetime` values through its own pipeline either |
| D4 | Dependency whose `output_path` exists | Re-clones every git source and adds files that happen to be missing | Not fetched at all | A repository with everything in place compiles offline |
| D5 | `force_fetch: true` on a dependency item | Honoured only when neither `--fetch` nor `--force-fetch` is given | Forces that item even under `--fetch` | Otherwise the per-item setting is unreachable in the common invocation |
| D6 | Boolean resolvers | Python truthiness | Python truthiness, kept on purpose | `${if:nonempty,…}` must stay true; a stricter mode is a planned opt-in rather than a silent change |
| D7 | Two target files with one name (`a/x.yml` and `b/x.yml` without `compose-target-name`) | Renders nothing at all, and says nothing: no targets, no diagnostic, exit code 0 | `inventory::conflicting_targets`, naming both files | An inventory that silently produces no targets cannot be debugged |
| D9 | `compile -t <target>` while another target fails to render | Renders the whole inventory first and compiles nothing | Compiles the selected targets and warns about the others; a selected target that reads a failing target, or the whole global inventory, still fails | One broken target should not block everyone else's compile |
| D8 | `refs --write` to a path that already holds a ref | Replaces the file | Refuses unless `--force` is given | An encrypted ref may have no other copy, and a mistyped path destroys it |
| D10 | `${write:dest,origin}` where a parent of `dest` does not exist | Crashes the render: `dictionary changed size during iteration` | Creates the missing mappings and writes | The write has an obvious meaning, and the crash is an iteration artefact of the reference |
| D11 | OCI credentials (`OCI_USERNAME` / `OCI_PASSWORD`) and the token realm of a bearer challenge | oras sends them as basic auth to whatever realm the registry names, on any host, over plain http with `insecure: true` | Never over http: a registry asking for authentication with `insecure: true` fails. A realm on another origin must be https, and gets credentials only with TLS verification | The realm comes from the registry's response, so anyone who can answer as the registry could collect the credentials |
| D12 | Digests of an OCI artifact (`source: registry/repo@sha256:...`, index entries, layers) | oras writes whatever manifest and layers the registry returns, checking no digest; an index yields no layers | A digest reference must match the manifest bytes; an index must list exactly one manifest, which must match its entry; every layer must match its `sha256` digest; other algorithms fail | Without the manifest check, a registry answering a pinned digest with another manifest chooses the layers, and they verify against their own digests |
| D13 | Modules a kadet component imports, across targets | A pool process that compiles a second target keeps them loaded, with the state they built for the first (`inventory()` read at import time, generators registered for that target). Which targets share a process depends on scheduling | The native backend gives each target its own evaluator process; a full compile gives what `compile -t` gives. `--backend python` shares a worker per thread, like the reference | Output must not depend on which targets happened to share a process |
| D15 | `.kapitan` `version:` | Compile refuses to run unless the pin matches the running kapitan's own version | Compared against kapitan 0.36.3, the release krab reproduces, not krab's own version; `--ignore-version-check` and `compile.ignore-version-check` skip it as in the reference | The pin names the kapitan output an inventory expects, and that is what krab produces |
| D16 | A wildcard class entry (`enable-class-wildcards`) that matches no class | Expands every target and class file up front and fails the whole inventory; `kapitan inventory` exits 1 without a message | `inventory::class_not_found` on that entry; only the targets that use it fail | One unused or mistyped pattern should not stop every target, and a failure without a message cannot be debugged |

D1 to D6 are the rows of the ledger introduced on the repository-governance
branch. D7 keeps the number it has there so the two versions of the file
merge without renumbering; if that branch lands first, drop this file and keep
only the rows.

## Adding one

A new deviation needs a row here before the change merges, and the row has to
say what the reference does. `docs/DESIGN.md` carries the semantics and this
file carries the decisions, so neither repeats the other.
