# Decisions

Rendering and compiled output are byte-identical to kapitan 0.36.3 with the
`omegaconf` inventory backend. Everywhere krab behaves differently on purpose,
it is listed here with the reason. A difference that is not in this list is a
bug.

Input types and outputs that are not implemented yet are status, not decisions,
and are listed in [../README.md](../README.md#compatibility).

## Deviations from the reference

| # | Subject | Reference | krab | Why |
|---|---|---|---|---|
| D7 | Two target files with one name (`a/x.yml` and `b/x.yml` without `compose-target-name`) | Renders nothing at all, and says nothing: no targets, no diagnostic, exit code 0 | `inventory::conflicting_targets`, naming both files | An inventory that silently produces no targets cannot be debugged |

D1 to D6 are the rows of the ledger introduced on the repository-governance
branch. This row keeps the number it has there so the two versions of the file
merge without renumbering; if that branch lands first, drop this file and keep
only the row.

## Adding one

A new deviation needs a row here before the change merges, and the row has to
say what the reference does.
