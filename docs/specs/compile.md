# Compile

Status: As-built
Verified against: main @ a4fb9c4
Code: `crates/krab-compile/src/engine.rs`, `plan.rs`, `manifest.rs`, `digest.rs`, `native.rs` (the per-target item loop and kadet item reuse), `python.rs`, `pyenv.rs`, `worker.rs`, `crates/krab-compile/runner/kapitan_runner.py`, `crates/krab/src/cmd_compile.rs`

## Problem

`krab compile` turns rendered targets into files under `compiled/`, and the
files must be byte-identical to what `kapitan compile` 0.36.3 writes with the
omegaconf backend. `compiled/` is what repositories commit or deploy, so
any byte that differs shows up as a diff nobody asked for.

kapitan compiles every selected target on every run. krab compiles only the
targets whose inputs changed, which makes a no-op compile cheap and an edit
cost proportional to what it touches. That only works if invalidation is
exact: a target that is reported up to date while its output is stale is worse
than a slow compile, because nobody notices. The design rule is therefore
"exact invalidation or nothing": every input a compile reads is recorded with
a fingerprint, and anything krab cannot track (#16) is a known gap rather
than a guess.

Python is the other constraint. kadet components are Python code and need an
interpreter with `kadet` and whatever the component imports. Everything else
runs natively in Rust, so a repository without kadet items must compile
without Python, and a repository with them must get the same interpreter on
every machine without installing the Python kapitan. `--backend python` keeps
kapitan's own input types available for comparison and for input types that
are not native yet.

This spec covers the pipeline, target selection, where output lands, the
incremental build and its manifest, kadet evaluator isolation and the choice
of Python. What each input type writes, and how output files are formatted,
is in [inputs-and-output.md](inputs-and-output.md). Dependency fetching is in
[fetch.md](fetch.md), refs in [refs.md](refs.md), flags in
[../CLI.md](../CLI.md).

## Requirements

### Pipeline and backends

```
CMP-1  A compile MUST run in this order: render every target, select the
       candidates, fetch their dependencies, decide staleness per target
       from the manifest and digests alone, compile the stale targets,
       install each result, record it in the manifest.
       Test: crates/krab/tests/incremental.rs::a_partial_compile_does_not_hide_other_stale_targets
       Since: 58f8ddd

CMP-2  The default backend MUST be `native`: input types run in Rust and
       Python only evaluates kadet `main()`.
       Test: crates/krab/tests/compile_python.rs::a_copy_only_target_compiles_without_python
       Since: 1b9c9c5

CMP-3  `--backend python` MUST compile each stale target with kapitan's own
       input types in a Python worker process. Each compile thread keeps one
       worker for all its targets; a worker that dies is replaced and the
       target retried once.
       Test: none
       Since: 58f8ddd

CMP-4  Stale targets MUST compile concurrently on min(--parallelism, number
       of stale targets) threads, at least one. --parallelism defaults to the
       number of available CPUs.
       Test: none
       Since: 58f8ddd

CMP-5  A failing target MUST NOT stop the others. After all targets ran,
       the command MUST exit non-zero with "<n> target(s) failed to compile".
       Test: crates/krab/tests/incremental.rs::a_target_that_failed_fails_again
       Since: 58f8ddd

CMP-6  Dependencies declared by the candidates MUST be fetched before
       staleness is decided, so fetched files count as reads. A failed fetch
       MUST abort the compile before any target compiles. Details in
       fetch.md.
       Test: none
       Since: #62
```

### Target selection

```
CMP-7  Without -t and -l every rendered target MUST be a candidate. -t
       MUST restrict the candidates to the named targets; a name that is not
       a target MUST fail the compile with "target `<name>` not found".
       Test: none
       Since: 58f8ddd

CMP-8  -l k=v MUST keep the candidates whose `parameters.kapitan.labels`
       contain every given pair. A label not of the form k=v is an error. An
       empty candidate set MUST fail with "no matching targets".
       Test: none
       Since: 58f8ddd

CMP-9  Without -t, any target that fails to render MUST fail the compile
       before anything is compiled or fetched.
       Test: crates/krab/tests/compile_selection.rs::selected_targets_compile_when_others_fail_to_render
       Since: 58f8ddd

CMP-10 With -t, unselected targets that fail to render MUST be skipped with
       one warning naming up to three of them. A selected target that fails
       to render MUST fail the compile. This is deviation D9 in
       docs/DECISIONS.md.
       Test: crates/krab/tests/compile_selection.rs::selected_targets_compile_when_others_fail_to_render
       Since: #160

CMP-11 A target whose compile read a target that fails to render, or read
       the whole global inventory while any target fails to render, MUST
       fail, and its output MUST NOT be installed.
       Test: crates/krab/tests/compile_selection.rs::a_selected_target_reading_the_whole_global_inventory_fails
       Since: #160

CMP-12 Reading the whole global inventory MUST succeed when every target
       renders.
       Test: crates/krab/tests/compile_selection.rs::a_target_reading_the_whole_global_inventory_compiles_when_every_target_renders
       Since: #200

CMP-13 A target that fails to render MUST keep its directory under
       `compiled/`, also when that directory is nested under a target that
       compiles.
       Test: crates/krab/tests/compile_selection.rs::a_skipped_nested_target_keeps_its_output
       Since: #160
```

### Output placement

```
CMP-14 `compiled/` MUST be created under the output path: --output-path,
       else `compile.output-path` from `.kapitan` (with the `global`
       fallback of [INV-42](inventory.md)), else `.`. A relative output
       path is relative to the directory holding `.kapitan`.
       Test: none
       Since: 58f8ddd

CMP-15 Target `a.b.c` MUST compile into `compiled/a/b/c`.
       Test: crates/krab/tests/compile_selection.rs::a_skipped_nested_target_keeps_its_output
       Since: 58f8ddd

CMP-16 An item's `output_path` MUST be joined under the target directory
       with `.` components dropped, so `output_path: .` writes directly into
       `compiled/<target path>/`. A directory that cannot be created MUST be
       an item error ("cannot create <path>: <error>").
       Test: crates/krab/tests/output_path.rs::output_path_dot_is_the_target_directory
       Since: #157

CMP-17 Each stale target MUST compile into a private temporary tree. Only a
       successful compile MUST replace the contents of
       `compiled/<target path>`, leaving the directories of nested targets
       and the manifest file in place. A nested target's directory that this
       target's compile wrote MUST be merged into the existing one, not
       replace it.
       Test: crates/krab/tests/compile_selection.rs::a_skipped_nested_target_keeps_its_output
       Since: 58f8ddd

CMP-18 A target that fails MUST leave its previous output untouched.
       Test: none
       Since: 58f8ddd

CMP-19 A target's temporary tree MUST be removed when the target finishes,
       and the run's temporary root when the run finishes.
       Test: none
       Since: 58f8ddd

CMP-20 A run without -t and -l MUST remove every directory under
       `compiled/` that belongs to no target (and every file beside them
       other than the manifest), and drop the manifest records of targets
       that no longer exist.
       Test: none
       Since: 58f8ddd

CMP-21 Output an item writes outside `compiled/<target path>` MUST NOT be
       discarded silently.
       Test: none
       Since: not met yet (#133)
```

### Item failures

```
CMP-22 An item that fails MUST fail its target with
       "<input_type> [<input_paths>]: <error>", unless the item sets
       `continue_on_compile_error: true`; then the same text MUST be reported
       as a warning for the target and the remaining items MUST compile.
       Both backends behave this way.
       Test: none
       Since: 1b9c9c5
```

### Incremental builds

```
CMP-23 A candidate MUST be compiled when one of these holds, checked in this
       order, and the first that holds is its reason:
         no record                               "never compiled"
         record engine differs                   "compiler changed"
         document digest differs                 "inventory changed"
         settings digest differs                 "compile settings changed"
         a recorded path's fingerprint differs   "<path> appeared" | "<path> removed" | "<path> changed"
         a recorded target's digest differs      "inventory of target <name> changed (read by this target)"
         the recorded `*` digest differs         "inventory of another target changed (this target reads the global inventory)"
         compiled/<target path> is missing       "compiled output missing"
         the output tree digest differs          "compiled output was modified"
       Otherwise it MUST NOT be compiled and is reported "up to date".
       Test: crates/krab/tests/helm_input.rs::an_edited_chart_template_makes_the_target_stale
       Since: 58f8ddd

CMP-24 --force MUST compile every candidate (reason "forced") and disable
       kadet item reuse (CMP-34).
       Test: none
       Since: 58f8ddd

CMP-25 --dry-run MUST report each candidate as up to date or "would
       compile" with its reason, and MUST NOT compile, install or write the
       manifest.
       Test: none
       Since: 58f8ddd

CMP-26 Each target record MUST keep the fingerprint every path it read had
       when that target compiled, so compiling one target never makes
       another look current.
       Test: crates/krab/tests/incremental.rs::a_partial_compile_does_not_hide_other_stale_targets
       Since: #173

CMP-27 A target that fails MUST NOT write a record, so it stays stale and
       fails again on the next run.
       Test: crates/krab/tests/incremental.rs::a_target_that_failed_fails_again
       Since: #173

CMP-28 The recorded paths MUST include every file read and directory listed
       by the target's input types, every module its kadet components
       imported, every chart file and values file of a helm render, every
       ref file read or created, and for each input path the location in
       every search path (for a glob, the directory it matches in). Paths
       outside the repository root are not recorded. Fingerprints are taken
       after the compile, so a file the compile wrote (a new ref) is
       recorded as written.
       Test: crates/krab/tests/kadet_isolation.rs::a_full_compile_gives_each_target_its_own_evaluator
       Since: 58f8ddd

CMP-29 A fingerprint MUST be `f:<blake3 of the executable bit and the
       content>` for a file, `d:<blake3 of the sorted entry names, a
       directory's name ending in "/">` for a directory, and `-` for a path
       that does not exist or cannot be read.
       Test: none
       Since: 58f8ddd

CMP-30 Targets read through the global inventory MUST be recorded by name
       with their document digests, or as `*` with the digest of all
       document digests when the compile iterated every target. The target
       itself is not recorded.
       Test: none
       Since: 58f8ddd

CMP-31 The settings digest MUST cover the search paths, the --flag values,
       the output path, the backend and the native options (refs path,
       embed refs, reveal, indent, rapidyaml, null as empty, multiline
       style).
       Test: none
       Since: 58f8ddd

CMP-32 The output tree digest MUST cover every file under
       `compiled/<target path>` (relative path and fingerprint), excluding
       the directories of nested targets.
       Test: none
       Since: 58f8ddd

CMP-33 The engine identity MUST change whenever the compiler that produced
       the output changes: the krab version, the evaluator (native) or worker
       script (python) digest, and the Python-side versions. The native
       identity also names `evaluator-per-target`, so output written while
       targets shared evaluators compiles again. Format under Interfaces.
       Test: none
       Since: #183
```

### Kadet item reuse

```
CMP-34 Within a stale target on the native backend, a kadet item MUST be
       reused instead of evaluated when all of these hold: not --force; the
       target's previous record has the same engine and settings digest; it
       holds an item with the same item digest not yet matched in this
       compile (previous items are matched in order, each at most once);
       and that item's document reads, path fingerprints, target digests
       and output file fingerprints all still match. Reuse copies the
       previous output files into the new tree and carries the item's
       record forward.
       Test: crates/krab-compile/src/native.rs::tests::reuse_needs_every_input_unchanged
       Since: #61

CMP-35 Reuse MUST compare an item's paths against the fingerprints in that
       item's own record.
       Test: crates/krab-compile/src/native.rs::tests::reuse_compares_the_item_s_own_fingerprints
       Since: #173

CMP-36 A document read MUST be recorded as `parameters.<key>`, another
       top-level key, or `*` for the whole document, each with the digest of
       that part (`-` when absent).
       Test: crates/krab-compile/src/native.rs::tests::document_part_digests
       Since: #61
```

### Kadet evaluator isolation

```
CMP-37 On the native backend each target MUST get its own kadet evaluator
       process, started at the target's first evaluated kadet item and
       stopped when the target finishes. No evaluator serves two targets. A
       target whose kadet items are all reused starts none. This is
       deviation D13 in docs/DECISIONS.md.
       Test: crates/krab/tests/kadet_isolation.rs::a_full_compile_gives_each_target_its_own_evaluator
       Since: #183

CMP-38 An evaluator that dies MUST fail the item with "kadet evaluator
       died: <error>"; the target's next kadet item starts a new one.
       Test: none
       Since: #183
```

### Python

```
CMP-39 The native backend MUST need Python only when a target it is about
       to compile has a kadet item. Then the interpreter detection error is
       the compile's error and nothing is compiled.
       Test: crates/krab/tests/compile_python.rs::a_kadet_target_without_python_fails_with_the_detection_error
       Since: #181

CMP-40 Interpreter detection MUST run before planning, whether or not a
       kadet item will compile, so every selection of the same repository
       has the same engine identity.
       Test: none
       Since: #181

CMP-41 `--backend python` MUST fail before planning when no interpreter
       with kapitan is found.
       Test: none
       Since: 58f8ddd

CMP-42 For kadet evaluation, `--python` or `$KRAB_PYTHON` MUST be used as
       it is. If it cannot `import kadet, yaml`, the compile MUST fail with
       an error naming it; no other interpreter is tried.
       Test: crates/krab/tests/compile_python.rs::a_kadet_target_without_python_fails_with_the_detection_error
       Since: #91

CMP-43 Without one, kadet evaluation MUST use krab's managed environment: a
       venv under `$XDG_CACHE_HOME/krab/python/<digest>`, where the digest
       covers the base interpreter (`python3` on PATH, its path and version),
       the specifiers (CMP-44) and the requirements file's content. It is
       built with `uv` when on PATH, else `python3 -m venv` and pip, in a
       sibling directory renamed into place, and counts as complete only
       with its `.krab-env.json` marker.
       Test: none
       Since: #91

CMP-44 The managed environment MUST install `kadet` and `jinja2`, then the
       entries of `compile.python-requirements`, then the lines of
       `$KRAB_PYTHON_REQUIREMENTS`. A `kadet` or `jinja2` named in either
       list replaces the baseline entry. A single
       `compile.python-requirements` value that is an existing file or ends
       in `.txt` is a requirements file.
       Test: crates/krab-compile/src/pyenv.rs::tests::a_named_baseline_package_replaces_the_baseline_entry
       Since: #91

CMP-45 `--backend python` MUST try, in order, `--python`/`$KRAB_PYTHON`, a
       kapitan PEX on PATH run with PEX_INTERPRETER=1, and `python3`, and
       use the first that imports `kapitan.inputs.kadet`, `kadet` and
       `kapitan.cli`.
       Test: none
       Since: 58f8ddd

CMP-46 Probe results and Python-side versions MUST be cached per
       interpreter (command, file size and mtime) and backend need in
       `$XDG_CACHE_HOME/krab/python-probe.json`, so an interpreter is probed
       once, not on every compile.
       Test: none
       Since: 58f8ddd
CMP-50 Every request to a Python worker, kadet evaluator or resolver
       worker MUST have a deadline, 600 seconds unless `.kapitan`
       `compile.python-timeout` gives another number of seconds. On expiry
       the worker MUST be killed and the diagnostic MUST name the target
       and the component or resolver.
       Test: none
       Since: not met yet (#222)

CMP-51 When no interpreter is set explicitly, krab MUST try
       `$VIRTUAL_ENV`, `$CONDA_PREFIX`, `.venv` in the project directory or
       its nearest parent, a kapitan PEX on PATH, then `python3`. The
       interpreter chosen MUST be visible in the compile progress line,
       in `krab server status` and in the info log.
       Test: none
       Since: not met yet (#223)

CMP-52 Every Python runner MUST send its replies on a duplicate of file
       descriptor 1 and point descriptor 1 at stderr before it runs user
       code, so that writes to descriptor 1 from C extensions, `os.write`
       or child processes cannot corrupt the protocol.
       Test: none
       Since: not met yet (#221)

CMP-53 README.md MUST state the Python versions the runner scripts
       support, and CI MUST run the Python-dependent tests on the oldest
       and the newest of them.
       Test: none
       Since: not met yet (#224)
```

### Manifest

```
CMP-47 The manifest MUST be `compiled/.krab-manifest.json` with `version`
       3, saved by writing a temporary file and renaming it, after every
       installed target.
       Test: crates/krab/tests/incremental.rs::an_older_manifest_recompiles_everything_once
       Since: #173

CMP-48 A manifest whose `version` is not 3 MUST be discarded without a
       warning, whatever its shape, so every target compiles once. A
       manifest that is version 3 but does not parse MUST be discarded with
       a logged warning.
       Test: crates/krab/tests/incremental.rs::a_version_2_manifest_is_replaced_without_a_warning
       Since: #173

CMP-49 When no candidate is stale, the manifest MUST be written only if it
       is missing or its engine identity differs.
       Test: none
       Since: 58f8ddd
```

## Acceptance criteria

AC-1 (CMP-1, CMP-23, CMP-26). Given three targets that render one shared
template and a full compile, when the template changes, `compile -t t1` runs,
and then `compile` runs, then all three targets hold the new output.
Test: `crates/krab/tests/incremental.rs::a_partial_compile_does_not_hide_other_stale_targets`.

AC-2 (CMP-5, CMP-27). Given a template that fails for one of three targets,
when `compile` runs twice, then both runs exit non-zero.
Test: `crates/krab/tests/incremental.rs::a_target_that_failed_fails_again`.

AC-3 (CMP-47, CMP-48). Given a compiled repository whose manifest `version`
is set to 2, when `compile` runs twice, then the first run reports "3
compiled" and the second "0 compiled, 3 up to date". Given a manifest in the
version 2 shape, when `compile` runs, then it reports "3 compiled" and prints
no "unreadable" warning.
Tests: `crates/krab/tests/incremental.rs::an_older_manifest_recompiles_everything_once`,
`crates/krab/tests/incremental.rs::a_version_2_manifest_is_replaced_without_a_warning`.

AC-4 (CMP-9, CMP-10, CMP-11, CMP-12). Given targets `ok` and `bad`, where `bad`
fails to render, when `compile -t ok` runs, then `ok` compiles and stderr
names `bad`; `compile -t bad` and `compile` fail and write nothing. When `ok`
iterates `inventory_global`, `compile -t ok` fails mentioning the global
inventory and installs nothing; without `bad`, `compile` succeeds.
Tests: `crates/krab/tests/compile_selection.rs` (all five tests).

AC-5 (CMP-13, CMP-15, CMP-17). Given `ok` and nested `ok.child` compiled into
`compiled/ok/child/`, when `ok.child` stops rendering and `compile -t ok`
runs, then `compiled/ok/child/out/x.yml` still exists.
Test: `crates/krab/tests/compile_selection.rs::a_skipped_nested_target_keeps_its_output`.

AC-6 (CMP-16). Given a `copy` item with `output_path: .`, when `compile` runs,
then the file lands in `compiled/t1/`.
Test: `crates/krab/tests/output_path.rs::output_path_dot_is_the_target_directory`.

AC-7 (CMP-2, CMP-39, CMP-42). Given `KRAB_PYTHON=/nonexistent`, when a
copy-only target compiles, then it succeeds; when a kadet target compiles,
then the compile fails with "/nonexistent cannot evaluate kadet components"
and `compiled/t` does not exist.
Tests: `crates/krab/tests/compile_python.rs` (both tests).

AC-8 (CMP-28, CMP-37). Given targets `a` and `b` sharing a component whose
helper module reads `inventory()` at import time, when `compile -p 1` runs,
then each output names its own target and each record lists the helper
module as a dependency. Skips without a `python3` that imports `kadet`.
Test: `crates/krab/tests/kadet_isolation.rs::a_full_compile_gives_each_target_its_own_evaluator`.

AC-9 (CMP-23, CMP-28). Given the helm fixture compiled once, when `compile`
runs again, then it reports "0 compiled"; after one chart template is edited,
it reports "1 compiled" with the edit in the output. Skips without `helm`.
Test: `crates/krab/tests/helm_input.rs::an_edited_chart_template_makes_the_target_stale`.

## Edge cases

CMP-EC-1. The run is interrupted (Ctrl-C) while a target installs: the target
directory can be left half-written, and the temporary root under `$TMPDIR`
stays behind. The next run recompiles the target, because no record was
written and the output digest no longer matches. Open deviation #137.

CMP-EC-2. An item writes outside its target directory (`output_path: ../x`):
the file is dropped with the temporary tree and the compile exits 0. Open
deviation #133.

CMP-EC-3. Two targets write the same path inside `compiled/`: no conflict is
reported, and the target installed last wins (#133).

CMP-EC-4. The managed environment cannot be built (no `python3-venv`, no
network): the error names what was being installed and points at
`compile.python-requirements` and `KRAB_PYTHON`. A native compile without
kadet items still succeeds (CMP-39), but the build is retried, and its
progress line printed, on every compile.

CMP-EC-5. Two krab processes build the same managed environment: each builds
in its own temporary directory; the one that loses the rename uses the
complete environment the other installed.

CMP-EC-6. The version probe of the interpreter fails, or there is no Python:
the engine identity carries `unknown (<reason>)`. Once Python is available,
every target compiles again ("compiler changed").

CMP-EC-7. A `--backend python` worker dies mid-target: the target is retried
once on a new worker, then fails with the worker error.

CMP-EC-8. An input file changes while the compile runs: kadet reuse compares
against fingerprints memoised at the start of the run, and records are taken
after the compile (CMP-28), so the output can reflect the old content while
the record holds the new fingerprint. No issue tracks this.

CMP-EC-9. A kadet item reuses a helper module an earlier item of the same
target imported: it records no reads of the helper's document keys, and after
a change to such a key it is reused with stale output. Open deviation #184.

## Interfaces

### Manifest file

`compiled/.krab-manifest.json`, pretty-printed JSON:

| Field | Meaning |
|---|---|
| `version` | 3 |
| `engine` | engine identity of the last run |
| `targets.<name>.target_path` | `a/b/c` for `a.b.c` |
| `targets.<name>.doc_digest` | digest of the rendered document |
| `targets.<name>.config_digest` | settings digest (CMP-31) |
| `targets.<name>.engine` | engine identity that built this target |
| `targets.<name>.deps` | path relative to the repository root to fingerprint (CMP-29) |
| `targets.<name>.globals` | target name, or `*`, to document digest (CMP-30) |
| `targets.<name>.output_digest` | output tree digest (CMP-32) |
| `targets.<name>.compiled_at` | Unix seconds |
| `targets.<name>.duration_ms` | compile time |
| `targets.<name>.items[]` | kadet items in compile order, omitted when empty |
| `items[].item_digest` | digest of the item as written in the inventory |
| `items[].doc_reads` | document part to digest (CMP-36) |
| `items[].deps`, `items[].globals` | as for the target, for this item |
| `items[].outputs` | file written, relative to `compiled/`, to fingerprint |

`compiled_at`, `duration_ms` and the Python versions in `engine` change on
every compile, so the file is not reproducible (#15).

### Engine identity

```
native:  krab <version> native <evaluator digest> evaluator-per-target <versions>
python:  krab <version> runner <worker script digest> <versions>
```

Digests are the first 16 hex characters of a blake3 over the embedded
scripts. `<versions>` is `kadet <v> python <v>` (native),
`kapitan-py <v> python <v> rapidyaml <0|1>` (python), or `unknown (<reason>)`.

### Files and environment

| Name | Use |
|---|---|
| `$TMPDIR/krab-compile-<pid>-<n>/` | the run's temporary root; `<target>/compiled/<target path>` per target, `inventory.json` snapshot when no inventory server runs |
| `$XDG_CACHE_HOME/krab/` (`~/.cache/krab`) | managed environments (`python/`), probe cache, evaluator and worker scripts, helm renders |
| `KRAB_PYTHON` | interpreter for kadet evaluation or the Python backend, as `--python` |
| `KRAB_PYTHON_REQUIREMENTS` | per-machine specifiers for the managed environment, one per line |

The `.kapitan` keys a compile reads and the `--json` report are listed in
[../CLI.md](../CLI.md). The kadet evaluator and the Python worker
speak newline-delimited JSON over stdin and stdout; see the Compile section
of [../DESIGN.md](../DESIGN.md).

## Out of scope

* Removing `--backend python` once every input type is native (#17).
* Whether `compiled/.krab-manifest.json` belongs in version control (#15,
  undecided).
* The cost of a fully reused target (#60) and staging output inside
  `compiled/` (#74). Both are performance notes with no behaviour change.
* Compiling through the inventory server: the daemon serves documents but has
  no compile RPC.

## Open deviations

| Requirement | Issue | What `main` does |
|---|---|---|
| CMP-21 | #133 | Output written outside the target's directory is discarded with the temporary tree; the compile exits 0 |
| CMP-17, CMP-19 | #137 | Ctrl-C can leave a half-written target directory and always leaks the temporary root; no signal handling beyond SIGPIPE |
| CMP-29 | #152 | Only the executable bit of the mode is hashed, and a symlink is fingerprinted by its target's content, never by where it points |
| CMP-34 | #184 | A kadet item that reuses a module an earlier item imported records no reads of it, so it can be reused with stale output |
| CMP-23, CMP-28 | #16 | Reads that krab cannot observe are not tracked: binaries an `external` item runs, network access, the helm binary's version for `input_type: helm` |
| CMP-50 | #222 | `Worker::call_with` waits for the reply with a blocking `read_line` and no deadline, so a hanging component or resolver hangs `krab compile` and the daemon thread serving it |
| CMP-51 | #223 | The order is `$KRAB_PYTHON`, the flag or `.kapitan`, a kapitan PEX on PATH, then `python3` (RES-63, CMP-45); `VIRTUAL_ENV`, `CONDA_PREFIX` and a project `.venv` are never consulted |
| CMP-52 | #221 | The runners replace `sys.stdout` with `sys.stderr`, which covers `print()` but not writes to descriptor 1 from C code, `os.write(1, ...)` or child processes |
| CMP-53 | #224 | No document states the supported Python versions; CI tests 3.12 only |
