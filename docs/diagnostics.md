# Diagnostic codes

Every diagnostic krab prints carries a code of the form `area::name`, in the
pretty output and as the `code` field under `--json` (shape in
[CLI.md](CLI.md#json-output)). This page lists each code, what triggers it and
the usual fix. All codes are errors unless the table says warning.
`crates/krab/tests/docs_consistency.rs` fails when a code in `crates/*/src`
has no entry here.

Compile errors other than rendering (input types, refs, fetch, writing
output) are plain messages without a code.

## yaml

Raised while reading a class, target or `.kapitan` file.

| Code | Trigger | Typical fix |
|---|---|---|
| `yaml::syntax` | The file is not valid YAML, or the parser reached an unexpected event. The label points at the position the parser reports. | Fix the YAML at that position (indentation, an unclosed quote or bracket). |
| `yaml::multiple_documents` | A file that must hold one document has a second one after `---`. | Remove the second document or move it into its own file. |
| `yaml::unknown_alias` | An alias `*name` refers to an anchor that is not defined earlier in the file. | Define the anchor `&name` before the alias, or fix the spelling. |
| `yaml::complex_key` | A mapping key is itself a mapping or a sequence. | Use a scalar key. |
| `yaml::merge` | The value of a `<<` merge key is not a mapping or a list of mappings. | Merge only mappings: `<<: *anchor` or `<<: [*a, *b]`. |
| `yaml::unknown_tag` | A scalar carries a tag that is not one of the core tags `!!str`, `!!int`, `!!float`, `!!bool` and `!!null`, for example `!custom` or `!!binary`. | Remove the tag, or quote the value and convert it where it is used. |
| `yaml::bad_int` | A value tagged `!!int` is not an integer. | Fix the value or drop the tag. |
| `yaml::bad_float` | A value tagged `!!float` is not a float. | Fix the value or drop the tag. |
| `yaml::bad_bool` | A value tagged `!!bool` is not a YAML 1.1 boolean. | Fix the value or drop the tag. |

## inventory

| Code | Trigger | Typical fix |
|---|---|---|
| `inventory::no_targets_dir` | The inventory path has no `targets/` directory. | Run krab from the directory holding `.kapitan`, or pass `--inventory-path`. |
| `inventory::conflicting_targets` | Two target files produce the same target name, for example `a/x.yml` and `b/x.yml` without `compose-target-name`. | Rename one file, or enable `compose-target-name` in `.kapitan` so nested targets get dotted names. |
| `inventory::unknown_target` | A command or RPC call names a target that does not exist. | Check the name with `krab inventory targets`. |
| `inventory::bad_document` | The top of a class or target file is not a mapping. | Make the file a mapping with `classes`, `parameters` and the other top-level keys. |
| `inventory::bad_classes` | `classes` is not a list. | Write `classes` as a list of class names. |
| `inventory::bad_class_ref` | An entry under `classes` is not a string. | Quote the class name, or remove the nested structure. |
| `inventory::bad_parameters` | `parameters` is not a mapping. | Write `parameters` as a mapping. |
| `inventory::bad_applications` | `applications` is not a list. | Write `applications` as a list. |
| `inventory::bad_exports` | `exports` is not a mapping. | Write `exports` as a mapping. |
| `inventory::class_not_found` | A class name resolves to no file. The help lists every path that was tried. | Fix the class name or create the file at one of the listed paths. |
| `inventory::class_cycle` | A class includes itself, directly or through its parents. | Remove the include that closes the cycle; the label points at it. |
| `inventory::invalid_kapitan_config` | `parameters.kapitan` does not fit kapitan's model: a field with the wrong type, an unknown field, an unknown `input_type` or an unknown dependency `type`. | Fix the field the message names; the label points at its declaration. |
| `inventory::path_not_found` | `krab inventory explain` was given a path with no value in the target. | Give the path relative to `parameters`, for example `cluster.name`. |
| `inventory::pattern_not_found` | `krab inventory -p` was given a path with no value in the selected targets. | Give the path relative to the target document, for example `parameters.kapitan.compile`. |
| `inventory::python_resolvers` | The `resolvers.py` configured under `inventory.python-resolvers` could not be loaded, or no Python interpreter was found to run it. | Fix the file or the interpreter (`KRAB_PYTHON`, `inventory.python-resolvers.python`), point `inventory.python-resolvers.file` elsewhere, or set `inventory.python-resolvers: false`. |
| `io` | A file or directory of the inventory could not be read. | Check that the path exists and is readable. |

## interpolation

Raised while resolving `${...}` in a target's parameters. The diagnostic
names the target and the parameter path.

| Code | Trigger | Typical fix |
|---|---|---|
| `interpolation::syntax` | The value is not a valid interpolation string. | Fix the expression; a literal `${` is escaped as `\${`. |
| `interpolation::key_not_found` | `${a.b}` names a key that does not exist. | Fix the path, or give a default with `${oc.select:a.b,default}`. |
| `interpolation::not_a_container` | A path goes through a value that is not a mapping or a list, for example `${a.b}` where `a` is a string. | Fix the path, or the value it goes through. |
| `interpolation::bad_index` | A path indexes a list with a key that is not an integer. | Use a numeric index. |
| `interpolation::bad_relative` | A relative interpolation (`${..x}`) has more leading dots than the value has parents. | Remove dots, or use an absolute path. |
| `interpolation::parent_reference` | An interpolation points at a container that encloses the value itself. | Point at a sibling or a child instead. |
| `interpolation::recursive` | Resolving the value leads back to the value itself. | Break the cycle; the label shows the value that refers back. |
| `interpolation::bad_key` | An interpolation used as a key segment (`${a.${b}}`) resolves to something other than a string or an integer. | Make the inner interpolation resolve to a key name or an index. |
| `interpolation::bad_resolver_name` | An interpolated resolver name resolves to something other than a string. | Make the name resolve to a string. |
| `interpolation::unknown_resolver` | `${name:...}` calls a resolver that is not registered. The help says where the known resolvers came from. | Fix the name, or configure the `resolvers.py` that defines it (`inventory.python-resolvers`). |
| `interpolation::resolver_failed` | A resolver returned an error, for example wrong arguments. | Fix the arguments as the message describes. |
| `interpolation::to_missing` | Internal: an interpolation reached a MISSING (`???`) value. The value becomes MISSING; the code is not shown to users. | None. |
| `interpolation::to_missing_via` | Internal: the same, reached through another interpolation. | None. |

## resolver

| Code | Trigger | Typical fix |
|---|---|---|
| `resolver::warning` | Warning. A resolver kept the render going and reported a problem: `oc.deprecated` was used, `write` found an origin that is not a mapping or a list, `from_file` found no file, `relpath` points at itself. | Act on the message; the value written is the reference's placeholder, such as `FILE NOT EXISTS`. |

## server

| Code | Trigger | Typical fix |
|---|---|---|
| `server::error` | The inventory server answered a request with an error that carries no diagnostic of its own. | Read `krab server logs`; `krab server stop` and a rerun start a fresh server. |
