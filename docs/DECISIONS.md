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

## Adding one

A new deviation needs a row here before the change merges, and the row has to
say what the reference does. `docs/DESIGN.md` carries the semantics and this
file carries the decisions, so neither repeats the other.
