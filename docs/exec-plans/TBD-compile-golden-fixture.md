# ExecPlan: `krab compile` has an end-to-end regression test

Status: Draft, awaiting review
Issue: none yet. Proposed. **Gated on decision D5** in
`docs/krab-architecture-assessment-and-implementation-plan.md`: is a
compile-level golden fixture wanted at all, or is parity against real
inventories (the method used in the audit, issue #128) the intended oracle?
Suggested title: "tests: a compiled fixture and a test that calls `compile()`".
Suggested labels: `area: compile`.
Baseline: `6e9e55ba18597b34ab056cfe29cb28698fa97993`

File name carries `TBD-` until an issue exists; rename to `<issue>-compile-golden-fixture.md` then.

## Purpose and acceptance criteria

`krab_compile::compile()` is never called by any test. `engine.rs` is 1007 lines
with zero `#[cfg(test)]`. `manifest.rs`, `digest.rs`, `plan.rs`, `docs.rs`,
`worker.rs`, `inputs/copy.rs`, `inputs/external.rs`, `inputs/remove.rs` and
`inputs/jinja.rs` have no tests at all. The closest thing that exists is
`native.rs`'s `reusable`, which tests the kadet item-reuse predicate in
isolation, not target-level staleness and not the manifest round trip.

Four findings from the assessment are defects a compile-level test would have
caught:

| | Defect |
|---|---|
| F1 | An `external` input records no reads, so changing the data its script reads leaves the compiled output stale while the run reports "up to date" |
| F2 | File fingerprints hash content and the executable bit only; a `0644` to `0640` change does not invalidate |
| F3 | Symlinks are fingerprinted by the content they resolve to, never by the link target path |
| F17 | Output whose `output_path` escapes the target directory is silently discarded; krab reports success and writes nothing where the reference writes the file |

PR #97 adds reference parity for *inventory* rendering and explicitly defers
this: *"The fixture inventory has a `kadet` compile input, but there is no
committed compiled snapshot to diff against, so that needs a fixture of its
own."* This plan is that fixture.

The outcome is a committed compiled output tree, a test that runs
`krab_compile::compile()` against it and diffs byte for byte, and coverage of
the staleness decision's distinct reasons.

Acceptance criteria, all observable:

1. A test calls `krab_compile::compile()` through a `DocSource` and compares the
   produced tree to a committed one, byte for byte.
2. The comparison covers file *contents*, the set of paths, and the absence of
   paths that should not exist. A missing file must fail, not pass quietly.
3. Each distinct reason in `why_stale` has a test that reaches it, and each
   fails if that arm stops firing.
4. A test asserts a target is **not** recompiled when nothing changed. The
   up-to-date path is as much a contract as the stale path.
5. The suite runs with no Python, no network, no external binary, and does not
   skip.
6. F17's reproduction is encoded: a target with `output_path: ../shared`
   produces a diagnostic, not a silent success. (The fix itself is M4 of the
   assessment; this plan only needs the case to be expressible.)

## Context and constraints

### Code paths

* `crates/krab-compile/src/engine.rs`: `compile()`, `why_stale`, `install`,
  `merge_dirs`, `cleanup`, `child_names`, `install_and_record`.
* `crates/krab-compile/src/plan.rs`: `TargetPlan::new`, `probes` derivation,
  glob-parent handling, `matches_labels`.
* `crates/krab-compile/src/manifest.rs`: `Manifest`, `TargetRecord`,
  `ItemRecord`, `load`, `save`, `prune_files`.
* `crates/krab-compile/src/digest.rs`: `Fingerprint`, `compute`, `tree`.
* `crates/krab-compile/src/docs.rs`: the `DocProvider` trait the test must
  implement.
* `crates/krab-compile/src/engine.rs`: the `DocSource` trait, likewise.

### Contracts to reuse rather than reinvent

* **`DocSource` and `DocProvider` are small and already exist.** Implement them
  in the test. Do not add a mock crate, and do not widen the traits to make
  testing easier without saying why in Discoveries.
* **`tests/fixtures/` already holds the inventory fixture** and
  `generate_expected.py`. Follow its shape: a small inventory, a committed
  expected output, a regeneration path.
* `crates/krab-inventory/tests/fixture.rs` is the model for a byte-for-byte
  golden comparison.

### Evidence already gathered, so it is not redone

During the assessment I ran `kapitan compile` and `krab compile` against a
synthetic inventory with a `jinja2` and a `copy` input. The output trees were
**identical**, with one difference: krab also writes
`compiled/.krab-manifest.json`, which the reference does not.

Two consequences for the fixture:

1. A compiled golden tree is achievable for the native input types; this is not
   speculative.
2. **The golden comparison must exclude `.krab-manifest.json`**, or it will
   compare a file containing absolute paths, timestamps (`compiled_at`) and
   durations (`duration_ms`). Excluding it is correct; asserting on it
   separately, with those fields ignored, is a possible second test.

### Compatibility obligations

* The committed tree must be what the **reference** produces, not merely what
  krab currently produces. Otherwise the test locks in current behaviour and
  stops being an oracle. Generate it with the pinned reference environment, as
  `generate_expected.py` does for the inventory fixture:
  `kapitan[omegaconf]==0.36.3` with `omegaconf==2.4.0.dev3` or `.dev4`.
* An existing test demonstrates behaviour; it does not establish the
  specification. Where krab and the reference differ on purpose, the row belongs
  in `docs/DECISIONS.md` and the fixture should encode the *chosen* behaviour
  with a comment pointing at the row.

### Exclusions, deliberate

* **`kadet`.** It needs a Python interpreter with `kadet` and `jinja2`, and
  gating on that is exactly how `kadet_runner.rs`'s two tests became silent
  skips. Kadet coverage stays in `kadet_runner.rs`.
* **`helm` input and dependency fetching.** They need external binaries and
  network.
* **Refs backends.** Covered by the secrets-backend plan.
* **Any production code change.** This plan adds tests and fixtures. F1, F2, F3
  and F17 are *fixed* in M3 and M4 of the assessment; this plan only makes them
  expressible and, where they are current behaviour, documents what the test
  currently asserts.
* Concurrency: nothing here exercises the multi-worker path or the manifest
  mutex under contention. Worth doing, not here.

### Prerequisites

* **D5.** If the answer is "real inventories are the oracle", this plan should
  not be executed as written and the effort belongs in the #128 follow-up
  instead.
* **PR #97 merged**, so the reference environment is pinned. Building a golden
  tree against an unpinned `omegaconf` reproduces the exact failure #97 fixes.
* **PR #75 resolved.** It moves compile staging from `TMPDIR` into `compiled/`,
  which changes what a crashed run leaves behind and what the output tree
  contains. Review recommends closing it; either way, do not build the fixture
  while it is undecided.
* Check at start that no open PR touches `crates/krab-compile/src/engine.rs`.

## Milestones and work

### Milestone 1: the smallest thing that calls `compile()`

Reviewable outcome: a test that constructs a `DocSource` over one target with
one `copy` input, runs `compile()`, and asserts one output file exists with the
right bytes.

This is the milestone that decides whether the rest is cheap or expensive. The
uncertainty to resolve first: **how much scaffolding does `compile()` need?** It
takes settings, search paths, an output path, a `DocSource`, a `DocProvider`,
worker counts and `NativeOptions`. If constructing that in a test turns out to
need more than a screenful, say so in Discoveries. That is a finding about
`engine.rs`'s shape, and it is the evidence that would justify splitting it
(which the assessment otherwise recommends against on size grounds alone).

Do not proceed to Milestone 2 until one target compiles from a test.

### Milestone 2: the fixture inventory and its committed tree

Reviewable outcome: `tests/fixtures/compile/` with an inventory exercising
`jinja2`, `copy` and `remove`, a committed `expected/` tree generated by the
reference, and a regeneration script beside `generate_expected.py`.

Design constraints, learned from the existing fixtures and from the review of
#97:

* **Small enough to read in a diff.** When this test fails, someone has to
  understand why from the diff alone.
* **Deterministic.** No timestamps, no random, no absolute paths, no ordering
  that depends on the filesystem.
* **The regeneration path must delete before writing.** The review of #97 found
  that `generate_expected.py` writes but never removes, so dropping a target
  leaves a stale expected file, nothing stages, and the parity job passes green.
  Do not reproduce that bug here: regenerate into a clean directory, and assert
  the file count.
* Include at least one nested target, because `install`'s `child_names` handling
  and `cleanup`'s orphan removal are among the least obvious code in the engine.

`external` is a judgement call: it exercises the code path behind F1, but it
shells out. A script restricted to shell builtins is probably acceptable; decide
and record. If it makes the suite non-hermetic, leave it out and cover F1 in the
staleness tests instead.

### Milestone 3: the staleness decision

Reviewable outcome: a table-driven test over `why_stale`'s distinct reasons:
never compiled, engine identity changed, document digest changed, config digest
changed, a recorded dependency changed, a global changed, output directory
missing, output tree modified, plus the negative case where nothing changed.

I verified five of these by hand against a real binary during the assessment and
they behave as documented; this milestone turns that into something that stays
true. The sixth case I tested, the `external` input, is F1 and does not
behave as documented. Encode the current behaviour with a comment naming F1 and
the issue, so the test does not silently bless it.

### Milestone 4: the output tree

Reviewable outcome: tests for `install` replacing a tree while leaving nested
targets alone, `cleanup` removing directories no target claims, and the F17 case
(`output_path` escaping the target directory).

For F17 the fix is M4 of the assessment, not this plan. Write the test to assert
**current** behaviour with a comment naming F17, then flip the assertion in the
fix PR. That way the fix PR shows the behaviour change in its own diff, which is
the point.

## Validation and recovery

### Commands

```sh
cargo test -p krab-compile              # new tests must appear and must not skip
cargo test --locked --no-fail-fast
cargo clippy --all-targets --locked
cargo fmt --all --check
```

Regeneration, run by a human when the reference version changes, not by CI on
every run:

```sh
# in a venv pinned exactly as PR #97 pins CI
rm -rf tests/fixtures/compile/expected
python tests/fixtures/compile/generate_expected.py
git add -A tests/fixtures/compile/expected && git diff --cached --exit-code
```

Required environment for the *test*: a Rust toolchain only. Required for
*regeneration*: the pinned reference. Keep those separate: the test must
not need Python.

### Expected results

* The test fails if any byte of the compiled tree changes.
* The test fails if a file disappears from the tree, not only if one changes.
  Assert the path set, not just the contents of paths that exist.
* Every `why_stale` arm has a case that reaches it.

### Coverage check

Assert the number of fixture targets and the number of expected files, and fail
on a mismatch. This is the specific defect the review found in #97's job, and
this plan must not repeat it. A green run that compared nothing is the failure
mode this whole milestone exists to prevent.

### Recovery

Test and fixture only; revert deletes two directories and one test file. No
production effect, no user-visible change, nothing to roll back in the field.

The one irreversible cost is reviewer attention: a committed golden tree is a
file everyone has to regenerate when behaviour legitimately changes. Keep it
small enough that regenerating is cheap, and document the regeneration command
where a contributor will find it.

### Fallback

If Milestone 1 shows `compile()` cannot be driven from a test without
restructuring `engine.rs`, **stop and report that**. It converts this plan into
a refactor with a different risk profile, and it is a genuine finding that
changes the assessment's recommendation against splitting `engine.rs`. Do not
quietly do the refactor inside this plan.

## Progress

- [ ] D5 answered
- [ ] PR #97 merged; PR #75 resolved
- [ ] M1: one target compiles from a test
- [ ] M1: record how much scaffolding `compile()` needed
- [ ] M2: fixture inventory with jinja2, copy, remove, and a nested target
- [ ] M2: decide whether `external` belongs in the fixture; record it
- [ ] M2: regeneration script that deletes before writing
- [ ] M2: committed `expected/` tree generated by the pinned reference
- [ ] M2: `.krab-manifest.json` excluded from the comparison
- [ ] M3: table-driven `why_stale` coverage, including the negative case
- [ ] M3: F1's current behaviour encoded with a comment naming it
- [ ] M4: `install` with a nested target, `cleanup`, and the F17 case
- [ ] Path-set assertion, not only content comparison
- [ ] Fixture and expected-file counts asserted
- [ ] Full suite, clippy, fmt clean

## Discoveries and decisions

*Nothing recorded yet. This plan has not been executed.*

What is already known is in Purpose and Context above; it is not repeated
here.

Record here: how much scaffolding `compile()` needed; whether `external` went
into the fixture; anything that made the golden tree non-deterministic.

## Outcomes and handoff

*Not started. Nothing to record.*

On completion, record: the exact test names and their output; how many
`why_stale` arms are covered and which are not; the reference version the tree
was generated with; and the remaining gaps, which at minimum include kadet,
helm, fetching, refs and the multi-worker path.

This plan is a prerequisite for M3 (fixing F1) and M4 (fixing F17) of the
assessment, which both need somewhere to write their regression case.
