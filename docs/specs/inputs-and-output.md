# Inputs and output

Status: As-built
Verified against: main @ a4fb9c4
Code: `crates/krab-compile/src/inputs/` (`mod.rs`, `jinja.rs`, `kadet.rs`, `helm.rs`, `copy.rs`, `remove.rs`, `external.rs`), `crates/krab-compile/src/native.rs` (item dispatch), `crates/krab-compile/src/output.rs`, `crates/krab-compile/runner/kadet_runner.py`, `crates/krab-compile/runner/kapitan/`, `crates/krab-inventory/src/emit/`

## Problem

Each entry of `parameters.kapitan.compile` names an input type, its input
paths and where its output goes. kapitan 0.36.3 implements the input types in
Python and writes the result through `InputType.to_file` with PyYAML or
rapidyaml. krab implements them in Rust and has to produce the same files, byte
for byte: the same names, the same YAML quoting and block styles, the same
document markers, the same empty files.

Byte parity rules out a general-purpose YAML library. The emitter is a port of
PyYAML's emitter, including its quirks (80-column folding, the fallback to
double quotes when a block scalar does not fit, `...` after a root scalar), and
each such rule is pinned by a unit test whose expected string came from PyYAML.
Where the reference behaves in a way krab does not reproduce yet, the row is
under Open deviations.

The second constraint is incremental compilation
([compile.md](compile.md)): every input type reports what it read, so that a
change to a template, a chart or a module recompiles the targets that used it.

kadet components remain Python. The evaluator runs only the component's
`main()`, and the component's `kapitan.*` imports resolve to a small package
shipped with krab, so the interpreter needs `kadet` but not the Python kapitan.
Formatting, refs and writing happen in Rust, as for every other input type.

This spec covers the native backend. `--backend python` runs kapitan's own
input types and writers, and its output is kapitan's by construction. Refs in
the output are compiled or revealed as described in [refs.md](refs.md).

## Requirements

### Compile items

```
OUT-1  Compile items MUST arrive normalised by the inventory model, with
       kapitan's defaults filled in: `output_type: yaml`, `prune: true`,
       `ignore_missing: false`, `continue_on_compile_error: false`,
       `input_params: {}`; for jinja2 `ignore_missing: true`,
       `suffix_remove: false`, `suffix_stripped: .j2`; for kadet
       `prune: false`; for helm `prune: false` and `output_type: auto`
       whatever the inventory says; for external `env_vars: {}`, `args: []`.
       Test: crates/krab-inventory/tests/fixture.rs::renders_like_the_reference (tests/fixtures/expected/env.dev.yaml)
       Since: 319ca84

OUT-2  Each input path MUST be looked up in every search path
       (`compile.search-paths`, default the repository root) and in the
       target's temporary directory, so an item can name output an earlier
       item of the same target wrote. A path containing `*`, `?` or `[` is a
       glob. Matches MUST be sorted and deduplicated. No match MUST be the
       error "compile error: <input> for target: <target> not found in
       search_paths: [...]" unless `ignore_missing` is set.
       Test: none
       Since: 1b9c9c5

OUT-3  The native input types MUST be jinja2, kadet, helm, copy, remove and
       external. Any other input type whose input paths resolve MUST fail
       with "input type `<type>` is not supported by the native compiler
       yet (use --backend python)".
       Test: none
       Since: #176

OUT-4  An `output_type` other than json, yaml, yml, plain, toml or auto
       MUST fail the item with "unknown output_type `<type>`".
       Test: none
       Since: 1b9c9c5
```

### jinja2

```
OUT-5  A file input MUST render to `<output dir>/<file name>`. A directory
       input MUST render every file under it whose name does not start with
       `.`, in sorted order, to the same relative path.
       Test: crates/krab/tests/incremental.rs::a_partial_compile_does_not_hide_other_stale_targets
       Since: 1b9c9c5

OUT-6  Templates MUST render with Jinja2's environment as kapitan builds it:
       undefined variables are errors, `trim_blocks` and `lstrip_blocks` on,
       no auto-escaping, the final newline of the template dropped,
       booleans and None written as `True`, `False`, `None`, lists and
       mappings as Python reprs. `include` and `import` look in the
       template's directory, then the search paths.
       Test: crates/krab/tests/compile_selection.rs::a_selected_target_reading_a_rendered_target_compiles
       Since: 1b9c9c5

OUT-7  The context MUST hold `inventory` (the target document),
       `inventory_global` (other targets, fetched and recorded on access),
       `input_params` (with `compile_path` set to the item's output directory
       unless given), and every key of `parameters.kapitan.vars` at the top
       level.
       Test: crates/krab/tests/compile_selection.rs::a_selected_target_reading_a_rendered_target_compiles
       Since: 1b9c9c5

OUT-8  The filters MUST be kapitan's: sha256, b64encode, b64decode, yaml,
       to_json, basename, dirname, bool, ternary, regex_replace,
       regex_escape, regex_search, regex_findall, reveal_maybe, fileglob,
       merge_strategic. toml, to_datetime, strftime and shuffle MUST fail
       with "the `<name>` filter is not supported by the native compiler
       yet".
       Test: none
       Since: 1b9c9c5

OUT-9  A rendered file MUST get the mode bits of its template.
       Test: none
       Since: 1b9c9c5

OUT-10 With `suffix_remove: true`, a file name ending in `suffix_stripped`
       MUST lose its trailing characters that occur in `suffix_stripped`,
       as Python's `str.rstrip(chars)` does.
       Test: none
       Since: 1b9c9c5
```

### kadet

```
OUT-11 The evaluator MUST import the component at the input path and call
       `main(input_params)` or `main()`; a `main` with more parameters is an
       error. `input_params` gets `compile_path` (the item's output
       directory) unless it is set.
       Test: crates/krab-compile/tests/kadet_runner.rs::evaluates_a_component_without_the_python_kapitan
       Since: 1b9c9c5

OUT-12 The component's `kapitan.*` imports MUST resolve to the package
       bundled with the evaluator (the module list is in docs/DESIGN.md);
       importing any other `kapitan.*` module MUST raise an error rather than
       load an installed kapitan.
       Test: crates/krab-compile/tests/kadet_runner.rs::evaluates_a_component_without_the_python_kapitan
       Since: #89

OUT-13 The evaluator MUST report the files it read, directories it listed,
       modules it imported (`.pyc` mapped to source), other targets it read
       (`*` when it iterated all), and the parts of the target document it
       read (`parameters.<key>`, another top-level key, or `*`).
       Test: crates/krab-compile/tests/kadet_runner.rs::evaluates_a_component_without_the_python_kapitan
       Since: #61

OUT-14 Reading a topic the target does not declare with `consume: true`
       MUST fail the item.
       Test: crates/krab-compile/tests/kadet_runner.rs::undeclared_topic_is_a_compile_error
       Since: #89

OUT-15 A `HelmChart` render inside a component MUST be done by the host with
       the arguments of OUT-17, recorded as reads like a helm item (OUT-22),
       and cached under `$XDG_CACHE_HOME/krab/helm-render`, keyed on the helm
       version, the arguments, the values files' content and the chart
       tree.
       Test: none
       Since: #55

OUT-16 An import error for a package the environment lacks MUST end with a
       hint to declare it under `compile.python-requirements`.
       Test: none
       Since: #91
```

### helm

```
OUT-17 Each chart in `input_paths` MUST be rendered with `helm template` and
       these arguments, in order: `--include-crds --skip-tests`,
       `--api-versions <kube_version>` when set, each `helm_params` entry as
       a long flag (`_` becomes `-`, `true` is the bare flag, `false` is
       dropped, other values are Python `str()`), `--values` for the values
       file of `helm_values` and then for each `helm_values_files` entry,
       `--output-dir` unless `helm_params.output_file` is set, the release
       name (`name`, else a non-boolean `release_name`, else
       `--generate-name`), and the chart path.
       Test: crates/krab-compile/src/inputs/helm.rs::tests::params_become_long_flags
       Since: #176

OUT-18 `helm_params` MUST reject single-character names, names containing
       `-`, `set`, `set_file`, `set_string`, `values`, `dry_run`,
       `generate_name`, `help`, `output_dir` and `show_only`, with
       kapitan's messages.
       Test: crates/krab-compile/src/inputs/helm.rs::tests::rejected_params
       Since: #55

OUT-19 The values file MUST be written only when `helm_values` is
       non-empty, in PyYAML's SafeDumper layout, with digit-only strings
       quoted when they start with `0` or are longer than six characters.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::helm_values_quote_digit_strings
       Since: #176

OUT-20 The charts of one item MUST share its `helm_params`: the first chart
       consumes `name` and `output_file`, later charts render without them.
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference (tests/fixtures/helm-expected)
       Since: #176

OUT-21 Each file helm renders MUST be parsed with the inventory's
       PyYAML-compatible loader, its null documents dropped, and written
       through the writer with `output_type: auto` under the same relative
       path; with `output_file`, helm's stdout is that one file.
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference (tests/fixtures/helm-expected)
       Since: #176

OUT-22 Every file and directory of the chart tree (except `__pycache__`)
       and every `helm_values_files` entry MUST be recorded as read.
       Test: crates/krab/tests/helm_input.rs::an_edited_chart_template_makes_the_target_stale
       Since: #176

OUT-23 The helm binary MUST be `helm_path`, else `$KAPITAN_HELM_PATH`, else
       `helm`. A missing binary MUST fail with kapitan's "helm binary not
       found. helm must be present in the PATH ..." message, a run longer
       than `$KAPITAN_HELM_TIMEOUT` seconds (default 30) is killed and
       fails, and a non-zero exit fails with helm's stderr.
       Test: crates/krab/tests/helm_input.rs::a_missing_helm_binary_is_reported_like_the_reference
       Since: #176
```

### copy, remove, external

```
OUT-24 copy MUST copy a file into the output directory (or onto the output
       path when that is an existing file) and a directory recursively into
       the output directory, keeping mode bits. A missing input with
       `ignore_missing` copies nothing; without it the item fails with "Path
       <p> does not exist and `ignore_missing` is false".
       Test: crates/krab/tests/output_path.rs::output_path_dot_is_the_target_directory
       Since: 1b9c9c5

OUT-25 remove MUST delete the resolved file or directory tree. A path that
       no longer exists is not an error.
       Test: none
       Since: 1b9c9c5

OUT-26 external MUST run `sh -c "<input path> <args>"` with
       `${compiled_target_dir}` in the arguments and `env_vars` values
       replaced by the item's output directory. The environment MUST be
       `env_vars` plus PATH and HOME from krab's environment unless
       `env_vars` sets them. A non-zero exit MUST fail the item with
       "executing external input with command '<command>' failed: <stderr>".
       Test: none
       Since: 1b9c9c5
```

### Output files

```
OUT-27 With `prune`, null values and empty lists and mappings MUST be
       removed recursively before writing. A mapping that becomes empty
       only through pruning stays.
       Test: crates/krab-compile/src/output.rs::tests::prunes_like_kapitan
       Since: 1b9c9c5

OUT-28 The output type MUST decide the file name: yaml appends `.yaml`, yml
       `.yml`, json `.json`, plain nothing; auto keeps a name ending in
       `.json`, `.yaml`, `.yml` or `.toml` and writes that type, and
       otherwise appends the default type's extension.
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference (tests/fixtures/helm-expected)
       Since: 1b9c9c5

OUT-29 toml output MUST be written as kapitan writes it.
       Test: none
       Since: not met yet (#12)

OUT-30 YAML or JSON content that is falsy in Python (null, empty mapping or
       list, empty string, 0, false) MUST leave an empty file.
       Test: crates/krab-compile/src/output.rs::tests::empty_content_leaves_an_empty_file
       Since: #182

OUT-31 plain output MUST write a string as it is and any other value as
       Python's `str()` of it.
       Test: none
       Since: 1b9c9c5

OUT-32 JSON MUST be Python's `json.dumps(obj, indent=<compile.indent>,
       sort_keys=True)`: ASCII-only escapes, `repr` floats, `NaN` and
       `Infinity`, no trailing newline.
       Test: crates/krab-inventory/src/emit/pyjson.rs::tests::matches_python_pretty_json
       Since: 1b9c9c5

OUT-33 Refs in every string of the content MUST be compiled, or revealed
       under `--reveal`, before the file is written (refs.md).
       Test: crates/krab/tests/helm_input.rs::helm_input_matches_the_reference (tests/fixtures/helm-expected)
       Since: 1b9c9c5
```

### YAML emission

```
OUT-34 YAML MUST be PyYAML's output with kapitan's PrettyDumper: indent
       `compile.indent` (default 2), sequences indented under their key,
       keys sorted, plain and quoted scalars folded past column 80,
       ASCII-only (other characters escaped in double quotes).
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::pretty_dumper_basics
       Since: 1b9c9c5

OUT-35 With `compile.yaml-use-rapidyaml: true`, YAML MUST be kapitan's
       rapidyaml dumper output instead, except for content holding a control
       character other than tab, LF or CR, or DEL, which falls back to OUT-34
       as kapitan's wrapper does.
       Test: crates/krab-inventory/tests/corpus.rs::corpus_parity (runs only with KRAB_CORPUS and KRAB_COMPILED set)
       Since: 1b9c9c5

OUT-36 A string that a YAML 1.1 loader would read as another type (int,
       float, bool, null, timestamp, `<<`, `=`) MUST be single-quoted; other
       quoting follows PyYAML's `analyze_scalar` and `choose_scalar_style`.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::quoting_rules
       Since: 1b9c9c5

OUT-37 Floats MUST be written as Python's `repr` with `.0` inserted before
       an exponent, and as `.nan`, `.inf`, `-.inf`. Null MUST be `null`, or
       empty with `compile.yaml-dump-null-as-empty: true`.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::pretty_dumper_basics
       Since: 1b9c9c5

OUT-38 A string containing a line break MUST get the style named by the
       target's `parameters.multiline_string_style`, else by
       `compile.yaml-multiline-string-style`, else `literal`. Values are
       `literal`, `folded` and `double-quotes`. `inventory.multiline-string-
       style` MUST NOT apply to compile output.
       Test: crates/krab/tests/kadet_output.rs::the_compile_setting_wins_over_the_inventory_one
       Since: #182

OUT-39 A literal or folded style that PyYAML would not allow for the string
       (empty, trailing space, space before a line break, special
       characters, a simple key, flow context) MUST fall back to double
       quotes.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::a_block_style_that_does_not_fit_falls_back_to_double_quotes
       Since: #182

OUT-40 Folded scalars MUST follow PyYAML's `write_folded`: an extra line
       break between lines unless either starts with a space, and a fold at
       a single space past column 80, never inside a run of spaces.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::folded_breaks_only_at_a_single_space_past_column_80
       Since: #187

OUT-41 A list MUST be written as PyYAML's `dump_all`: one document per
       element, `---` before every document but the first, an empty mapping
       as `--- {}`, and `...` only at the end of the stream when the last
       document is open-ended. A mapping or a scalar is one document; a root
       scalar ends with `...`.
       Test: crates/krab-inventory/src/emit/yaml.rs::tests::dump_all_writes_document_markers_like_pyyaml
       Since: #182

OUT-45 A folded scalar MUST read back as the string it was written from.
       When folding per OUT-40 would change the value, the string MUST fall
       back to double quotes.
       Test: none
       Since: not met yet (#218)
```

### kadet output shapes

```
OUT-42 `main()`'s result MUST be converted to plain data, a BaseObj or
       BaseModel becoming its `root` mapping, recursively. Each top-level key
       of the result MUST become one file, the key being the path relative
       to the item's output directory and the output type adding the
       extension (OUT-28, default yaml).
       Test: crates/krab/tests/kadet_output.rs::multiline_strings_are_literal_blocks_by_default (tests/fixtures/kadet-output-expected)
       Since: 1b9c9c5

OUT-43 A file's value MUST be written per OUT-41: a mapping as one document,
       a list as one document per element.
       Test: crates/krab/tests/kadet_isolation.rs::a_full_compile_gives_each_target_its_own_evaluator
       Since: 1b9c9c5

OUT-44 A file whose value is a string, under the `yaml` or `json` output
       type, MUST be written as one scalar and MUST produce a warning that
       suggests `output_type: plain`.
       Test: none
       Since: not met yet (#126)
```

## Acceptance criteria

AC-1 (OUT-17, OUT-19, OUT-20, OUT-21, OUT-28, OUT-33, OUT-41). Given
`tests/fixtures/helm`: two charts with a CRD, a subchart, a test hook, a
conditional template, a two-document template, a `.json` template, a template
with literal, `|-`, `|2` and `|+` blocks, one rendering only a null document
and `{}`, one rendering only a comment, `helm_values` with digit strings and a
`?{base64:...}` ref, a values file, and a second item with `output_file`.
When it compiles, then `compiled/` holds exactly the files of
`tests/fixtures/helm-expected/compiled` (generated by kapitan 0.36.3 with helm
3.17), byte for byte. Skips without `helm`.
Test: `crates/krab/tests/helm_input.rs::helm_input_matches_the_reference`.

AC-2 (OUT-23). Given the helm fixture with `helm_path: /nonexistent/helm`,
when it compiles, then the compile fails with kapitan's "helm binary not
found" message.
Test: `crates/krab/tests/helm_input.rs::a_missing_helm_binary_is_reported_like_the_reference`.

AC-3 (OUT-38, OUT-40, OUT-42). Given a kadet component writing a ConfigMap
whose data holds a multi-line string, when it compiles with the default
settings, with `compile.yaml-multiline-string-style: double-quotes` (and
`inventory.multiline-string-style: literal`), and with `folded`, then
`compiled/cm/manifests/cm.yaml` matches the file kapitan 0.36.3 wrote for that
style in `tests/fixtures/kadet-output-expected/`.
Tests: `crates/krab/tests/kadet_output.rs` (all three tests).

AC-4 (OUT-11, OUT-12, OUT-13, OUT-14). Given `tests/fixtures/kadet`, a
component using `inventory()`, `inventory_global()`, `topics()`,
`load_from_search_paths`, `cached.args.search_paths`, `render_jinja2_file`
with kapitan filters, `prune_empty` and `resources.inventory`, when it is
evaluated for `app.web`, then the output matches, no installed kapitan module
was imported, and the reads list the component, its library, the template,
target `app.api`, `*`, and the three `parameters.<key>` reads but not `*`.
Evaluated for `app.api`, which reads a topic it does not consume, it fails.
Tests: `crates/krab-compile/tests/kadet_runner.rs` (both tests).

## Edge cases

OUT-EC-1. An input path matches nothing and `ignore_missing` is false: the
item fails with the "not found in search_paths" message (OUT-2). jinja2
items default to `ignore_missing: true` and compile nothing.

OUT-EC-2. A template references an undefined variable: the item fails with
"Jinja2 TemplateError: <cause> in <path>".

OUT-EC-3. An unsupported input type (jsonnet, kustomize, cuelang) whose
input paths resolve to nothing (an empty list, or `ignore_missing` with no
match): the item does nothing and does not fail.

OUT-EC-4. helm runs longer than the timeout: it is killed and the item fails
with kapitan's timeout message, which names `KAPITAN_HELM_TIMEOUT`.

OUT-EC-5. helm writes output the loader cannot parse: the item fails with
"cannot parse the output of helm template for <file>".

OUT-EC-6. A kadet `main()` returns something other than a mapping: nothing is
written and the item does not fail.

OUT-EC-7. A kadet component raises: the item fails with "Could not load Kadet
module: <name>: <error>" followed by the Python traceback.

OUT-EC-8. An item uses `output_type: toml`: the item fails with "toml output
is not supported by the native compiler yet" (#12).

OUT-EC-9. An external command reads files or the network: krab does not see
those reads, so a change to them does not recompile the target (#16).

## Interfaces

### Compile item keys

| Key | Used by |
|---|---|
| `input_type`, `input_paths`, `output_path`, `output_type`, `prune`, `ignore_missing`, `continue_on_compile_error`, `input_params` | every type |
| `suffix_remove`, `suffix_stripped` | jinja2 |
| `helm_params`, `helm_values`, `helm_values_files`, `helm_path`, `kube_version` | helm |
| `args`, `env_vars` | external |

`continue_on_compile_error` is specified in [compile.md](compile.md) (CMP-22).

### Settings

| Source | Effect |
|---|---|
| `compile.search-paths` | where input paths are looked up (OUT-2) |
| `compile.indent` | YAML and JSON indent (OUT-32, OUT-34) |
| `compile.yaml-use-rapidyaml` | rapidyaml layout (OUT-35) |
| `compile.yaml-dump-null-as-empty` | null as an empty scalar (OUT-37) |
| `compile.yaml-multiline-string-style`, `parameters.multiline_string_style` | multi-line string style (OUT-38) |
| `KAPITAN_HELM_PATH`, `KAPITAN_HELM_TIMEOUT` | helm binary and timeout (OUT-23) |

### kadet evaluator

The evaluator (`runner/kadet_runner.py`) reads one JSON request per line on
stdin and answers on stdout: `init` (repository root, inventory snapshot file
or server socket, compile settings), `eval` (target, input path, input params,
compile path, temporary directory) and `exit`. During an `eval` it may send a
`helm` request and wait for the host's answer. The `eval` reply carries
`output`, `files`, `dirs`, `globals` and `doc_reads`. The bundled `kapitan`
package and its modules are listed in the Compile section of
[../DESIGN.md](../DESIGN.md).

## Out of scope

* Python-defined jinja2 filters beyond the list in OUT-8.
* Output of `--backend python`, which is kapitan's own.
* How refs are compiled, created and revealed ([refs.md](refs.md)).
* Dropping `--backend python` once every input type is native (#17).

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| OUT-3 | #11 | jsonnet, kustomize and cuelang are not native and need `--backend python`. helm is native since #176 |
| OUT-29 | #12 | `output_type: toml` fails the item (`output.rs:159`); the jinja2 `toml` filter fails too |
| OUT-44 | #126 | A kadet file value that is a string (a BaseObj whose root is a string) is written as one scalar document without a warning |
| OUT-45 | #218 | A line that starts with a space and runs past column 80 is folded as PyYAML folds it (`emit/yaml.rs:1154-1172`), and the file reads back without the line break before the next line and with a new one at the fold |
| OUT-36 | #202 | `0o644` is read as a string and written unquoted; kapitan reads it as the integer 420 and quotes the string `'0o644'`, an import side effect of yamllint in the reference |
