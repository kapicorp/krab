# Interpolation and resolvers

```
Status: As-built
Verified against: main @ a4fb9c4
Code: crates/krab-inventory/src/interp/, crates/krab-inventory/src/resolvers/, crates/krab-inventory/runner/resolver_runner.py, crates/krab-inventory/src/python.rs, crates/krab/src/app.rs (registry setup)
```

## Problem

After the classes of a target are merged ([inventory.md](inventory.md)), its
parameters still hold `${...}` strings: references to other values, relative
references, and calls to resolvers such as `${merge:...}` or user functions
from a repository's `resolvers.py`. kapitan's omegaconf backend evaluates them
with `OmegaConf.resolve()` twice and then `to_container(resolve=True)`, and
much of what inventories rely on is a consequence of that sequence rather
than a documented rule: a resolver that returns a string with `${` gets it
evaluated a pass later, an escaped `\${...}` survives one pass and is
evaluated where the value has been copied to, `${key:}` inside a copied block
names the key the block was written under, and a value that reaches `???`
becomes `???` itself.

krab has to produce the same document, so the evaluator reproduces the passes
and the grammar token for token (`parse.rs` is a port of OmegaConf's ANTLR
grammar) instead of implementing a cleaner model. The resolver set has three
native parts: OmegaConf's `oc.*`, kapitan's own resolvers, and `contrib`, a
set of general helpers. A fourth part is the repository's own `resolvers.py`,
run in Python workers because a native port of it cannot follow later edits
to the file.
Those workers must not leak Python tracebacks for failures that are really
evaluator failures, and must not deadlock when a Python resolver's lookup
evaluates another Python resolver.

## Requirements

### Grammar

```
RES-1  The parser MUST accept OmegaConf node interpolations: dotted paths
       (`${a.b}`), bracket indices (`${a.parts[1]}`), relative paths
       (`${.x}`, `${..x}`), surrounding whitespace (`${ a }`) and keys with
       dashes (`${foo-bar.baz}`), alone or embedded in text.
       Test: crates/krab-inventory/src/interp/parse.rs::tests::plain_and_node
       Since: 319ca84

RES-2  A key segment MAY itself be an interpolation (`${a.${b}.c}`); it MUST
       resolve to a string or an integer, else `interpolation::bad_key`.
       Test: crates/krab-inventory/src/interp/parse.rs::tests::nested_key
       Since: 319ca84

RES-3  Resolver arguments MUST be typed as OmegaConf types them: unquoted
       `3` and `-1` are ints, `1_000` is 1000, `.5`, `1.` and `inf` are
       floats, `null` is null, `true` is a bool, while `1-2`, `01`, `info`
       and `a b` stay strings; quoted strings, lists, dicts and nested
       interpolations are elements of their own; `\,` escapes a comma.
       Test: crates/krab-inventory/src/interp/parse.rs::tests::resolver_args
       Since: 319ca84

RES-4  `\${a}` MUST be the literal text `${a}`, `\\${a}` a backslash
       followed by the interpolation, and a `$` not followed by `{` plain
       text.
       Test: crates/krab-inventory/src/interp/parse.rs::tests::escapes
       Since: 319ca84

RES-5  An unterminated or empty interpolation MUST fail with
       `interpolation::syntax`, quoting the expression.
       Test: crates/krab-inventory/src/interp/parse.rs::tests::errors
       Since: 319ca84
```

### Evaluation

```
RES-6  The evaluator MUST run three passes over the tree, each visiting
       nodes in document order, evaluating every string that contains `${`
       and writing the result back. Results MUST be memoised within a pass
       only.
       Test: crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-7  An interpolation that aliases a container MUST resolve that
       container in place first and then copy it.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`config: ${defaults}`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-8  A resolver result MUST be written back verbatim, so a returned
       string containing `${` is evaluated again by a later read.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`added_default`, `values`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-9  A node whose evaluation left an interpolation in place (`${relpath:...}`
       gives `${.ns}`) MUST be evaluated where it now sits when it is read
       again in the same pass, not answered from the memo with the text.
       Test: tests/fixtures/inventory/targets/realias.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #175

RES-10 A deferred interpolation, written `\${...}`, MUST stay a string
       through the pass that unescapes it and be evaluated in a later pass
       at the place the value then occupies.
       Test: tests/fixtures/inventory/targets/copies.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-11 An interpolation embedded in text MUST be converted with Python
       `str()`: `None`, `True`, `False`, and floats as Python's `repr`.
       Test: crates/krab-inventory/src/value.rs::tests::float_repr_matches_python
       Since: 319ca84

RES-12 A relative path with one leading dot MUST start at the container of
       the node, each further dot one level up; going above the root MUST
       fail with `interpolation::bad_relative`.
       Test: tests/fixtures/inventory/classes/common.yml (`tier: ${..replicas}`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-13 A segment applied to a list MUST be an integer index, negative
       indices counting from the end; a non-integer segment MUST fail with
       `interpolation::bad_index`, and an index out of range MUST count as
       not found.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`part1`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-14 A path that does not exist MUST fail with
       `interpolation::key_not_found`, with help pointing at
       `${oc.select:key,default}`. A segment applied to a scalar MUST fail
       with `interpolation::not_a_container`; a segment applied to `null`
       after the first one MUST count as not found.
       Test: none
       Since: 319ca84

RES-15 An interpolation that refers back to itself, directly or through a
       chain, MUST fail with `interpolation::recursive`, labelling every
       node of the chain.
       Test: none
       Since: 319ca84

RES-16 An interpolation that points at a container enclosing the node MUST
       fail with `interpolation::parent_reference`.
       Test: none
       Since: 319ca84

RES-17 After the passes, `${escape:x}` results MUST become the literal text
       `${x}` and MUST NOT be evaluated.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`tf_ref`, `tf_expr`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84
```

### Where a value was written

```
RES-18 `${key:}`, `${parentkey:}` and `${fullkey:}` MUST answer for the
       place the value was written, following the copies that brought it to
       its current place (an alias, a `${merge:}` argument, a nested
       `${merge:}`), up to eight copies deep.
       Test: tests/fixtures/inventory/targets/copies.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #129

RES-19 Other resolvers and node interpolations MUST NOT use that origin: an
       ordinary interpolation, including a relative one produced by
       `${relpath:...}`, MUST resolve from where the value now sits.
       Test: tests/fixtures/inventory/targets/realias.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #129

RES-20 `${fullkey:}` MUST use OmegaConf's key format (`a.b[0].c`).
       Test: crates/krab-inventory/src/path.rs::tests::formats_paths
       Since: 319ca84
```

### MISSING values

```
RES-21 An interpolation that reaches a `???` value MUST make the whole
       value `???`: alone, embedded in text, as a key segment, as a resolver
       argument, through an intermediate node, and through another
       interpolation.
       Test: tests/fixtures/inventory/targets/missing.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #175

RES-22 `oc.select`, `access` and `write` MUST treat a node that already
       holds `???` as absent: `oc.select` gives its default or `null`,
       `access` fails as for a missing key, `write` gives `NOT FOUND`. A
       `???` reached through another interpolation on the way MUST still
       make the value `???`.
       Test: tests/fixtures/inventory/targets/missing.yml (`r_select`, `s_nested`, `s_alias`, `r_write`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #175

RES-23 An escaped `\${m}` and a string that merely contains `???` MUST NOT
       become MISSING.
       Test: tests/fixtures/inventory/targets/missing.yml (`r_escaped`, `r_literal`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #175

RES-24 `krab inventory explain` MUST show the expression of a value that
       became `???`.
       Test: crates/krab/tests/explain_missing.rs::explain_shows_where_a_missing_value_came_from
       Since: #175
```

The merge-time rule for `???` is INV-27 in [inventory.md](inventory.md).

### Resolver registry

```
RES-25 A resolver MUST be looked up by its full dotted name. An unknown
       name MUST fail with `interpolation::unknown_resolver`, with the known
       names and the origin of the non-native ones in the help text.
       Test: none
       Since: 319ca84

RES-26 A resolver that fails MUST produce `interpolation::resolver_failed`
       naming the resolver; a failure of a nested lookup MUST surface as
       the evaluator's own diagnostic instead.
       Test: crates/krab-inventory/tests/python_resolvers.rs::small_fixture_with_the_installed_omegaconf
       Since: 319ca84

RES-27 A resolver called with the wrong number of arguments MUST fail with
       `<name>() takes <n> argument(s) but <m> were given`.
       Test: none
       Since: 319ca84

RES-28 A resolver warning MUST be a non-fatal `resolver::warning`
       diagnostic attached to the target, naming the resolver and path.
       Test: none
       Since: 319ca84

RES-29 Boolean resolvers (`if`, `ifelse`, `and`, `or`, `not`) MUST use
       Python truthiness and `equal` Python equality (D6).
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`flag`, `choice`, `neg`, `both`, `either`, `same`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84
```

### Built-in resolvers

Every resolver in this table MUST be registered and MUST behave as stated.
The `Test` column names the fixture value in `tests/fixtures/inventory`
(run by `crates/krab-inventory/tests/fixture.rs::renders_like_the_reference`)
or says none.

| ID | Resolver | Behaviour | Test | Since |
|---|---|---|---|---|
| RES-30 | `oc.select:key[,default]` | resolved value at `key` (relative when it starts with `.`), else the default, else `null` | `region_default`, `maybe`, `empty_arg` | 319ca84 |
| RES-31 | `oc.env:name[,default]` | environment variable; a missing one gives the default as a string, `null` for a `null` default, else an error | none | 319ca84 |
| RES-32 | `oc.decode`, `oc.create`, `oc.deprecated` | parse a string as a grammar element; return the argument; warn and select the new key | none | 319ca84 |
| RES-33 | `oc.dict.keys:key`, `oc.dict.values:key` | keys of the mapping at `key`; a list of `${key.k}` interpolations, evaluated by a later read (RES-8) | `keys`, `values` | 319ca84 |
| RES-34 | `merge:a,b,...` | arguments MUST be containers; mappings merge, lists concatenate without deduplication | `merged` | 319ca84 |
| RES-35 | `dict:[...]` | a literal list of mappings becomes one mapping; a list read from the tree is returned unchanged | `as_dict` | 319ca84 |
| RES-36 | `list:x` | a mapping becomes a list of one-entry mappings, a string a list of characters, a list stays | `as_list` | 319ca84 |
| RES-37 | `default:a,b,...,fallback` | returns `${oc.select:a,${oc.select:b,...fallback}}`, evaluated by a later read | `added_default` | 319ca84 |
| RES-38 | `relpath:path` | the absolute path as a relative interpolation from the current node; the node's own path gives `SELF REFERENCE DETECTED` and a warning | `relpath` | 319ca84 |
| RES-39 | `add:a,b` | Python `+` for ints, floats, bools, strings and lists; other operands fail | `sum`, `concat` | 319ca84 |
| RES-40 | `yaml:key` | `yaml.dump` of the resolved value at `key`, stock PyYAML layout | none | 319ca84 |
| RES-41 | `access:k1,k2,...` | the value at the literal key chain, without splitting on dots; not found is an error | none | 319ca84 |
| RES-42 | `from_file:path` | file content, path relative to the working directory; a missing file gives `FILE NOT EXISTS` and a warning | none | 319ca84 |
| RES-43 | `filename`, `parent_filename`, `path`, `parent_path` | `null` | none | 319ca84 |
| RES-44 | `escape:x` | see RES-17 | `tf_ref`, `tf_expr` | 319ca84 |

### The write resolver

`${write:destination,origin}` follows kapitan's `write_to_key`.

```
RES-45 `write` MUST select `origin`. A missing or falsy origin MUST give
       `NOT FOUND`. A scalar origin, or an origin whose selection fails,
       MUST give `ERROR WHILE RESOLVING` and a warning.
       Test: tests/fixtures/inventory/classes/writes.yml, run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #162

RES-46 Otherwise `write` MUST merge the fully resolved origin into
       `destination` as `OmegaConf.update(..., merge=True, force_add=True)`
       does (mappings key by key, other values replace, `???` leaves an
       existing value, a scalar on the path becomes a mapping) and give
       `DONE`.
       Test: tests/fixtures/inventory/classes/writes.yml, tests/fixtures/inventory/targets/missing.yml (`r_write_keep`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #162

RES-47 A destination whose parents do not exist MUST be created (D10).
       Test: crates/krab-inventory/tests/write_resolver.rs::write_creates_a_missing_destination
       Since: #162

RES-48 Merging a list onto a mapping, or a mapping onto a list, MUST fail
       the render.
       Test: crates/krab-inventory/tests/write_resolver.rs::write_of_a_list_onto_a_mapping_fails
       Since: #162

RES-49 Values resolved earlier in the render MUST keep what they saw, and
       the memo MUST be cleared after a write. A result MUST be written back
       only while the node still holds the expression that produced it, so
       a write onto the calling node or one of its containers keeps the
       written value.
       Test: tests/fixtures/inventory/classes/writes.yml (`seen_by`, `edge`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: #162
```

### Contributed resolvers

```
RES-50 The `contrib` set MUST be registered for every inventory: `replace`,
       `json`, `to_yaml`, `sha256`, `truncate`, `to_csv`, `pluck`,
       `nested_dict_to_list_of_dicts`, `select_fields`, `filter_keys`,
       `join`, `join_quoted`.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`hostname`, `labels_json`, `labels_yaml`, `digest`, `short_hostname`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-51 `contrib` MUST hold general purpose helpers only. Resolvers that
       encode one repository's data shape or a cloud's naming belong in
       that repository's `resolvers.py`.
       Test: none
       Since: #154

RES-52 `sha256:value[,length]` MUST give the hex digest cut to `length`
       (default 16); a length of zero or less, or longer than the digest,
       MUST give the full digest.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`digest`, `digest_short`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84

RES-53 `truncate:value,length` MUST return a value of at most `length`
       characters unchanged, and otherwise its first `length - 5`
       characters, `-`, and the first four hex digits of its MD5.
       Test: tests/fixtures/inventory/classes/components/app/init.yml (`short_hostname`), run by crates/krab-inventory/tests/fixture.rs::renders_like_the_reference
       Since: 319ca84
```

### Python resolvers

```
RES-54 The `resolvers.py` MUST be `inventory.python-resolvers` from
       `.kapitan` (a path relative to the working directory) when set, else
       `<inventory>/resolvers.py`, else
       `<cwd>/system/omegaconf/resolvers/resolvers.py`. Without a file, or
       with `python-resolvers: false`, only native resolvers MUST be used.
       Test: crates/krab-inventory/src/dotkapitan.rs::tests::reads_python_resolvers_section
       Since: #77

RES-55 Every entry of the dict `pass_resolvers()` returns MUST be
       registered. A Python resolver MUST replace a native one of the same
       name unless `prefer-native` is set.
       Test: crates/krab-inventory/tests/python_resolvers.rs::prefer_native_keeps_the_rust_resolver
       Since: #77

RES-56 Arguments MUST reach the function as Python values of the types the
       grammar gave them. `_root_`, `_parent_` and `_node_` MUST be passed
       when the signature names them. `OmegaConf.select`,
       `to_container`, `is_config`, `is_dict` and `is_list` MUST accept
       `_root_` and `_parent_` and see fully resolved values; a missing key
       MUST select the default or `None`.
       Test: crates/krab-inventory/tests/python_resolvers.rs::small_fixture_with_the_installed_omegaconf
       Since: #77

RES-57 Without the `omegaconf` package the worker MUST provide a stand-in
       module with the same calls.
       Test: crates/krab-inventory/tests/python_resolvers.rs::small_fixture_with_the_stand_in_omegaconf
       Since: #77

RES-58 A Python exception MUST become `interpolation::resolver_failed`
       with the exception type and message and without the traceback. When
       a `_root_` lookup failed first, that evaluator diagnostic MUST be
       reported instead.
       Test: crates/krab-inventory/tests/python_resolvers.rs::small_fixture_with_the_installed_omegaconf
       Since: #77

RES-59 The fixture inventory rendered through its own `resolvers.py`, with
       every `contrib` name answered by Python, MUST match the reference
       output byte for byte.
       Test: crates/krab-inventory/tests/python_resolvers.rs::fixture_inventory_matches_the_reference_through_python
       Since: #77

RES-60 What the file defines MUST be cached by the digest of its content,
       the interpreter and the worker script under
       `$XDG_CACHE_HOME/krab/resolvers` (else `~/.cache/krab/resolvers`),
       so no worker starts until a Python resolver is called. A cached
       entry whose recorded project module no longer exists MUST be
       ignored.
       Test: none
       Since: #77

RES-61 Workers MUST start on demand up to `workers` (default: CPUs, capped
       at 8). A call made while the same thread holds a worker MUST start a
       new worker rather than wait. A call that waits for a free worker for
       60 seconds MUST fail.
       Test: crates/krab-inventory/tests/python_resolvers.rs::nested_python_calls_do_not_deadlock
       Since: #86

RES-62 A worker that dies during a call MUST be replaced and the call
       retried once; a second death MUST fail the call.
       Test: none
       Since: #77

RES-63 The interpreter MUST be `$KRAB_PYTHON`, else
       `inventory.python-resolvers.python`, else a kapitan PEX on `PATH`,
       else `python3`. A configured interpreter that is not an executable
       MUST fail with `inventory::python_resolvers` before any target
       renders.
       Test: none
       Since: #86

RES-64 The registry MUST record `resolvers.py`, the project modules it
       imported and `.kapitan` as its sources, so the daemon restarts when
       one changes ([daemon.md](daemon.md)).
       Test: crates/krab-server/tests/restart.rs::a_changed_configuration_source_stops_the_server
       Since: #77, #155
```

### Not met yet

These requirements describe the behaviour krab is meant to have. `main` does
not meet them yet; the open deviations below say what it does instead.

```
RES-65 Native `truncate` with a `length` below 5 MUST fail the target
       with a diagnostic saying that the Python original returns a value
       longer than `length` there; lengths of 5 and more follow RES-53.
       Test: none
       Since: not met yet (#165)

RES-66 An unquoted dotted path with a numeric segment in a resolver
       argument (`${oc.select:l.1.z}`) MUST select the list element, as
       the reference does.
       Test: none
       Since: not met yet (#167)

RES-67 A `resolvers.py` that cannot be imported, or lacks
       `pass_resolvers()`, MUST produce a diagnostic carrying the real
       exception while every target renders with the built-in resolvers,
       so that a target fails only where it calls a resolver that is
       missing.
       Test: none
       Since: not met yet (#121)

RES-68 A `contrib` resolver used without `.kapitan`
       `inventory.contrib-resolvers: true` MUST produce the warning "krab
       extension; kapitan 0.36.3 fails with Unsupported interpolation
       type" and leave the rendered value unchanged. With the key set it
       MUST NOT warn.
       Test: none
       Since: not met yet (#122)

RES-69 `OmegaConf.update(_root_, ...)` in a Python resolver MUST update
       the tree, as it does on the real config root.
       Test: none
       Since: not met yet (#117)
```

## Acceptance criteria

1. Given the fixture inventory and its expected files, when it renders with
   the native registry, then every target matches the reference output.
   Covers RES-6 to RES-13, RES-17 to RES-23, RES-29, RES-30, RES-33 to
   RES-39, RES-45, RES-46, RES-49, RES-50, RES-52, RES-53. Test:
   `crates/krab-inventory/tests/fixture.rs::renders_like_the_reference`.
2. Given the same inventory with `tests/fixtures/inventory/resolvers.py`
   registered through Python, when it renders, then the output is the same.
   Covers RES-55, RES-56, RES-59. Test:
   `crates/krab-inventory/tests/python_resolvers.rs::fixture_inventory_matches_the_reference_through_python`.
3. Given `tests/fixtures/python`, when the target `py` renders, then
   literal types, varargs and defaults, `_parent_`, `_root_` selects of a
   nested interpolation, `_node_`, `to_container` and select defaults give
   the values OmegaConf gives; `err` fails with `ValueError: boom` naming
   `fail`, and `nested_err` reports the unset environment variable without a
   traceback. Covers RES-26, RES-56 to RES-58. Tests:
   `python_resolvers.rs::small_fixture_with_the_installed_omegaconf`,
   `python_resolvers.rs::small_fixture_with_the_stand_in_omegaconf`.
4. Given a pool of one worker and a Python resolver whose `_root_` lookup
   evaluates another Python resolver, when the target renders, then it
   finishes within 90 seconds with the inner result. Covers RES-61. Test:
   `python_resolvers.rs::nested_python_calls_do_not_deadlock`.
5. Given a block written in a class with `\${parentkey:}`, `\${key:}` and
   `\${fullkey:}`, copied by an alias, a `${merge:}` and a nested
   `${merge:}`, when the target renders, then each copy names the class's
   keys. Covers RES-10, RES-18. Test: fixture target `copies` in
   `renders_like_the_reference`.

## Edge cases

* RES-EC-1: an unknown resolver, a missing key, a cycle and a reference to an
  enclosing container fail the target with the codes of RES-25, RES-14,
  RES-15 and RES-16; other targets still render (INV-38).
* RES-EC-2: `oc.env` of an unset variable without a default fails the target
  with `interpolation::resolver_failed`.
* RES-EC-3: `write` into a list index that does not exist fails the target
  with `cannot write to <dest>: list index <i> out of range`.
* RES-EC-4: no usable Python interpreter while a `resolvers.py` is
  configured or discovered: every command that loads the inventory fails
  with `inventory::python_resolvers` and help naming `KRAB_PYTHON`.
* RES-EC-5: a `resolvers.py` that cannot be imported, or has no
  `pass_resolvers()`, fails the whole inventory with
  `inventory::python_resolvers` (open deviation RES-67).
* RES-EC-6: a worker that dies mid-call is retried once (RES-62); a pool
  that stays busy for 60 seconds fails the call (RES-61).

## Interfaces

`.kapitan` `inventory.python-resolvers` is a path, `false`, or a mapping:

| Key | Meaning |
|---|---|
| `file` (or `path`) | the `resolvers.py`, relative to the working directory |
| `python` | interpreter command; `$KRAB_PYTHON` overrides it |
| `prefer-native` | keep native resolvers over same-named Python ones (default `false`) |
| `workers` | upper bound on worker processes (positive integer) |
| `enabled` | `false` disables Python resolvers |

The worker protocol between `python.rs` and `runner/resolver_runner.py` is
newline-delimited JSON: `init` (file, cwd) answered with the resolver names
and the special arguments each wants, `call` (name, args, argument kinds,
node keys) answered with a value or an error, and, while a call runs,
`select` requests from the worker that the host answers. The module
docstring of `resolver_runner.py` is the reference for the message shapes.

Native resolvers are Rust functions `Fn(&mut Ctx, &[Value]) ->
ResolverResult` registered by name with `Registry::register`; `Ctx` gives
the node path, the anchored key, parent key and full key, `select`,
`decode`, `write` and `warn`. README.md, "Writing a resolver", shows an
example.

Diagnostic codes of this area: `interpolation::syntax`, `bad_key`,
`bad_resolver_name`, `bad_relative`, `bad_index`, `key_not_found`,
`not_a_container`, `recursive`, `parent_reference`, `unknown_resolver`,
`resolver_failed` (all prefixed `interpolation::`), `resolver::warning` and
`inventory::python_resolvers`. The codes `interpolation::to_missing` and
`interpolation::to_missing_via` are internal and never reach the user.

## Out of scope

* Resolvers that encode one repository's data or a cloud's naming in
  `contrib` (RES-51); they run from that repository's `resolvers.py`.
* A stricter boolean mode for `if` and friends. D6 keeps Python truthiness;
  a strict mode would be an opt-in.

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| RES-65 | #165 | The kept prefix is clamped to empty (`contrib.rs:89`): `truncate:abcdef,3` gives `-e80b` without a diagnostic |
| RES-66 | #167 | `.1` is parsed as a float and the argument becomes `l0.1.z`, which is not found; `write` destinations are affected the same way |
| RES-67 | #121 | The whole inventory fails with `inventory::python_resolvers` |
| RES-68 | #122 | The `contrib` set (RES-50) is always registered without notice |
| RES-69 | #117 | `_root_` is a proxy that `OmegaConf.update` rejects; `OmegaConf.select` works |
