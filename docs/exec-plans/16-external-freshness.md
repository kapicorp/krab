# ExecPlan: `external` inputs no longer serve stale output

Status: Draft, **blocked on decision D3**
Issue: [#16](https://github.com/kapicorp/krab/issues/16), "Untracked reads:
external binaries and network fetches". The issue names both candidate
resolutions and picks neither; D3 in
`docs/krab-architecture-assessment-and-implementation-plan.md` is that choice.
Baseline: `6e9e55ba18597b34ab056cfe29cb28698fa97993`

## Purpose and acceptance criteria

### The defect (F1), reproduced end to end

An `external` input runs a command through `sh -c`. The compile records **no**
reads for it, so nothing the command touches becomes a dependency. Reproduced
during the assessment with a script that copies `source-of-truth.txt` into the
output:

```
krab compile                  -> compiled output contains VERSION-ONE
echo VERSION-TWO > source-of-truth.txt
krab compile --explain        -> "up to date ext"
                                 compiled output STILL contains VERSION-ONE
```

The reference recompiles every run, so its output always tracks the file:

```
kapitan compile               -> VERSION-TWO
echo VERSION-THREE > ...
kapitan compile               -> VERSION-THREE
```

This contradicts the stated principle in `docs/DESIGN.md`, *"exact invalidation
or nothing"*. For `external` it is neither: the manifest claims a dependency set
it never collected.

It is also invisible to the project's own parity check.
`CONTRIBUTING.md` prescribes `krab compile --force && git status --short
compiled`, and `--force` recompiles, so the stale path is never exercised.

### Why it happens, structurally

The dispatch arms in `crates/krab-compile/src/native.rs` are not symmetric:

```rust
"copy"     => copy::compile(&input, target_compile_path, item.ignore_missing, reads)?,
"external" => external::compile(&input, target_compile_path, &item.raw)?,
```

`external::compile` has no `reads` parameter. This is not a case of an
implementation forgetting to call `reads.file(...)`. The signature makes
it impossible. Nothing in the type system or the review process would have caught
it, which is the second half of what this plan fixes.

### Outcome

Two parts, and the second matters more than the first:

1. **F1 closed.** After the data an `external` command reads changes, the next
   `krab compile` produces output matching the reference.
2. **A second F1 becomes impossible to introduce.** Every input type declares
   how it is invalidated, as a mandatory field, so an input that cannot report
   its reads has to say so rather than silently reporting none.

### Acceptance criteria

Common to both options:

1. The reproduction above produces current output after the change, and the
   test encoding it fails on the baseline commit.
2. Every input type (`kadet`, `jinja2`, `copy`, `remove`, `external`) carries an
   explicit freshness declaration. Adding a new input type without one does not
   compile.
3. `krab compile --explain` states the reason for an always-stale target in
   words a user can act on, rather than showing it as an ordinary change.
4. No target that is correctly up to date today becomes stale, except those
   using `external`.

Option A only (always stale):

5. A target with an `external` item recompiles on every run, matching the
   reference.

Option B only (declared inputs):

5. Declared `input_paths` for an `external` item are recorded as dependencies
   and invalidate normally.
6. An `external` item with no declaration is **still** always stale. Silence
   must not mean "tracked", or the defect returns for anyone who does not
   declare.
7. A `docs/DECISIONS.md` row records the new key, because the reference has no
   equivalent.

## Context and constraints

### The decision that blocks this (D3)

Issue #16 states it as *"Record their environment and inputs, or mark such
targets as always stale."*

| | Option A: always stale | Option B: declared inputs |
|---|---|---|
| Behaviour | Matches the reference exactly | Diverges: adds a key the reference does not have |
| Cost | Small; a branch in the staleness decision | Larger; schema, parsing, docs, a `DECISIONS.md` row |
| Loses | Incrementality for every target using `external` | Nothing, for those who declare |
| Risk | A slow `external` item now runs every compile | Under-declaration silently reintroduces F1 |
| Reversible | Yes, cleanly | No: removing a published key is a breaking change |

**Recommendation, for the decision maker rather than the implementer:** A,
unless there is evidence that a real inventory has an `external` item slow
enough to matter. It is correct by construction, matches the reference, and is
revertible. B can be added later on top of A without a second migration; A
cannot be retrofitted under B without breaking people who declared.

**Evidence gathered since, and it points at A.** `kapitan/inputs/base.py`
defines `cacheable() -> False` and an abstract `inputs_hash`, and **only
`kadet` overrides them**. `cuelang`, `kustomize`, `jsonnet`, `helm`, `external`,
`jinja2` and `copy` are all non-cacheable in the reference. Caching is opt-in
there, and only a type that can report what it read may opt in. The `Freshness`
field below is that same rule, made mandatory rather than defaulted, and it
extends to cue and kustomize whenever krab implements them natively.

**Checked, and it points the same way.** Of the six corpora in issue #128,
exactly one uses `external`: corpus D, "5 targets, kadet + jinja2 + copy +
external + jsonnet". D is also the repository where krab is already a drop-in,
with 39 of 39 compiled files byte-identical. Its full compile is recorded at
6.4 s for the reference against 2.2 s for krab, of which 0.6 s is real work. An
`external` target rebuilding every run therefore costs a fraction of 0.6 s in
the only audited inventory that would be affected.

Not established, and a five-minute check for someone with access to D: how many
of its 5 targets use `external`, and whether the script itself is expensive. A
`grep -r 'input_type: external'` plus per-target timings from `krab compile
--force` answers both. An inventory outside the audit could also use it more
heavily.

**Decision status: deliberately left open.** The evidence favours Option A and
no decision has been taken.

### Scope of issue #16 versus this plan

The issue title says *"external binaries **and network fetches**"*. This plan
covers the `external` half only. The fetch half is a distinct mechanism:
dependency fetching runs **before** the staleness decision so that fetched files
become ordinary inputs, and `FetchOutcome`s never enter the manifest by design.
Whether a fetched-but-unchanged remote should invalidate is a separate question
with a separate answer, and bundling it here would make both harder to review.

Say so on #16 when this lands, so the issue is not closed with half of it
unaddressed.

### Code paths

* `crates/krab-compile/src/native.rs`: the `match item.input_type` dispatch,
  all five arms.
* `crates/krab-compile/src/inputs/mod.rs`: `Item`, `Item::from_json`, and the
  `Reads` accumulator.
* `crates/krab-compile/src/inputs/external.rs`: `compile`, which today takes
  no `reads`.
* `crates/krab-compile/src/engine.rs`: `why_stale`, where an always-stale
  declaration has to take effect, and `--explain`'s reason strings.
* `crates/krab-compile/src/manifest.rs`: `TargetRecord`, `ItemRecord`.

### Contracts to reuse rather than reinvent

* **`Reads` already exists and is already threaded to most inputs.** It is the
  contract; it does not need replacing. Give `external::compile` the parameter
  the other arms already have.
* `why_stale` already returns a human reason per target; add a variant, do not
  build a parallel mechanism.
* `docs/DECISIONS.md` is the home for a deliberate divergence, one row, in the
  same PR. Arriving with PR #96.

### Explicitly not a trait

The assessment considered introducing an input-type trait here and rejected it,
on evidence: the reference implementation **has** one, a 443-line `InputType`
abstract base class, and extensibility still fell short, because `inputs_hash`
and `cacheable` are optional and the surrounding globals leak around the
interface. A trait would not have prevented F1; `external` would simply not call
`reads.file(...)`, exactly as it does now.

What prevents F1 is a **mandatory declaration**, not an interface. Keep the
`match`. If this plan starts growing a trait, stop and re-read this paragraph.

### Compatibility obligations

* Option A restores reference behaviour, so it needs no `DECISIONS.md` row.
* Option B adds a key the reference does not have; that row is mandatory.
* Neither option may change compiled output bytes for any target that does not
  use `external`.
* `krab compile --force` behaviour is unchanged.

### Exclusions, deliberate

* **F2** (only the executable bit is hashed, so `0644` to `0640` does not
  invalidate) and **F3** (symlinks fingerprinted by resolved content, never by
  link target path). Same family, different mechanism, in `digest.rs` rather
  than the dispatch. Include them only if D3's answer produces a general
  definition of what counts as an input; otherwise file separately.
* Network fetches, per the scope note above.
* The observation that **only `kadet` produces `ItemRecord`s**, so a stale target
  re-runs `jinja2`, `copy` and `external` in full. Adding the freshness field
  makes this visible and is the natural next step, but per-item reuse for other
  input types is its own change.
* Any environment-variable recording for `external`. The command sees `PATH` and
  `HOME` from the ambient environment; capturing that is a larger design
  question.

### Prerequisites

* **D3 answered.** Without it, the implementation is a coin flip.
* **M2 merged** (`TBD-compile-golden-fixture.md`). This plan's regression test
  needs somewhere to live, and M2 builds the harness that can drive
  `compile()`. Writing a one-off test here instead would duplicate that work.
* Check at start that no open PR touches `native.rs` or `inputs/`.

## Milestones and work

### Milestone 1: the declaration exists and every input fills it

Reviewable outcome: a freshness declaration on `Item`, filled at all five
dispatch arms, with **no behaviour change yet**. All existing tests pass
untouched.

```rust
pub enum Freshness {
    /// The input reports every path it reads into `Reads`.
    Tracked,
    /// The input cannot report its reads; the target is stale every run.
    AlwaysStale,
}
```

Fill it honestly rather than optimistically: `kadet` and `jinja2` and `copy`
are `Tracked`; `external` is `AlwaysStale`; `remove` needs a judgement, since it
deletes rather than reads and arguably has no inputs at all. Record the `remove`
reasoning in Discoveries.

Do this as a separate commit from Milestone 2, so the reviewer can see that the
declaration is inert before the behaviour changes.

### Milestone 2: the declaration takes effect (F1 closed)

Reviewable outcome: a target with an `AlwaysStale` item recompiles every run,
and `--explain` says why.

Under Option A this is the whole change. Under Option B, `external` becomes
`Tracked` when it declares `input_paths` and `AlwaysStale` otherwise, and
`external::compile` gains the `reads` parameter the other arms already have.

The `--explain` string matters more than it looks. A user who sees a target
recompile every run with no stated reason will file a bug. Something explicit,
along the lines of *"always recompiled: the `external` input cannot report what
it reads"*, with the issue number.

### Milestone 3: the regression test

Reviewable outcome: the reproduction from the top of this plan, encoded in the
M2 fixture harness, failing on the baseline and passing after.

The test must assert the **output contents**, not merely the run's up-to-date
count. The defect is that the file is stale, and a test that only checks "was it
recompiled" would pass against a change that recompiles but writes nothing.

Under Option A, also assert the negative: a target with no `external` item is
still reported up to date on an unchanged second run. Making everything stale
would also close F1, and would be wrong.

### Milestone 4: record it

Reviewable outcome: a `docs/DECISIONS.md` row if Option B was chosen; an update
to `docs/DESIGN.md`'s *"exact invalidation or nothing"* paragraph either way,
because that sentence is currently false for `external` and should say what the
new rule is; and a comment on issue #16 stating that the fetch half remains
open.

## Validation and recovery

### Commands

```sh
cargo test -p krab-compile
cargo test --locked --no-fail-fast
cargo clippy --all-targets --locked
cargo fmt --all --check
```

Manual reproduction, before and after, which is what actually convinced me the
defect was real:

```sh
# in a throwaway inventory with an external item whose script reads a data file
krab compile                    # note the output contents
printf 'CHANGED\n' > the-data-file
krab compile --explain          # before: "up to date"; after: recompiled
cat compiled/<target>/...       # before: stale; after: CHANGED
```

Required environment: a Rust toolchain and `sh`. No Python, no network.

Note: on a machine with commit signing enabled the full suite currently fails in
`fetch.rs` for an unrelated reason (issue #98, fixed by PR #99). Until that
lands, use `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null`.

### Expected results

* The new test fails on the baseline commit. **Verify that direction
  explicitly.**
* No existing test changes behaviour. If `native.rs`'s `reuse_needs_every_input_unchanged`
  or any fixture output moves, something is wrong.
* A compile of an inventory with no `external` items produces a byte-identical
  tree and the same up-to-date counts as before.

### Coverage check

Assert that the freshness field is exercised for more than one variant. A test
that only ever sees `AlwaysStale` would pass if `Tracked` were broken.

### Recovery

Option A reverts in one commit with no residue. Option B does not: once the
declaration key is documented and users put it in their inventories, removing it
is a breaking change. That asymmetry is the main argument for A and should be
stated in the PR description so the reviewer weighs it deliberately.

One effect neither option rolls back: a compiled tree that is currently stale
because of F1 stays stale until the next compile. The first run after this
change will recompile more than usual and may produce a large diff in
`compiled/`. That is the bug being corrected, not a regression. Say so in the
release note, because someone will see a big diff and worry.

### Fallback

If Option B is chosen and the declaration turns out to need more than a list of
paths (environment variables, tool versions, anything ambient), **stop and ship
Option A**. A half-declared input is worse than an honestly always-stale one,
because it looks tracked. Record the reason in Discoveries.

## Progress

- [ ] **D3 answered** (blocking)
- [ ] M2 (`TBD-compile-golden-fixture.md`) merged
- [ ] Confirm no open PR touches `native.rs` or `inputs/`
- [ ] M1: `Freshness` on `Item`, filled at all five arms, inert
- [ ] M1: record the `remove` judgement
- [ ] M2: declaration takes effect; `--explain` states the reason
- [ ] M2 (Option B only): `external::compile` gains the `reads` parameter
- [ ] M3: regression test asserting output **contents**, failing on baseline
- [ ] M3: negative case, unchanged targets still up to date
- [ ] M4: `docs/DESIGN.md` "exact invalidation or nothing" paragraph corrected
- [ ] M4: `docs/DECISIONS.md` row, if Option B
- [ ] M4: comment on #16 that the network-fetch half remains open
- [ ] Full suite, clippy, fmt clean

## Discoveries and decisions

*Nothing recorded yet. This plan has not been executed.*

What is already known is in Purpose and Context above; it is not repeated
here.

Record here: the D3 answer and its reasoning; the `remove` judgement; whether
the `--explain` wording survived review; and anything that made Option B look
larger than estimated.

## Outcomes and handoff

*Not started. Nothing to record.*

On completion, record: which option was implemented and why; the exact test
names and output; confirmation that the regression test fails on the baseline;
and the remaining limitations, which at minimum include F2, F3, environment
variables read by `external` commands, and the network-fetch half of #16.

The natural next work is F2 and F3 if D3 produced a general definition of an
input, and per-item records for the non-`kadet` input types, which the freshness
field makes visible.
