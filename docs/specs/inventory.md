# Inventory

```
Status: As-built
Verified against: main @ a4fb9c4
Code: crates/krab-inventory/src/{inventory.rs,classfile.rs,yaml.rs,merge.rs,model.rs,dotkapitan.rs,explain.rs,error.rs}, crates/krab/src/app.rs (App::new)
```

## Problem

An inventory is a directory with `targets/` and `classes/`. Each target file
names classes, each class may name further classes, and the parameters of all
of them are merged into one document per target. Everything downstream reads
that document: `krab inventory -t`, the compile pipeline, the daemon, the
language server and kadet components through `inventory()` and
`inventory_global()`.

krab replaces kapitan 0.36.3 with the omegaconf inventory backend, so the
document it renders has to be the one kapitan renders, byte for byte once it is
emitted. That rules out "close enough" semantics in three places where
kapitan's behaviour is a side effect of its implementation rather than a
design: class names resolve through a fixed list of candidate files including
two reclass compatibility fallbacks, parameters merge with
`OmegaConf.unsafe_merge` and `EXTEND_UNIQUE` lists, and `parameters.kapitan` is
rewritten by pydantic models after interpolation. Where krab deliberately
differs, the difference is a row in [DECISIONS.md](../DECISIONS.md) and this
spec refers to it.

Two further constraints shape the loader. A single target must render without
touching the files of other targets, because the daemon re-renders only what a
file change affects; closures of class files are therefore memoised and record
every path they probed. And every value keeps the file, line and column it was
written at, so `explain`, diagnostics and the language server can point at
source.

Interpolation of `${...}` values after merging is specified in
[interpolation.md](interpolation.md).

## Requirements

### Loading

```
INV-1  Every file with the extension `.yml` or `.yaml` under
       `<inventory>/targets/`, at any depth, MUST be one target. Files with
       other extensions MUST be ignored.
       Test: crates/krab-inventory/src/inventory.rs::tests::a_target_in_a_subdirectory_is_named_after_its_file
       Since: 319ca84

INV-2  YAML MUST be read with PyYAML `safe_load` scalar rules (YAML 1.1):
       `yes`/`no`/`on`/`off` are booleans, `0755` is octal, `0x1f` is hex,
       `1_000` is 1000, `1:30` is 90, `1e5` and `1.5e3` are strings,
       `1.5e+3` and `.5` are floats, an empty scalar and `~` are null.
       Test: crates/krab-inventory/src/yaml.rs::tests::yaml11_scalars
       Since: 319ca84

INV-3  Anchors, aliases and `<<` merge keys MUST work, with explicit keys
       of the mapping winning over merged ones.
       Test: crates/krab-inventory/src/yaml.rs::tests::positions_and_merge_keys
       Since: 319ca84

INV-4  Every loaded value MUST carry the file, line and column it was
       written at.
       Test: crates/krab-inventory/src/yaml.rs::tests::positions_and_merge_keys
       Since: 319ca84

INV-5  Timestamps MUST stay strings (D3).
       Test: crates/krab-inventory/src/yaml.rs::tests::yaml11_scalars
       Since: 319ca84

INV-6  A tag other than the core `!!str`, `!!int`, `!!float`, `!!bool` and
       `!!null` on a scalar, `!!map` on a mapping or `!!seq` on a sequence
       MUST fail with `yaml::unknown_tag` (D2, D22), with the message
       printing the tag as written.
       Test: none
       Since: 319ca84 (scalars), not met yet for mappings and sequences (#219)

INV-7  A class or target file MUST be read as the sections `classes`,
       `parameters`, `applications` and `exports`. An empty file or a
       `null` section MUST count as empty, and other top-level keys MUST be
       ignored.
       Test: tests/fixtures/inventory/classes/empty.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-8  A falsy `classes` value (`null`, `[]`, `{}`, `""`, `0`, `false`)
       MUST mean no classes, as the reference's `content.get("classes") or
       []` reads it.
       Test: tests/fixtures/inventory/targets/falsy_classes.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #131

INV-9  A truthy `classes` value that is not a list MUST fail with
       `inventory::bad_classes`, and a list entry that is not a string MUST
       fail with `inventory::bad_class_ref`, each labelled at the value.
       Test: none
       Since: 319ca84
```

### Target names

```
INV-10 A target MUST be named after its file stem (`targets/prod/app.yml`
       is `app`) unless `compose-target-name` is set, in which case it MUST
       be named after its path relative to `targets/` without the
       extension, with `/` replaced by `.` (`prod.app`).
       Test: crates/krab-inventory/src/inventory.rs::tests::a_target_in_a_subdirectory_is_named_after_its_file
       Since: #132

INV-11 `compose-target-name` MUST default to off. It MUST be read from
       `.kapitan` as kapitan does: `global.compose-target-name` first, then
       `compose-node-name` from the `compile` section, then from `global`
       (INV-45 lists where `main` differs).
       Test: none
       Since: #132

INV-12 `_kapitan_` and `_reclass_` MUST both hold `name.full` (the target
       name), `name.parts` (the name split on `.`), `name.short` (the last
       part) and `name.path` (the file path relative to `targets/` without
       the extension, whatever the naming mode).
       Test: tests/fixtures/expected/env.prod.yaml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-13 A target MUST be selectable by its name and by its dotted path in
       both naming modes.
       Test: crates/krab-inventory/src/inventory.rs::tests::a_target_is_selectable_by_name_and_by_its_dotted_path
       Since: #132

INV-14 Selecting a target that does not exist MUST fail with
       `inventory::unknown_target`.
       Test: crates/krab-inventory/src/inventory.rs::tests::a_target_is_selectable_by_name_and_by_its_dotted_path
       Since: 319ca84

INV-15 Two files that end up with one name MUST fail target discovery
       with `inventory::conflicting_targets` naming both files (D7).
       Test: crates/krab-inventory/src/inventory.rs::tests::two_files_with_one_name_are_reported_rather_than_rendered
       Since: #132
```

### Class resolution

```
INV-16 A class name `n` with parts `p1.p2...pk` MUST resolve to the first
       existing file of this sequence, all `.yml` candidates before all
       `.yaml` candidates: `<base>/p1/.../pk/init.<ext>`,
       `<base>/p1/.../pk.<ext>`, and the two reclass compatibility
       fallbacks that drop the first two parts, `<base>/p3/.../pk.<ext>`
       and `<base>/p3/.../pk/init.<ext>`. `<base>` is `classes/`.
       Test: tests/fixtures/inventory/classes/components/app/init.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-17 A class name starting with `.` MUST resolve with `<base>` set to the
       directory of the including file under `classes/`.
       Test: tests/fixtures/inventory/classes/components/app/database.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-18 A class that resolves to no file MUST fail the target with
       `inventory::class_not_found`, labelled at the class entry, with the
       candidate paths in the help text.
       Test: none
       Since: 319ca84

INV-19 A class that includes itself, directly or through other classes,
       MUST fail the target with `inventory::class_cycle` labelled where the
       cycle closes (D1).
       Test: none
       Since: 319ca84

INV-20 A file's parameters MUST be built as the closures of its classes,
       depth first in list order, followed by its own parameters. A target
       MUST be its initial parameters (INV-29) merged with the closure of
       its file.
       Test: tests/fixtures/expected/env.prod.yaml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-21 The rendered `classes` list MUST hold, for each entry in order, the
       included class's own `classes` list followed by the entry as written
       (relative names keep their leading `.`).
       Test: tests/fixtures/expected/env.prod.yaml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-22 Rendering one target MUST read only the files of its own class
       closure, and MUST record every candidate path probed while resolving
       class names, hits and misses.
       Test: crates/krab-inventory/tests/fixture.rs::single_target_render_touches_only_its_closure
       Since: 319ca84
```

### Merge semantics

The rules are those of `OmegaConf.unsafe_merge(dest, src,
list_merge_mode=EXTEND_UNIQUE)` (DESIGN.md, "Merge semantics").

```
INV-23 Merging a mapping into a mapping MUST recurse per key and append new
       keys in source order.
       Test: crates/krab-inventory/src/merge.rs::tests::dict_and_list_semantics
       Since: 319ca84

INV-24 Merging a list into a list MUST append the source items not already
       present, compared with Python `==` (`1 == 1.0 == True`).
       Test: crates/krab-inventory/src/merge.rs::tests::dict_and_list_semantics, crates/krab-inventory/src/value.rs::tests::python_equality
       Since: 319ca84

INV-25 Merging a container onto a string containing `${` MUST evaluate the
       string against the tree merged so far. If that yields a container,
       the container MUST be copied in and the source merged into the copy;
       otherwise the source MUST replace the string.
       Test: crates/krab-inventory/src/merge.rs::tests::interpolation_dereferenced_then_merged, crates/krab-inventory/src/merge.rs::tests::interpolation_replaced_by_container_without_deref
       Since: 319ca84

INV-26 Any other combination MUST replace the destination with the
       source, subject to INV-27.
       Test: crates/krab-inventory/src/merge.rs::tests::dict_and_list_semantics
       Since: 319ca84

INV-27 A source value of `???` MUST leave an existing destination value in
       place. A key that only the source has keeps `???`.
       Test: tests/fixtures/inventory/targets/missing_merge.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #175

INV-28 Every override, list append and merge-time dereference MUST be
       recorded with the origins involved, and `explain` MUST report them
       for a path. Recording MAY be turned off with
       `track_provenance = false`.
       Test: crates/krab-inventory/tests/fixture.rs::explain_reports_overrides
       Since: 319ca84
```

### Kapitan model

```
INV-29 Before any class is merged, a target's parameters MUST be
       `kapitan: {compile: [], vars: {target: null}, labels: {},
       dependencies: [], target_full_path: "", secrets: null, validate: []}`
       plus the `_kapitan_` and `_reclass_` metadata of INV-12.
       Test: tests/fixtures/expected/falsy_classes.yaml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-30 After interpolation, `parameters.kapitan` MUST be normalised as the
       reference's pydantic models do: fields in model order, defaults
       filled per `input_type` and per dependency `type`, helm's
       `output_type` forced to `auto`, unknown fields rejected with
       `inventory::invalid_kapitan_config` (`vars` accepts extra fields).
       Test: tests/fixtures/expected/env.prod.yaml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

INV-31 A compile entry without a known `input_type`, a dependency without a
       known `type`, or an entry missing a required field MUST fail with
       `inventory::invalid_kapitan_config`.
       Test: none
       Since: 319ca84

INV-32 `--raw` MUST skip the normalisation of INV-30.
       Test: none
       Since: 58f8ddd

INV-33 The target document MUST have the keys `parameters`, `classes`,
       `applications`, `exports` and `resolved: false` (kapitan 0.36 leaks
       the last one into its output).
       Test: crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84
```

### `.kapitan`

The keys and the sections they are read from are listed once in
[CLI.md](../CLI.md#kapitan). `inventory.python-resolvers` belongs to
[interpolation.md](interpolation.md).

```
INV-34 `.kapitan` MUST be read from the working directory only. Without
       the file every setting MUST take its default.
       Test: crates/krab-inventory/src/dotkapitan.rs::tests::reads_compile_section, crates/krab-inventory/src/dotkapitan.rs::tests::warns_unless_omegaconf_is_selected
       Since: 319ca84

INV-35 The inventory directory MUST be `--inventory-path` when given, else
       `inventory-path` from `.kapitan`, else `./inventory`.
       Test: none
       Since: 319ca84

INV-36 Every command that loads the inventory MUST print one warning line
       on stderr when kapitan would not render it with omegaconf: no
       `.kapitan`, no `inventory-backend`, or another backend. The value
       MUST be read from the `inventory_backend` section first, then
       `global`, as kapitan does. An `inventory_backend` section without the
       `inventory-backend` key MUST be named in the warning. Stdout, `--json`
       output and the exit code MUST NOT change.
       Test: crates/krab-inventory/src/dotkapitan.rs::tests::warns_unless_omegaconf_is_selected, crates/krab/tests/backend_warning.rs::warns_when_the_backend_is_not_omegaconf
       Since: #161
```

### Diagnostics

```
INV-37 Every inventory failure MUST be a diagnostic with a stable code, a
       message, the target and parameter path when known, labels that
       resolve to `file:line:col`, and an optional help text.
       Test: none
       Since: 319ca84

INV-38 Rendering all targets MUST collect a failure per target and still
       render the others.
       Test: none
       Since: 319ca84
```

### Not met yet

These requirements describe the behaviour krab is meant to have. `main` does
not meet them yet; the open deviations below say what it does instead.

```
INV-39 When `enable-class-wildcards` is set (`--enable-class-wildcards`,
       or `.kapitan` `inventory_backend`, then `global`), a `classes` entry
       with a glob pattern (`comp.*`) MUST expand to the matching class
       names, in sorted order, as kapitan does. Without it, the entry MUST
       stay a literal class name. A pattern that matches nothing MUST fail
       only the targets that use it (D16).
       Test: none
       Since: not met yet (#69)

INV-40 A `.kapitan` `version:` that kapitan 0.36.3 does not satisfy MUST
       stop the run, as the reference refuses to compile. When the value is
       a YAML float, the message MUST say it was read as a number and
       should be quoted.
       Test: none
       Since: not met yet (#119)

INV-41 An unknown `.kapitan` key MUST be reported. `init.*` keys MUST be
       reported as settings of `kapitan init`, a command krab does not
       have, not as unknown.
       Test: none
       Since: not met yet (#142)

INV-42 A compile key MUST fall back from the `compile` section to
       `global`, as kapitan's `from_dot_kapitan` does.
       Test: none
       Since: not met yet (#164)

INV-43 A mapping key written twice in one file SHOULD produce a warning.
       Test: none
       Since: not met yet (#8)

INV-44 An inventory that kapitan renders with reclass SHOULD NOT render
       with omegaconf semantics without the user opting in.
       Test: none
       Since: not met yet (#118)

INV-45 `compose-target-name` MUST be resolved in the order of INV-11, and
       a `compose-target-name` or `compose-node-name` key in a section
       kapitan does not read it from MUST produce a warning.
       Test: none
       Since: not met yet (#220)

INV-46 A class resolved through one of the two reclass compatibility
       fallbacks of INV-16 MUST produce a warning naming the file used and
       the paths expected. When that file is `classes/init.yml` or
       `<inventory>/classes.yml` (the dropped parts leave nothing), the
       warning MUST also say that a later release makes it an error.
       Test: none
       Since: not met yet (#214)

INV-47 A top-level key in a class or target file other than the sections
       of INV-7 MUST produce a warning, with a "did you mean" naming the
       section within edit distance 2 when there is one.
       Test: none
       Since: not met yet (#215)
```

## Acceptance criteria

1. Given the fixture inventory under `tests/fixtures/inventory`, opened with
   `compose_target_name` on as `generate_expected.py` does, when every target
   is rendered and emitted, then each document equals
   `tests/fixtures/expected/<target>.yaml` (kapitan 0.36.3 output) byte for
   byte, and every rendered target has an expected file. Covers INV-2,
   INV-7, INV-8, INV-12, INV-16, INV-17, INV-20, INV-21, INV-24 to INV-27,
   INV-29, INV-30, INV-33. Test:
   `crates/krab-inventory/tests/fixture.rs::renders_like_the_reference`.
2. Given `targets/prod/app.yml` and no `compose-target-name`, when the
   targets are listed, then the target is `app`, `-t app` and `-t prod.app`
   both select it, and with the setting on it is `prod.app`. Covers INV-10,
   INV-13. Tests: `inventory.rs::tests::a_target_in_a_subdirectory_is_named_after_its_file`,
   `inventory.rs::tests::a_target_is_selectable_by_name_and_by_its_dotted_path`.
3. Given a repository without `.kapitan`, when `krab --no-daemon inventory
   targets` runs, then stderr holds exactly one warning naming
   `inventory-backend: omegaconf` and the command succeeds; with
   `global.inventory-backend: omegaconf` there is no warning. Covers INV-36.
   Test: `crates/krab/tests/backend_warning.rs::warns_when_the_backend_is_not_omegaconf`.
4. Given the target `bare` of the fixture inventory, when it is rendered
   alone, then its files are `bare.yml`, `common.yml`, `empty.yml` and its
   probes include the missed `classes/common/init.yml`. Covers INV-22.
   Test: `crates/krab-inventory/tests/fixture.rs::single_target_render_touches_only_its_closure`.

## Edge cases

* INV-EC-1: no `targets/` directory. The CLI fails before rendering with
  `inventory::no_targets_dir` and help to run from the directory holding
  `inventory/` or pass `--inventory-path`.
* INV-EC-2: a file that cannot be read fails the targets that need it with
  code `io`; a file with invalid YAML, or with a second YAML document, fails
  them with a `yaml::*` diagnostic.
* INV-EC-3: a top-level value that is not a mapping fails with
  `inventory::bad_document`; a `parameters` or `exports` that is not a
  mapping, or `applications` that is not a list, fails with
  `inventory::bad_parameters`, `inventory::bad_exports` or
  `inventory::bad_applications`.
* INV-EC-4: a missing class, a class cycle and a conflicting target name
  behave as INV-18, INV-19 and INV-15. Only the conflicting name stops every
  target; the other two fail the targets that include the class.
* INV-EC-5: a `.kapitan` that is not valid YAML stops every command that
  loads the inventory, before any target renders.
* INV-EC-6: a mapping with the same key twice keeps the last value without a
  diagnostic (open deviation INV-43).

## Interfaces

Diagnostic codes emitted by the inventory loader, in addition to the
interpolation codes of [interpolation.md](interpolation.md):

| Code | Raised for |
|---|---|
| `inventory::no_targets_dir` | INV-EC-1 |
| `inventory::unknown_target` | INV-14 |
| `inventory::conflicting_targets` | INV-15 |
| `inventory::class_not_found` | INV-18 |
| `inventory::class_cycle` | INV-19 |
| `inventory::bad_document`, `bad_classes`, `bad_class_ref`, `bad_parameters`, `bad_applications`, `bad_exports` | INV-9, INV-EC-3 |
| `inventory::invalid_kapitan_config` | INV-30, INV-31 |
| `yaml::unknown_tag`, `yaml::syntax`, `yaml::bad_int`, `yaml::bad_float`, `yaml::bad_bool` | INV-6, INV-EC-2 |
| `io` | INV-EC-2 |

The diagnostic JSON shape (`severity`, `code`, `message`, `target`, `path`,
`labels`, `help`) is the one `--json` prints; see
[cli-and-release.md](cli-and-release.md).

The library entry points other crates use are `Inventory::new`,
`discover_targets`, `target_spec`, `render`, `render_all`,
`resolve_class_file` (also used by the language server) and
`explain::explain`.

## Out of scope

* The reclass and reclass-rs backends. krab renders with omegaconf
  semantics only; INV-36 says so when kapitan would pick another backend.
* Interpolated class names (`clusters.${cluster.name}`). The omegaconf
  backend passes class names through verbatim and so does krab; supporting
  them would be a krab extension with reclass semantics (#70).

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| INV-6 | #219 | A tag on a mapping or sequence (`!!omap`, `!!set`, `!custom`) is dropped and the value renders as a plain mapping or list; `!foo` on a scalar is reported as `!!foo` |
| INV-39 | #69 | No `enable-class-wildcards` setting; a pattern is always taken literally and fails with `inventory::class_not_found` |
| INV-40 | #119 | `version` is not read |
| INV-41 | #142 | Unknown keys are accepted and have no effect, without a word |
| INV-42 | #164 | `DotKapitan::compile_*` read the `compile` section only |
| INV-43 | #8 | The last value wins silently |
| INV-44 | #118 | krab renders it with omegaconf semantics and prints the INV-36 warning; the refusal is not implemented |
| INV-45 | #220 | `dotkapitan.rs:114-115` reads `compose-node-name` before `compose-target-name`, each from `compile`, `inventory` or `global`, so the two disagree when both keys are set with different values or when `compose-target-name` is written outside `global`; no warning names a key in a section kapitan ignores |
| INV-46 | #214 | A fallback hit loads without a diagnostic: `componets.nginx` loads `classes/init.yml` |
| INV-47 | #215 | Other top-level keys are ignored without a diagnostic: `paramters: {replicas: 3}` is dropped |
